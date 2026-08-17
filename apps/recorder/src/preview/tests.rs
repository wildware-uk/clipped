//! What `open_preview` answers, against caches built for the test.
//!
//! Every cache here is a directory this module creates and deletes, never
//! `%LOCALAPPDATA%\Clipped` (AGENTS.md section 25): a test that read the
//! thumbnail cache of whoever ran it would pass or fail on their library.
//!
//! The entries are written by hand, from the two formats `docs/thumbnails.md`
//! and `docs/waveforms.md` publish, rather than by generating a thumbnail or a
//! waveform. That is deliberate and it does two jobs. It keeps these tests off
//! FFmpeg — there is no recording to decode, so nothing here depends on a
//! machine having an encoder — and it holds the *documented* formats against
//! the readers, so a sidecar written from the documentation and a sidecar the
//! cache will read are proven to be the same thing.

use std::fs;
use std::path::{Path, PathBuf};

use clipped_ipc::{OpenPreview, PreviewKind, PreviewState};
use clipped_library::thumbnail::{
    ServiceOptions, SourceIdentity, ThumbnailCache, ThumbnailService,
};
use clipped_waveform::{
    ServiceOptions as WaveformOptions, WaveformCache, WaveformService, OVERVIEW_BUCKETS,
};

use super::*;
use crate::test_support::Scratch as ScratchDirectory;

/// A directory of this test's own, removed when the test that made it passes.
///
/// Named after the test as well as after the process, so that a directory a
/// failure left behind says which test left it. The removal is
/// [`ScratchDirectory`]'s: what this had was `let _ = fs::remove_dir_all(…)` in
/// [`Drop`], which took a failing test's evidence with it and could not report
/// a removal that did not happen (issue #598).
struct Scratch {
    root: ScratchDirectory,
}

impl Scratch {
    fn new(test: &str) -> Self {
        Self {
            root: ScratchDirectory::new(&format!("preview-{test}")),
        }
    }

    fn join(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).expect("a scratch subdirectory can be made");
        path
    }

    /// A file standing in for a recording: the caches key on its path, its
    /// length and its modification time, and never on what is in it.
    fn recording(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the stand-in recording can be written");
        path
    }
}

/// Asking for one recording's thumbnail.
fn asking(source: &Path, kind: PreviewKind) -> OpenPreview {
    OpenPreview {
        source: source.to_string_lossy().into_owned(),
        kind,
        buckets: None,
    }
}

/// Writes a thumbnail cache entry for `recording`, as `docs/thumbnails.md`
/// specifies one: the picture, and a sidecar naming the recording it came from.
fn store_thumbnail(cache: &Path, recording: &Path, picture: &[u8]) {
    let identity = SourceIdentity::of(recording).expect("the stand-in recording can be stat-ed");
    let key = identity.cache_key();
    fs::write(cache.join(format!("{key}.jpg")), picture).expect("the picture can be written");
    let sidecar = serde_json::json!({
        "version": 1,
        "recording": recording.to_string_lossy(),
        "size_bytes": identity.size(),
        "modified_nanos": identity.modified_nanos(),
        "image": {
            "file": format!("{key}.jpg"),
            "width": 640,
            "height": 360,
            "at_seconds": 184.5,
            "blank": false
        }
    });
    fs::write(
        cache.join(format!("{key}.json")),
        serde_json::to_vec(&sidecar).expect("the sidecar serialises"),
    )
    .expect("the sidecar can be written");
}

/// Writes a thumbnail cache entry that records why there is no picture.
fn store_thumbnail_failure(cache: &Path, recording: &Path, reason: &str) {
    let identity = SourceIdentity::of(recording).expect("the stand-in recording can be stat-ed");
    let key = identity.cache_key();
    let sidecar = serde_json::json!({
        "version": 1,
        "recording": recording.to_string_lossy(),
        "size_bytes": identity.size(),
        "modified_nanos": identity.modified_nanos(),
        "failure": reason
    });
    fs::write(
        cache.join(format!("{key}.json")),
        serde_json::to_vec(&sidecar).expect("the sidecar serialises"),
    )
    .expect("the sidecar can be written");
}

/// One track of a waveform entry, as this test wants it written.
struct Track {
    index: u32,
    name: &'static str,
    /// One pair per bucket, at [`BUCKET_NANOS`].
    peaks: Vec<(i8, i8)>,
}

/// The bucket every entry here is written at: `clipped-waveform`'s base
/// resolution, ten milliseconds (`docs/waveforms.md`).
const BUCKET_NANOS: u64 = 10_000_000;

/// Writes a `.cwf` cache entry, byte for byte as `docs/waveforms.md` specifies
/// one.
///
/// One level, not a pyramid: `TrackWaveform::overview` reads whichever level it
/// finds and merges downwards, and a single base level is a legal entry — which
/// is also what makes this a check of the reader rather than of a fixture
/// generated by the writer beside it.
fn store_waveform(cache: &Path, recording: &Path, tracks: &[Track]) {
    let identity = SourceIdentity::of(recording).expect("the stand-in recording can be stat-ed");
    let path = recording.to_string_lossy();
    let path = path.as_bytes();

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"CLIPWAVE");
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(tracks.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&identity.size().to_le_bytes());
    bytes.extend_from_slice(&identity.modified_nanos().to_le_bytes());
    bytes.extend_from_slice(path);

    for track in tracks {
        let name = track.name.as_bytes();
        bytes.extend_from_slice(&track.index.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(name);
        let duration_nanos = BUCKET_NANOS * track.peaks.len() as u64;
        bytes.extend_from_slice(&duration_nanos.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&BUCKET_NANOS.to_le_bytes());
        bytes.extend_from_slice(&(track.peaks.len() as u32).to_le_bytes());
        for (minimum, maximum) in &track.peaks {
            bytes.push(*minimum as u8);
            bytes.push(*maximum as u8);
        }
    }

    fs::write(cache.join(format!("{}.cwf", identity.cache_key())), &bytes)
        .expect("the waveform entry can be written");
}

fn thumbnails(directory: &Path) -> ThumbnailService {
    ThumbnailService::start(ThumbnailCache::at(directory), ServiceOptions::new())
}

fn waveforms(directory: &Path) -> WaveformService {
    WaveformService::start(WaveformCache::at(directory), WaveformOptions::new())
}

#[test]
fn a_thumbnail_reaches_the_window_as_the_picture_of_that_recording() {
    // The claim worth making, and the one a "it answered with some bytes" test
    // would not: the base64 in the reply decodes to *this* recording's picture
    // and not to the other recording's, which is the failure a cache keyed on
    // the wrong thing produces and which nothing else here would catch.
    let scratch = Scratch::new("of-that-recording");
    let cache = scratch.join("thumbnails");
    let one = scratch.recording("one.mkv", b"the first recording");
    let two = scratch.recording("two.mkv", b"the second recording, of a different length");
    store_thumbnail(&cache, &one, b"picture of one");
    store_thumbnail(&cache, &two, b"picture of two");
    let service = thumbnails(&cache);

    let preview = open(&asking(&one, PreviewKind::Thumbnail), Some(&service), None)
        .expect("a thumbnail that is there is not a refusal");

    assert_eq!(preview.state, PreviewState::Ready);
    assert_eq!(preview.kind, PreviewKind::Thumbnail);
    let picture = preview
        .picture
        .expect("a ready thumbnail carries a picture");
    assert_eq!(
        picture.bytes,
        clipped_ipc::base64(b"picture of one"),
        "the window was handed the wrong recording's picture"
    );
    assert_eq!(picture.media_type, "image/jpeg");
    assert_eq!((picture.width, picture.height), (640, 360));
    assert!((picture.at_seconds - 184.5).abs() < f64::EPSILON);
}

#[test]
fn a_recording_with_no_picture_yet_is_told_apart_from_one_whose_picture_could_not_be_read() {
    // Issue #448's second acceptance criterion, and the whole reason the state
    // is on the wire rather than inferred from an empty `picture` field. Both
    // recordings below draw a tile with no picture in it; only one of them says
    // why, and a screen that could not tell them apart would report a
    // disconnected drive as a library that has not been indexed yet.
    let scratch = Scratch::new("pending-against-unreadable");
    let cache = scratch.join("thumbnails");
    let never = scratch.recording("never.mkv", b"nothing has looked at this one yet");
    let gone = scratch.recording("gone.mkv", b"this one is about to be deleted");
    store_thumbnail(&cache, &gone, b"picture of a recording that is about to go");
    fs::remove_file(&gone).expect("the recording can be taken away");
    let service = thumbnails(&cache);

    let not_yet = open(
        &asking(&never, PreviewKind::Thumbnail),
        Some(&service),
        None,
    )
    .expect("a recording with no entry is not a refusal");
    let unreadable = open(&asking(&gone, PreviewKind::Thumbnail), Some(&service), None)
        .expect("a recording that has gone is not a refusal either");

    assert_eq!(not_yet.state, PreviewState::Pending);
    assert_eq!(
        not_yet.reason, None,
        "a picture that has not been made yet has nothing to explain"
    );
    assert!(not_yet.picture.is_none());
    assert_eq!(
        unreadable.state,
        PreviewState::Unavailable,
        "a recording that cannot be read must not look like one nobody has reached yet"
    );
    assert!(
        unreadable.reason.is_some_and(|reason| !reason.is_empty()),
        "an unavailable preview says why"
    );
}

#[test]
fn a_remembered_failure_reaches_the_window_as_a_reason_rather_than_as_nothing() {
    // The state `docs/thumbnails.md` writes down so that a broken recording is
    // not re-decoded once a tile: it has to survive the crossing, or the window
    // shows the same empty square it shows for a recording that has simply not
    // been reached yet and asks again for ever.
    let scratch = Scratch::new("remembered-failure");
    let cache = scratch.join("thumbnails");
    let truncated = scratch.recording("truncated.mkv", b"25 bytes of nothing much..");
    store_thumbnail_failure(&cache, &truncated, "the container could not be opened");
    let service = thumbnails(&cache);

    let preview = open(
        &asking(&truncated, PreviewKind::Thumbnail),
        Some(&service),
        None,
    )
    .expect("a remembered failure is not a refusal");

    assert_eq!(preview.state, PreviewState::Unavailable);
    assert!(
        preview
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("the container could not be opened")),
        "the remembered reason is what the window is told: {:?}",
        preview.reason
    );
    assert!(preview.picture.is_none());
}

#[test]
fn a_picture_made_for_a_recording_that_has_since_changed_is_not_shown() {
    // Invalidation, from the far end: the sidecar names the recording's length
    // and modification time, so a recording that was trimmed or re-encoded must
    // come back `Pending` rather than carrying the picture of what it used to
    // be. Without this a window would draw last week's frame against this
    // week's clip and nothing would ever notice.
    let scratch = Scratch::new("changed-since");
    let cache = scratch.join("thumbnails");
    let recording = scratch.recording("trimmed.mkv", b"the original recording");
    store_thumbnail(&cache, &recording, b"picture of the original");
    fs::write(
        &recording,
        b"the recording after it was trimmed, which is longer",
    )
    .expect("the recording can be rewritten");
    let service = thumbnails(&cache);

    let preview = open(
        &asking(&recording, PreviewKind::Thumbnail),
        Some(&service),
        None,
    )
    .expect("a stale entry is not a refusal");

    assert_eq!(preview.state, PreviewState::Pending);
    assert!(preview.picture.is_none(), "last week's frame was drawn");
}

#[test]
fn peaks_reach_the_window_by_the_same_command_and_are_the_peaks_of_that_track() {
    // Issue #448's third criterion. The same `open_preview`, the same three
    // states, the same reply — and the numbers are the ones in the entry, at
    // the width the caller said it could draw.
    let scratch = Scratch::new("peaks-of-that-track");
    let cache = scratch.join("waveforms");
    let recording = scratch.recording("sound.mkv", b"a recording with sound in it");
    store_waveform(
        &cache,
        &recording,
        &[
            Track {
                index: 1,
                name: "Game",
                peaks: vec![(-100, 120), (-4, 6), (0, 0), (-127, 127)],
            },
            Track {
                index: 2,
                name: "Microphone",
                peaks: vec![(-1, 1), (-1, 1), (-1, 1), (-1, 1)],
            },
        ],
    );
    let service = waveforms(&cache);

    let preview = open(
        &OpenPreview {
            buckets: Some(4),
            ..asking(&recording, PreviewKind::Waveform)
        },
        None,
        Some(&service),
    )
    .expect("a waveform that is there is not a refusal");

    assert_eq!(preview.state, PreviewState::Ready);
    assert_eq!(preview.kind, PreviewKind::Waveform);
    assert_eq!(preview.tracks.len(), 2, "one entry per sound track");
    let game = &preview.tracks[0];
    assert_eq!(game.index, 1);
    assert_eq!(game.name.as_deref(), Some("Game"));
    assert_eq!(game.sample_rate, 48_000);
    assert_eq!(game.channels, 2);
    assert_eq!(
        game.peaks,
        vec![-100, 120, -4, 6, 0, 0, -127, 127],
        "the minimum and then the maximum of each bucket, in order"
    );
    assert_eq!(game.peaks.len() / 2, 4, "four buckets, two numbers each");
    // The second track's numbers are different, which is what says the loop
    // reads each track rather than repeating the first.
    assert_eq!(preview.tracks[1].peaks, vec![-1, 1, -1, 1, -1, 1, -1, 1]);
}

#[test]
fn a_waveform_is_answered_at_the_width_the_caller_can_draw() {
    // The pyramid is what makes this cheap, and the request is what chooses the
    // rung. A caller that asked for eight buckets of a forty-bucket track gets
    // eight, each the merge of five — not forty for it to throw away, which for
    // an hour-long recording is the difference between a few kilobytes and more
    // than a frame holds.
    let scratch = Scratch::new("at-the-width-asked-for");
    let cache = scratch.join("waveforms");
    let recording = scratch.recording("wide.mkv", b"a long recording");
    let peaks: Vec<(i8, i8)> = (0..40)
        .map(|bucket| (-(bucket as i8), bucket as i8))
        .collect();
    store_waveform(
        &cache,
        &recording,
        &[Track {
            index: 1,
            name: "Game",
            peaks,
        }],
    );
    let service = waveforms(&cache);

    let asked = |buckets: Option<u32>| {
        open(
            &OpenPreview {
                buckets,
                ..asking(&recording, PreviewKind::Waveform)
            },
            None,
            Some(&service),
        )
        .expect("a waveform that is there is not a refusal")
    };

    assert_eq!(asked(Some(8)).tracks[0].peaks.len() / 2, 8);
    assert_eq!(
        asked(None).tracks[0].peaks.len() / 2,
        OVERVIEW_BUCKETS,
        "a caller that names no width is answered with an overview"
    );
    assert_eq!(
        asked(Some(MAX_PREVIEW_BUCKETS * 4)).tracks[0].peaks.len() / 2,
        MAX_PREVIEW_BUCKETS as usize,
        "a width past the bound is clamped rather than refused"
    );
    assert_eq!(
        asked(Some(0)).tracks[0].peaks.len() / 2,
        OVERVIEW_BUCKETS,
        "a row that has not been laid out yet is not a mistake"
    );
    // The merge is exact and outward: the last bucket of eight covers the last
    // five of forty, whose extremes are the 39th.
    let merged = asked(Some(8));
    assert_eq!(
        merged.tracks[0].peaks[14..16],
        [-39, 39],
        "merging buckets must keep the extremes rather than average them"
    );
}

#[test]
fn a_recording_with_no_sound_is_a_waveform_with_no_tracks_and_not_a_failure() {
    // `docs/waveforms.md`: zero tracks is a supported answer, and it is what
    // every recording Clipped writes today produces. A screen drawing a row per
    // track needs no branch, and nothing puts a banner over a silent file.
    let scratch = Scratch::new("no-sound");
    let cache = scratch.join("waveforms");
    let recording = scratch.recording("silent.mkv", b"a recording with no audio device");
    store_waveform(&cache, &recording, &[]);
    let service = waveforms(&cache);

    let preview = open(
        &asking(&recording, PreviewKind::Waveform),
        None,
        Some(&service),
    )
    .expect("a recording with no sound is not a refusal");

    assert_eq!(preview.state, PreviewState::Ready);
    assert!(preview.tracks.is_empty());
    assert_eq!(preview.reason, None);
}

#[test]
fn a_request_that_names_no_recording_is_refused_by_name() {
    let error = open(
        &OpenPreview {
            source: "   ".to_owned(),
            kind: PreviewKind::Thumbnail,
            buckets: None,
        },
        None,
        None,
    )
    .expect_err("a request with no source is not a preview");

    assert_eq!(error.code, ErrorCode::InvalidParameters);
    assert!(error.message.contains("source"), "{}", error.message);
}

#[test]
fn a_machine_with_nowhere_to_cache_refuses_rather_than_reporting_an_empty_library() {
    // Distinguishable from every per-recording answer, and deliberately: "this
    // build has nowhere to keep a picture" is one fact about the machine, not
    // one fact per row, and a screen that drew it as a pending tile would wait
    // for a picture that is never coming.
    let error = open(
        &OpenPreview {
            source: r"D:\clips\cs2.mkv".to_owned(),
            kind: PreviewKind::Waveform,
            buckets: None,
        },
        None,
        None,
    )
    .expect_err("no cache is a refusal");

    assert_eq!(error.code, ErrorCode::LibraryUnavailable);
    assert!(error.message.contains("waveforms"), "{}", error.message);
}

#[test]
fn a_picture_too_large_for_a_frame_is_refused_here_rather_than_by_the_transport() {
    // Nothing this build generates comes near the bound — a stored thumbnail is
    // 640 pixels wide and about 20 kB — which is exactly why it is worth having
    // a test drive it. Without one the guard is a comment: a cache holding
    // something enormous, from a build that stored a second size or from a file
    // somebody dropped in, would become a reply `write_message` refuses to send
    // and a window that waits for ever. Answered as a state a screen draws
    // instead.
    let scratch = Scratch::new("too-large");
    let cache = scratch.join("thumbnails");
    let recording = scratch.recording("enormous.mkv", b"a recording with a huge picture");
    store_thumbnail(&cache, &recording, &vec![0x5A; PICTURE_BUDGET + 1]);
    let service = thumbnails(&cache);

    let preview = open(
        &asking(&recording, PreviewKind::Thumbnail),
        Some(&service),
        None,
    )
    .expect("an oversized picture is not a refusal of the question");

    assert_eq!(preview.state, PreviewState::Unavailable);
    assert!(preview.picture.is_none(), "it must not be sent anyway");
    assert!(
        preview
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("more than one message carries")),
        "the reason says what happened: {:?}",
        preview.reason
    );
}

#[test]
fn a_page_of_thumbnails_is_a_page_of_frames_and_each_one_fits() {
    // Issue #448's last criterion, measured rather than assumed. `docs/ipc.md`
    // pages the library 25 sittings at a time and `docs/library.md` measures
    // that against ten thousand of them, so the question is what twenty-five
    // pictures cost — and the answer that matters is that each is its own
    // frame, so the bound that has to hold is per picture and not per page.
    //
    // The figures are printed with `--nocapture` and quoted in
    // `docs/thumbnails.md`.
    let scratch = Scratch::new("a-page-of-frames");
    let cache = scratch.join("thumbnails");
    let service = thumbnails(&cache);

    // A stored thumbnail is 640 pixels wide and about 20 kB
    // (`docs/thumbnails.md`, measured). Incompressible bytes, because a JPEG of
    // a game is not compressible either and a page of zeroes would measure the
    // wrong thing.
    let picture: Vec<u8> = (0..20_073_u32).map(|byte| (byte % 251) as u8).collect();
    let mut recordings = Vec::new();
    for index in 0_u32..25 {
        let recording = scratch.recording(&format!("page-{index}.mkv"), &index.to_le_bytes());
        store_thumbnail(&cache, &recording, &picture);
        recordings.push(recording);
    }

    let mut total = 0_usize;
    let mut largest = 0_usize;
    for recording in &recordings {
        let preview = open(
            &asking(recording, PreviewKind::Thumbnail),
            Some(&service),
            None,
        )
        .expect("each picture is there");
        let frame = serde_json::to_vec(&clipped_ipc::Reply::PreviewOpened { preview })
            .expect("a reply serialises");
        total += frame.len();
        largest = largest.max(frame.len());
    }

    println!(
        "a page of 25 thumbnails: {total} bytes in 25 frames, largest {largest}, \
         frame limit {MAX_FRAME_BYTES}"
    );

    assert!(
        largest < MAX_FRAME_BYTES as usize,
        "one thumbnail must fit in one frame: {largest} against {MAX_FRAME_BYTES}"
    );
    // Base64 is four characters for every three bytes, so 20,073 bytes is
    // 26,764 characters and the envelope around it is small. The upper bound is
    // what would fail if a future change started sending something else — a
    // second size, or the recording's path — alongside the picture.
    assert!(
        (26_764..28_000).contains(&largest),
        "a 20 kB picture should cross as about 27 kB of base64, and this was {largest}"
    );
    assert!(
        total < MAX_FRAME_BYTES as usize,
        "a whole page of 25 is {total} bytes, which happens to be inside one frame; \
         it is sent as 25 frames regardless, which is what makes the page size not a bound"
    );
}
