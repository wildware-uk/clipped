//! The `open_playback` command: a finished recording, ready for a `<video>`.
//!
//! # What the window can and cannot do for itself
//!
//! The window is a WebView2, which is Chromium, and two facts about it decide
//! everything here.
//!
//! **It plays the recording.** Measured rather than assumed: a Clipped
//! recording — AV1 picture, uncompressed PCM sound, in Matroska — loads,
//! decodes and plays in the engine the window is drawn by
//! (`docs/adr/0011-what-the-webview-plays.md`). So the ordinary case costs
//! nothing at all: the answer to "what should I play" is the recording itself.
//!
//! **It cannot choose an audio track.** `HTMLMediaElement.audioTracks` is not
//! implemented in Chromium, so a media element handed a recording with four
//! sound tracks plays the first one and offers no way off it. Hearing the
//! microphone on its own therefore means being handed a file that holds the
//! microphone and nothing else, and only a process that links the muxer can
//! make one — which is this one
//! ([issue #304](https://github.com/wildware-uk/clipped/issues/304)).
//!
//! So this module answers with one of two things, and says which:
//!
//! | The track asked for | What is served | What it costs |
//! | --- | --- | --- |
//! | the one a media element would reach anyway | the recording | nothing |
//! | any other | a copy carrying that track alone | one pass over the file |
//!
//! # Why the recording is never modified
//!
//! `clipped_muxer::remux_to_mp4_carrying` opens the source for reading and
//! writes somewhere else, and the somewhere else is a cache directory of this
//! application's own — never beside the recording, and never over anything a
//! person put there (AGENTS.md sections 56 and 57).
//!
//! # The cache
//!
//! `%LOCALAPPDATA%\Clipped\playback`. An entry is named after the recording it
//! was made from, the track it carries, and a hash of the recording's size and
//! modification time — so a recording that changes cannot be answered with a
//! copy of what it used to be, and a track played twice is prepared once.
//! Entries older than [`KEEP_FOR`] are removed whenever one is asked for, so
//! the directory is bounded by what somebody has watched recently rather than
//! by everything they have ever watched.
//!
//! Nothing here is a user's file: a prepared copy holds one of the recording's
//! sound tracks, it is not in the library, and `PlaybackStream::prepared` says
//! so, so that nothing presents it as somebody's recording.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipped_ipc::{ErrorCode, OpenPlayback, PlaybackStream, PlaybackTrack, ProtocolError};
use clipped_logging::RedactedPath;
use clipped_muxer::{remux_to_mp4_carrying, AudioTracks, Mp4Plan, PlannedTrack, TrackKind};

/// The directory prepared copies are kept in, under Clipped's per-user data.
const CACHE_DIRECTORY: &str = "playback";

/// How long a prepared copy is kept before it is swept up.
///
/// A day, which is the span over which somebody goes back to the same
/// recording. Long enough that switching between tracks, or coming back to a
/// clip after lunch, costs nothing; short enough that a week of watching does
/// not leave a week of copies. There is no size budget beside it because a copy
/// is only ever made when a track is *chosen*, which is a deliberate act.
const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Opens a recording for playback, on the track that was asked for.
///
/// # Errors
///
/// - [`ErrorCode::InvalidParameters`] when the request names no recording.
/// - [`ErrorCode::PlaybackFailed`] when the recording's file has gone, when it
///   cannot be read, or when the track asked for is not one it has. Each says
///   which, because they are different things to do something about.
pub fn open(request: &OpenPlayback) -> Result<PlaybackStream, ProtocolError> {
    let source = named(&request.source)?;

    // Before the muxer, so that the commonest failure gets the sentence it
    // deserves rather than FFmpeg's. A recording can go between the library
    // listing it and somebody pressing play — a drive unplugged, a folder tidied
    // — and "the file has gone" is something a person can act on, where
    // "could not be read: I/O error" is not (AGENTS.md sections 15 and 45).
    if !matches!(source.try_exists(), Ok(true)) {
        return Err(gone(source));
    }

    let plan = Mp4Plan::inspect(source).map_err(|error| {
        ProtocolError::new(
            ErrorCode::PlaybackFailed,
            // The muxer's own sentence, unchanged, exactly as an export passes
            // it on.
            error.to_string(),
        )
    })?;

    let tracks = sound_tracks(&plan);
    let chosen = choose(&tracks, request.audio_track, source)?;

    if served_as_recorded(&tracks, chosen) {
        return Ok(PlaybackStream {
            path: source.to_string_lossy().into_owned(),
            audio_track: chosen,
            audio_tracks: tracks,
            prepared: false,
        });
    }

    let Some(index) = chosen else {
        // Unreachable: a recording with no sound is always served as recorded.
        // Written as an answer rather than an `unwrap`, because a panic in a
        // connection thread is a recorder that stops answering (AGENTS.md
        // section 15).
        return Ok(PlaybackStream {
            path: source.to_string_lossy().into_owned(),
            audio_track: None,
            audio_tracks: tracks,
            prepared: false,
        });
    };

    let prepared = prepare(source, index)?;
    Ok(PlaybackStream {
        path: prepared.to_string_lossy().into_owned(),
        audio_track: Some(index),
        audio_tracks: tracks,
        prepared: true,
    })
}

/// The recording the request names, or a refusal saying it named none.
fn named(value: &str) -> Result<&Path, ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidParameters,
            "`open_playback` needs a `source`, and was given none".to_owned(),
        ));
    }
    Ok(Path::new(value))
}

/// The refusal for a recording whose file is not there.
///
/// The same sentence the window's own check produces for Open and Reveal
/// (`apps/desktop/src-tauri/src/main.rs`), so that a recording which has gone
/// reads the same however somebody reached it.
fn gone(source: &Path) -> ProtocolError {
    let name = source
        .file_name()
        .map_or_else(|| source.to_string_lossy(), |name| name.to_string_lossy());
    ProtocolError::new(
        ErrorCode::PlaybackFailed,
        format!(
            "{name} is not there any more. It may have been moved or deleted, or the drive it is \
             on may not be connected."
        ),
    )
}

/// The recording's sound tracks, in the order the container declares them.
fn sound_tracks(plan: &Mp4Plan) -> Vec<PlaybackTrack> {
    plan.tracks()
        .iter()
        .filter(|track| matches!(track.kind(), TrackKind::Audio))
        .map(|track: &PlannedTrack| PlaybackTrack {
            index: track.index(),
            name: track.name().map(str::to_owned),
            language: track.language().map(str::to_owned),
            default: track.is_default(),
        })
        .collect()
}

/// Which track to serve: the one asked for, or the one a player should take.
///
/// [`None`] when the recording has no sound at all, which is a real recording
/// (a capture that found no audio device) rather than a failure.
///
/// # Errors
///
/// [`ErrorCode::PlaybackFailed`] when the request named a track the recording
/// has not got. Refused rather than quietly answered with the default: a window
/// that asked for the microphone and was handed the mix would play something,
/// with sound, and look exactly as though it had worked.
fn choose(
    tracks: &[PlaybackTrack],
    asked: Option<usize>,
    source: &Path,
) -> Result<Option<usize>, ProtocolError> {
    match asked {
        Some(index) if tracks.iter().any(|track| track.index == index) => Ok(Some(index)),
        Some(index) => Err(ProtocolError::new(
            ErrorCode::PlaybackFailed,
            format!(
                "{} has no sound track {index}. It has {}.",
                source
                    .file_name()
                    .map_or_else(|| source.to_string_lossy(), |name| name.to_string_lossy()),
                match tracks.len() {
                    0 => "none at all".to_owned(),
                    1 => "one".to_owned(),
                    count => format!("{count}"),
                }
            ),
        )),
        // The track the container flags, which for a Clipped recording is the
        // compatibility mix and leads the file (`docs/muxing.md`). Falling back
        // to the first is for everything else: a file with no flag anywhere is
        // not a file to refuse to play.
        None => Ok(tracks
            .iter()
            .find(|track| track.default)
            .or_else(|| tracks.first())
            .map(|track| track.index)),
    }
}

/// Whether the recording itself already plays the chosen track.
///
/// **The first sound track, not the flagged one.** Chromium ignores Matroska's
/// default-track flag and takes the first audio stream it finds — measured, and
/// recorded in `docs/adr/0011-what-the-webview-plays.md` — so this asks the
/// question the media element will actually answer. For a Clipped recording the
/// two agree, because the compatibility mix both leads the file and carries the
/// flag; for anything else they need not, and a copy is what makes the answer
/// true rather than likely.
fn served_as_recorded(tracks: &[PlaybackTrack], chosen: Option<usize>) -> bool {
    match (tracks.first(), chosen) {
        (None, _) | (_, None) => true,
        (Some(first), Some(index)) => first.index == index,
    }
}

/// Makes, or finds, the copy that carries one track.
fn prepare(source: &Path, index: usize) -> Result<PathBuf, ProtocolError> {
    let directory = cache_directory()?;
    sweep(&directory);

    let destination = directory.join(cache_name(source, index));
    // A copy that is already there is a copy of *this* recording as it is now:
    // the name carries a hash of the file's size and modification time, so a
    // recording that has changed since has a different name rather than a stale
    // answer.
    if matches!(destination.try_exists(), Ok(true)) {
        return Ok(destination);
    }

    std::fs::create_dir_all(&directory).map_err(|error| {
        ProtocolError::new(
            ErrorCode::PlaybackFailed,
            format!("Clipped could not create its playback cache: {error}"),
        )
    })?;

    // Written under another name and moved into place, so that a second request
    // for the same track never finds a half-written file and plays it.
    let partial = directory.join(format!(
        "{}.partial-{}",
        cache_name(source, index),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&partial);

    let summary =
        remux_to_mp4_carrying(source, &partial, AudioTracks::Only(index)).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            ProtocolError::new(ErrorCode::PlaybackFailed, error.to_string())
        })?;

    tracing::info!(
        source = %RedactedPath::new(source),
        destination = %RedactedPath::new(&destination),
        audio_track = index,
        elapsed_ms = summary.elapsed().as_millis(),
        bytes = summary.byte_len(),
        "a recording was prepared for playback on one of its sound tracks"
    );

    if let Err(error) = std::fs::rename(&partial, &destination) {
        let _ = std::fs::remove_file(&partial);
        // Another connection preparing the same track at the same moment is the
        // ordinary way this happens on Windows, and its copy is as good as this
        // one — the name says which recording and which track.
        if matches!(destination.try_exists(), Ok(true)) {
            return Ok(destination);
        }
        return Err(ProtocolError::new(
            ErrorCode::PlaybackFailed,
            format!("Clipped could not finish preparing that track for playback: {error}"),
        ));
    }

    Ok(destination)
}

/// Where prepared copies live.
fn cache_directory() -> Result<PathBuf, ProtocolError> {
    clipped_logging::application_directory()
        .map(|directory| directory.join(CACHE_DIRECTORY))
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::PlaybackFailed,
                "this account has no per-user data directory (%LOCALAPPDATA% is unset), so \
                 Clipped has nowhere to prepare a track for playback"
                    .to_owned(),
            )
        })
}

/// What a prepared copy of one track of one recording is called.
///
/// The recording's own name, so the directory is readable; the track, so two
/// tracks of one recording do not collide; and a hash of the path, the size and
/// the modification time, so that two recordings with the same file name in
/// different folders do not share an entry and a recording that has been
/// rewritten does not reuse one.
fn cache_name(source: &Path, index: usize) -> String {
    let stem = source.file_stem().map_or_else(
        || "recording".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let stem: String = stem
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(48)
        .collect();

    let mut hasher = DefaultHasher::new();
    source.to_string_lossy().hash(&mut hasher);
    if let Ok(metadata) = source.metadata() {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified() {
            if let Ok(since) = modified.duration_since(SystemTime::UNIX_EPOCH) {
                since.as_nanos().hash(&mut hasher);
            }
        }
    }

    format!("{stem}-{:016x}-t{index}.mp4", hasher.finish())
}

/// Removes prepared copies nobody has asked for in [`KEEP_FOR`].
///
/// Reports nothing: a cache that could not be swept is not a reason to refuse
/// to play a recording, and the next request tries again. Failures are logged
/// rather than raised for the same reason the thumbnail cache's are.
fn sweep(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();

    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > KEEP_FOR));
        if stale {
            let path = entry.path();
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::debug!(
                    path = %RedactedPath::new(&path),
                    %error,
                    "a prepared playback file could not be swept up"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tracks a Clipped recording carries: the mix first and flagged, then
    /// the sources (`clipped_muxer::AudioSource`).
    fn clipped_recording() -> Vec<PlaybackTrack> {
        vec![
            PlaybackTrack {
                index: 1,
                name: Some("Compatibility Mix".to_owned()),
                language: None,
                default: true,
            },
            PlaybackTrack {
                index: 2,
                name: Some("Game".to_owned()),
                language: None,
                default: false,
            },
            PlaybackTrack {
                index: 3,
                name: Some("Microphone".to_owned()),
                language: None,
                default: false,
            },
        ]
    }

    #[test]
    fn a_request_that_names_no_track_gets_the_one_the_container_flags() {
        let chosen = choose(&clipped_recording(), None, Path::new("recording.mkv"))
            .expect("a recording with sound has a track to choose");

        assert_eq!(chosen, Some(1));
    }

    #[test]
    fn the_flagged_track_is_taken_even_when_it_does_not_lead_the_file() {
        // Not a Clipped recording: something else's file, whose second sound
        // track carries the flag. Falling back to "the first one" here would
        // play the track the file says not to.
        let tracks = vec![
            PlaybackTrack {
                index: 1,
                name: Some("Commentary".to_owned()),
                language: None,
                default: false,
            },
            PlaybackTrack {
                index: 2,
                name: Some("Main".to_owned()),
                language: None,
                default: true,
            },
        ];

        let chosen = choose(&tracks, None, Path::new("film.mkv")).expect("a track is chosen");

        assert_eq!(chosen, Some(2));
        // And because it is not the track a media element would reach, it is
        // the case a copy exists for.
        assert!(
            !served_as_recorded(&tracks, chosen),
            "a file whose flagged track is not first has to be prepared, or the element plays \
             the other one"
        );
    }

    #[test]
    fn the_recording_itself_is_served_when_it_already_plays_the_chosen_track() {
        // The whole reason playing a recording can cost nothing: the
        // compatibility mix leads a Clipped file, and the first sound track is
        // what Chromium takes.
        let tracks = clipped_recording();

        assert!(served_as_recorded(&tracks, Some(1)));
        assert!(
            !served_as_recorded(&tracks, Some(3)),
            "the microphone is not what a media element would play out of this file"
        );
    }

    #[test]
    fn a_recording_with_no_sound_is_played_rather_than_refused() {
        let chosen =
            choose(&[], None, Path::new("silent.mkv")).expect("a silent recording still plays");

        assert_eq!(chosen, None);
        assert!(served_as_recorded(&[], None));
    }

    #[test]
    fn a_track_the_recording_has_not_got_is_refused_rather_than_swapped_for_the_default() {
        let error = choose(&clipped_recording(), Some(7), Path::new("recording.mkv"))
            .expect_err("a track that is not there is refused");

        assert_eq!(error.code, ErrorCode::PlaybackFailed);
        assert!(
            error.message.contains('7') && error.message.contains("recording.mkv"),
            "the refusal should name the track and the recording: {}",
            error.message
        );
    }

    #[test]
    fn two_recordings_with_the_same_name_in_different_folders_do_not_share_a_prepared_copy() {
        // The bug this prevents is silent and awful: play yesterday's match,
        // then today's, and hear yesterday's sound over today's picture.
        let yesterday = cache_name(Path::new(r"D:\clips\2026-08-15\match.mkv"), 2);
        let today = cache_name(Path::new(r"D:\clips\2026-08-16\match.mkv"), 2);

        assert_ne!(yesterday, today);
        assert!(
            yesterday.starts_with("match-"),
            "unexpected name: {yesterday}"
        );
        assert!(
            yesterday.ends_with("-t2.mp4"),
            "unexpected name: {yesterday}"
        );
    }

    #[test]
    fn two_tracks_of_one_recording_do_not_share_a_prepared_copy() {
        let source = Path::new(r"D:\clips\match.mkv");

        assert_ne!(cache_name(source, 2), cache_name(source, 3));
        assert_eq!(
            cache_name(source, 2),
            cache_name(source, 2),
            "the same track of the same recording has to be found again rather than remade"
        );
    }

    #[test]
    fn a_request_with_no_recording_in_it_is_refused_before_anything_is_opened() {
        let error = open(&OpenPlayback {
            source: "   ".to_owned(),
            audio_track: None,
        })
        .expect_err("a playback request has to say what to play");

        assert_eq!(error.code, ErrorCode::InvalidParameters);
        assert!(
            error.message.contains("source"),
            "the refusal should name the field: {}",
            error.message
        );
    }

    #[test]
    fn a_recording_whose_file_has_gone_is_reported_as_that_rather_than_as_a_broken_file() {
        // The criterion this exists for: a player that does nothing is worse
        // than no player, so the answer names the file and says what probably
        // happened to it (AGENTS.md section 27).
        let error = open(&OpenPlayback {
            source: r"D:\clips\a recording nobody has\match.mkv".to_owned(),
            audio_track: None,
        })
        .expect_err("a recording that is not there cannot be played");

        assert_eq!(error.code, ErrorCode::PlaybackFailed);
        assert!(
            error.message.contains("match.mkv") && error.message.contains("not there any more"),
            "the refusal should name the file and say it has gone: {}",
            error.message
        );
    }
}
