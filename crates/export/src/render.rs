//! Turning a plan into a file.
//!
//! Only the copy path is here. Everything a copy does is move coded packets
//! from one container into another and change what time they are stamped at,
//! and the change of time is the whole of "the exported file matches the
//! timeline": a packet presented at `t` in the recording is presented at
//! `segment.output_start + (t - segment.span.start)` in the clip.
//!
//! # There is no second muxer here
//!
//! The output is `clipped_muxer::MkvWriter`, the writer a recording and a
//! replay clip are both written by, given the packets the recording already
//! holds (AGENTS.md section 55). Everything the container does for a recording
//! — rebasing onto the first packet, forcing decode order to increase, bounding
//! what an interrupted write costs — an export therefore gets for free.
//!
//! # Which packets a segment takes
//!
//! Half-open, in both media, exactly as `docs/editing.md` fixes it: everything
//! presented at or after `span.start` and strictly before `span.end`. A picture
//! belongs to one side of a cut and never to both.
//!
//! The video loop stops when a packet's *decode* timestamp reaches the end of
//! the segment. That is exact rather than conservative: a packet is never
//! presented before it is decoded, so a picture with a presentation time inside
//! the segment always has a decode time inside it too, and nothing wanted can
//! be behind a packet that has already been passed.
//!
//! # Threading
//!
//! [`export`] blocks for as long as the copy takes and must not be called on a
//! capture thread (AGENTS.md section 20). It touches no shared state: it reads
//! the recording, which nothing else is writing, and writes a new file nothing
//! else knows about. Two exports at once are two readers and two writers.

use core::time::Duration;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clipped_edit::{EditDocument, RecordingId};
use clipped_logging::RedactedPath;
use clipped_muxer::{
    AudioTrack, EncodedPacket, MkvWriter, PacketTimestamp, RecordingLayout, TrackId, VideoTrack,
};
use tracing::{info, warn};

use crate::error::ExportError;
use crate::media::SourceMedia;
use crate::plan::{audio_codec_of, video_codec_of, ExportMethod, ExportPlan};
use crate::progress::{ExportOptions, ExportProgress};
use crate::source::{SourceProfile, SourceStream, StreamFormat};

/// Where the recordings an edit names actually are.
///
/// The document names recordings by the library's identifiers and deliberately
/// holds no path (`clipped-edit` performs no file access at all), so something
/// has to bring the two together. That is this, and it is the caller's job
/// because the caller is what owns the library.
#[derive(Debug, Clone, Default)]
pub struct SourceFiles {
    entries: Vec<(RecordingId, PathBuf)>,
}

impl SourceFiles {
    /// No recordings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same set, with `recording` at `path`.
    #[must_use]
    pub fn with(mut self, recording: RecordingId, path: impl Into<PathBuf>) -> Self {
        self.entries.push((recording, path.into()));
        self
    }

    /// Where a recording is, if this set knows.
    #[must_use]
    pub fn path(&self, recording: &RecordingId) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(named, _)| named == recording)
            .map(|(_, path)| path.as_path())
    }
}

/// A finished export, and what it turned out to contain.
#[derive(Debug, Clone)]
pub struct Export {
    path: PathBuf,
    plan: ExportPlan,
    packets: u64,
    frames: u64,
    bytes: u64,
    elapsed: Duration,
}

impl Export {
    /// The file that was written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How the export was made, and what it had to work with.
    #[must_use]
    pub const fn plan(&self) -> &ExportPlan {
        &self.plan
    }

    /// How many packets were written, across every track.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    /// How many pictures were written.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many bytes of coded media were written, before the container's own
    /// overhead.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.bytes
    }

    /// How long the clip is.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.plan.output_nanos())
    }

    /// How long the export took.
    ///
    /// Measured rather than estimated, and worth reporting: the whole argument
    /// for copying instead of re-encoding is that this number is small, and a
    /// caller that wants to say so should be quoting a measurement (AGENTS.md
    /// section 19).
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

impl core::fmt::Display for Export {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{:.3}s in {} pictures and {} packets, {} in {:.3}s",
            self.duration().as_secs_f64(),
            self.frames,
            self.packets,
            self.plan.method(),
            self.elapsed.as_secs_f64()
        )
    }
}

/// Works out how `document` would be exported, without writing anything.
///
/// Opens every recording the document names, reads it once without decoding,
/// and answers. A caller can show what an export will cost — and whether it
/// will be refused — before somebody waits for it (AGENTS.md section 45).
///
/// # Errors
///
/// [`ExportError::Plan`] for a document that cannot be read or names a
/// recording this set has no path for, and the source errors for a recording
/// that cannot be opened. Nothing is written on any path.
pub fn plan_export(
    document: &EditDocument,
    sources: &SourceFiles,
) -> Result<ExportPlan, ExportError> {
    let profiles = profile_sources(document, sources)?;
    let plan = ExportPlan::of(document, &profiles)?;
    Ok(plan)
}

/// Renders `document` to `destination`.
///
/// The recordings are opened for reading and are not modified, whether this
/// succeeds, fails or is cancelled.
///
/// # Errors
///
/// - [`ExportError::Plan`] and the source errors before anything is created.
/// - [`ExportError::ReencodeRequired`] when the edit cannot be copied. Nothing
///   is created, and every reason is named: this build does not re-encode, and
///   saying so is better than writing something that is not the clip.
/// - [`ExportError::Output`] when the write fails — a destination that already
///   exists (nothing here overwrites a file), a directory that is not there, a
///   disk that filled up.
/// - [`ExportError::Cancelled`] when the caller's [`Cancellation`] was set.
///
/// Anything this call created is removed again on every failure, so a caller
/// retrying the same name is not told it is about to overwrite its own failed
/// attempt.
///
/// [`Cancellation`]: crate::Cancellation
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
///
/// use clipped_edit::{EditDocument, RecordingId, SourceSpan, SourceTime};
/// use clipped_export::{export, ExportOptions, SourceFiles};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let span = SourceSpan::new(SourceTime::ZERO, SourceTime::from_nanos(4_000_000_000))
///     .expect("the span ends after it starts");
/// let document = EditDocument::from_recording("Ace", RecordingId::new("rec-1"), span);
/// let sources = SourceFiles::new().with(RecordingId::new("rec-1"), "match.mkv");
///
/// let export = export(
///     &document,
///     &sources,
///     Path::new("ace.mkv"),
///     &ExportOptions::new(),
/// )?;
/// println!("{export}");
/// # Ok(())
/// # }
/// ```
pub fn export(
    document: &EditDocument,
    sources: &SourceFiles,
    destination: &Path,
    options: &ExportOptions<'_>,
) -> Result<Export, ExportError> {
    let started = Instant::now();

    let profiles = profile_sources(document, sources)?;
    let plan = ExportPlan::of(document, &profiles)?;

    if plan.method() != ExportMethod::StreamCopy {
        return Err(ExportError::ReencodeRequired {
            blockers: plan.blockers().to_vec(),
        });
    }

    // A copy has exactly one recording — the plan refuses otherwise — so this
    // and its path are both there.
    let recording = plan
        .recording()
        .cloned()
        .ok_or_else(|| ExportError::ReencodeRequired {
            blockers: plan.blockers().to_vec(),
        })?;
    let path = sources
        .path(&recording)
        .ok_or_else(|| {
            ExportError::Plan(crate::plan::PlanError::RecordingNotDescribed {
                recording: recording.clone(),
            })
        })?
        .to_path_buf();

    let profile = profiles
        .iter()
        .find(|(named, _)| *named == recording)
        .map(|(_, profile)| profile)
        .ok_or_else(|| {
            ExportError::Plan(crate::plan::PlanError::RecordingNotDescribed {
                recording: recording.clone(),
            })
        })?;

    let mut media = SourceMedia::open(&path)?;
    let (layout, routes) = describe_output(&plan, profile)?;

    let mut writer =
        MkvWriter::create(destination, &layout).map_err(|source| ExportError::Output {
            destination: destination.to_path_buf(),
            source,
        })?;

    // The file exists from here on, so every failure has to take it away again.
    let written = copy_segments(
        &mut media,
        &mut writer,
        &plan,
        &routes,
        options,
        destination,
    );
    let written = match written {
        Ok(written) => written,
        Err(error) => {
            drop(writer);
            remove_partial(destination);
            return Err(error);
        }
    };

    if let Err(source) = writer.finish() {
        remove_partial(destination);
        return Err(ExportError::Output {
            destination: destination.to_path_buf(),
            source,
        });
    }

    // The last report, so that a progress bar reaches its end rather than
    // stopping wherever the last packet happened to fall.
    options.report(ExportProgress {
        written_nanos: plan.output_nanos(),
        total_nanos: plan.output_nanos(),
        packets: written.packets,
    });

    let export = Export {
        path: destination.to_path_buf(),
        plan,
        packets: written.packets,
        frames: written.frames,
        bytes: written.bytes,
        elapsed: started.elapsed(),
    };

    info!(
        source = %RedactedPath::new(&path),
        destination = %RedactedPath::new(destination),
        method = %export.plan.method(),
        packets = export.packets,
        frames = export.frames,
        bytes = export.bytes,
        duration_ms = export.duration().as_millis(),
        elapsed_ms = export.elapsed.as_millis(),
        "clip exported"
    );

    Ok(export)
}

/// Opens and reads every recording the document names.
fn profile_sources(
    document: &EditDocument,
    sources: &SourceFiles,
) -> Result<Vec<(RecordingId, SourceProfile)>, ExportError> {
    let mut profiles: Vec<(RecordingId, SourceProfile)> = Vec::new();

    for source in &document.sources {
        if profiles.iter().any(|(named, _)| *named == source.recording) {
            continue;
        }
        let Some(path) = sources.path(&source.recording) else {
            return Err(ExportError::Plan(
                crate::plan::PlanError::RecordingNotDescribed {
                    recording: source.recording.clone(),
                },
            ));
        };
        let mut media = SourceMedia::open(path)?;
        profiles.push((source.recording.clone(), media.profile()?));
    }

    Ok(profiles)
}

/// Which output track a container stream's packets go to.
#[derive(Debug, Clone, Copy)]
struct Route {
    /// The stream of the source container.
    stream: usize,
    /// The track of the export.
    track: TrackId,
}

/// Describes the file to be written, and where each source stream goes in it.
fn describe_output(
    plan: &ExportPlan,
    profile: &SourceProfile,
) -> Result<(RecordingLayout, Vec<Route>), ExportError> {
    // Both are checked by the plan before the method is a copy; the fallbacks
    // are here so that a refusal is an error rather than a panic (AGENTS.md
    // section 15).
    let video = profile.video().ok_or_else(|| refused(plan))?;
    let codec = video_codec_of(video).ok_or_else(|| refused(plan))?;
    let (width, height, frame_rate) = match video.format() {
        StreamFormat::Video {
            width,
            height,
            frame_rate,
            ..
        } => (*width, *height, *frame_rate),
        _ => return Err(refused(plan)),
    };

    let mut track = VideoTrack::new(codec, width, height).with_codec_private(video.extradata());
    if let Some(frame_rate) = frame_rate {
        track = track.with_frame_rate(frame_rate);
    }
    if let Some(name) = usable_name(video) {
        track = track.with_name(name);
    }

    let mut layout = RecordingLayout::new(track);
    let mut routes = vec![Route {
        stream: video.index(),
        track: TrackId::Video,
    }];

    for (position, planned) in plan.audio_tracks().iter().enumerate() {
        let source = profile
            .audio_stream(planned.stream)
            .ok_or_else(|| refused(plan))?;
        let codec = audio_codec_of(source).ok_or_else(|| refused(plan))?;
        let (sample_rate, channels) = match source.format() {
            StreamFormat::Audio {
                sample_rate,
                channels,
                ..
            } => (*sample_rate, *channels),
            _ => return Err(refused(plan)),
        };

        let mut track = AudioTrack::new(codec, sample_rate, channels)
            .with_codec_private(source.extradata())
            .with_default_flag(source.is_default());
        // The document's name where it has one, the container's otherwise, and
        // nothing at all rather than a blank — which the writer refuses,
        // because a blank name is worse than none in an editor's track list.
        let name = planned
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| usable_name(source));
        if let Some(name) = name {
            track = track.with_name(name);
        }
        if let Some(language) = source
            .language()
            .and_then(|tag| clipped_muxer::Language::new(tag).ok())
        {
            track = track.with_language(language);
        }

        layout = layout.with_audio_track(track);
        routes.push(Route {
            stream: source.index(),
            // Every audio track here is described by hand and carries no
            // `AudioSource`, so `RecordingLayout` keeps them in the order they
            // were added and this position is the track's number.
            track: TrackId::Audio(u16::try_from(position).unwrap_or(u16::MAX)),
        });
    }

    Ok((layout, routes))
}

/// The refusal to fall back on when the plan and the layout disagree.
///
/// Unreachable: the plan checks all of this before it says a copy is possible.
/// It exists so that a disagreement is an error naming what the plan found
/// rather than an unwrap.
fn refused(plan: &ExportPlan) -> ExportError {
    ExportError::ReencodeRequired {
        blockers: plan.blockers().to_vec(),
    }
}

/// A track's name, when it has one that is worth writing.
fn usable_name(stream: &SourceStream) -> Option<&str> {
    stream.name().filter(|name| !name.trim().is_empty())
}

/// What the copying loop counted.
#[derive(Debug, Default, Clone, Copy)]
struct Written {
    packets: u64,
    frames: u64,
    bytes: u64,
}

/// Copies every segment's packets into the writer, in the order they play.
fn copy_segments(
    media: &mut SourceMedia,
    writer: &mut MkvWriter,
    plan: &ExportPlan,
    routes: &[Route],
    options: &ExportOptions<'_>,
    destination: &Path,
) -> Result<Written, ExportError> {
    let video_stream = media.video_stream_index();
    let mut written = Written::default();
    let mut reported_nanos = 0_u64;

    for segment in plan.segments() {
        media.seek_before_nanos(segment.span.start().as_nanos())?;

        let start = segment.span.start().as_nanos();
        let end = segment.span.end().as_nanos();
        // One flag per route: a segment is finished when every track it writes
        // has passed the end of it. Stopping on the video track alone would
        // truncate audio that the container interleaved a little behind.
        let mut finished = vec![false; routes.len()];

        while !finished.iter().all(|done| *done) {
            if options.cancellation().is_cancelled() {
                return Err(ExportError::Cancelled {
                    destination: destination.to_path_buf(),
                });
            }

            let Some(packet) = media.read()? else {
                break;
            };
            let Some(position) = routes
                .iter()
                .position(|route| route.stream == packet.stream)
            else {
                continue;
            };
            let route = routes[position];

            let presentation = packet.presentation_nanos.max(0).unsigned_abs();
            let decode = packet.decode_nanos.max(0).unsigned_abs();

            // A packet is never presented before it is decoded, so a decode
            // time at or past the end means everything after it is past the end
            // too — for a picture. Sound is not reordered at all, so its own
            // presentation time answers.
            let past_the_end = if Some(packet.stream) == video_stream {
                decode >= end
            } else {
                presentation >= end
            };
            if past_the_end {
                finished[position] = true;
                continue;
            }
            // The far end of the range is the test above and nothing else: a
            // packet that reached here is before it. What is left is the near
            // end, which is the material between where the seek landed — a
            // keyframe at or before the cut, plus a second of slack — and the
            // cut itself.
            if presentation < start {
                continue;
            }

            let data = media.packet_data();
            if data.is_empty() {
                continue;
            }

            let output_presentation = segment.output_start.as_nanos() + (presentation - start);
            // A packet is never presented before it is decoded, so this is
            // never past the presentation time; `saturating_sub` covers a
            // decode time from before the cut, which only a reordered stream
            // could produce and which the plan refuses to copy.
            let output_decode = segment.output_start.as_nanos() + decode.saturating_sub(start);

            let mut encoded = EncodedPacket::new(route.track, timestamp(output_presentation), data)
                .with_decode_timestamp(timestamp(output_decode))
                .with_keyframe(packet.keyframe);
            if packet.duration_nanos > 0 {
                encoded = encoded
                    .with_duration(Duration::from_nanos(packet.duration_nanos.unsigned_abs()));
            }

            writer
                .write_packet(&encoded)
                .map_err(|source| ExportError::Output {
                    destination: destination.to_path_buf(),
                    source,
                })?;

            written.packets += 1;
            written.bytes += data.len() as u64;
            if route.track == TrackId::Video {
                written.frames += 1;
                report(
                    options,
                    plan,
                    &written,
                    output_presentation,
                    &mut reported_nanos,
                );
            }
        }
    }

    Ok(written)
}

/// Reports progress, at most once per the caller's interval of output.
fn report(
    options: &ExportOptions<'_>,
    plan: &ExportPlan,
    written: &Written,
    output_nanos: u64,
    reported_nanos: &mut u64,
) {
    if !options.reports() {
        return;
    }
    let interval = u64::try_from(options.progress_interval().as_nanos()).unwrap_or(u64::MAX);
    if output_nanos < reported_nanos.saturating_add(interval) && *reported_nanos > 0 {
        return;
    }
    *reported_nanos = output_nanos.max(1);
    options.report(ExportProgress {
        written_nanos: output_nanos,
        total_nanos: plan.output_nanos(),
        packets: written.packets,
    });
}

/// A media time as the container's clock reading.
///
/// Saturating rather than wrapping: 2^63 nanoseconds is 292 years of media
/// time, so this is unreachable, and a silent wrap would put one picture at the
/// far end of a clip's timeline.
fn timestamp(nanos: u64) -> PacketTimestamp {
    PacketTimestamp::from_nanos(i64::try_from(nanos).unwrap_or(i64::MAX))
}

/// Takes away a file this call created and could not finish.
///
/// A partial export is not a clip, and leaving one behind would also make the
/// name unusable: `MkvWriter::create` refuses a destination that already
/// exists rather than truncating it (AGENTS.md section 56), so a retry would be
/// told it was about to overwrite a recording.
fn remove_partial(destination: &Path) {
    match std::fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(
            path = %RedactedPath::new(destination),
            %error,
            "an export failed and the part-written clip left behind could not be removed; that \
             name cannot be exported to until it is"
        ),
    }
}

#[cfg(test)]
mod tests {
    use clipped_edit::{OutputTime, SourceSpan, SourceTime};

    use super::*;
    use crate::plan::PlannedSegment;

    #[test]
    fn a_source_set_answers_only_for_the_recordings_it_was_given() {
        let sources = SourceFiles::new().with(RecordingId::new("rec-1"), r"C:\clips\match.mkv");

        assert_eq!(
            sources.path(&RecordingId::new("rec-1")),
            Some(Path::new(r"C:\clips\match.mkv"))
        );
        assert_eq!(sources.path(&RecordingId::new("rec-2")), None);
    }

    #[test]
    fn a_packets_output_time_is_its_source_time_moved_by_the_segments_offset() {
        // The one conversion a copy makes, and the whole of "the export matches
        // the timeline". A picture at 5.5 s of the recording, in a segment that
        // plays 5 s onwards starting two seconds into the clip, is at 2.5 s.
        let segment = PlannedSegment {
            segment: 1,
            source: clipped_edit::SourceId::new(0),
            span: SourceSpan::new(
                SourceTime::from_nanos(5_000_000_000),
                SourceTime::from_nanos(7_000_000_000),
            )
            .expect("a valid span"),
            output_start: OutputTime::from_nanos(2_000_000_000),
            opening_frame: None,
            frames: 0,
        };

        assert_eq!(
            segment.output_of(SourceTime::from_nanos(5_500_000_000)),
            Some(OutputTime::from_nanos(2_500_000_000))
        );
        assert_eq!(
            segment.output_of(SourceTime::from_nanos(5_000_000_000)),
            Some(OutputTime::from_nanos(2_000_000_000)),
            "the first picture of a segment opens it"
        );
    }
}
