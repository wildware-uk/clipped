//! [`facts_about`] against real files, because its whole job is reading one.
//!
//! Issue #272: `clipped-library` never opens a media file, so a recording whose
//! session sidecar is gone is invisible to the application for ever. Recovering
//! one means somebody opens it and says what it is, and this is that somebody.
//!
//! Every case builds its own file with the pinned FFmpeg rather than checking
//! one in, for the reason the rest of this crate's tests do: a fixture that can
//! be varied is what makes "two sound tracks" and "no sound tracks" separate
//! questions rather than one file's accident.

use clipped_export::facts_about;
use clipped_media_validation::{require_media_tools, TemporaryDirectory};

mod support;

use support::recording_with_sound;

/// The fixtures run for this many seconds.
const SECONDS: u32 = 3;

#[test]
fn a_recording_says_how_long_it_is_how_big_its_picture_is_and_how_many_tracks_it_has() {
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("facts-about-a-recording");
    let path = recording_with_sound(&tools, &directory, "two-tracks.mkv", SECONDS, 2);

    let facts = facts_about(&path).expect("a recording this test just wrote is readable");

    let duration = facts
        .duration()
        .expect("a container that declares a duration");
    assert!(
        duration.as_secs_f64() > f64::from(SECONDS) - 0.5
            && duration.as_secs_f64() < f64::from(SECONDS) + 0.5,
        "declared {duration:?} for a {SECONDS} second fixture"
    );
    assert!(
        facts.has_video(),
        "a recording has a picture, and a recovery that could not tell would adopt anything"
    );
    assert!(
        facts.picture_size().is_some(),
        "the picture size is what a recovered row shows as its resolution"
    );
    assert_eq!(
        facts.audio_tracks(),
        2,
        "the track count is the one thing that says at a glance whether a recovered file is one \
         of Clipped's own"
    );
}

#[test]
fn a_recording_with_one_sound_track_is_not_reported_as_two() {
    // The count has to come from the file rather than from a constant. A
    // recovery pass that reported every file as two-track would look right on
    // the fixture above and be wrong about every file that wandered into the
    // folder from somewhere else.
    let Some(tools) = require_media_tools() else {
        return;
    };
    let directory = TemporaryDirectory::new("facts-about-one-track");
    let path = recording_with_sound(&tools, &directory, "one-track.mkv", SECONDS, 1);

    let facts = facts_about(&path).expect("a recording this test just wrote is readable");

    assert_eq!(facts.audio_tracks(), 1);
    assert!(facts.has_video());
}

#[test]
fn something_that_is_not_media_is_refused_rather_than_described() {
    // A recovery pass walks a folder somebody else owns. A stray text file, a
    // half-written download or a screenshot must produce an error this can carry
    // past, and never a panic in a loop over a user's own directory.
    let directory = TemporaryDirectory::new("facts-about-not-media");
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, b"this is not a recording").expect("the temporary directory is writable");

    let error = facts_about(&path).expect_err("a text file is not media");

    let described = error.to_string();
    assert!(
        !described.contains("notes.txt") || described.contains('#'),
        "a path in an error is redacted before it can reach a log: {described}"
    );
}

#[test]
fn a_file_that_is_not_there_is_refused_rather_than_described() {
    let directory = TemporaryDirectory::new("facts-about-absent");
    let path = directory.path().join("never-existed.mkv");

    facts_about(&path).expect_err("a file that is not there cannot be described");
}
