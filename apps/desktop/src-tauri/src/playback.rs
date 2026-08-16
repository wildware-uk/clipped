//! The `clip` URI scheme: the only way media reaches this window.
//!
//! # Why a scheme of this application's own
//!
//! The window has no file-system permission and no asset protocol, and issue
//! #304's last criterion is that whatever it gains to play a recording is *the
//! smallest thing that works*. The two alternatives are both larger:
//!
//! - **Tauri's asset protocol** serves any path inside a scope, and a recording
//!   lives wherever the recorder's output directory points — a setting — so the
//!   only scope that would work is every path on the machine. That is the same
//!   objection `open_recording` and `reveal_recording` exist for
//!   (`main.rs`, `capabilities/default.json`).
//! - **A file-system permission** would let the interface read anything it can
//!   name, for a feature that needs to read one file it was told about.
//!
//! What is here instead: a scheme that serves **nothing at all** until the
//! recorder has answered `open_playback` for a recording, and then serves that
//! one file. The window never learns a path — it is handed
//! `http://clip.localhost/<number>` — and a number nothing has registered is a
//! 404. So the privilege the window gains is exactly "read the recordings you
//! have opened for playback in this session", and it is a *consequence* of the
//! recorder's answer rather than something the window can ask for.
//!
//! `apps/desktop/src/playbackReach.test.ts` is the test that holds this
//! boundary to its description.
//!
//! # Ranges
//!
//! Seeking is a range request, so this answers them: `206` with a
//! `Content-Range`, `Accept-Ranges: bytes` on every answer, and `416` for a
//! range that starts past the end. Without that a media element has to fetch a
//! whole recording before it can play the last ten seconds of it.
//!
//! Each answer is bounded by [`CHUNK`] rather than being the rest of the file:
//! a protocol handler answers with bytes in memory rather than a stream, and a
//! recording is measured in gigabytes.

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::http::{Request, Response, StatusCode};

/// The scheme this application registers.
pub(crate) const SCHEME: &str = "clip";

/// The origin the webview sees it as, which is what the policy has to name.
///
/// Windows serves a custom scheme as `http://<scheme>.localhost`, so this is
/// the string in `tauri.conf.json`'s `csp` and the one
/// `apps/desktop/src/playbackReach.test.ts` reads.
pub(crate) const ORIGIN: &str = "http://clip.localhost";

/// The most any one answer carries.
///
/// Four mebibytes: enough that a media element's opening read reaches the
/// moov atom and the first seconds of media in one or two answers, small enough
/// that a dozen of them in flight is not a problem. A range request for more
/// than this is answered with this much and the `Content-Range` that says so,
/// which is what a partial answer is for.
const CHUNK: u64 = 4 * 1024 * 1024;

/// The recordings the recorder has vouched for in this session.
///
/// Keyed both ways on purpose: by number, because that is what the window is
/// given and what an answer is looked up by; and by path, so that opening the
/// same track twice hands back the same URL and the element carries on playing
/// rather than reloading.
static SERVED: Mutex<Option<Registry>> = Mutex::new(None);

/// The next number to hand out.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// What has been vouched for.
#[derive(Debug, Default)]
struct Registry {
    by_number: HashMap<u64, PathBuf>,
    by_path: HashMap<PathBuf, u64>,
}

/// Registers a file the recorder answered with, and returns the URL for it.
///
/// The only way anything becomes reachable from the window. It is called with
/// what `open_playback` returned and nothing else — never with a path the
/// interface supplied — which is what makes the reach the recorder's decision
/// rather than the window's.
pub(crate) fn url_for(path: &Path) -> String {
    let mut guard = SERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let registry = guard.get_or_insert_with(Registry::default);

    let number = if let Some(number) = registry.by_path.get(path) {
        *number
    } else {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        registry.by_number.insert(number, path.to_path_buf());
        registry.by_path.insert(path.to_path_buf(), number);
        number
    };

    format!("{ORIGIN}/{number}")
}

/// The file a number stands for, if anything does.
fn served(number: u64) -> Option<PathBuf> {
    SERVED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|registry| registry.by_number.get(&number).cloned())
}

/// Answers one request on the `clip` scheme.
///
/// A free function over a lookup rather than a closure, so that the whole of it
/// can be tested without a webview: Tauri's mock runtime does not carry URI
/// scheme protocols, so a handler written inline would be covered by nothing at
/// all.
pub(crate) fn respond(
    request: &Request<Vec<u8>>,
    look_up: impl Fn(u64) -> Option<PathBuf>,
) -> Response<Vec<u8>> {
    let number = request
        .uri()
        .path()
        .trim_start_matches('/')
        .parse::<u64>()
        .ok();

    let Some(path) = number.and_then(look_up) else {
        // Deliberately the same answer for a number that was never handed out
        // and one that is not a number: this says nothing about what exists.
        return refusal(
            StatusCode::NOT_FOUND,
            "Clipped is not serving anything at that address.",
        );
    };

    let range = request
        .headers()
        .get(tauri::http::header::RANGE)
        .and_then(|value| value.to_str().ok());

    read(&path, range)
}

/// Reads the part of the file the request asked for.
fn read(path: &Path, range: Option<&str>) -> Response<Vec<u8>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        // The recording was there when the recorder opened it and is not there
        // now — a drive unplugged mid-playback. The element reports an error
        // and the screen says what happened, which beats a stall.
        Err(error) => {
            return refusal(
                StatusCode::NOT_FOUND,
                &format!("that recording could not be read: {error}"),
            )
        }
    };
    let length = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("that recording could not be measured: {error}"),
            )
        }
    };

    let asked = range.and_then(parse_range);
    let start = asked.map_or(0, |(start, _)| start);

    if start >= length && length > 0 {
        // A seek past the end. `*/length` is what tells the element how long the
        // file really is, so it can ask again for something that exists.
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(
                tauri::http::header::CONTENT_RANGE,
                format!("bytes */{length}"),
            )
            .header(tauri::http::header::ACCEPT_RANGES, "bytes")
            .body(Vec::new())
            .unwrap_or_else(|_| Response::new(Vec::new()));
    }

    let last = asked
        .and_then(|(_, end)| end)
        .unwrap_or(length.saturating_sub(1))
        .min(length.saturating_sub(1));
    let wanted = last.saturating_sub(start).saturating_add(1).min(CHUNK);

    let mut body = vec![0; usize::try_from(wanted).unwrap_or(0)];
    if file.seek(SeekFrom::Start(start)).is_err() {
        return refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "that recording could not be read from where the player asked",
        );
    }
    let read = match file.read(&mut body) {
        Ok(read) => read,
        Err(error) => {
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("that recording could not be read: {error}"),
            )
        }
    };
    body.truncate(read);

    let end = start + body.len().max(1) as u64 - 1;
    // A partial answer whenever it is not the whole file, whether or not the
    // element asked in as many words: the alternative is holding a recording in
    // memory, and `Content-Range` is what makes a partial answer honest.
    let partial = asked.is_some() || (body.len() as u64) < length;

    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(tauri::http::header::ACCEPT_RANGES, "bytes")
        .header(tauri::http::header::CONTENT_TYPE, content_type(path))
        .header(tauri::http::header::CONTENT_LENGTH, body.len().to_string());
    if partial {
        builder = builder.header(
            tauri::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}"),
        );
    }

    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// The first byte range of a `Range` header, where it asks for one.
///
/// Only `bytes=start-` and `bytes=start-end` are understood, which is what a
/// media element sends. A suffix range (`bytes=-500`) and a multi-range request
/// are answered as though no range had been asked for, because answering half
/// of what was asked would be worse than answering all of it.
fn parse_range(header: &str) -> Option<(u64, Option<u64>)> {
    let ranges = header.trim().strip_prefix("bytes=")?;
    if ranges.contains(',') {
        return None;
    }
    let (start, end) = ranges.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = match end.trim() {
        "" => None,
        value => Some(value.parse::<u64>().ok()?),
    };
    Some((start, end))
}

/// What a file is, as far as a media element needs to know.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mkv") => "video/x-matroska",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        // Everything Clipped serves is one of the three above. Anything else is
        // answered as bytes rather than as a guess.
        _ => "application/octet-stream",
    }
}

/// A refusal, in words rather than as an empty body.
fn refusal(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(tauri::http::header::CONTENT_TYPE, "text/plain")
        .body(message.as_bytes().to_vec())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// The handler Tauri registers, over the real registry.
pub(crate) fn handle(request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    respond(request, served)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte a fixture holds at one offset.
    ///
    /// A rule rather than a table, so that an assertion about the bytes that
    /// came back is written the same way the file was written.
    fn byte_at(offset: usize) -> u8 {
        u8::try_from(offset % 251).unwrap_or_default()
    }

    /// A file of `length` predictable bytes.
    fn media(name: &str, length: usize) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("clipped-playback-{}-{name}", std::process::id()));
        let bytes: Vec<u8> = (0..length).map(byte_at).collect();
        std::fs::write(&path, bytes).expect("the fixture can be written");
        path
    }

    /// A request for `path`, with an optional `Range`.
    fn asking(path: &str, range: Option<&str>) -> Request<Vec<u8>> {
        let mut builder = Request::builder().uri(format!("{ORIGIN}{path}"));
        if let Some(range) = range {
            builder = builder.header(tauri::http::header::RANGE, range);
        }
        builder.body(Vec::new()).expect("the request is built")
    }

    #[test]
    fn an_address_nothing_has_been_registered_at_is_not_served() {
        // The whole of the boundary this scheme exists to keep: the window
        // cannot name a file, so a number nothing vouched for is a 404 rather
        // than a read of something.
        let response = respond(&asking("/7", None), |_| None);

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            String::from_utf8_lossy(response.body()).contains("not serving"),
            "a refusal says what happened rather than being an empty body"
        );
    }

    #[test]
    fn something_that_is_not_a_number_is_refused_the_same_way() {
        // Including a path. A handler that fell back to treating the address as
        // a file name would hand the window the file-system reach this exists
        // to withhold.
        let response = respond(&asking("/C:/Windows/System32/config/SAM", None), |_| {
            panic!("a path must never reach the lookup")
        });

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_range_is_answered_with_exactly_the_bytes_it_asked_for() {
        let path = media("range.mkv", 10_000);
        let served = path.clone();

        let response = respond(&asking("/1", Some("bytes=100-199")), move |number| {
            (number == 1).then(|| served.clone())
        });

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes 100-199/10000")
        );
        assert_eq!(response.body().len(), 100);
        // The bytes themselves, not merely the right number of them: an
        // off-by-one in the seek is invisible to a length assertion and audible
        // in a player.
        assert_eq!(response.body()[0], byte_at(100));
        assert_eq!(response.body()[99], byte_at(199));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_open_ended_range_is_answered_in_one_bounded_piece() {
        // What a media element opens with is `bytes=0-`. Answering it with the
        // whole file would hold a recording in memory, so the answer is bounded
        // and says so — which is what lets the element ask for the next piece.
        let length = (CHUNK + 1_000) as usize;
        let path = media("open.mkv", length);
        let served = path.clone();

        let response = respond(&asking("/1", Some("bytes=0-")), move |_| {
            Some(served.clone())
        });

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body().len() as u64, CHUNK);
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some(format!("bytes 0-{}/{length}", CHUNK - 1).as_str())
        );
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes"),
            "an element that is not told ranges are accepted will not seek"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_seek_past_the_end_is_told_how_long_the_recording_is() {
        let path = media("past.mkv", 500);
        let served = path.clone();

        let response = respond(&asking("/1", Some("bytes=900-")), move |_| {
            Some(served.clone())
        });

        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("bytes */500")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_recording_small_enough_to_answer_whole_is_answered_whole() {
        let path = media("small.mkv", 64);
        let served = path.clone();

        let response = respond(&asking("/1", None), move |_| Some(served.clone()));

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body().len(), 64);
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("video/x-matroska"),
            "Matroska is what a Clipped recording is, and the element is told so"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_recording_that_has_gone_since_it_was_opened_is_reported_rather_than_stalled() {
        let missing = std::env::temp_dir().join("clipped-playback-nothing-here.mkv");
        let _ = std::fs::remove_file(&missing);

        let response = respond(&asking("/1", None), move |_| Some(missing.clone()));

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_same_file_keeps_the_same_address() {
        // Choosing the same track twice must not move the URL: the element
        // would reload and lose its position for no reason.
        let path = std::env::temp_dir().join("clipped-playback-stable.mkv");

        assert_eq!(url_for(&path), url_for(&path));
        assert_ne!(
            url_for(&path),
            url_for(&std::env::temp_dir().join("clipped-playback-other.mkv"))
        );
        assert!(
            url_for(&path).starts_with("http://clip.localhost/"),
            "the window is handed an address rather than a path: {}",
            url_for(&path)
        );
    }

    #[test]
    fn a_registered_recording_is_served_through_the_real_registry() {
        // `url_for` and `handle` over the static, rather than over a lookup a
        // test supplied: the two halves have to agree about what a number means.
        let path = media("registered.mkv", 32);
        let url = url_for(&path);
        let number = url
            .rsplit('/')
            .next()
            .expect("the address ends in a number")
            .to_owned();

        let response = handle(&asking(&format!("/{number}"), None));

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.body().len(), 32);

        let _ = std::fs::remove_file(&path);
    }
}
