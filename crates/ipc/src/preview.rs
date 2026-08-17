//! What a screen draws for a recording that is not the recording itself: its
//! thumbnail, and the peaks of its sound.
//!
//! Both are **derived** — a JPEG chosen from a frame
//! ([thumbnails.md](../../../docs/thumbnails.md)) and a pyramid of minima and
//! maxima ([waveforms.md](../../../docs/waveforms.md)) — both are generated in
//! the background by `clipped-background`'s worker, and both are cached in
//! Clipped's own directory rather than in the library index. Until
//! [issue #448](https://github.com/wildware-uk/clipped/issues/448) neither could
//! reach the window at all: the generator, the cache and the worker were built
//! and tested, and nothing drew the result.
//!
//! # Why the bytes are on this protocol
//!
//! [ADR 0015](../../../docs/adr/0015-derived-pictures-cross-the-control-protocol.md)
//! is the record. The alternative was a Tauri asset scope over the cache
//! directory, letting the webview load a thumbnail as a file. It was not taken,
//! for three reasons in this order:
//!
//! - **It does not carry peaks.** A waveform entry is a `.cwf`, a binary
//!   sidecar `crates/waveform` defines; the Tauri host may not link that crate
//!   ([`crate::library`], `tests/integration/tests/workspace_layering.rs`) and
//!   neither may the window, so serving the file would mean a second
//!   implementation of the format in TypeScript, of the one surface where the
//!   two halves disagreeing is a picture that is quietly wrong. So a scope
//!   would have carried the picture and left the peaks needing a mechanism of
//!   their own, which is what #448's third criterion forbids.
//! - **The window gains nothing at all this way.** A scope is a file-system
//!   permission `apps/desktop/src-tauri/capabilities/default.json` has never
//!   granted, plus an origin in the content security policy. A picture arriving
//!   as base64 in a reply is drawn from a `data:` URI, which
//!   `tauri.conf.json`'s `img-src` already permits — so the transport costs no
//!   permission, no policy change and no new origin.
//! - **A page of thumbnails was the objection, and this is not one.** A
//!   [`Preview`] is asked for one recording at a time, as a row is drawn, so
//!   what crosses the boundary is one 20 kB picture in a frame that may hold a
//!   mebibyte ([`crate::frame::MAX_FRAME_BYTES`]) — never a page of them at
//!   once. `apps/recorder/src/preview/tests.rs` measures a page of 25 against
//!   that bound rather than assuming it, and `docs/thumbnails.md` records the
//!   figures.
//!
//! # One at a time is a real constraint, not a style
//!
//! [`RecorderLink::call`](crate::RecorderLink) opens a connection per call and a
//! recorder serves [`MAX_CONCURRENT_CONNECTIONS`](crate::MAX_CONCURRENT_CONNECTIONS)
//! of them, so a screen that asked for a preview per row *simultaneously* would
//! be the first thing in this application to run into that cap — and would take
//! whatever else the window was asking the recorder for down with it. A caller
//! drawing many rows has to bound how many of these it has in flight;
//! `apps/desktop/src/preview.ts` is where that is done, and it drops the oldest
//! waiting request for the reason `clipped-background`'s own queue does.
//!
//! That is a property of asking per recording, and it is the price of not
//! putting a page of pictures in one frame. It is the cheaper price: a bound on
//! concurrency is a number in one file, and a page of base64 is a frame that
//! does not fit.
//!
//! # Three answers, and none of them is an error
//!
//! [`PreviewState`] is the protocol's copy of `ThumbnailState` and
//! `WaveformState`, which both have exactly three cases and no error case.
//! "Not generated yet" is the ordinary state of a recording that has just been
//! written, and a screen that treated it as a failure would put a banner over
//! every new recording. A screen has to tell it from "there will not be one",
//! though, which is #448's second criterion: an empty tile and a broken one are
//! different facts about a library, and collapsing them is the fabricated state
//! AGENTS.md section 27 forbids.
//!
//! Asking is also what causes one to be made. The recorder answers a
//! [`PreviewState::Pending`] *and* queues the work, so a screen that draws a
//! recording is what puts it at the front of the queue — which is why the queue
//! drops its oldest entry rather than its newest.

use serde::{Deserialize, Serialize};

/// The most buckets a waveform answer will carry, per track.
///
/// A bound on the reply rather than a preference: peaks are drawn one bucket to
/// a pixel or wider, so 4,096 is past the width of any display this runs on,
/// and it is what keeps a three-track answer comfortably inside
/// [`MAX_FRAME_BYTES`](crate::frame::MAX_FRAME_BYTES). A request for more is
/// answered with this many rather than refused, because the caller asked for a
/// picture and getting a slightly coarser one is not a failure.
pub const MAX_PREVIEW_BUCKETS: u32 = 4096;

/// Which of a recording's two derived pictures is wanted.
///
/// A closed set that does not tolerate a stranger, for the reason
/// [`RecorderStatus`](crate::RecorderStatus) does not: there are two kinds of
/// answer and they are drawn by different code, so a window that met a third
/// and guessed would draw peaks as a picture or a picture as peaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    /// The picture for a recording: one frame, chosen and scaled
    /// (`docs/thumbnails.md`).
    Thumbnail,
    /// The peaks of each of its sound tracks (`docs/waveforms.md`).
    Waveform,
}

impl PreviewKind {
    /// The wire string for this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Waveform => "waveform",
        }
    }
}

/// Asking for one recording's thumbnail or waveform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPreview {
    /// The recording to draw, as the library reported its path.
    ///
    /// Required, and with no `serde` default, for the reason
    /// [`OpenPlayback::source`](crate::OpenPlayback) has none: a request that
    /// leaves it out is [`ErrorCode::InvalidParameters`](crate::ErrorCode)
    /// naming the field, rather than an empty path something further down has
    /// to recognise as "not given".
    ///
    /// It is opened for reading and is never modified, whatever the answer.
    pub source: String,
    /// Which picture is wanted.
    ///
    /// Required for the same reason: the two answers are shaped differently and
    /// a default would be a guess at which screen is asking.
    pub kind: PreviewKind,
    /// How many buckets of peaks the caller can draw, for
    /// [`PreviewKind::Waveform`].
    ///
    /// The width in pixels of the space the waveform goes in, in the ordinary
    /// case. It is the caller's rather than the recorder's because a pyramid of
    /// resolutions is exactly what `crates/waveform` stores, and merging
    /// buckets is *exact* — the maximum of two maxima is the maximum of the
    /// union — so answering at the width that will be drawn is not an
    /// approximation, it is the same answer on the caller's own grid.
    ///
    /// Absent means an overview: the coarsest resolution the recorder keeps.
    /// Clamped to [`MAX_PREVIEW_BUCKETS`]. Ignored for a thumbnail, which has
    /// one stored size (`docs/thumbnails.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buckets: Option<u32>,
}

/// Where a preview stands.
///
/// The protocol's copy of `clipped_library::thumbnail::ThumbnailState` and
/// `clipped_waveform::WaveformState`, which have the same three cases for the
/// same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewState {
    /// It has not been generated yet, and it has now been asked for.
    ///
    /// The ordinary state of a recording that has just been written, and of one
    /// that was trimmed or replaced since its picture was made. A screen draws
    /// the tile with no picture in it.
    Pending,
    /// Here it is.
    Ready,
    /// There will not be one, and [`Preview::reason`] says why.
    ///
    /// The recording is gone from the disk, holds no video stream, or could not
    /// be opened at all — and the last of those is *remembered*, so that a
    /// screen redraw does not become a seek and a decode per broken tile for
    /// ever (`docs/thumbnails.md`).
    Unavailable,
}

impl PreviewState {
    /// The wire string for this state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One recording's thumbnail or waveform, as far as it exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    /// Which picture this is an answer about.
    ///
    /// Echoed rather than assumed: a window draws several recordings at once
    /// and an answer has to be matched to what it answers.
    pub kind: PreviewKind,
    /// Where it stands.
    pub state: PreviewState,
    /// The picture, for a [`PreviewKind::Thumbnail`] that is
    /// [`PreviewState::Ready`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<PreviewPicture>,
    /// One entry per sound track, for a [`PreviewKind::Waveform`] that is
    /// [`PreviewState::Ready`].
    ///
    /// Empty for every other state, and legitimately empty for a recording that
    /// has no sound at all — which is what Clipped writes today, until
    /// multi-track audio ([issue #180](https://github.com/wildware-uk/clipped/issues/180)).
    /// A window that draws a row per track therefore needs no branch, exactly
    /// as `WaveformState::tracks` intends.
    #[serde(default)]
    pub tracks: Vec<PreviewTrack>,
    /// Why there will not be one, for [`PreviewState::Unavailable`].
    ///
    /// A sentence naming what happened. It never carries a directory: the two
    /// generators format their errors through `clipped_logging::RedactedPath`,
    /// so what crosses this boundary is a file name and a digest rather than
    /// the account name in `%LOCALAPPDATA%` or the folders somebody chose
    /// (AGENTS.md section 14, `docs/waveforms.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Preview {
    /// A preview that has not been generated yet.
    #[must_use]
    pub fn pending(kind: PreviewKind) -> Self {
        Self {
            kind,
            state: PreviewState::Pending,
            picture: None,
            tracks: Vec::new(),
            reason: None,
        }
    }

    /// A preview there will not be one of, and why.
    #[must_use]
    pub fn unavailable(kind: PreviewKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            state: PreviewState::Unavailable,
            picture: None,
            tracks: Vec::new(),
            reason: Some(reason.into()),
        }
    }

    /// A thumbnail that is here.
    #[must_use]
    pub fn thumbnail(picture: PreviewPicture) -> Self {
        Self {
            kind: PreviewKind::Thumbnail,
            state: PreviewState::Ready,
            picture: Some(picture),
            tracks: Vec::new(),
            reason: None,
        }
    }

    /// A waveform that is here, with one entry per sound track.
    #[must_use]
    pub fn waveform(tracks: Vec<PreviewTrack>) -> Self {
        Self {
            kind: PreviewKind::Waveform,
            state: PreviewState::Ready,
            picture: None,
            tracks,
            reason: None,
        }
    }
}

/// A thumbnail, ready to be drawn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PreviewPicture {
    /// What [`Self::bytes`] is, as a media type — `image/jpeg` for what this
    /// build stores.
    ///
    /// Carried rather than assumed so that a window builds its `data:` URI
    /// without knowing which format the generator chose. `docs/thumbnails.md`
    /// argues JPEG against WebP and the argument is not settled; whichever way
    /// it goes, nothing in the window has to change.
    pub media_type: String,
    /// The picture itself, base64 (RFC 4648, with padding).
    ///
    /// Straight into `src="data:{media_type};base64,{bytes}"`. Base64 rather
    /// than an array of numbers because JSON has no bytes and an array is three
    /// to four characters per byte against base64's 1.33.
    pub bytes: String,
    /// How wide the picture is, in pixels.
    pub width: u32,
    /// How tall it is.
    pub height: u32,
    /// How far into the recording the frame came from, in seconds.
    ///
    /// Not the first frame, and deliberately: `docs/thumbnails.md` explains
    /// which frame is chosen and why. Worth carrying because it is the one
    /// thing about a thumbnail a person might want to check.
    pub at_seconds: f64,
    /// Whether every candidate frame was a flat colour, so this one is too.
    ///
    /// True is honest rather than a failure — that is what the recording looks
    /// like — and a screen may draw it or fall back to its no-picture tile.
    #[serde(default)]
    pub blank: bool,
}

/// One sound track of a recording, reduced to peaks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PreviewTrack {
    /// The stream index the container declares the track at.
    ///
    /// The same numbering [`PlaybackTrack::index`](crate::PlaybackTrack) uses,
    /// so a screen showing a waveform beside a track selector can put the two
    /// together.
    pub index: u32,
    /// What the track is called — `Microphone`, `Game` — where the recording
    /// named it.
    ///
    /// Absent for a file that named none, which is anything not written by
    /// Clipped. A window shows the position instead rather than inventing a
    /// name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The track's sample rate, in hertz.
    pub sample_rate: u32,
    /// How many channels it carries.
    ///
    /// The channels are **merged** into these peaks rather than averaged, so a
    /// sound panned hard to one side is as visible as one in the middle
    /// (`docs/waveforms.md`). This is what the track holds, not how many rows
    /// of peaks there are — there is always one.
    pub channels: u16,
    /// How long the track is, in seconds.
    ///
    /// What [`Self::peaks`] spans: the first bucket starts at zero and the last
    /// one ends here.
    pub duration_seconds: f64,
    /// The peaks, two numbers per bucket: the lowest sample and then the
    /// highest, each scaled to ±127.
    ///
    /// Interleaved rather than two arrays, so that the two halves of a bucket
    /// cannot arrive at different lengths, and rather than a list of objects,
    /// which costs four times the bytes for the same numbers. The length is
    /// therefore always even and there are `peaks.len() / 2` buckets, each
    /// covering `duration_seconds / (peaks.len() / 2)`.
    ///
    /// Minimum *and* maximum rather than one magnitude, because asymmetric
    /// audio is a real thing and drawing it as a mirror image is a lie about
    /// the recording. Rounding is outwards, so a drawn waveform is never
    /// smaller than the audio it came from.
    #[serde(default)]
    pub peaks: Vec<i8>,
}

/// The alphabet RFC 4648 defines for base64.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes bytes as base64, with padding.
///
/// Written here rather than taken as a dependency: it is twenty lines of
/// arithmetic with a fixed, ancient specification, `clipped-ipc` is the leaf
/// crate both ends of the application link, and the alternative is putting a
/// crate in front of every build of the recorder *and* the window to avoid
/// them. The same argument `clipped-background` makes for its own copy of
/// FNV-1a (AGENTS.md section 10).
///
/// The output is padded, which is what a `data:` URI takes and what `atob` in
/// the window reads. `base64_encodes_the_vectors_rfc_4648_states` below is what
/// holds it to the specification rather than to itself.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let block = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or_default()) << 8
            | u32::from(chunk.get(2).copied().unwrap_or_default());

        for position in 0..4 {
            // A chunk of one byte carries two characters and two `=`; a chunk of
            // two carries three and one. Padding rather than a shorter string,
            // because `data:` URIs and `atob` both take the padded form.
            if position > chunk.len() {
                encoded.push('=');
            } else {
                let index = (block >> (18 - 6 * position)) & 0b0011_1111;
                encoded.push(char::from(ALPHABET[index as usize]));
            }
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_the_vectors_rfc_4648_states() {
        // The specification's own test vectors, which is what makes this a check
        // of the encoding rather than of itself. They cover all three padding
        // cases: no remainder, one byte over, two bytes over.
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "encoding `{plain}`");
        }
    }

    #[test]
    fn base64_covers_every_byte_and_both_of_the_last_two_characters() {
        // A JPEG is arbitrary bytes, so the two characters at the end of the
        // alphabet — `+` and `/`, the ones a URL-safe variant would have moved —
        // have to come out of this encoder, and every byte value has to survive
        // it. An alphabet with a typo in it passes the vectors above.
        let every: Vec<u8> = (0..=255).collect();
        let encoded = base64(&every);

        assert!(encoded.contains('+'), "the 63rd character never appeared");
        assert!(encoded.contains('/'), "the 64th character never appeared");
        assert_eq!(
            encoded.len(),
            344,
            "256 bytes is 344 characters with padding"
        );
        assert!(
            encoded
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || matches!(character, '+' | '/' | '=')),
            "base64 is ASCII, and a `data:` URI carries it unescaped"
        );
    }

    #[test]
    fn a_pending_preview_carries_neither_a_picture_nor_a_reason() {
        // The three states have to be told apart by the state and never by which
        // fields happen to be present, which is only true if the fields that do
        // not belong are actually absent.
        let pending = Preview::pending(PreviewKind::Thumbnail);
        let json = serde_json::to_value(&pending).expect("a preview serialises");

        assert_eq!(json["state"], "pending");
        assert_eq!(json["kind"], "thumbnail");
        assert!(json.get("picture").is_none(), "{json}");
        assert!(json.get("reason").is_none(), "{json}");
    }

    #[test]
    fn an_unavailable_preview_says_why() {
        let refused = Preview::unavailable(PreviewKind::Waveform, "there is no audio in that file");

        assert_eq!(refused.state, PreviewState::Unavailable);
        assert_eq!(
            refused.reason.as_deref(),
            Some("there is no audio in that file")
        );
        assert!(
            refused.tracks.is_empty(),
            "a screen drawing a row per track needs no branch"
        );
    }

    #[test]
    fn a_request_that_leaves_out_the_kind_is_not_a_request() {
        // Rather than defaulting to a thumbnail, which would answer a screen
        // asking for peaks with a picture it cannot draw.
        let error = serde_json::from_value::<OpenPreview>(serde_json::json!({
            "source": r"D:\clips\cs2.mkv"
        }))
        .expect_err("a request with no kind is refused");

        assert!(error.to_string().contains("kind"), "{error}");
    }

    #[test]
    fn a_waveform_request_may_leave_out_the_resolution() {
        let asked: OpenPreview = serde_json::from_value(serde_json::json!({
            "source": r"D:\clips\cs2.mkv",
            "kind": "waveform"
        }))
        .expect("the resolution is optional");

        assert_eq!(asked.buckets, None);
        assert_eq!(asked.kind, PreviewKind::Waveform);
    }

    #[test]
    fn a_preview_survives_the_round_trip_it_will_actually_make() {
        // Serialised and read back, because that is what the wire does to it,
        // and because a field that serialises and does not deserialise is a
        // field the window silently never sees.
        let sent = Preview::thumbnail(PreviewPicture {
            media_type: "image/jpeg".to_owned(),
            bytes: base64(&[0xFF, 0xD8, 0xFF, 0xE0]),
            width: 640,
            height: 360,
            at_seconds: 184.5,
            blank: false,
        });

        let text = serde_json::to_string(&sent).expect("a preview serialises");
        let back: Preview = serde_json::from_str(&text).expect("and reads back");

        assert_eq!(back, sent);
        assert_eq!(back.picture.expect("the picture").bytes, "/9j/4A==");
    }

    #[test]
    fn a_field_this_build_has_never_heard_of_is_ignored() {
        // The compatibility policy, on the newest thing on the wire: a recorder
        // that grows a field here must not stop an older window reading the
        // picture (`crate::message`).
        let back: Preview = serde_json::from_value(serde_json::json!({
            "kind": "thumbnail",
            "state": "ready",
            "picture": {
                "media_type": "image/jpeg",
                "bytes": "Zm9v",
                "width": 640,
                "height": 360,
                "at_seconds": 1.0,
                "blank": false,
                "colour_space": "bt709"
            },
            "generated_by": "a later build"
        }))
        .expect("an unknown field is ignored");

        assert_eq!(back.state, PreviewState::Ready);
        assert_eq!(back.picture.expect("the picture").width, 640);
    }
}
