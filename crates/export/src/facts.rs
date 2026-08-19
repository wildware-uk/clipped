//! What a media file says about itself, read from its header alone.
//!
//! # Why this is here and not in the library
//!
//! `clipped-library` never opens a media file — it reads the session sidecars
//! the recorder writes and indexes what they name
//! ([issue #272](https://github.com/wildware-uk/clipped/issues/272)). A
//! recording whose sidecar is gone is therefore invisible, and recovering one
//! means somebody has to open it and say what it is.
//!
//! This crate already opens containers for export, so the machinery is here and
//! nowhere else worth duplicating. What was missing was a way to ask it a small
//! question: everything [`SourceStream`] describes was already public, and only
//! the thing that opens a file was not.
//!
//! # It reads the header and stops
//!
//! [`SourceProfile`](crate::SourceProfile) walks every packet in the file to
//! build a frame index, which is what planning an export needs and is far more
//! than "what is this". This reads what the container declares when it is
//! opened — duration and the streams — and reads no packets at all.
//!
//! That is a real difference on a long recording: an hour of gameplay is
//! hundreds of thousands of packets, and a recovery pass over a folder would
//! walk all of them for every file.
//!
//! # What it will not tell you
//!
//! Which game it was. Nothing in a Matroska file records that, and guessing it
//! from a file name is the thing issue #272 exists to avoid — a wrong guess
//! files somebody's footage under a game they were not playing and never says
//! so.

use std::path::Path;
use std::time::Duration;

use crate::error::ExportError;
use crate::media::SourceMedia;
use crate::source::{SourceStream, StreamFormat};

/// What one media file declares about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaFacts {
    duration: Option<Duration>,
    streams: Vec<SourceStream>,
}

impl MediaFacts {
    /// How long the container says it runs for.
    ///
    /// [`None`] when the container declares no duration, which a file an
    /// interrupted recorder left behind may not. Absent rather than zero: a
    /// caller writing this into a record should leave the field out rather than
    /// assert a length nobody measured.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Every stream in it, in container order.
    #[must_use]
    pub fn streams(&self) -> &[SourceStream] {
        &self.streams
    }

    /// The picture size of the first video track, where there is one.
    #[must_use]
    pub fn picture_size(&self) -> Option<(u32, u32)> {
        self.streams
            .iter()
            .find_map(|stream| match stream.format() {
                StreamFormat::Video { width, height, .. } => Some((*width, *height)),
                _ => None,
            })
    }

    /// How many sound tracks it carries.
    ///
    /// The number a person would see in an editor, and the one thing that says
    /// at a glance whether a recovered file is one of Clipped's own: a recording
    /// this application made carries a track per source, and a file that
    /// wandered into the folder from somewhere else usually carries one or none.
    #[must_use]
    pub fn audio_tracks(&self) -> usize {
        self.streams
            .iter()
            .filter(|stream| matches!(stream.format(), StreamFormat::Audio { .. }))
            .count()
    }

    /// Whether there is a picture track at all.
    #[must_use]
    pub fn has_video(&self) -> bool {
        self.picture_size().is_some()
    }
}

/// Opens `path`, reads what it declares, and closes it.
///
/// # Errors
///
/// [`ExportError::SourceNotRepresentable`] for a path FFmpeg cannot be given,
/// and [`ExportError::SourceUnreadable`] for a file that cannot be opened or
/// described — which is the answer for a file that is not media at all, and is
/// what a caller walking a folder wants rather than a panic.
pub fn facts_about(path: &Path) -> Result<MediaFacts, ExportError> {
    let media = SourceMedia::open(path)?;
    Ok(MediaFacts {
        duration: media.declared_duration(),
        streams: media.streams().to_vec(),
    })
}
