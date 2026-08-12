//! Measuring audio whose loudness is known before it is measured.
//!
//! AGENTS.md section 22 asks that generated media be validated rather than
//! assumed valid; the same argument applies in reverse to something that reads
//! media. A waveform that "looks plausible" proves nothing, so every assertion
//! here is against a file this test wrote, whose amplitude at every moment is
//! arithmetic rather than observation.
//!
//! These tests open no device and take no foreground: they write a file and read
//! it back.

mod support;

use core::num::NonZeroUsize;
use core::time::Duration;

use clipped_media_validation::TemporaryDirectory;
use clipped_waveform::{analyse, Peak, TrackWaveform, WaveformError};

use support::{mux_tracks, write_silent_video, write_wav, Tone};

/// The sample rate everything here is written at.
const RATE: u32 = 48_000;

/// How far a measured peak may be from the amplitude that was written.
///
/// Two steps of 127. A sine does not reach its peak exactly at a bucket
/// boundary, and rounding a sample to 16 bits and a peak to 8 costs a fraction
/// of a step each; anything wider than this would be a different signal.
const TOLERANCE: i8 = 2;

fn buckets(count: usize) -> NonZeroUsize {
    NonZeroUsize::new(count).expect("a positive width")
}

/// The peaks of `track` in 10 ms buckets, which is what it was stored at.
fn per_hundredth(track: &TrackWaveform, seconds: f64) -> Vec<Peak> {
    let count = (seconds * 100.0).round() as usize;
    track.peaks(
        Duration::ZERO..Duration::from_secs_f64(seconds),
        buckets(count),
    )
}

fn assert_near(peak: Peak, expected: i8, at: usize) {
    assert!(
        (peak.maximum() - expected).abs() <= TOLERANCE
            && (peak.minimum() + expected).abs() <= TOLERANCE,
        "bucket {at} is {peak:?}, which is not a tone at ±{expected}"
    );
}

#[test]
fn a_waveform_follows_the_amplitude_that_was_written() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("envelope.wav");
    // Full scale, then silence, then a quarter — half a second each.
    write_wav(
        &path,
        RATE,
        &[vec![
            Tone::at(0.5, 1.0),
            Tone::silence(0.5),
            Tone::at(0.5, 0.25),
        ]],
    );

    let waveform = analyse(&path).expect("the file can be summarised");
    assert_eq!(waveform.tracks().len(), 1);

    let track = &waveform.tracks()[0];
    assert_eq!(track.descriptor().sample_rate(), RATE);
    assert_eq!(track.descriptor().channels(), 1);
    let drift = track.duration().as_secs_f64() - 1.5;
    assert!(drift.abs() < 0.02, "duration is {:?}", track.duration());

    let peaks = per_hundredth(track, 1.5);
    assert_eq!(peaks.len(), 150);

    // The buckets either side of a boundary hold both sides of it, so they are
    // excluded: everything else must be exactly what was written.
    for (index, peak) in peaks.iter().enumerate().take(49) {
        assert_near(*peak, 127, index);
    }
    for (index, peak) in peaks.iter().enumerate().take(99).skip(51) {
        assert_eq!(
            *peak,
            Peak::SILENT,
            "bucket {index} is {peak:?}, but silence was written there"
        );
    }
    for (index, peak) in peaks.iter().enumerate().take(149).skip(101) {
        // A quarter of full scale is 31.75 steps, and quantising rounds
        // outwards, so 32.
        assert_near(*peak, 32, index);
    }
}

#[test]
fn zooming_in_and_out_describes_the_same_audio() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("spike.wav");
    // One second of quiet with a single loud tenth in it, placed so that it
    // fills exactly one bucket of a ten-bucket overview. Whatever resolution it
    // is read at, the loud part has to be in the right place.
    write_wav(
        &path,
        RATE,
        &[vec![
            Tone::at(0.4, 0.1),
            Tone::at(0.1, 1.0),
            Tone::at(0.5, 0.1),
        ]],
    );

    let waveform = analyse(&path).expect("the file can be summarised");
    let track = &waveform.tracks()[0];

    // Ten buckets across the second: the fifth of them holds the spike. Reading
    // this from level 0 would mean reducing a hundred buckets by hand; the
    // pyramid is what makes it a read of ten.
    let overview = track.peaks(Duration::ZERO..Duration::from_secs(1), buckets(10));
    assert_eq!(overview.len(), 10);
    assert_near(overview[4], 127, 4);
    for (index, peak) in overview.iter().enumerate() {
        if index != 4 {
            assert!(
                peak.maximum() < 20,
                "bucket {index} is {peak:?}, but only the fifth is loud"
            );
        }
    }

    // And a hundred buckets across the same second put it in 40..50.
    let zoomed = per_hundredth(track, 1.0);
    assert_near(zoomed[45], 127, 45);
    assert!(zoomed[20].maximum() < 20, "{:?}", zoomed[20]);
    assert!(zoomed[80].maximum() < 20, "{:?}", zoomed[80]);
}

#[test]
fn an_overview_of_a_long_recording_still_finds_a_short_sound() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("long.wav");
    // Eight seconds is 800 base buckets, which is more than one level of the
    // pyramid: an eight-bucket overview of it is read from a level whose buckets
    // are 80 ms wide, four times coarser than the base. A 50 ms sound has to
    // survive that reduction — a coarse level that dropped half of what it
    // merged would show this recording as quiet throughout.
    write_wav(
        &path,
        RATE,
        &[vec![
            Tone::at(4.0, 0.05),
            Tone::at(0.05, 1.0),
            Tone::at(3.95, 0.05),
        ]],
    );

    let waveform = analyse(&path).expect("the file can be summarised");
    let track = &waveform.tracks()[0];
    assert!(track.duration() > Duration::from_secs(7));

    let overview = track.peaks(Duration::ZERO..Duration::from_secs(8), buckets(8));
    assert_eq!(overview.len(), 8);
    assert_near(overview[4], 127, 4);
    for (index, peak) in overview.iter().enumerate() {
        if index != 4 {
            assert!(
                peak.maximum() < 20,
                "bucket {index} is {peak:?}, but only the fifth is loud"
            );
        }
    }
}

#[test]
fn a_sound_in_one_channel_only_is_still_in_the_waveform() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("panned.wav");
    // Left silent throughout; right loud for the middle fifth. Averaging the
    // channels would halve it; taking only the first would lose it entirely.
    write_wav(
        &path,
        RATE,
        &[
            vec![Tone::silence(0.5)],
            vec![Tone::silence(0.2), Tone::at(0.1, 1.0), Tone::silence(0.2)],
        ],
    );

    let waveform = analyse(&path).expect("the file can be summarised");
    let track = &waveform.tracks()[0];
    assert_eq!(track.descriptor().channels(), 2);

    let peaks = per_hundredth(track, 0.5);
    assert_near(peaks[25], 127, 25);
    assert_eq!(peaks[5], Peak::SILENT);
    assert_eq!(peaks[45], Peak::SILENT);
}

#[test]
fn silence_is_reported_as_silence_rather_than_as_noise() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("quiet.wav");
    write_wav(&path, RATE, &[vec![Tone::silence(0.3)]]);

    let waveform = analyse(&path).expect("the file can be summarised");
    let peaks = per_hundredth(&waveform.tracks()[0], 0.3);
    assert!(
        peaks.iter().all(|peak| *peak == Peak::SILENT),
        "a silent file produced {:?}",
        peaks.iter().find(|peak| **peak != Peak::SILENT)
    );
}

#[test]
fn an_unusual_sample_rate_is_summarised_at_the_same_resolution() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("awkward.wav");
    // 22,050 Hz is not a whole number of samples per 10 ms bucket, which is the
    // case the accumulator rounds.
    write_wav(
        &path,
        22_050,
        &[vec![Tone::at(0.5, 0.5), Tone::silence(0.5)]],
    );

    let waveform = analyse(&path).expect("the file can be summarised");
    let track = &waveform.tracks()[0];
    assert_eq!(track.descriptor().sample_rate(), 22_050);
    assert_eq!(track.base_bucket(), Duration::from_millis(10));

    let peaks = per_hundredth(track, 1.0);
    assert_near(peaks[10], 64, 10);
    assert_eq!(peaks[70], Peak::SILENT);
    let drift = track.duration().as_secs_f64() - 1.0;
    assert!(drift.abs() < 0.02, "duration is {:?}", track.duration());
}

#[test]
fn a_file_that_is_not_media_is_reported_rather_than_summarised() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("notes.txt");
    std::fs::write(&path, b"this is not a recording").expect("the file can be written");

    let error = analyse(&path).expect_err("nothing can be summarised from this");
    assert!(
        matches!(error, WaveformError::Undecodable { .. }),
        "{error:?}"
    );
    // And the message names the file and what was being attempted.
    let message = error.to_string();
    assert!(message.contains("notes.txt"), "{message}");
}

#[test]
fn a_recording_that_is_not_there_is_reported_as_the_missing_file_it_is() {
    let directory = TemporaryDirectory::new("waveform-known");
    let error = analyse(directory.file("absent.mkv")).expect_err("there is nothing to summarise");
    assert!(
        matches!(error, WaveformError::Unreadable { .. }),
        "{error:?}"
    );
}

#[test]
fn every_audio_track_of_a_container_keeps_its_own_waveform() {
    let directory = TemporaryDirectory::new("waveform-known");
    let game = directory.file("game.wav");
    let microphone = directory.file("microphone.wav");
    // Two tracks whose loud parts do not overlap. If the analyser mixed them,
    // or read one twice, both waveforms would be loud in both halves.
    write_wav(&game, RATE, &[vec![Tone::at(0.3, 1.0), Tone::silence(0.3)]]);
    write_wav(
        &microphone,
        RATE,
        &[vec![Tone::silence(0.3), Tone::at(0.3, 0.5)]],
    );

    let container = directory.file("session.mkv");
    if !mux_tracks(&[(&game, "Game"), (&microphone, "Microphone")], &container) {
        return;
    }

    let waveform = analyse(&container).expect("the container can be summarised");
    assert_eq!(waveform.tracks().len(), 2, "one waveform per audio track");

    let first = &waveform.tracks()[0];
    let second = &waveform.tracks()[1];
    assert_eq!(first.descriptor().name(), Some("Game"));
    assert_eq!(second.descriptor().name(), Some("Microphone"));
    assert_ne!(
        first.descriptor().stream_index(),
        second.descriptor().stream_index()
    );

    let game_peaks = per_hundredth(first, 0.6);
    let microphone_peaks = per_hundredth(second, 0.6);
    assert_near(game_peaks[10], 127, 10);
    assert_eq!(game_peaks[45], Peak::SILENT);
    assert_eq!(microphone_peaks[10], Peak::SILENT);
    assert_near(microphone_peaks[45], 64, 45);
}

#[test]
fn a_recording_with_no_audio_track_is_summarised_as_having_none() {
    let directory = TemporaryDirectory::new("waveform-known");
    let path = directory.file("video-only.mkv");
    if !write_silent_video(&path, 1) {
        return;
    }

    // What every recording Clipped writes looks like until issue #180 adds
    // audio: this must be an answer, not an error.
    let waveform = analyse(&path).expect("a video-only recording is not a failure");
    assert!(waveform.is_silent());
    assert_eq!(waveform.duration(), Duration::ZERO);
    assert!(waveform.tracks().is_empty());
}
