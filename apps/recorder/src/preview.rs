//! Answering `open_preview`: turning a cached thumbnail or waveform into
//! something a window can draw.
//!
//! The two generators, their caches and the background worker were finished and
//! tested long before anything asked them for an answer — `docs/thumbnails.md`
//! said so in as many words: *"The generator, the cache and the background
//! worker exist and are tested. Nothing draws the result."* This module is what
//! asks ([issue #448](https://github.com/wildware-uk/clipped/issues/448)).
//!
//! # Why here and not in the window
//!
//! Both caches are files under `%LOCALAPPDATA%\Clipped`, and the window can
//! read neither: it has no file-system permission, and
//! `tests/integration/tests/workspace_layering.rs` permits
//! `apps/desktop/src-tauri` exactly one crate of this workspace, `clipped-ipc`
//! — so it links neither `clipped-library` nor `clipped-waveform` and could not
//! read a `.cwf` even if it could open one. The recorder holds both services
//! already, because it is the process that generates them (`crate::library`).
//!
//! # Asking is what makes one
//!
//! [`ThumbnailService::thumbnail`] and [`WaveformService::waveform`] both look
//! the recording up **and queue the work when they miss**, so a window drawing
//! a row is what puts that recording at the front of the queue. That is the
//! consumer the generators never had: before this, the only thing that ever
//! requested one was a whole-library sweep after reconciliation, in whatever
//! order the index happened to be in.
//!
//! # What is deliberately not done here
//!
//! **No blocking wait for generation to finish.** A miss is answered
//! [`PreviewState::Pending`](clipped_ipc::PreviewState) immediately and the
//! window asks again; the alternative is a connection thread parked for the
//! tens of milliseconds a thumbnail costs, or the seconds a waveform does,
//! multiplied by every row on a screen. `docs/ipc.md`'s shape of a conversation
//! is request then response, and a response that takes a second is a screen
//! that does not draw.

use std::num::NonZeroUsize;
use std::path::Path;

use clipped_ipc::{
    ErrorCode, OpenPreview, Preview, PreviewKind, PreviewPicture, PreviewTrack, ProtocolError,
    MAX_FRAME_BYTES, MAX_PREVIEW_BUCKETS,
};
use clipped_library::thumbnail::{ThumbnailService, ThumbnailState};
use clipped_waveform::{WaveformService, WaveformState};

/// How many buckets a waveform is answered with when the caller names none.
///
/// `clipped_waveform::OVERVIEW_BUCKETS`, which is the coarsest rung of the
/// pyramid and the one a whole-recording overview is stored at. Named through
/// the crate rather than repeated, so that changing the pyramid changes this.
const DEFAULT_BUCKETS: usize = clipped_waveform::OVERVIEW_BUCKETS;

/// The most of a frame one picture may take.
///
/// A frame is [`MAX_FRAME_BYTES`] and carries the envelope as well as the
/// picture, so a thumbnail that would not leave room for the rest is refused
/// **here**, where the answer is a state a screen draws, rather than in
/// `write_message`, where it would be a frame the recorder could not send and a
/// window left waiting. Nothing this build generates comes near it — a stored
/// thumbnail is 640 pixels wide and about 20 kB (`docs/thumbnails.md`) — which
/// is the point: this is the bound that makes that true rather than assumed.
const PICTURE_BUDGET: usize = MAX_FRAME_BYTES as usize / 2;

/// Answers one `open_preview`.
///
/// # Errors
///
/// [`ErrorCode::InvalidParameters`] when the request names no recording, and
/// [`ErrorCode::LibraryUnavailable`] when this machine describes no per-user
/// directory, so there is nowhere for either cache to be. Everything else —
/// including a recording that has gone, one that cannot be decoded and one
/// whose picture has not been made yet — is a [`Preview`] rather than a
/// refusal, because none of those is a failure of the question
/// (`clipped_ipc::preview`).
pub fn open(
    request: &OpenPreview,
    thumbnails: Option<&ThumbnailService>,
    waveforms: Option<&WaveformService>,
) -> Result<Preview, ProtocolError> {
    let source = named(&request.source)?;

    match request.kind {
        PreviewKind::Thumbnail => {
            let service = thumbnails.ok_or_else(|| nowhere_to_keep("thumbnails"))?;
            Ok(from_thumbnail(&service.thumbnail(source)))
        }
        PreviewKind::Waveform => {
            let service = waveforms.ok_or_else(|| nowhere_to_keep("waveforms"))?;
            Ok(from_waveform(&service.waveform(source), request.buckets))
        }
    }
}

/// The recording the request names, or a refusal saying it named none.
fn named(value: &str) -> Result<&Path, ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "`open_preview` needs a `source`, and was given none".to_owned(),
        ));
    }
    Ok(Path::new(value))
}

/// The refusal for a machine with no per-user directory to cache in.
///
/// A refusal rather than an `unavailable` preview, and deliberately: it is
/// not a fact about the recording, it is the same "this machine has nowhere to
/// keep a library" that every other library command answers with, and a screen
/// that drew it per tile would say it once a row.
fn nowhere_to_keep(what: &str) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::LibraryUnavailable,
        format!("Clipped has nowhere on this machine to keep {what}"),
    )
}

/// One thumbnail lookup, as the protocol carries it.
fn from_thumbnail(state: &ThumbnailState) -> Preview {
    match state {
        ThumbnailState::Ready(thumbnail) => match std::fs::read(thumbnail.image_path()) {
            Ok(bytes) if bytes.len() > PICTURE_BUDGET => Preview::unavailable(
                PreviewKind::Thumbnail,
                format!(
                    "that recording's picture is {} bytes, which is more than one message carries",
                    bytes.len()
                ),
            ),
            Ok(bytes) => Preview::thumbnail(PreviewPicture {
                media_type: media_type(thumbnail.image_path()).to_owned(),
                bytes: clipped_ipc::base64(&bytes),
                width: thumbnail.width(),
                height: thumbnail.height(),
                at_seconds: thumbnail.at().as_secs_f64(),
                blank: thumbnail.is_blank(),
            }),
            // The lookup found the file and the read did not: the drive went
            // away between the two, or something removed the picture in the
            // moment between them. A deliberate deletion is not this branch —
            // `ThumbnailCache::lookup` checks the picture is there and answers
            // `Pending` when it is not (`docs/thumbnails.md`) — so this is the
            // race and nothing else, which is why no test drives it. It is
            // still an answer rather than a panic, and it is `Unavailable`
            // rather than `Pending` because a screen that redrew this tile for
            // ever would be hiding a disconnected drive.
            Err(cause) => Preview::unavailable(
                PreviewKind::Thumbnail,
                format!("that recording's picture could not be read: {cause}"),
            ),
        },
        ThumbnailState::Pending => Preview::pending(PreviewKind::Thumbnail),
        ThumbnailState::Unavailable(reason) => {
            Preview::unavailable(PreviewKind::Thumbnail, reason.to_string())
        }
        // `ThumbnailState` is `#[non_exhaustive]`, so a state added to
        // `clipped-library` arrives here rather than stopping the build.
        // Pending is the honest reading of one this build cannot name: it says
        // "there is nothing to draw yet", which is true, and asking again costs
        // a lookup.
        _ => Preview::pending(PreviewKind::Thumbnail),
    }
}

/// One waveform lookup, reduced to the resolution the caller can draw.
fn from_waveform(state: &WaveformState, buckets: Option<u32>) -> Preview {
    match state {
        WaveformState::Ready(waveform) => {
            let buckets = wanted(buckets);
            Preview::waveform(
                waveform
                    .tracks()
                    .iter()
                    .map(|track| {
                        let descriptor = track.descriptor();
                        PreviewTrack {
                            index: descriptor.stream_index(),
                            name: descriptor.name().map(ToOwned::to_owned),
                            sample_rate: descriptor.sample_rate(),
                            channels: descriptor.channels(),
                            duration_seconds: track.duration().as_secs_f64(),
                            // Interleaved, minimum first, which is what
                            // `PreviewTrack::peaks` documents. `overview`
                            // answers exactly `buckets` of them however long
                            // the track is, so a window drawing a fixed width
                            // needs no arithmetic of its own.
                            peaks: track
                                .overview(buckets)
                                .into_iter()
                                .flat_map(|peak| [peak.minimum(), peak.maximum()])
                                .collect(),
                        }
                    })
                    .collect(),
            )
        }
        WaveformState::Pending => Preview::pending(PreviewKind::Waveform),
        WaveformState::Unavailable(reason) => {
            Preview::unavailable(PreviewKind::Waveform, reason.to_string())
        }
    }
}

/// How many buckets to answer with, given what the caller asked for.
///
/// Clamped rather than refused: somebody asked for a picture of their audio and
/// a slightly coarser one is not a failure. Zero is the caller's row having no
/// width yet, which is a screen mid-layout rather than a mistake, so it is the
/// same answer as asking for none.
fn wanted(buckets: Option<u32>) -> NonZeroUsize {
    let asked = buckets
        .filter(|count| *count > 0)
        .map(|count| usize::try_from(count.min(MAX_PREVIEW_BUCKETS)).unwrap_or(DEFAULT_BUCKETS))
        .unwrap_or(DEFAULT_BUCKETS);
    NonZeroUsize::new(asked).unwrap_or(NonZeroUsize::MIN)
}

/// What a cached picture is, as far as a window needs to know.
///
/// Read from the file the cache named rather than assumed to be JPEG, so that
/// `docs/thumbnails.md`'s unsettled argument about WebP is a change to the
/// generator and to nothing else.
fn media_type(image: &Path) -> &'static str {
    match image
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("png") => "image/png",
        // Answered as bytes rather than as a guess. A window will not draw it,
        // which is the right outcome for a cache holding something this build
        // does not recognise.
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests;
