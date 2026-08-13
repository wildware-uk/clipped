//! Taking a file apart with `ffprobe`, and describing what came back.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;

use crate::audio::AudioContent;
use crate::expect::Validation;
use crate::tools::{MediaTools, ToolsUnavailable};

/// A file on disk, as `ffprobe` reports it.
///
/// Opening one runs `ffprobe` over the whole file, decoding every frame:
/// `nb_read_frames` is how many pictures actually came out of a decoder, which
/// is the difference between "the container lists a video stream" and "the
/// video plays". Packets are read separately and only when an assertion needs
/// them, because that is a second pass over the file.
#[derive(Debug)]
pub struct Media {
    path: PathBuf,
    tools: MediaTools,
    format: Value,
    streams: Vec<Value>,
    diagnostics: String,
    packets: OnceLock<Vec<Packet>>,
}

impl Media {
    /// Opens `path` and reads what it contains.
    ///
    /// Fails when the tooling is missing, when `ffprobe` cannot make sense of
    /// the file, or when the file contains no streams at all — which is what a
    /// recording truncated before its header reached the disk looks like.
    ///
    /// Warnings on `ffprobe`'s standard error are *not* a failure on their own.
    /// A recording interrupted by a crash ends in the middle of an element and
    /// FFmpeg says so — `File ended prematurely` — while reading everything in
    /// front of it perfectly well, and whether that survivor is usable is a
    /// question about its content (`crates/muxer/tests/abrupt_termination.rs`).
    /// [`Media::diagnostics`] has the text for a test that wants to assert on
    /// it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        let tools = MediaTools::locate().map_err(MediaError::ToolsUnavailable)?;

        let output = Command::new(tools.ffprobe())
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-count_frames",
                "-count_packets",
                "-of",
                "json",
            ])
            .arg(&path)
            .output()
            .map_err(|error| MediaError::ToolFailed {
                tool: tools.ffprobe().to_path_buf(),
                error,
            })?;

        let diagnostics = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        let streams = parsed
            .get("streams")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if streams.is_empty() {
            return Err(MediaError::NotMedia {
                path: path.clone(),
                bytes: std::fs::metadata(&path).map(|file| file.len()).ok(),
                diagnostics,
            });
        }

        Ok(Self {
            path,
            tools,
            format: parsed
                .get("format")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new())),
            streams,
            diagnostics,
            packets: OnceLock::new(),
        })
    }

    /// The file this was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whatever `ffprobe` wrote to standard error, which is where it reports
    /// damage it read past.
    pub fn diagnostics(&self) -> &str {
        &self.diagnostics
    }

    /// Every stream, in the order the container declares them.
    pub fn streams(&self) -> Vec<Stream<'_>> {
        let mut seen = std::collections::HashMap::<&str, usize>::new();
        self.streams
            .iter()
            .map(|value| {
                let kind = value
                    .get("codec_type")
                    .and_then(Value::as_str)
                    .unwrap_or("data");
                let position = seen.entry(kind).or_default();
                let stream = Stream {
                    value,
                    kind,
                    position: *position,
                };
                *position += 1;
                stream
            })
            .collect()
    }

    /// The streams of one kind — `"video"`, `"audio"`, `"subtitle"` — in
    /// declaration order.
    pub fn streams_of(&self, kind: &str) -> Vec<Stream<'_>> {
        self.streams()
            .into_iter()
            .filter(|stream| stream.kind() == kind)
            .collect()
    }

    /// The video streams.
    pub fn video_streams(&self) -> Vec<Stream<'_>> {
        self.streams_of("video")
    }

    /// The audio streams.
    pub fn audio_streams(&self) -> Vec<Stream<'_>> {
        self.streams_of("audio")
    }

    /// The container's duration in seconds, when it records one.
    ///
    /// Matroska stores it in the segment header, which is written when the file
    /// is finished, so an interrupted recording has none and this returns
    /// [`None`].
    pub fn duration_seconds(&self) -> Option<f64> {
        number_of(&self.format, "duration")
    }

    /// The container format's short name, as FFmpeg knows it.
    pub fn format_name(&self) -> Option<&str> {
        self.format.get("format_name").and_then(Value::as_str)
    }

    /// Every packet in the file, in the order they are stored.
    ///
    /// Read on first use and kept, because this is a second pass over the
    /// whole file.
    pub fn packets(&self) -> &[Packet] {
        self.packets.get_or_init(|| self.read_packets())
    }

    /// The payload of every packet, as a hash, grouped by stream and kept in
    /// the order the container stores them.
    ///
    /// This is what proves a **stream copy**: the coded bytes of the source and
    /// the coded bytes of the result are the same bytes, so the picture and the
    /// sound are identical and nothing was traded for the change of container.
    /// Counting packets, comparing durations or even comparing decoded frames
    /// would all pass just as happily on a file that had been through an
    /// encoder (`crates/muxer/src/remux.rs`, `docs/muxing.md`).
    ///
    /// Hashes rather than the bytes themselves because a four-second recording
    /// is megabytes and a mismatch is answered by which packet, not by which
    /// byte.
    ///
    /// The outer index is the container's stream index, so a file whose first
    /// stream is video and whose second is audio comes back as two vectors in
    /// that order. A stream with no packets is an empty vector rather than a
    /// gap, so the two files being compared line up.
    ///
    /// # Panics
    ///
    /// When `ffprobe` cannot read the file, or reads no packets at all — a
    /// comparison against nothing would pass whatever the two files held.
    pub fn packet_payloads_by_stream(&self) -> Vec<Vec<String>> {
        let output = Command::new(self.tools.ffprobe())
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-show_packets",
                "-show_data_hash",
                "MD5",
                "-show_entries",
                "packet=stream_index,data_hash",
                "-of",
                "compact=p=0:nk=0",
            ])
            .arg(&self.path)
            .output()
            .expect("the ffprobe that opened this file can be run again");

        assert!(
            output.status.success(),
            "ffprobe could not read the packets of {}: {}",
            self.path.display(),
            String::from_utf8_lossy(&output.stderr)
        );

        let text = String::from_utf8_lossy(&output.stdout);
        let mut streams: Vec<Vec<String>> = Vec::new();
        let mut counted = 0_usize;
        for line in text.lines() {
            let mut index = None;
            let mut hash = None;
            for field in line.trim().split('|') {
                match field.split_once('=') {
                    Some(("stream_index", value)) => index = value.parse::<usize>().ok(),
                    Some(("data_hash", value)) => hash = Some(value.to_owned()),
                    _ => {}
                }
            }
            let (Some(index), Some(hash)) = (index, hash) else {
                continue;
            };
            while streams.len() <= index {
                streams.push(Vec::new());
            }
            streams[index].push(hash);
            counted += 1;
        }

        assert!(
            counted > 0,
            "no packet hashes came back for {}, so a comparison against them would prove \
             nothing\n{}",
            self.path.display(),
            self.inventory()
        );
        streams
    }

    /// Decodes one audio stream to mono samples, for the tone assertions.
    ///
    /// `index` counts audio streams, not container streams, so `0` is the first
    /// audio track whatever the video is.
    pub fn audio_content(&self, index: usize) -> Result<AudioContent, MediaError> {
        let stream = self.audio_streams().into_iter().nth(index).ok_or_else(|| {
            MediaError::NoSuchStream {
                wanted: format!("a:{index}"),
                inventory: self.inventory(),
            }
        })?;
        let sample_rate = stream
            .number("sample_rate")
            .map(|rate| rate as u32)
            .unwrap_or_default();

        AudioContent::decode(&self.tools, &self.path, index, sample_rate)
    }

    /// Expectations to check against this file. Nothing is asserted until
    /// [`Validation::assert_valid`] or [`Validation::check`].
    pub fn validate(&self) -> Validation<'_> {
        Validation::new(self)
    }

    /// What the file holds, one line per stream, as a failure report prints it.
    pub fn inventory(&self) -> String {
        let mut lines: Vec<String> = self
            .streams()
            .iter()
            .map(|stream| format!("  {}", stream.describe()))
            .collect();
        lines.push(format!(
            "  container: {}, duration {}",
            self.format_name().unwrap_or("<unknown>"),
            self.duration_seconds().map_or_else(
                || "not recorded".to_owned(),
                |seconds| format!("{seconds:.3}s")
            )
        ));
        if !self.diagnostics.is_empty() {
            lines.push(format!("  ffprobe said: {}", self.diagnostics));
        }
        lines.join("\n")
    }

    fn read_packets(&self) -> Vec<Packet> {
        let output = Command::new(self.tools.ffprobe())
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-show_packets",
                "-show_entries",
                "packet=stream_index,pts_time,dts_time,duration_time",
                "-of",
                "json",
            ])
            .arg(&self.path)
            .output()
            .expect("the ffprobe that opened this file can be run again");

        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        parsed
            .get("packets")
            .and_then(Value::as_array)
            .map(|packets| {
                packets
                    .iter()
                    .filter_map(|packet| {
                        Some(Packet {
                            stream_index: packet.get("stream_index")?.as_i64()?,
                            decode_seconds: number_of(packet, "dts_time")?,
                            presentation_seconds: number_of(packet, "pts_time")?,
                            duration_seconds: number_of(packet, "duration_time"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One stream of a file.
#[derive(Debug, Clone, Copy)]
pub struct Stream<'a> {
    value: &'a Value,
    kind: &'a str,
    position: usize,
}

impl<'a> Stream<'a> {
    /// `"video"`, `"audio"`, or whatever else the container declared.
    pub fn kind(&self) -> &'a str {
        self.kind
    }

    /// How this stream is named in a failure report and by `ffmpeg -map`:
    /// `v:0` for the first video stream, `a:2` for the third audio one.
    pub fn label(&self) -> String {
        let initial = self.kind.chars().next().unwrap_or('?');
        format!("{initial}:{}", self.position)
    }

    /// A string field, which is how `ffprobe` reports most of them.
    pub fn field(&self, name: &str) -> Option<&'a str> {
        self.value.get(name).and_then(Value::as_str)
    }

    /// A numeric field, whether `ffprobe` quoted it or not.
    pub fn number(&self, name: &str) -> Option<f64> {
        number_of(self.value, name)
    }

    /// A metadata tag. `title` is Matroska's `Name` element, which is the track
    /// name an editor shows.
    pub fn tag(&self, name: &str) -> Option<&'a str> {
        self.value.get("tags")?.get(name)?.as_str()
    }

    /// Whether the track carries Matroska's `FlagDefault` — the track a player
    /// should choose on its own.
    pub fn is_default(&self) -> bool {
        self.value
            .get("disposition")
            .and_then(|disposition| disposition.get("default"))
            .and_then(Value::as_i64)
            == Some(1)
    }

    /// The raw `ffprobe` object, for an assertion this harness has no opinion
    /// about.
    pub fn raw(&self) -> &'a Value {
        self.value
    }

    /// A stream built straight from an `ffprobe` object.
    ///
    /// For this crate's own tests, which have to check what an expectation says
    /// about a file `ffprobe` would report a particular way — including files
    /// no encoder here can be made to produce on demand, such as a video track
    /// that lists packets and decodes none.
    #[cfg(test)]
    pub(crate) const fn from_probe(value: &'a Value, kind: &'a str, position: usize) -> Self {
        Self {
            value,
            kind,
            position,
        }
    }

    /// One line naming the stream and everything a reader needs to see why an
    /// expectation about it failed.
    pub fn describe(&self) -> String {
        let codec = self.field("codec_name").unwrap_or("<unknown codec>");
        let mut description = format!("{} {codec}", self.label());

        match self.kind {
            "video" => {
                if let (Some(width), Some(height)) = (self.number("width"), self.number("height")) {
                    description.push_str(&format!(" {width}x{height}"));
                }
                if let Some(format) = self.field("pix_fmt") {
                    description.push(' ');
                    description.push_str(format);
                }
                if let Some(rate) = self.field("avg_frame_rate") {
                    description.push_str(&format!(" {rate} fps"));
                }
            }
            "audio" => {
                if let Some(rate) = self.number("sample_rate") {
                    description.push_str(&format!(" {rate} Hz"));
                }
                description.push(' ');
                description.push_str(&channels(self.number("channels")));
            }
            _ => {}
        }

        if let Some(frames) = self.number("nb_read_frames") {
            description.push_str(&format!(", {frames} decoded frames"));
        }
        if let Some(packets) = self.number("nb_read_packets") {
            description.push_str(&format!(", {packets} packets"));
        }
        if let Some(title) = self.tag("title") {
            description.push_str(&format!(" '{title}'"));
        }
        if let Some(language) = self.tag("language") {
            description.push_str(&format!(" [{language}]"));
        }
        if self.is_default() {
            description.push_str(" (default)");
        }
        description
    }
}

/// How a channel count reads in a report.
fn channels(count: Option<f64>) -> String {
    match count {
        Some(1.0) => "mono".to_owned(),
        Some(2.0) => "stereo".to_owned(),
        Some(other) => format!("{other} channels"),
        None => "<unknown channel count>".to_owned(),
    }
}

/// One packet, as `ffprobe` reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Packet {
    /// Which container stream it belongs to.
    pub stream_index: i64,
    /// Its decode timestamp, in seconds.
    pub decode_seconds: f64,
    /// Its presentation timestamp, in seconds.
    pub presentation_seconds: f64,
    /// How long it plays for, where the container records that.
    pub duration_seconds: Option<f64>,
}

/// A field `ffprobe` may have quoted or may have left as a JSON number.
fn number_of(value: &Value, name: &str) -> Option<f64> {
    match value.get(name) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

/// Why a file could not be inspected.
#[derive(Debug)]
pub enum MediaError {
    /// FFmpeg is not installed here.
    ToolsUnavailable(ToolsUnavailable),
    /// One of its programs could not be started.
    ToolFailed {
        /// The program that would not run.
        tool: PathBuf,
        /// Why the operating system refused.
        error: std::io::Error,
    },
    /// `ffprobe` found no streams in the file, so it is not media at all.
    NotMedia {
        /// The file that was inspected.
        path: PathBuf,
        /// Its size, when that could be read. A very small file is usually a
        /// recording that died before its header was flushed.
        bytes: Option<u64>,
        /// What `ffprobe` said about it.
        diagnostics: String,
    },
    /// An assertion named a stream the file does not have.
    NoSuchStream {
        /// The stream that was asked for, as `a:1`.
        wanted: String,
        /// What the file holds instead.
        inventory: String,
    },
    /// A track could not be decoded.
    Undecodable {
        /// The stream that was being decoded, as `a:1`.
        stream: String,
        /// What `ffmpeg` said about it.
        diagnostics: String,
    },
}

impl fmt::Display for MediaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolsUnavailable(unavailable) => write!(formatter, "{unavailable}"),
            Self::ToolFailed { tool, error } => {
                write!(formatter, "{} could not be run: {error}", tool.display())
            }
            Self::NotMedia {
                path,
                bytes,
                diagnostics,
            } => {
                write!(
                    formatter,
                    "{} is not media that can be opened: ffprobe found no streams in it",
                    path.display()
                )?;
                match bytes {
                    Some(bytes) => write!(formatter, ". The file is {bytes} bytes")?,
                    None => write!(formatter, ". The file could not be read at all")?,
                }
                if !diagnostics.is_empty() {
                    write!(formatter, ". ffprobe said: {diagnostics}")?;
                }
                Ok(())
            }
            Self::NoSuchStream { wanted, inventory } => write!(
                formatter,
                "there is no {wanted} in this file. It holds:\n{inventory}"
            ),
            Self::Undecodable {
                stream,
                diagnostics,
            } => write!(
                formatter,
                "{stream} could not be decoded. ffmpeg said: {diagnostics}"
            ),
        }
    }
}

impl std::error::Error for MediaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_describes_itself_with_everything_a_reader_needs() {
        // The description is what turns "expected 3 audio streams, found 1"
        // into a report somebody can act on, so its content is asserted rather
        // than left to whatever `ffprobe` happened to include.
        let value = serde_json::json!({
            "codec_type": "audio",
            "codec_name": "opus",
            "sample_rate": "48000",
            "channels": 2,
            "nb_read_packets": "250",
            "tags": { "title": "Game", "language": "eng" },
            "disposition": { "default": 1 },
        });
        let stream = Stream {
            value: &value,
            kind: "audio",
            position: 1,
        };

        assert_eq!(
            stream.describe(),
            "a:1 opus 48000 Hz stereo, 250 packets 'Game' [eng] (default)"
        );
    }

    #[test]
    fn a_video_stream_describes_its_shape() {
        let value = serde_json::json!({
            "codec_type": "video",
            "codec_name": "h264",
            "width": 640,
            "height": 360,
            "pix_fmt": "yuv420p",
            "avg_frame_rate": "30/1",
            "nb_read_frames": "120",
        });
        let stream = Stream {
            value: &value,
            kind: "video",
            position: 0,
        };

        assert_eq!(
            stream.describe(),
            "v:0 h264 640x360 yuv420p 30/1 fps, 120 decoded frames"
        );
    }

    #[test]
    fn a_quoted_number_reads_the_same_as_an_unquoted_one() {
        // `ffprobe` quotes some numbers and not others, and which is which has
        // changed between FFmpeg releases.
        let value = serde_json::json!({ "quoted": "48000", "plain": 48000 });
        assert_eq!(number_of(&value, "quoted"), Some(48_000.0));
        assert_eq!(number_of(&value, "plain"), Some(48_000.0));
        assert_eq!(number_of(&value, "absent"), None);
    }
}
