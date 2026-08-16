//! Copying a finished recording into MP4 without decoding it.
//!
//! Clipped records Matroska because it survives an interrupted recording
//! (ADR 0001), and ADR 0001 names the consequence: MKV is not accepted by every
//! upload target, chat client or editor, so a user needs an MP4 — without
//! waiting for a re-encode and without losing quality to one.
//!
//! # What a remux is
//!
//! Copying the *coded packets* into a different container. No decoder runs, no
//! encoder runs, and the bytes that come out of the source are the bytes that go
//! into the destination; only the boxes around them change. That is what makes
//! it fast and what makes it lossless, and both of those are properties to
//! check rather than assume — `crates/muxer/tests/mp4_remux.rs` compares the
//! payload of every packet of the source against the payload of every packet of
//! the result.
//!
//! # Why this does not go through `MkvWriter`
//!
//! A saved replay clip is written by [`MkvWriter`](crate::MkvWriter) rather than
//! by a second muxer, because a clip really is a recording made of the same
//! encoded packets (AGENTS.md section 55). A remux is not that shape, and
//! forcing it into it would lose things:
//!
//! - [`RecordingLayout`](crate::RecordingLayout) describes what *Clipped*
//!   records — one picture, some audio, six codecs between them. A file being
//!   remuxed may hold a codec no encoder here produces, and refusing it would be
//!   refusing a file the container can carry perfectly well.
//! - It describes a track by the handful of fields a recorder needs to set. A
//!   source stream carries more than that — pixel format, profile and level,
//!   colour signalling, a channel layout that is not the default for its channel
//!   count — and rebuilding a track from the smaller description would quietly
//!   drop the rest.
//!
//! So the track description is copied wholesale with
//! `avcodec_parameters_copy`, which is what "copy the stream" means, and what is
//! shared with the writer is the machinery that has to be identical: the
//! container objects and their ownership rules ([`crate::av`]) and the decode
//! order policy ([`crate::timeline`]).
//!
//! # What MP4 cannot carry, and what happens then
//!
//! Matroska accepts nearly anything; MP4 stores only what has a registered
//! mapping into it. So each stream is put to the linked FFmpeg — *this* build's
//! MP4 muxer, not a list written down here that would drift out of date — and
//! the answer decides what happens:
//!
//! | Stream | Answer | What happens |
//! | --- | --- | --- |
//! | Picture or sound | carried | copied into the MP4 |
//! | Picture or sound | not carried | the remux is refused before anything is created |
//! | Anything else | carried | copied into the MP4 |
//! | Anything else | not carried | left out, and named in the plan and the log |
//!
//! Refusing rather than dropping is the important half. A file that is missing
//! one of its five audio tracks looks exactly like a file that had four, and the
//! person who finds out is the one who uploaded it (AGENTS.md sections 15 and
//! 45). Subtitles, attachments and data streams are a different matter: they are
//! not the recording, and a plan that says "this font will not be in the MP4" is
//! a better outcome than a refusal.
//!
//! [`Mp4Plan::inspect`] answers all of that *without writing anything*, so a
//! caller can warn somebody before they wait for a copy that is going to be
//! refused, or before they accept one that will not be complete.
//!
//! # A copy that carries one sound track
//!
//! [`remux_to_mp4_carrying`] is the same copy with one of the recording's audio
//! tracks named, and every other one left out. It is here because of something
//! a browser cannot do: `HTMLMediaElement.audioTracks` is not implemented in
//! Chromium, so a `<video>` handed a multi-track file plays whichever track its
//! demuxer reaches first and offers no way off it. A window that lets somebody
//! hear the microphone track on its own therefore has to be handed a file that
//! holds the microphone track and nothing else — the selection happens on the
//! way out of the recorder rather than in the element
//! ([issue #304](https://github.com/wildware-uk/clipped/issues/304),
//! `docs/desktop-ui.md`).
//!
//! A track left out that way is [`Carriage::NotChosen`] rather than a loss: it
//! does not make the copy refuse, and it is what was asked for. The plan still
//! lists every track of the source, so the caller can say what it made.
//!
//! # The source is never touched
//!
//! `avformat_open_input` opens for reading and nothing here ever opens the
//! source any other way, so a remux — including one that fails half-way — leaves
//! the recording byte for byte as it found it (AGENTS.md section 56). The tests
//! hash the source before and after, on the failing paths as well as the
//! succeeding one.
//!
//! # Timestamps
//!
//! The part that is worth being careful about, because a naive copy is where
//! audio and video drift apart. `docs/muxing.md` sets out the whole of it; in
//! short, the source's timestamps are rescaled into the destination's units and
//! otherwise left exactly as they are, including a first timestamp that is not
//! zero and a decode timestamp that precedes its own presentation timestamp.
//! MP4 has an edit list for the first and a composition offset for the second,
//! and FFmpeg's MP4 muxer writes both.

use core::fmt;
use core::ptr;
use core::time::Duration;
use std::error::Error;
use std::ffi::{c_int, CStr};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clipped_logging::RedactedPath;
use rusty_ffmpeg::ffi;
use tracing::{info, warn};

use crate::av::{InputContext, OutputContext, PacketSlot};
use crate::error::{AvError, MuxError};
use crate::linkage;
use crate::timeline::DecodeOrder;

/// FFmpeg's name for the MP4 muxer, as `ffmpeg -muxers` lists it.
///
/// One spelling, used to check the linked build has the muxer, to allocate the
/// context and to ask what that muxer can carry, so the three cannot drift into
/// asking about one container and writing another.
const MP4_MUXER: &CStr = c"mp4";

/// FFmpeg's `AVERROR_EOF`, which is `FFERRTAG('E','O','F',' ')`.
///
/// The binding does not carry it: it is a macro over another macro, and
/// `bindgen` expands neither.
const AVERROR_EOF: c_int = -0x2046_4F45;

/// FFmpeg's `AV_NOPTS_VALUE`, the timestamp that means "there is none".
const AV_NOPTS_VALUE: i64 = i64::MIN;

/// What kind of track a source stream is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrackKind {
    /// A picture track.
    Video,
    /// A sound track.
    Audio,
    /// A subtitle track.
    Subtitle,
    /// Anything else: an attached font, a data stream, a timecode track.
    Ancillary,
}

impl TrackKind {
    /// Whether this is the recording itself, rather than something alongside it.
    ///
    /// The distinction the refusal policy turns on: losing picture or sound
    /// silently produces a file that looks finished and is not, while losing an
    /// attached font produces a file that is exactly as watchable.
    #[must_use]
    pub const fn is_media(self) -> bool {
        matches!(self, Self::Video | Self::Audio)
    }

    /// The kind FFmpeg's media type stands for.
    fn from_media_type(media_type: ffi::AVMediaType) -> Self {
        match media_type {
            ffi::AVMEDIA_TYPE_VIDEO => Self::Video,
            ffi::AVMEDIA_TYPE_AUDIO => Self::Audio,
            ffi::AVMEDIA_TYPE_SUBTITLE => Self::Subtitle,
            _ => Self::Ancillary,
        }
    }
}

impl fmt::Display for TrackKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Video => "video track",
            Self::Audio => "audio track",
            Self::Subtitle => "subtitle track",
            Self::Ancillary => "ancillary track",
        })
    }
}

/// What becomes of one track of the source when it is remuxed into MP4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Carriage {
    /// The coded packets are copied into the MP4 unchanged.
    Copied,
    /// The caller asked for a copy carrying one audio track, and this is not it.
    ///
    /// Different from [`Self::CodecUnsupported`] in the one way that matters:
    /// nothing is wrong. The track could have been carried and was not asked
    /// for, so it is a track left out rather than a recording that cannot be
    /// stored, and it does not make [`remux_to_mp4_carrying`] refuse. See
    /// [`AudioTracks`].
    NotChosen,
    /// MP4 has no registered mapping for this codec in the linked FFmpeg build,
    /// so the track cannot be stored at all.
    CodecUnsupported,
}

impl Carriage {
    /// Whether the track survives into the MP4.
    #[must_use]
    pub const fn is_copied(self) -> bool {
        matches!(self, Self::Copied)
    }
}

/// Which of a recording's sound tracks a copy is to carry.
///
/// The whole reason this is a choice: **a media element cannot select an audio
/// track.** `HTMLMediaElement.audioTracks` is not implemented in Chromium, so a
/// multi-track file handed to a `<video>` plays whichever track its demuxer
/// reaches first and offers no way off it. A player that lets somebody hear the
/// microphone track on its own therefore has to be given a file that holds that
/// track and no other — the choice happens here, on the way out, rather than in
/// the element ([issue #304](https://github.com/wildware-uk/clipped/issues/304),
/// `docs/desktop-ui.md`).
///
/// Picture is never affected: the video track is carried either way, and it is
/// copied rather than re-encoded in both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioTracks {
    /// Every sound track the source holds, which is what an export wants.
    #[default]
    All,
    /// One source stream, named by the index the container declares it at.
    ///
    /// It must be a sound track of that source, or the copy is refused with
    /// [`RemuxError::NoSuchAudioTrack`] — an index that turned out to be the
    /// video track, or one past the end, would otherwise produce a silent file
    /// that looks exactly like a recording which never had sound.
    Only(usize),
}

impl AudioTracks {
    /// Whether a source stream at this index is to be carried.
    ///
    /// Only sound tracks are ever excluded. A subtitle or an attached font is
    /// governed by what MP4 can store, exactly as it is for an export.
    const fn carries(self, index: usize, kind: TrackKind) -> bool {
        match self {
            Self::All => true,
            Self::Only(chosen) => !matches!(kind, TrackKind::Audio) || chosen == index,
        }
    }
}

/// One track of the source, and what an MP4 made from it would do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTrack {
    index: usize,
    kind: TrackKind,
    codec: String,
    name: Option<String>,
    language: Option<String>,
    default: bool,
    carriage: Carriage,
}

impl PlannedTrack {
    /// Which stream of the source this is, counting from zero in the order the
    /// container declares them.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Whether this is picture, sound, or something alongside them.
    #[must_use]
    pub const fn kind(&self) -> TrackKind {
        self.kind
    }

    /// The codec, as FFmpeg names it: `h264`, `opus`, `pcm_s16le`.
    #[must_use]
    pub fn codec(&self) -> &str {
        &self.codec
    }

    /// The track's name, where the source gave it one.
    ///
    /// This is what an editor shows instead of `Audio 3`, and it is the reason
    /// the recording container is Matroska (ADR 0001). MP4 keeps it too, in a
    /// different place — see [`Mp4Plan`].
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The track's language tag, where the source gave it one.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Whether this is the track a player should choose on its own.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.default
    }

    /// What an MP4 would do with it.
    #[must_use]
    pub const fn carriage(&self) -> Carriage {
        self.carriage
    }

    /// What this track's absence costs, in a sentence, where it is absent.
    ///
    /// [`None`] for a track that is carried. The two absences are worded
    /// differently on purpose: one is something the container cannot do, and
    /// the other is what the caller asked for.
    fn loss(&self) -> Option<String> {
        match self.carriage {
            Carriage::Copied => None,
            Carriage::NotChosen => Some(format!("{self} was not the track asked for")),
            Carriage::CodecUnsupported => Some(format!("{self} cannot be stored in MP4")),
        }
    }
}

impl fmt::Display for PlannedTrack {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {} ({})", self.kind, self.index, self.codec)?;
        if let Some(name) = &self.name {
            write!(formatter, " '{name}'")?;
        }
        Ok(())
    }
}

/// What remuxing a recording into MP4 would produce, and what it would cost.
///
/// Built by [`Mp4Plan::inspect`], which reads the source and writes nothing, so
/// that a caller can tell somebody what they are about to get *before* they wait
/// for it (AGENTS.md section 45). [`remux_to_mp4`] makes the same plan and
/// returns it with the summary, so a caller that skipped the inspection still
/// learns what happened.
///
/// # What survives, and where it goes
///
/// Every track MP4 can carry keeps its coded packets byte for byte, its
/// timestamps, its language tag and its default-track flag. Its **name moves**:
/// Matroska stores it in the track entry's `Name` element, and MP4 stores it in
/// a `udta`/`name` box on the track, which `ffprobe` reports as the `name` tag
/// rather than the `title` tag it reports for Matroska. The name is not lost —
/// but a tool that only looks for `title` will not find it, which is worth
/// knowing before somebody concludes the remux dropped it.
///
/// Chapters are not carried; [`Self::chapters`] says how many were there so a
/// caller can say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Plan {
    source: PathBuf,
    tracks: Vec<PlannedTrack>,
    chapters: usize,
}

impl Mp4Plan {
    /// Reads `source` and works out what an MP4 made from it would contain.
    ///
    /// The file is opened for reading and closed again; nothing is written
    /// anywhere.
    ///
    /// # Errors
    ///
    /// [`RemuxError::SourceUnreadable`] when the file cannot be opened or has no
    /// streams libavformat can describe, and
    /// [`RemuxError::SourceNotRepresentable`] for a path that is not valid
    /// Unicode.
    pub fn inspect(source: &Path) -> Result<Self, RemuxError> {
        let input = open_source(source)?;
        Self::read(&input, source, AudioTracks::All)
    }

    /// The plan for an already-open source, carrying the sound the caller asked
    /// for.
    fn read(input: &InputContext, source: &Path, audio: AudioTracks) -> Result<Self, RemuxError> {
        let mut tracks = Vec::with_capacity(input.stream_count());
        for index in 0..input.stream_count() {
            let Some(stream) = input.stream(index) else {
                break;
            };
            tracks.push(describe(index, stream, audio));
        }

        if let AudioTracks::Only(chosen) = audio {
            let is_audio = tracks
                .iter()
                .any(|track| track.index == chosen && matches!(track.kind, TrackKind::Audio));
            if !is_audio {
                return Err(RemuxError::NoSuchAudioTrack {
                    source: source.to_path_buf(),
                    index: chosen,
                });
            }
        }

        if tracks.is_empty() {
            return Err(RemuxError::SourceUnreadable {
                source: source.to_path_buf(),
                error: AvError::new(-(ffi::EINVAL as i32)),
            });
        }

        Ok(Self {
            source: source.to_path_buf(),
            tracks,
            chapters: input.chapter_count(),
        })
    }

    /// The file this was read from.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Every track of the source, in the order the container declares them.
    #[must_use]
    pub fn tracks(&self) -> &[PlannedTrack] {
        &self.tracks
    }

    /// How many chapters the source holds, none of which are carried.
    #[must_use]
    pub const fn chapters(&self) -> usize {
        self.chapters
    }

    /// The picture and sound tracks MP4 cannot carry.
    ///
    /// While this is not empty, [`remux_to_mp4`] refuses: an MP4 missing one of
    /// a recording's audio tracks is indistinguishable from one that never had
    /// it.
    ///
    /// A track the caller *chose* to leave out ([`Carriage::NotChosen`]) is not
    /// in here, and that is the distinction the two variants exist for: an
    /// export that would silently drop sound is refused, and a player's copy of
    /// one named track is exactly what was asked for.
    #[must_use]
    pub fn blocking(&self) -> Vec<&PlannedTrack> {
        self.tracks
            .iter()
            .filter(|track| track.kind.is_media() && track.carriage == Carriage::CodecUnsupported)
            .collect()
    }

    /// Whether an MP4 made from this source would hold everything it does.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        self.chapters == 0 && self.tracks.iter().all(|track| track.carriage.is_copied())
    }

    /// Everything the MP4 will not contain, phrased for somebody to read.
    ///
    /// Empty when nothing is lost. This is what a caller shows before starting,
    /// and it deliberately includes the tracks that would make the remux refuse
    /// as well as the ones it would merely leave out, because the person being
    /// asked wants one list rather than two.
    #[must_use]
    pub fn losses(&self) -> Vec<String> {
        self.losses_where(|_| true)
    }

    /// The losses nobody asked for.
    ///
    /// What [`Self::losses`] holds, minus the tracks the caller chose to leave
    /// out. That is what belongs in the log: a warning per unchosen track would
    /// mean three lines every time somebody played a recording, saying that the
    /// tracks they did not select are not in the copy made because they did not
    /// select them.
    fn unasked_losses(&self) -> Vec<String> {
        self.losses_where(|track| track.carriage != Carriage::NotChosen)
    }

    /// The losses of the tracks a predicate admits, and the chapters.
    fn losses_where(&self, admit: impl Fn(&PlannedTrack) -> bool) -> Vec<String> {
        let mut losses: Vec<String> = self
            .tracks
            .iter()
            .filter(|track| admit(track))
            .filter_map(PlannedTrack::loss)
            .collect();
        if self.chapters > 0 {
            losses.push(format!(
                "{} chapter marks are not carried into MP4",
                self.chapters
            ));
        }
        losses
    }
}

impl fmt::Display for Mp4Plan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let carried = self
            .tracks
            .iter()
            .filter(|track| track.carriage.is_copied())
            .count();
        write!(
            formatter,
            "{carried} of {} tracks carried into MP4",
            self.tracks.len()
        )?;
        for loss in self.losses() {
            write!(formatter, "; {loss}")?;
        }
        Ok(())
    }
}

/// What a finished remux turned out to have copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemuxSummary {
    plan: Mp4Plan,
    destination: PathBuf,
    packets: u64,
    bytes: u64,
    duration: Duration,
    elapsed: Duration,
    timestamps_forced_monotonic: u64,
    timestamps_presented_before_decoded: u64,
}

impl RemuxSummary {
    /// The MP4 that was written.
    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// What the source held and what became of it.
    #[must_use]
    pub const fn plan(&self) -> &Mp4Plan {
        &self.plan
    }

    /// How many packets were copied, across every carried track.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    /// How many bytes of coded media were copied, before the container's own
    /// overhead.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.bytes
    }

    /// How much media the result holds: from the earliest packet to the end of
    /// the latest one.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// How long the copy took.
    ///
    /// Measured rather than estimated, and worth reporting: the whole argument
    /// for remuxing instead of re-encoding is that this number is small, and a
    /// caller that wants to say so should be quoting a measurement (AGENTS.md
    /// section 18).
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Packets whose decode timestamps did not advance and were forced forward.
    ///
    /// Zero for anything Clipped recorded, because the writer already enforces
    /// this. Anything else means the source's own decode order was broken, and
    /// the count is how badly.
    #[must_use]
    pub const fn timestamps_forced_monotonic(&self) -> u64 {
        self.timestamps_forced_monotonic
    }

    /// Packets that would have been presented before they were decoded.
    #[must_use]
    pub const fn timestamps_presented_before_decoded(&self) -> u64 {
        self.timestamps_presented_before_decoded
    }

    /// How many packets needed a timestamp changed to keep the file valid.
    #[must_use]
    pub const fn timestamps_corrected(&self) -> u64 {
        self.timestamps_forced_monotonic + self.timestamps_presented_before_decoded
    }
}

impl fmt::Display for RemuxSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} packets over {:.3}s copied in {:.3}s, {} timestamps corrected",
            self.packets,
            self.duration.as_secs_f64(),
            self.elapsed.as_secs_f64(),
            self.timestamps_corrected()
        )
    }
}

/// Copies `source` into `destination` as an MP4, without decoding anything.
///
/// The coded packets are copied unchanged, so the result is the same picture and
/// the same sound as the source, in a container more things accept. Nothing is
/// re-encoded and nothing is re-timed beyond the change of units the destination
/// counts in.
///
/// The source is opened for reading only and is not modified, whether this
/// succeeds or fails.
///
/// # Errors
///
/// - [`RemuxError::SourceUnreadable`] and [`RemuxError::SourceNotRepresentable`]
///   when the recording cannot be read. Nothing is created.
/// - [`RemuxError::MediaNotCarried`] when MP4 cannot store one of the source's
///   picture or sound tracks. Nothing is created: the check happens before the
///   destination is opened, precisely so that a refusal does not leave a stub
///   behind.
/// - [`RemuxError::Output`] when writing fails — a destination that already
///   exists ([`MuxError::OutputExists`]; nothing here overwrites a file), a
///   directory that is not there, a disk that filled up. Anything this call
///   created is removed again, so a caller retrying the same name is not told
///   it is about to overwrite its own failed attempt.
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use clipped_muxer::{remux_to_mp4, Mp4Plan};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let source = Path::new("recording.mkv");
///
/// // What will this cost? Answered without writing anything.
/// let plan = Mp4Plan::inspect(source)?;
/// for loss in plan.losses() {
///     eprintln!("warning: {loss}");
/// }
///
/// let summary = remux_to_mp4(source, Path::new("recording.mp4"))?;
/// println!("{summary}");
/// # Ok(())
/// # }
/// ```
pub fn remux_to_mp4(source: &Path, destination: &Path) -> Result<RemuxSummary, RemuxError> {
    remux_to_mp4_carrying(source, destination, AudioTracks::All)
}

/// Copies `source` into `destination` as an MP4, carrying the sound named.
///
/// Everything [`remux_to_mp4`] does — the packets are copied rather than
/// decoded, the source is opened for reading only, a destination that exists is
/// refused — with one difference: [`AudioTracks::Only`] leaves every other sound
/// track out.
///
/// That exists for the player and not for the export. A `<video>` cannot choose
/// an audio track, so hearing one track of a recording on its own means being
/// handed a file that holds one track
/// ([issue #304](https://github.com/wildware-uk/clipped/issues/304)). The
/// returned [`Mp4Plan`] still describes **every** track of the source, with the
/// ones left out marked [`Carriage::NotChosen`], so a caller can say what it
/// made rather than having to remember what it asked for.
///
/// # Errors
///
/// Everything [`remux_to_mp4`] returns, and one more:
/// [`RemuxError::NoSuchAudioTrack`] when the chosen index is not a sound track
/// of the source. Nothing is created — the check happens before the destination
/// is opened, because the alternative is a file with no sound in it that looks
/// exactly like a recording which never had any.
pub fn remux_to_mp4_carrying(
    source: &Path,
    destination: &Path,
    audio: AudioTracks,
) -> Result<RemuxSummary, RemuxError> {
    let started = Instant::now();

    let input = open_source(source)?;
    let plan = Mp4Plan::read(&input, source, audio)?;

    let blocking = plan.blocking();
    if !blocking.is_empty() {
        return Err(RemuxError::MediaNotCarried {
            source: source.to_path_buf(),
            tracks: blocking.into_iter().cloned().collect(),
        });
    }

    // Said once per loss, before the copy rather than after it, so that a
    // diagnostic log explains a short MP4 without anyone having to reproduce it.
    // A static message with the detail in a field, which is the habit
    // docs/logging.md sets.
    for loss in plan.unasked_losses() {
        warn!(
            source = %RedactedPath::new(source),
            loss = %loss,
            "part of the recording will not be in the MP4"
        );
    }

    let copied = write_mp4(&input, &plan, destination).map_err(|source| RemuxError::Output {
        destination: destination.to_path_buf(),
        source,
    })?;

    let summary = RemuxSummary {
        plan,
        destination: destination.to_path_buf(),
        packets: copied.packets,
        bytes: copied.bytes,
        duration: copied.duration(),
        elapsed: started.elapsed(),
        timestamps_forced_monotonic: copied.timestamps_forced_monotonic,
        timestamps_presented_before_decoded: copied.timestamps_presented_before_decoded,
    };

    info!(
        source = %RedactedPath::new(source),
        destination = %RedactedPath::new(destination),
        packets = summary.packets,
        bytes = summary.bytes,
        duration_ms = summary.duration.as_millis(),
        elapsed_ms = summary.elapsed.as_millis(),
        lossless = summary.plan.is_lossless(),
        "recording remuxed to MP4"
    );

    Ok(summary)
}

/// Opens the source for reading, reporting the path the way the rest of the
/// crate does.
fn open_source(source: &Path) -> Result<InputContext, RemuxError> {
    let Some(text) = source.to_str() else {
        return Err(RemuxError::SourceNotRepresentable {
            source: source.to_path_buf(),
        });
    };

    // `file:` rather than the bare path so that the file protocol is used
    // whatever the path looks like, for the reason `MkvWriter::create` gives:
    // FFmpeg picks a protocol from the text before the first colon.
    InputContext::open(&format!("file:{text}")).map_err(|error| RemuxError::SourceUnreadable {
        source: source.to_path_buf(),
        error,
    })
}

/// Reads one source stream into the plan's description of it.
fn describe(index: usize, stream: *mut ffi::AVStream, audio: AudioTracks) -> PlannedTrack {
    // SAFETY: `stream` came from the input context's own array and points at a
    // stream that context owns and outlives this call. `codecpar` is allocated
    // with the stream and is never null, and `disposition` is a plain integer.
    let (media_type, codec_id, default) = unsafe {
        let parameters = (*stream).codecpar;
        (
            (*parameters).codec_type,
            (*parameters).codec_id,
            ((*stream).disposition & ffi::AV_DISPOSITION_DEFAULT as c_int) != 0,
        )
    };

    let kind = TrackKind::from_media_type(media_type);

    PlannedTrack {
        index,
        kind,
        codec: codec_name(codec_id),
        name: metadata(stream, c"title"),
        language: metadata(stream, c"language"),
        default,
        // What the container can hold is asked first, so that a track which is
        // both unchosen and unstorable is reported as unchosen: it is the
        // answer that is true of this copy, and the one that does not send
        // somebody looking for a codec problem in a file they never asked for.
        carriage: if !audio.carries(index, kind) {
            Carriage::NotChosen
        } else if mp4_can_carry(codec_id) {
            Carriage::Copied
        } else {
            Carriage::CodecUnsupported
        },
    }
}

/// The name FFmpeg's own descriptor table has for a codec.
fn codec_name(codec_id: ffi::AVCodecID) -> String {
    // SAFETY: `avcodec_get_name` accepts any value of `AVCodecID` and returns a
    // pointer to a string constant inside libavcodec — the descriptor's name, or
    // a literal for an unknown identifier — which is NUL-terminated and lives for
    // the process.
    let name = unsafe { CStr::from_ptr(ffi::avcodec_get_name(codec_id)) };
    name.to_string_lossy().into_owned()
}

/// One metadata entry of a stream, where it has one.
fn metadata(stream: *mut ffi::AVStream, key: &CStr) -> Option<String> {
    // SAFETY: `stream` is live and owns its metadata dictionary. `av_dict_get`
    // reads the NUL-terminated key and returns either null or a pointer to an
    // entry the dictionary owns, whose `value` is a NUL-terminated string. The
    // string is copied here and the entry is not kept.
    unsafe {
        let entry = ffi::av_dict_get((*stream).metadata, key.as_ptr(), ptr::null(), 0);
        if entry.is_null() || (*entry).value.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*entry).value)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Whether the linked build's MP4 muxer has a mapping for this codec.
///
/// Asked of FFmpeg rather than answered from a table written down here. A list
/// of codecs would be a second source of truth that goes stale the moment the
/// pinned build moves — FFmpeg 8 added uncompressed audio to MP4 as `ipcm`, for
/// instance, which an older list would still be refusing — and being wrong in
/// that direction means refusing a file that would have worked, while being
/// wrong in the other means a half-open MP4 and an error four calls further on.
pub(crate) fn mp4_can_carry(codec_id: ffi::AVCodecID) -> bool {
    // SAFETY: `av_guess_format` reads the NUL-terminated short name and returns
    // either null or a pointer to an `AVOutputFormat` that is a static inside
    // libavformat, so it lives for the process.
    let format = unsafe { ffi::av_guess_format(MP4_MUXER.as_ptr(), ptr::null(), ptr::null()) };
    if format.is_null() {
        return false;
    }

    // SAFETY: `format` is the live static above. `avformat_query_codec` accepts
    // any `AVCodecID` and reads only the descriptor. It answers 1 for a codec the
    // muxer can store, 0 for one it cannot, and a negative error for a muxer that
    // has no opinion — which is not an answer to build a file on, so only 1
    // counts.
    let answer =
        unsafe { ffi::avformat_query_codec(format, codec_id, ffi::FF_COMPLIANCE_NORMAL as c_int) };
    answer == 1
}

/// What the copying loop counted.
#[derive(Debug, Default)]
struct Copied {
    packets: u64,
    bytes: u64,
    /// The earliest presentation time written, in seconds.
    ///
    /// Both ends are optional rather than starting at zero, because a source's
    /// timeline need not contain zero at all: a stream whose first timestamp is
    /// negative would otherwise be reported as longer than it is.
    first_seconds: Option<f64>,
    /// The end of the latest packet written, in seconds.
    last_seconds: Option<f64>,
    timestamps_forced_monotonic: u64,
    timestamps_presented_before_decoded: u64,
}

impl Copied {
    /// How much media the result holds.
    fn duration(&self) -> Duration {
        let (Some(first), Some(last)) = (self.first_seconds, self.last_seconds) else {
            return Duration::ZERO;
        };
        Duration::try_from_secs_f64((last - first).max(0.0)).unwrap_or_default()
    }
}

/// One carried stream, and everything needed to move a packet across.
#[derive(Debug)]
struct CarriedStream {
    output_index: c_int,
    source_time_base: ffi::AVRational,
    output_time_base: ffi::AVRational,
    order: DecodeOrder,
}

/// The output streams, and which of them each source stream became.
///
/// The second half is indexed by source stream index so that the read loop can
/// route a packet without searching: a five-minute recording is tens of
/// thousands of packets, and a linear scan per packet would be the muxer's own
/// contribution to the frame budget.
#[derive(Debug)]
struct StreamMap {
    carried: Vec<CarriedStream>,
    destinations: Vec<Option<usize>>,
}

/// Creates the MP4 and copies every carried packet into it.
///
/// Separated from [`remux_to_mp4`] so that every failure after the destination
/// exists leaves through one place, which is where it is removed again.
fn write_mp4(input: &InputContext, plan: &Mp4Plan, destination: &Path) -> Result<Copied, MuxError> {
    // `to_str` cannot fail: the constant is an ASCII literal. Written as a
    // fallback rather than an unwrap so that a remux never ends in a panic
    // (AGENTS.md section 15).
    let Ok(mp4) = MP4_MUXER.to_str() else {
        return Err(MuxError::ContainerUnsupported);
    };
    if !linkage::muxer_available(mp4) {
        return Err(MuxError::ContainerUnsupported);
    }

    // Checked rather than left to the open, for the reason `MkvWriter::create`
    // gives: every avio mode that would create the file also truncates one, and
    // truncating is how a tool destroys footage nobody can get back (AGENTS.md
    // section 56).
    if matches!(destination.try_exists(), Ok(true)) {
        return Err(MuxError::OutputExists {
            path: destination.to_path_buf(),
        });
    }

    let Some(destination_text) = destination.to_str() else {
        return Err(MuxError::PathNotRepresentable {
            path: destination.to_path_buf(),
        });
    };

    let mut format = OutputContext::allocate(MP4_MUXER)?;
    let mut streams = add_streams(input, plan, &format)?;

    format.open_output(&format!("file:{destination_text}"))?;

    // The destination exists from here on, so a failure has to take it away
    // again.
    copy_packets(input, &format, &mut streams, destination).inspect_err(|_| {
        // The context, and with it the open file, was dropped by the call above;
        // Windows would refuse to remove it otherwise.
        if let Err(error) = std::fs::remove_file(destination) {
            warn!(
                path = %RedactedPath::new(destination),
                %error,
                "a remux failed and the partial MP4 left behind could not be removed; that \
                 name cannot be remuxed to until it is"
            );
        }
    })
}

/// Declares every carried track on the output context.
fn add_streams(
    input: &InputContext,
    plan: &Mp4Plan,
    format: &OutputContext,
) -> Result<StreamMap, MuxError> {
    let mut carried = Vec::new();
    let mut destinations = vec![None; input.stream_count()];

    for track in plan.tracks() {
        if !track.carriage.is_copied() {
            continue;
        }
        let Some(source) = input.stream(track.index()) else {
            continue;
        };

        // SAFETY: the context is live and owns whatever this returns; passing a
        // null codec is the documented way to add a stream that will be
        // described by its `codecpar` rather than by an encoder.
        let stream = unsafe { ffi::avformat_new_stream(format.as_ptr(), ptr::null()) };
        if stream.is_null() {
            return Err(MuxError::Ffmpeg {
                operation: "adding a track to the MP4",
                source: AvError::new(-(ffi::ENOMEM as i32)),
            });
        }

        // SAFETY: `source` and `stream` are live streams owned by their
        // contexts, and neither `codecpar` is null.
        //
        // `avcodec_parameters_copy` copies the whole description — codec,
        // dimensions, pixel format, profile and level, colour signalling,
        // channel layout, and the out-of-band header, which it duplicates rather
        // than aliases, so the two contexts do not share a buffer. That is what
        // makes this a copy of the stream rather than a re-description of it.
        //
        // `codec_tag` is cleared because a tag is a property of the *container*:
        // whatever Matroska recorded means nothing to MP4, and leaving it would
        // make the muxer either refuse the stream or write a four-character code
        // no MP4 reader knows.
        let code = unsafe {
            let copied = ffi::avcodec_parameters_copy((*stream).codecpar, (*source).codecpar);
            if copied >= 0 {
                (*(*stream).codecpar).codec_tag = 0;
                // A hint. The MP4 muxer replaces it with the timescale it
                // chooses — the sample rate for sound, something derived from
                // the frame rate for picture — and the real value is read back
                // after the header is written.
                (*stream).time_base = (*source).time_base;
                (*stream).avg_frame_rate = (*source).avg_frame_rate;
                (*stream).r_frame_rate = (*source).r_frame_rate;
                // The default-track flag and everything beside it. A recording
                // with five audio tracks has one a player should pick on its
                // own, and losing that produces a file that plays the wrong
                // sound (SPEC.md section 13).
                (*stream).disposition = (*source).disposition;
                // Names and language tags, copied wholesale so that a tag this
                // crate has never heard of is carried rather than dropped. The
                // MP4 muxer writes `title` into the track's `udta`/`name` box
                // and `language` into its media header.
                ffi::av_dict_copy(&mut (*stream).metadata, (*source).metadata, 0)
            } else {
                copied
            }
        };
        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "copying a track's description into the MP4",
                source: AvError::new(code),
            });
        }

        name_the_channel_layout(stream);

        // SAFETY: `index` is filled in by `avformat_new_stream`, and the two
        // time bases are plain fields of live streams. The output's is a hint
        // until the header is written; it is read back below.
        let (output_index, source_time_base) = unsafe { ((*stream).index, (*source).time_base) };

        destinations[track.index()] = Some(carried.len());
        carried.push(CarriedStream {
            output_index,
            source_time_base,
            // Replaced with what the muxer chose, once the header is written.
            output_time_base: source_time_base,
            order: DecodeOrder::default(),
        });
    }

    Ok(StreamMap {
        carried,
        destinations,
    })
}

/// Gives a sound track whose channel arrangement is unstated the conventional
/// one for its channel count.
///
/// Matroska stores a channel *count* and nothing else, so its demuxer reports an
/// unspecified channel order for every uncompressed track. MP4 has no way to
/// write that: its `chnl` box names an arrangement, and FFmpeg's MP4 muxer
/// rejects the track at the trailer — after the whole file has been written —
/// with "unsupported channel layout 2 channels".
///
/// The conventional arrangement for a channel count is what
/// `av_channel_layout_default` gives, it is what
/// [`MkvWriter`](crate::MkvWriter) writes when it creates such a track in the
/// first place, and it is what every player assumes when a file does not say. So
/// this states what was already meant rather than inventing anything: a stereo
/// track becomes front left and front right, which is what two channels of a
/// Windows loopback capture are.
///
/// A track that *did* state its arrangement is left alone, which is the case
/// that matters for a 5.1 source: overwriting a stated layout with the default
/// for its count is how a surround mix ends up with its channels relabelled.
fn name_the_channel_layout(stream: *mut ffi::AVStream) {
    // SAFETY: `stream` is live and owns its parameters, which are never null.
    // `av_channel_layout_uninit` releases whatever the layout held — nothing, for
    // the unspecified order this only acts on — and `av_channel_layout_default`
    // fills it in with a layout that owns no allocation. The context frees the
    // result with the stream.
    unsafe {
        let parameters = (*stream).codecpar;
        if (*parameters).codec_type != ffi::AVMEDIA_TYPE_AUDIO {
            return;
        }
        let channels = (*parameters).ch_layout.nb_channels;
        if (*parameters).ch_layout.order != ffi::AV_CHANNEL_ORDER_UNSPEC || channels <= 0 {
            return;
        }
        ffi::av_channel_layout_uninit(&mut (*parameters).ch_layout);
        ffi::av_channel_layout_default(&mut (*parameters).ch_layout, channels);
    }
}

/// Writes the header, copies every packet, and writes the trailer.
fn copy_packets(
    input: &InputContext,
    format: &OutputContext,
    streams: &mut StreamMap,
    destination: &Path,
) -> Result<Copied, MuxError> {
    write_header(format)?;

    for stream in &mut streams.carried {
        let time_base = format
            .stream_time_base(stream.output_index)
            .ok_or(MuxError::Ffmpeg {
                operation: "reading back the unit the MP4 counts time in",
                source: AvError::new(-(ffi::EINVAL as i32)),
            })?;
        if time_base.num <= 0 || time_base.den <= 0 {
            return Err(MuxError::Ffmpeg {
                operation: "reading back the unit the MP4 counts time in",
                source: AvError::new(-(ffi::EINVAL as i32)),
            });
        }
        stream.output_time_base = time_base;
    }

    let slot = PacketSlot::allocate()?;
    let mut copied = Copied::default();

    loop {
        // SAFETY: both pointers are live and exclusively owned. `av_read_frame`
        // unreferences whatever the packet held and fills it with a reference of
        // its own, which is released either by the write below — which takes
        // ownership — or by the slot's `Drop`.
        let code = unsafe { ffi::av_read_frame(input.as_ptr(), slot.as_ptr()) };
        if code == AVERROR_EOF {
            break;
        }
        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "reading a packet from the recording",
                source: AvError::new(code),
            });
        }

        // SAFETY: the packet is live and filled in; both fields are plain
        // scalars.
        let (source_index, size) = unsafe {
            (
                usize::try_from((*slot.as_ptr()).stream_index).unwrap_or(usize::MAX),
                i64::from((*slot.as_ptr()).size),
            )
        };

        let Some(&Some(position)) = streams.destinations.get(source_index) else {
            // A stream that is not carried. Unreferenced rather than written, so
            // the reference this iteration took is released.
            // SAFETY: the packet is live and holds at most one reference.
            unsafe { ffi::av_packet_unref(slot.as_ptr()) };
            continue;
        };
        let stream = &mut streams.carried[position];

        let stamps = stamp(&slot, stream);
        if stamps.fixes.not_monotonic {
            copied.timestamps_forced_monotonic += 1;
        }
        if stamps.fixes.presented_before_decoded {
            copied.timestamps_presented_before_decoded += 1;
        }

        // SAFETY: the packet is live and every field assigned is a plain scalar.
        // `pos` is cleared because it is the byte offset the packet had in the
        // *source*, which means nothing in the destination and which libavformat
        // would otherwise carry into its own bookkeeping.
        unsafe {
            let packet = slot.as_ptr();
            (*packet).stream_index = stream.output_index;
            (*packet).pts = stamps.presentation;
            (*packet).dts = stamps.decode;
            (*packet).duration = stamps.duration;
            (*packet).pos = -1;
        }

        let seconds = |ticks: i64| {
            ticks as f64 * f64::from(stream.output_time_base.num)
                / f64::from(stream.output_time_base.den)
        };
        let start = seconds(stamps.presentation);
        let end = seconds(stamps.presentation.saturating_add(stamps.duration));

        // SAFETY: both pointers are live. This call takes ownership of the
        // packet's reference and unreferences it before returning, whether it
        // succeeds or fails, which is why nothing here releases it afterwards.
        let code = unsafe { ffi::av_interleaved_write_frame(format.as_ptr(), slot.as_ptr()) };
        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "writing a packet into the MP4",
                source: AvError::new(code),
            });
        }

        copied.packets += 1;
        copied.bytes += size.unsigned_abs();
        copied.first_seconds = Some(copied.first_seconds.map_or(start, |first| first.min(start)));
        copied.last_seconds = Some(copied.last_seconds.map_or(end, |last| last.max(end)));
    }

    // SAFETY: the context is live and the header was written above, so the
    // trailer is written exactly once.
    let code = unsafe { ffi::av_write_trailer(format.as_ptr()) };
    if code < 0 {
        return Err(MuxError::Ffmpeg {
            operation: "writing the MP4's index",
            source: AvError::new(code),
        });
    }

    info!(
        path = %RedactedPath::new(destination),
        packets = copied.packets,
        "MP4 finished"
    );

    Ok(copied)
}

/// Moves one packet's timestamps from the source's units into the destination's.
fn stamp(slot: &PacketSlot, stream: &mut CarriedStream) -> crate::timeline::ContainerTimestamps {
    // SAFETY: the packet is live and filled in by `av_read_frame`; all three
    // fields are plain scalars.
    let (presentation, decode, duration) = unsafe {
        let packet = slot.as_ptr();
        ((*packet).pts, (*packet).dts, (*packet).duration)
    };

    // A packet with only one of the two timestamps is normal — Matroska stores
    // no decode timestamp for a stream that does not reorder — so the missing
    // one is the other rather than a guess. A packet with neither is left to the
    // decode order below, which puts it one tick after its predecessor and
    // counts the correction.
    let presentation = first_timestamp(presentation, decode).unwrap_or(0);
    let decode = first_timestamp(decode, presentation).unwrap_or(0);

    let rescale =
        |ticks: i64| rescale_timestamp(ticks, stream.source_time_base, stream.output_time_base);

    stream.order.place(
        rescale(presentation),
        rescale(decode),
        rescale(duration.max(0)),
    )
}

/// The first of two timestamps that is really there.
const fn first_timestamp(preferred: i64, fallback: i64) -> Option<i64> {
    if preferred != AV_NOPTS_VALUE {
        Some(preferred)
    } else if fallback != AV_NOPTS_VALUE {
        Some(fallback)
    } else {
        None
    }
}

/// Converts a timestamp from one time base into another.
///
/// FFmpeg's own rounding — to nearest, away from zero on a tie — so that a
/// remuxed timestamp is the one `ffmpeg -c copy` would have written, and so that
/// truncation cannot drag every timestamp towards the start of the file.
fn rescale_timestamp(ticks: i64, from: ffi::AVRational, to: ffi::AVRational) -> i64 {
    // A time base that is not a positive fraction of a second is not a time base,
    // and would make the conversion below divide by zero. The destination's is
    // checked when it is read back from the muxer; the source's is checked here,
    // because it comes from a file somebody else wrote.
    if from.num <= 0 || from.den <= 0 || to.num <= 0 || to.den <= 0 {
        return ticks;
    }

    // SAFETY: `av_rescale_q_rnd` is pure arithmetic over the values it is given
    // and reads no state at all. Both fractions have just been checked to be
    // positive, so nothing divides by zero, and the rounding argument is a
    // combination of two of libavutil's own flags.
    unsafe {
        ffi::av_rescale_q_rnd(
            ticks,
            from,
            to,
            ffi::AV_ROUND_NEAR_INF | ffi::AV_ROUND_PASS_MINMAX,
        )
    }
}

/// Writes the MP4 header, with the options that decide how usable the result is.
fn write_header(format: &OutputContext) -> Result<(), MuxError> {
    let mut options: *mut ffi::AVDictionary = ptr::null_mut();

    // SAFETY: `options` starts null, which `av_dict_set` treats as "allocate
    // one", and every string passed is NUL-terminated and outlives the call. The
    // dictionary is freed on every path below.
    //
    // `faststart` moves the index to the front of the file once it is finished,
    // which costs one extra pass over the output and is the difference between
    // an MP4 that plays while it is still downloading and one that has to be
    // fetched whole first. Sharing a clip is the entire reason this container
    // exists here, so the pass is worth paying for.
    let set = unsafe {
        ffi::av_dict_set(
            &mut options,
            c"movflags".as_ptr(),
            c"+faststart".as_ptr(),
            0,
        )
    };
    if set < 0 {
        // SAFETY: `options` is either null or a dictionary owned here; freeing
        // nulls the pointer.
        unsafe { ffi::av_dict_free(&mut options) };
        return Err(MuxError::Ffmpeg {
            operation: "setting the MP4 options that make the result streamable",
            source: AvError::new(set),
        });
    }

    // SAFETY: the context is live with its output open and its streams
    // described. `avformat_write_header` consumes the dictionary it is given and
    // writes back one holding whatever it did not recognise, which is freed
    // below in both the success and the failure case.
    let code = unsafe { ffi::avformat_write_header(format.as_ptr(), &mut options) };

    // SAFETY: `options` is either null or a dictionary owned here.
    let unrecognised = unsafe { ffi::av_dict_count(options) };
    // SAFETY: as above; freeing nulls the pointer.
    unsafe { ffi::av_dict_free(&mut options) };

    if code < 0 {
        return Err(MuxError::Ffmpeg {
            operation: "writing the MP4 header",
            source: AvError::new(code),
        });
    }

    if unrecognised > 0 {
        warn!(
            unrecognised,
            "the linked FFmpeg's MP4 muxer did not recognise every option Clipped sets; the \
             result may have to be downloaded whole before it will play"
        );
    }

    Ok(())
}

/// Remuxing a recording into MP4 failed.
///
/// The variants are the ones a caller has to be able to tell apart, because they
/// need different things said to the person waiting: a recording that cannot be
/// read, a recording MP4 cannot hold, and a destination that could not be
/// written (AGENTS.md section 45).
#[derive(Debug)]
#[non_exhaustive]
pub enum RemuxError {
    /// The recording could not be opened or described.
    SourceUnreadable {
        /// The recording that was being read.
        source: PathBuf,
        /// What FFmpeg said.
        error: AvError,
    },

    /// The recording's path cannot be expressed as UTF-8.
    ///
    /// FFmpeg's file protocol takes a UTF-8 path and converts it to the wide
    /// form Windows wants, so a path that is not valid Unicode — an unpaired
    /// surrogate in a file name, which Windows permits — has no representation
    /// to pass on.
    SourceNotRepresentable {
        /// The path that could not be converted.
        source: PathBuf,
    },

    /// MP4 cannot store one of the recording's picture or sound tracks.
    ///
    /// Nothing was created. Remuxing anyway would produce a file that is missing
    /// part of the recording and looks exactly like one that never had it, so
    /// the refusal is the answer and the tracks are named
    /// (AGENTS.md sections 15 and 45).
    MediaNotCarried {
        /// The recording that was being read.
        source: PathBuf,
        /// The tracks that stopped it.
        tracks: Vec<PlannedTrack>,
    },

    /// The sound track a copy was asked to carry is not one the recording has.
    ///
    /// Nothing was created. A copy made anyway would be silent, and a silent
    /// file is indistinguishable from a recording that never had sound — so the
    /// index is named instead ([`AudioTracks::Only`]).
    NoSuchAudioTrack {
        /// The recording that was being read.
        source: PathBuf,
        /// The stream index that was asked for.
        index: usize,
    },

    /// The MP4 could not be written.
    Output {
        /// Where the MP4 was going.
        destination: PathBuf,
        /// What the container writer said.
        source: MuxError,
    },
}

impl fmt::Display for RemuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Redacted rather than printed whole, for the reason `MuxError`
            // gives: an error message reaches the log files at least as reliably
            // as a `Debug` string does, and a recording's path contains the
            // account name (docs/logging.md).
            Self::SourceUnreadable { source, error } => write!(
                formatter,
                "the recording {} could not be read: {error}",
                RedactedPath::new(source)
            ),
            Self::SourceNotRepresentable { source } => write!(
                formatter,
                "the recording's path {} is not valid Unicode, so it cannot be passed to FFmpeg",
                RedactedPath::new(source)
            ),
            Self::MediaNotCarried { source, tracks } => {
                write!(
                    formatter,
                    "{} cannot be remuxed to MP4 without losing part of the recording: ",
                    RedactedPath::new(source)
                )?;
                for (position, track) in tracks.iter().enumerate() {
                    if position > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{track}")?;
                }
                formatter.write_str(
                    ". Nothing was written; the recording is unchanged and still playable as it \
                     is",
                )
            }
            Self::NoSuchAudioTrack { source, index } => write!(
                formatter,
                "{} has no sound track at index {index}, so a copy carrying that track alone \
                 would have no sound at all. Nothing was written",
                RedactedPath::new(source)
            ),
            Self::Output {
                destination,
                source,
            } => write!(
                formatter,
                "the MP4 {} could not be written: {source}",
                RedactedPath::new(destination)
            ),
        }
    }
}

impl Error for RemuxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SourceUnreadable { error, .. } => Some(error),
            Self::Output { source, .. } => Some(source),
            Self::SourceNotRepresentable { .. }
            | Self::MediaNotCarried { .. }
            | Self::NoSuchAudioTrack { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A planned track, for the reporting tests.
    fn track(kind: TrackKind, codec: &str, carriage: Carriage) -> PlannedTrack {
        PlannedTrack {
            index: 0,
            kind,
            codec: codec.to_owned(),
            name: None,
            language: None,
            default: false,
            carriage,
        }
    }

    /// A plan holding exactly these tracks, numbered in order.
    fn plan(tracks: Vec<PlannedTrack>, chapters: usize) -> Mp4Plan {
        let tracks = tracks
            .into_iter()
            .enumerate()
            .map(|(index, mut track)| {
                track.index = index;
                track
            })
            .collect();
        Mp4Plan {
            source: PathBuf::from(r"C:\Users\some-person\Videos\Clipped\match.mkv"),
            tracks,
            chapters,
        }
    }

    #[test]
    fn what_mp4_can_carry_is_answered_by_the_linked_build_and_agrees_with_what_it_writes() {
        // The gate the whole refusal policy rests on, so it is checked against
        // codecs whose real behaviour was established by asking the pinned
        // build's own `ffmpeg` to copy each one into an MP4 and recording which
        // ones it refused with "Could not find tag for codec ... not currently
        // supported in container". If `avformat_query_codec` and the muxer ever
        // disagree, this is where it shows up — and the consequence of a
        // disagreement is either a file refused that would have worked, or a
        // half-open MP4 and an error four calls further on.
        for codec in [
            ffi::AV_CODEC_ID_H264,
            ffi::AV_CODEC_ID_HEVC,
            ffi::AV_CODEC_ID_AV1,
            ffi::AV_CODEC_ID_AAC,
            ffi::AV_CODEC_ID_OPUS,
            ffi::AV_CODEC_ID_FLAC,
            ffi::AV_CODEC_ID_MP3,
            // Uncompressed audio, which MP4 gained in FFmpeg 8 as `ipcm`. A
            // hand-written list of supported codecs would still be refusing it,
            // which is the reason there is not one.
            ffi::AV_CODEC_ID_PCM_S16LE,
        ] {
            assert!(
                mp4_can_carry(codec),
                "{} is refused, but the pinned build's MP4 muxer writes it",
                codec_name(codec)
            );
        }

        for codec in [
            ffi::AV_CODEC_ID_THEORA,
            ffi::AV_CODEC_ID_WAVPACK,
            ffi::AV_CODEC_ID_TTA,
            ffi::AV_CODEC_ID_SUBRIP,
        ] {
            assert!(
                !mp4_can_carry(codec),
                "{} is accepted, but the pinned build's MP4 muxer refuses it when the header \
                 is written — by which point the file exists",
                codec_name(codec)
            );
        }
    }

    #[test]
    fn a_sound_track_mp4_cannot_hold_blocks_the_remux_and_a_font_does_not() {
        // The distinction the module is built around. Losing an audio track
        // produces a file indistinguishable from one that never had it; losing
        // an attachment produces a file that is exactly as watchable.
        let recording = plan(
            vec![
                track(TrackKind::Video, "h264", Carriage::Copied),
                track(TrackKind::Audio, "wavpack", Carriage::CodecUnsupported),
                track(TrackKind::Ancillary, "ttf", Carriage::CodecUnsupported),
            ],
            0,
        );

        let blocking = recording.blocking();
        assert_eq!(blocking.len(), 1, "{blocking:?}");
        assert_eq!(blocking[0].codec(), "wavpack");
        assert!(!recording.is_lossless());

        // Both are reported, because somebody deciding what to do wants one
        // list rather than two.
        assert_eq!(
            recording.losses(),
            vec![
                "audio track 1 (wavpack) cannot be stored in MP4".to_owned(),
                "ancillary track 2 (ttf) cannot be stored in MP4".to_owned(),
            ]
        );
    }

    #[test]
    fn a_recording_mp4_holds_whole_reports_no_losses() {
        let recording = plan(
            vec![
                track(TrackKind::Video, "h264", Carriage::Copied),
                track(TrackKind::Audio, "opus", Carriage::Copied),
                track(TrackKind::Audio, "opus", Carriage::Copied),
            ],
            0,
        );

        assert!(recording.blocking().is_empty());
        assert!(recording.is_lossless());
        assert!(recording.losses().is_empty());
        assert_eq!(recording.to_string(), "3 of 3 tracks carried into MP4");
    }

    #[test]
    fn chapters_are_a_loss_even_when_every_track_is_carried() {
        // A file whose tracks all survive is not necessarily a file that
        // survives whole, and saying so is the difference between a warning and
        // a surprise (AGENTS.md section 54).
        let recording = plan(vec![track(TrackKind::Video, "h264", Carriage::Copied)], 4);

        assert!(recording.blocking().is_empty());
        assert!(!recording.is_lossless());
        assert_eq!(
            recording.losses(),
            vec!["4 chapter marks are not carried into MP4".to_owned()]
        );
    }

    #[test]
    fn a_refusal_names_the_tracks_without_naming_whose_recording_it_is() {
        let error = RemuxError::MediaNotCarried {
            source: PathBuf::from(r"C:\Users\some-person\Videos\Clipped\match.mkv"),
            tracks: vec![track(
                TrackKind::Audio,
                "wavpack",
                Carriage::CodecUnsupported,
            )],
        };

        let message = error.to_string();
        assert!(
            message.contains("audio track 0 (wavpack)"),
            "the refusal has to say which track and which codec: {message}"
        );
        assert!(
            message.contains("match.mkv"),
            "the file name is what identifies the recording: {message}"
        );
        // An error message reaches the log files, so the directories above the
        // file — which carry the account name — must not (docs/logging.md).
        assert!(
            !message.contains("some-person"),
            "the message carries the account name into the logs: {message}"
        );
        assert!(
            message.contains("Nothing was written"),
            "a refusal has to say the recording is untouched, because the next question is \
             always whether it still exists: {message}"
        );
    }

    #[test]
    fn a_packet_missing_one_timestamp_borrows_the_other_rather_than_guessing() {
        // Matroska stores no decode timestamp for a stream that does not
        // reorder, so half the packets in a recording arrive this way. Treating
        // the absent one as zero would put every such packet at the start of the
        // file.
        assert_eq!(first_timestamp(1_000, AV_NOPTS_VALUE), Some(1_000));
        assert_eq!(first_timestamp(AV_NOPTS_VALUE, 1_000), Some(1_000));
        assert_eq!(first_timestamp(AV_NOPTS_VALUE, AV_NOPTS_VALUE), None);
        // A negative timestamp is a real one — Opus carries its pre-skip as one
        // — and must not be mistaken for an absent one.
        assert_eq!(first_timestamp(-7, AV_NOPTS_VALUE), Some(-7));
    }

    #[test]
    fn rescaling_matches_ffmpegs_own_rounding_and_keeps_a_negative_timestamp_negative() {
        let milliseconds = ffi::AVRational { num: 1, den: 1000 };
        let sample_rate = ffi::AVRational {
            num: 1,
            den: 48_000,
        };

        assert_eq!(rescale_timestamp(1, milliseconds, sample_rate), 48);
        assert_eq!(rescale_timestamp(1_000, milliseconds, sample_rate), 48_000);
        // Half a tick rounds away from zero in both directions, which is what
        // `av_rescale_q` does and therefore what `ffmpeg -c copy` would write.
        assert_eq!(rescale_timestamp(48, sample_rate, milliseconds), 1);
        assert_eq!(rescale_timestamp(24, sample_rate, milliseconds), 1);
        assert_eq!(rescale_timestamp(-24, sample_rate, milliseconds), -1);
        assert_eq!(rescale_timestamp(-7, milliseconds, sample_rate), -336);
    }

    #[test]
    fn a_time_base_that_is_not_a_fraction_of_a_second_leaves_the_timestamp_alone() {
        // Unreachable through the public API — the destination's time base is
        // checked when it is read back — but the arithmetic would divide by zero
        // rather than fail, and a remux must not end in a panic.
        let broken = ffi::AVRational { num: 0, den: 0 };
        let milliseconds = ffi::AVRational { num: 1, den: 1000 };
        assert_eq!(rescale_timestamp(42, broken, milliseconds), 42);
        assert_eq!(rescale_timestamp(42, milliseconds, broken), 42);
    }
}
