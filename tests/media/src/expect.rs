//! What a test expects a recording to contain, and what it is told when the
//! recording does not contain it.
//!
//! Every expectation is recorded rather than asserted, so one run reports all
//! of them. "media invalid" is useless at two in the morning; "audio stream
//! count: expected 3, found 1 (a:0 opus 48000 Hz stereo 'Game')", followed by
//! an inventory of the file, is what a contributor can act on.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::audio::Tone;
use crate::probe::{Media, Stream};

/// Expectations gathered against one file.
///
/// Built by [`Media::validate`]. Nothing is checked until [`Self::check`] or
/// [`Self::assert_valid`].
#[derive(Debug)]
pub struct Validation<'a> {
    media: &'a Media,
    failures: Vec<String>,
}

impl<'a> Validation<'a> {
    pub(crate) fn new(media: &'a Media) -> Self {
        Self {
            media,
            failures: Vec::new(),
        }
    }

    /// How many streams of every kind the container holds.
    #[must_use]
    pub fn stream_count(mut self, expected: usize) -> Self {
        let found = self.media.streams();
        if found.len() != expected {
            let listing = describe_all(&found);
            self.failures.push(format!(
                "stream count: expected {expected}, found {} ({listing})",
                found.len()
            ));
        }
        self
    }

    /// How many video streams the container holds.
    #[must_use]
    pub fn video_stream_count(self, expected: usize) -> Self {
        self.count_of("video", expected)
    }

    /// How many audio streams the container holds.
    ///
    /// The assertion multi-track audio lives or dies by: a recorder that wrote
    /// every source into one track passes every other check in this file
    /// (SPEC.md section 11, AGENTS.md section 21).
    #[must_use]
    pub fn audio_stream_count(self, expected: usize) -> Self {
        self.count_of("audio", expected)
    }

    fn count_of(mut self, kind: &str, expected: usize) -> Self {
        let found = self.media.streams_of(kind);
        if found.len() != expected {
            let listing = describe_all(&found);
            self.failures.push(format!(
                "{kind} stream count: expected {expected}, found {} ({listing})",
                found.len()
            ));
        }
        self
    }

    /// What the file's only video stream must be.
    ///
    /// A recording has exactly one, so this reports a file with none or with
    /// several rather than silently checking the first.
    #[must_use]
    pub fn video(self, expected: VideoStream) -> Self {
        let count = self.media.video_streams().len();
        if count != 1 {
            return self.count_of("video", 1);
        }
        self.video_at(0, expected)
    }

    /// What one of several video streams must be.
    #[must_use]
    pub fn video_at(mut self, index: usize, expected: VideoStream) -> Self {
        match self.media.video_streams().get(index) {
            Some(stream) => expected.check(stream, &mut self.failures),
            None => self
                .failures
                .push(missing_stream(&format!("v:{index}"), &self.media.streams())),
        }
        self
    }

    /// What one audio stream must be. `index` counts audio streams, so `0` is
    /// the first audio track whatever the video is.
    #[must_use]
    pub fn audio(mut self, index: usize, expected: AudioStream) -> Self {
        match self.media.audio_streams().get(index) {
            Some(stream) => expected.check(stream, &mut self.failures),
            None => self
                .failures
                .push(missing_stream(&format!("a:{index}"), &self.media.streams())),
        }
        self
    }

    /// That the container's own duration is `expected` seconds, give or take
    /// `tolerance`.
    ///
    /// A recording is never exactly the length it was asked for — it ends on a
    /// packet boundary — so the tolerance is the caller's statement of how much
    /// slack the thing being tested is allowed.
    #[must_use]
    pub fn duration_seconds(mut self, expected: f64, tolerance: f64) -> Self {
        match self.media.duration_seconds() {
            Some(duration) if (duration - expected).abs() <= tolerance => {}
            Some(duration) => self.failures.push(format!(
                "duration: expected {expected:.3}s +/- {tolerance:.3}s, found {duration:.3}s"
            )),
            None => self.failures.push(format!(
                "duration: expected {expected:.3}s +/- {tolerance:.3}s, but the container records \
                 no duration at all, which is what a recording that never finished looks like"
            )),
        }
        self
    }

    /// That every stream's decode timestamps strictly increase.
    ///
    /// Per stream, not per file: packets from different tracks interleave and
    /// their timestamps cross constantly, so a whole-file check would be
    /// asserting something untrue of every correct recording.
    #[must_use]
    pub fn monotonic_timestamps(mut self) -> Self {
        let labels = stream_labels(self.media);
        let packets = self.media.packets();

        if packets.is_empty() {
            self.failures
                .push("timestamps: the file contains no packets at all".to_owned());
            return self;
        }

        let mut last: HashMap<i64, (f64, usize)> = HashMap::new();
        // One report per stream: a clock that stepped once puts every packet
        // after it out of order, and the first of them is the one worth
        // reading. A second stream going backwards is a different fault, so
        // that is still reported.
        let mut reported: std::collections::HashSet<i64> = std::collections::HashSet::new();

        for (position, packet) in packets.iter().enumerate() {
            let label = labels
                .get(&packet.stream_index)
                .cloned()
                .unwrap_or_else(|| format!("stream {}", packet.stream_index));
            if let Some((previous, previous_position)) =
                last.insert(packet.stream_index, (packet.decode_seconds, position))
            {
                if packet.decode_seconds <= previous && reported.insert(packet.stream_index) {
                    self.failures.push(format!(
                        "timestamps: {label} goes backwards — packet {position} of the file is at \
                         {:.6}s, after packet {previous_position} at {previous:.6}s",
                        packet.decode_seconds
                    ));
                }
            }
        }
        self
    }

    /// That the tracks are in sync to within `bound`.
    ///
    /// Two measurements, because a recording can lose synchronisation at either
    /// end. The tracks must *start* within `bound` of each other — audio that
    /// begins half a second into the video is out of sync however monotonic its
    /// timestamps are — and they must *end* within it too, which is what
    /// catches a clock that drifted over the length of the recording rather
    /// than an offset that was there from the first packet (AGENTS.md section
    /// 21, "clock drift").
    ///
    /// A file with fewer than two streams fails this rather than passing it: a
    /// caller asserting synchronisation on a recording that lost its audio
    /// track wants to be told, not reassured.
    #[must_use]
    pub fn synchronised_within(mut self, bound: Duration) -> Self {
        let bound_seconds = bound.as_secs_f64();
        let streams = self.media.streams();
        if streams.len() < 2 {
            let listing = describe_all(&streams);
            self.failures.push(format!(
                "A/V synchronisation: nothing to compare — a recording whose tracks are in sync \
                 needs at least two of them, and this file has {} ({listing})",
                streams.len()
            ));
            return self;
        }

        let starts = self.stream_starts("A/V synchronisation");
        self.report_spread("start", &starts, bound_seconds);

        let labels = stream_labels(self.media);
        let mut ends: HashMap<i64, f64> = HashMap::new();
        for packet in self.media.packets() {
            let end = packet.presentation_seconds + packet.duration_seconds.unwrap_or_default();
            let last = ends.entry(packet.stream_index).or_insert(end);
            *last = last.max(end);
        }
        let ends: Vec<(String, f64)> = ends
            .into_iter()
            .map(|(index, end)| {
                (
                    labels
                        .get(&index)
                        .cloned()
                        .unwrap_or_else(|| format!("stream {index}")),
                    end,
                )
            })
            .collect();
        if ends.len() > 1 {
            self.report_spread("end", &ends, bound_seconds);
        }
        self
    }

    /// That every stream's own timeline begins at `expected` seconds, give or
    /// take `tolerance`.
    ///
    /// Stronger than [`Self::synchronised_within`], and a different question:
    /// that one bounds how far apart the tracks are from *each other*, so a
    /// recording whose every track began three seconds in satisfies it. This
    /// one says where the recording starts, which is what a writer that rebases
    /// its timestamps onto the first packet is for (`crates/muxer/src/timeline.rs`).
    #[must_use]
    pub fn streams_start_at(mut self, expected: f64, tolerance: f64) -> Self {
        let starts = self.stream_starts("start time");
        for (label, start) in starts {
            if (start - expected).abs() > tolerance {
                self.failures.push(format!(
                    "{label} start time: expected {expected:.3}s +/- {tolerance:.3}s, \
                     found {start:.3}s"
                ));
            }
        }
        self
    }

    /// Each stream's declared start, and a recorded failure for any stream that
    /// declares none.
    ///
    /// A missing `start_time` is not 0.000s. Everywhere else in this harness a
    /// field the file does not report is a failure ("expected N, but the file
    /// does not report it"); defaulting it here would turn absence of evidence
    /// into evidence of synchronisation, which is the substitution AGENTS.md
    /// section 22 exists to prevent — and the case is reachable, since a
    /// declared but empty track has no start time to declare.
    fn stream_starts(&mut self, what: &str) -> Vec<(String, f64)> {
        let mut starts = Vec::new();
        for stream in self.media.streams() {
            match stream.number("start_time") {
                Some(start) => starts.push((stream.label(), start)),
                None => self.failures.push(format!(
                    "{what}: {} reports no start time at all, so there is nothing to place it on \
                     the recording's timeline ({})",
                    stream.label(),
                    stream.describe()
                )),
            }
        }
        starts
    }

    /// Records a failure when the tracks are further apart than `bound` at one
    /// end of the recording.
    fn report_spread(&mut self, moment: &str, times: &[(String, f64)], bound: f64) {
        let Some(earliest) = times
            .iter()
            .min_by(|left, right| left.1.total_cmp(&right.1))
        else {
            return;
        };
        let Some(latest) = times
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
        else {
            return;
        };

        let spread = latest.1 - earliest.1;
        if spread > bound {
            let mut sorted: Vec<&(String, f64)> = times.iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(&right.0));
            let listing: Vec<String> = sorted
                .iter()
                .map(|(label, time)| format!("{label} {moment}s at {time:.3}s"))
                .collect();
            self.failures.push(format!(
                "A/V synchronisation: the tracks {moment} {spread:.3}s apart, which is more than \
                 the stated {bound:.3}s bound ({})",
                listing.join(", ")
            ));
        }
    }

    /// That one audio track carries the tone it is supposed to, and not the
    /// ones it is supposed to be isolated from.
    ///
    /// The assertion the multi-track model needs: a recorder that mixed the
    /// game into the microphone track produces a file whose stream count,
    /// codecs and timestamps are all perfect (AGENTS.md sections 21 and 26).
    #[must_use]
    pub fn audio_tone(mut self, index: usize, expected: Tone) -> Self {
        match self.media.audio_content(index) {
            Ok(content) => expected.check(&format!("a:{index}"), &content, &mut self.failures),
            Err(error) => self.failures.push(format!(
                "a:{index} tone: the track could not be read — {error}"
            )),
        }
        self
    }

    /// A bespoke expectation, for something this harness has no opinion about.
    ///
    /// `detail` is only built when `holds` is false, and should say what was
    /// expected and what was there.
    #[must_use]
    pub fn that(mut self, holds: bool, detail: impl FnOnce() -> String) -> Self {
        if !holds {
            self.failures.push(detail());
        }
        self
    }

    /// Every expectation that failed, or nothing.
    pub fn check(self) -> Result<(), ValidationReport> {
        if self.failures.is_empty() {
            return Ok(());
        }
        Err(ValidationReport {
            path: self.media.path().to_path_buf(),
            inventory: self.media.inventory(),
            failures: self.failures,
        })
    }

    /// Checks every expectation and panics with all of the failures.
    ///
    /// # Panics
    ///
    /// When any expectation was not met.
    pub fn assert_valid(self) {
        if let Err(report) = self.check() {
            panic!("{report}");
        }
    }
}

/// Everything that was wrong with a file.
#[derive(Debug, Clone)]
pub struct ValidationReport {
    path: PathBuf,
    failures: Vec<String>,
    inventory: String,
}

impl ValidationReport {
    /// The expectations that were not met, in the order they were declared.
    pub fn failures(&self) -> &[String] {
        &self.failures
    }
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "{} expectation{} not met for {}:",
            self.failures.len(),
            if self.failures.len() == 1 {
                " was"
            } else {
                "s were"
            },
            self.path.display()
        )?;
        for (position, failure) in self.failures.iter().enumerate() {
            writeln!(formatter, "  {}. {failure}", position + 1)?;
        }
        write!(
            formatter,
            "what the file actually holds:\n{}",
            self.inventory
        )
    }
}

impl std::error::Error for ValidationReport {}

/// What a video stream has to be.
///
/// Only the parts a caller states are checked, so a test asserts on what it
/// cares about and stays readable.
#[derive(Debug, Clone, Default)]
pub struct VideoStream {
    codec: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pixel_format: Option<String>,
    frame_rate: Option<String>,
    title: Option<String>,
    decoded_frames: Option<u64>,
    packets: Option<u64>,
}

impl VideoStream {
    /// The codec `ffprobe` must name: `h264`, `hevc`, `av1`.
    pub fn codec(name: &str) -> Self {
        Self {
            codec: Some(name.to_owned()),
            ..Self::default()
        }
    }

    /// The picture size the encoder was configured for.
    #[must_use]
    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// The pixel format, as `yuv420p`.
    #[must_use]
    pub fn pixel_format(mut self, format: &str) -> Self {
        self.pixel_format = Some(format.to_owned());
        self
    }

    /// The nominal frame rate, as `ffprobe` writes it: `30/1`.
    #[must_use]
    pub fn frame_rate(mut self, rate: &str) -> Self {
        self.frame_rate = Some(rate.to_owned());
        self
    }

    /// The track name an editor shows.
    #[must_use]
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    /// How many pictures must come *out of a decoder*.
    ///
    /// Not how many packets the container lists: the difference between the two
    /// is the difference between a file that describes a video and a file that
    /// plays one.
    #[must_use]
    pub fn decoded_frames(mut self, frames: u64) -> Self {
        self.decoded_frames = Some(frames);
        self
    }

    /// How many packets the stream must hold.
    #[must_use]
    pub fn packets(mut self, packets: u64) -> Self {
        self.packets = Some(packets);
        self
    }

    fn check(&self, stream: &Stream<'_>, failures: &mut Vec<String>) {
        expect_field(
            stream,
            "codec",
            "codec_name",
            self.codec.as_deref(),
            failures,
        );
        expect_field(
            stream,
            "pixel format",
            "pix_fmt",
            self.pixel_format.as_deref(),
            failures,
        );
        expect_field(
            stream,
            "frame rate",
            "avg_frame_rate",
            self.frame_rate.as_deref(),
            failures,
        );
        expect_tag(stream, "title", self.title.as_deref(), failures);
        expect_count(stream, "width", self.width.map(u64::from), failures);
        expect_count(stream, "height", self.height.map(u64::from), failures);
        expect_count(stream, "nb_read_frames", self.decoded_frames, failures);
        expect_count(stream, "nb_read_packets", self.packets, failures);
    }
}

/// What an audio stream has to be.
#[derive(Debug, Clone, Default)]
pub struct AudioStream {
    codec: Option<String>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    title: Option<String>,
    language: Option<String>,
    default: Option<bool>,
    packets: Option<u64>,
}

impl AudioStream {
    /// The codec `ffprobe` must name: `opus`, `pcm_s16le`.
    pub fn codec(name: &str) -> Self {
        Self {
            codec: Some(name.to_owned()),
            ..Self::default()
        }
    }

    /// The sample rate in hertz. A track that was resampled on the way to disk
    /// is a track whose tone is at the wrong frequency.
    #[must_use]
    pub fn sample_rate(mut self, rate: u32) -> Self {
        self.sample_rate = Some(rate);
        self
    }

    /// The channel count. A microphone track declared mono has to stay mono.
    #[must_use]
    pub fn channels(mut self, channels: u16) -> Self {
        self.channels = Some(channels);
        self
    }

    /// The track name an editor shows instead of `Audio 2`.
    #[must_use]
    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_owned());
        self
    }

    /// The ISO language tag, as `eng`.
    #[must_use]
    pub fn language(mut self, language: &str) -> Self {
        self.language = Some(language.to_owned());
        self
    }

    /// Whether this is the track a player should choose on its own.
    #[must_use]
    pub fn default_track(mut self, default: bool) -> Self {
        self.default = Some(default);
        self
    }

    /// How many packets the track must hold. A writer that routed every source
    /// to the first track leaves the others empty.
    #[must_use]
    pub fn packets(mut self, packets: u64) -> Self {
        self.packets = Some(packets);
        self
    }

    fn check(&self, stream: &Stream<'_>, failures: &mut Vec<String>) {
        expect_field(
            stream,
            "codec",
            "codec_name",
            self.codec.as_deref(),
            failures,
        );
        expect_tag(stream, "title", self.title.as_deref(), failures);
        expect_tag(stream, "language", self.language.as_deref(), failures);
        expect_count(
            stream,
            "sample_rate",
            self.sample_rate.map(u64::from),
            failures,
        );
        expect_count(stream, "channels", self.channels.map(u64::from), failures);
        expect_count(stream, "nb_read_packets", self.packets, failures);

        if let Some(default) = self.default {
            if stream.is_default() != default {
                failures.push(format!(
                    "{} default flag: expected {default}, found {}",
                    stream.label(),
                    stream.is_default()
                ));
            }
        }
    }
}

fn expect_field(
    stream: &Stream<'_>,
    what: &str,
    field: &str,
    expected: Option<&str>,
    failures: &mut Vec<String>,
) {
    let Some(expected) = expected else {
        return;
    };
    let found = stream.field(field);
    if found != Some(expected) {
        failures.push(format!(
            "{} {what}: expected {expected}, found {}",
            stream.label(),
            found.unwrap_or("nothing")
        ));
    }
}

/// A metadata tag, where `None` is a meaningful expectation: Matroska omits the
/// language element entirely when it is `und`.
fn expect_tag(stream: &Stream<'_>, tag: &str, expected: Option<&str>, failures: &mut Vec<String>) {
    let Some(expected) = expected else {
        return;
    };
    let found = stream.tag(tag);
    if found != Some(expected) {
        failures.push(format!(
            "{} {tag}: expected {expected}, found {}",
            stream.label(),
            found.unwrap_or("no tag at all")
        ));
    }
}

fn expect_count(
    stream: &Stream<'_>,
    field: &str,
    expected: Option<u64>,
    failures: &mut Vec<String>,
) {
    let Some(expected) = expected else {
        return;
    };
    let what = match field {
        "nb_read_frames" => "decoded frames",
        "nb_read_packets" => "packets",
        "sample_rate" => "sample rate",
        other => other,
    };
    match stream.number(field) {
        Some(found) if (found - expected as f64).abs() < f64::EPSILON => {}
        Some(found) => failures.push(format!(
            "{} {what}: expected {expected}, found {found}",
            stream.label()
        )),
        None => failures.push(format!(
            "{} {what}: expected {expected}, but the file does not report it",
            stream.label()
        )),
    }
}

fn missing_stream(wanted: &str, found: &[Stream<'_>]) -> String {
    format!(
        "{wanted}: the file has no such stream ({})",
        describe_all(found)
    )
}

fn describe_all(streams: &[Stream<'_>]) -> String {
    if streams.is_empty() {
        return "the file has none".to_owned();
    }
    streams
        .iter()
        .map(Stream::describe)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every container stream index, against the label a report calls it by.
fn stream_labels(media: &Media) -> HashMap<i64, String> {
    media
        .streams()
        .iter()
        .filter_map(|stream| Some((stream.number("index")? as i64, stream.label())))
        .collect()
}
