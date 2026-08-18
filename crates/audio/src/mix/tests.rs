//! What the mix does with sources that disagree with each other.
//!
//! These are the mechanics: placement, levels, channel layouts, the boundary a
//! block may be emitted up to, and the refusals. What is *audible* in the result
//! is measured separately, in `tests/compatibility_mix.rs`, with a Goertzel
//! filter over the samples — because a mixer that summed nothing and emitted
//! silence of exactly the right length would satisfy every assertion in this
//! file (AGENTS.md section 21).

use core::num::{NonZeroU16, NonZeroU32};

use clipped_logging::AudioSource;

use super::*;
use crate::format::{ChannelMask, SampleFormat};

const SECOND: u64 = 1_000_000_000;
const RATE: u32 = 48_000;

/// A counter reading of the kind a capture really produces: the performance
/// counter counts from boot, not from the recording, so nothing here may depend
/// on the anchor being zero.
const BASE: u64 = 31_107_000 * SECOND;

fn format(channels: u16) -> AudioFormat {
    AudioFormat::new(
        NonZeroU32::new(RATE).expect("48 kHz is not zero"),
        NonZeroU16::new(channels).expect("a format has at least one channel"),
        ChannelMask::from_bits(if channels == 2 { 0x3 } else { 0x4 }),
        SampleFormat::Float32,
    )
}

fn at(offset: u64) -> AudioTimestamp {
    AudioTimestamp::from_nanos(BASE + offset)
}

fn millis(count: u64) -> u64 {
    count * 1_000_000
}

/// A mix anchored where a recording would anchor it.
fn mixer(channels: u16) -> Mixer {
    Mixer::new(format(channels)).anchored_at(at(0))
}

/// Everything the mix will give up, and the timestamp of each block.
fn take_all(mixer: &mut Mixer) -> (Vec<f32>, Vec<u64>) {
    let mut samples = Vec::new();
    let mut timestamps = Vec::new();
    while let Some(block) = mixer.take() {
        timestamps.push(block.timestamp().as_nanos());
        samples.extend_from_slice(block.samples());
    }
    (samples, timestamps)
}

/// Everything the mix is holding, at the end of a recording.
fn drain_all(mixer: &mut Mixer) -> Vec<f32> {
    let mut samples = Vec::new();
    while let Some(block) = mixer.drain() {
        samples.extend_from_slice(block.samples());
    }
    samples
}

/// `frames` interleaved stereo frames of a constant amplitude.
fn steady(amplitude: f32, frames: usize) -> Vec<f32> {
    vec![amplitude; frames * 2]
}

#[test]
fn a_source_is_placed_where_its_timestamp_says_and_never_appended() {
    // The failure this prevents: a microphone opened half a second after the
    // game, whose audio is written from the start of the mix because the mixer
    // concatenated what it was given. Every word would then be half a second
    // early against the picture, on the one track most people will ever hear.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a stereo source fits a stereo mix");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo, Level::UNITY)
        .expect("a stereo source fits a stereo mix");

    // A second of game audio from the start, and half a second of microphone
    // from halfway in.
    mixer
        .contribute(game, at(0), &steady(0.25, RATE as usize))
        .expect("the game's buffer is placed");
    mixer
        .contribute(microphone, at(millis(500)), &steady(0.5, RATE as usize / 2))
        .expect("the microphone's buffer is placed");

    let (mixed, _) = take_all(&mut mixer);

    assert_eq!(mixed.len(), RATE as usize * 2, "a second of stereo frames");
    // Before the microphone said anything: the game alone.
    assert!(
        (mixed[200] - 0.25).abs() < 1e-6,
        "the first half second should be the game on its own, got {}",
        mixed[200]
    );
    // And from the moment it did: both, summed.
    let halfway = RATE as usize; // frame 24_000, sample index 48_000
    assert!(
        (mixed[halfway] - 0.75).abs() < 1e-6,
        "from half a second in the mix should carry both sources, got {}",
        mixed[halfway]
    );
    assert_eq!(mixer.report().late_frames, 0);
    assert_eq!(mixer.report().discarded_frames, 0);
}

#[test]
fn a_mono_source_is_heard_in_every_channel_of_the_mix() {
    // A microphone is usually mono and the mix is usually stereo. Writing it
    // into channel zero alone would put the person speaking hard left in the
    // one track most people hear.
    let mut mixer = mixer(2);
    let microphone = mixer
        .add_source(AudioSource::Microphone, format(1), Level::UNITY)
        .expect("a mono source spreads into a stereo mix");

    mixer
        .contribute(microphone, at(0), &[0.5; 480])
        .expect("the buffer is placed");
    let mixed = drain_all(&mut mixer);

    assert_eq!(mixed.len(), 960, "480 mono frames become 480 stereo frames");
    for (index, sample) in mixed.iter().enumerate() {
        assert!(
            (sample - 0.5).abs() < 1e-6,
            "sample {index} is {sample}, so the source is not centred"
        );
    }
}

#[test]
fn a_multi_channel_source_folded_into_a_mono_mix_is_averaged() {
    let mut mixer = mixer(1);
    let game = mixer
        .add_source(AudioSource::Game, format(2), Level::UNITY)
        .expect("a stereo source folds into a mono mix");

    // Hard left: 1.0 in the left channel and nothing in the right.
    let samples: Vec<f32> = (0..480).flat_map(|_| [0.8, 0.0]).collect();
    mixer
        .contribute(game, at(0), &samples)
        .expect("the buffer is placed");
    let mixed = drain_all(&mut mixer);

    assert_eq!(mixed.len(), 480);
    assert!(
        (mixed[0] - 0.4).abs() < 1e-6,
        "a hard-left source should fold to half its amplitude, got {}",
        mixed[0]
    );
}

#[test]
fn a_source_that_cannot_be_placed_is_refused_when_it_is_added() {
    // Refused before the recording rather than dropped during it. A caller that
    // is told can record the source on its own track and say the mix does not
    // have it; a caller that is not told ships a mix that is missing a source
    // and nothing anywhere says so (AGENTS.md section 27).
    let mut mixer = mixer(2);

    let surround = AudioFormat::new(
        NonZeroU32::new(RATE).expect("48 kHz is not zero"),
        NonZeroU16::new(6).expect("5.1 is not zero channels"),
        ChannelMask::from_bits(0x3f),
        SampleFormat::Float32,
    );
    assert_eq!(
        mixer.add_source(AudioSource::Game, surround, Level::UNITY),
        Err(MixError::UnmixableLayout { source: 6, mix: 2 })
    );

    // And the message says what to do about it rather than naming a constant.
    let message = MixError::UnmixableLayout { source: 6, mix: 2 }.to_string();
    assert!(
        message.contains("own track"),
        "a refusal should say the source is still recorded: {message}"
    );
}

/// A stereo format at `rate`, for the sources a mix has to take at rates that
/// are not its own.
fn stereo_at(rate: u32) -> AudioFormat {
    AudioFormat::new(
        NonZeroU32::new(rate).expect("a sample rate is not zero"),
        NonZeroU16::new(2).expect("stereo is not zero channels"),
        ChannelMask::from_bits(0x3),
        SampleFormat::Float32,
    )
}

#[test]
fn a_source_at_another_rate_is_taken_into_the_mix_rather_than_refused() {
    // A 44.1 kHz headset microphone beside a 48 kHz render endpoint is ordinary
    // hardware. Refusing it left the microphone out of the one track a player
    // that takes a track arbitrarily takes — the exact failure the compatibility
    // mix exists to prevent, arriving by another route.
    let mut mixer = mixer(2);
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo_at(44_100), Level::UNITY)
        .expect("a source at another rate belongs in the mix");

    // A tenth of a second of 44.1 kHz audio: 4410 frames in, and it has to
    // occupy a tenth of a second of the *mix* — 4800 frames — not 4410 of them,
    // or every source at another rate would slide against the rest of the
    // recording for as long as the recording lasted.
    let samples = vec![0.5_f32; 4_410 * 2];
    mixer
        .contribute(microphone, at(0), &samples)
        .expect("the mix takes samples at the source's own rate");

    let (mixed, _) = take_all(&mut mixer);
    let frames = mixed.len() / 2;
    // Short of 4800 by the filter's own width — the last few dozen frames need
    // input this packet has not been followed by yet, and arrive with the next
    // one. What matters is that it is 4800 rather than 4410: a mix that took
    // the source's frame count for its own would run 8% fast against every
    // other source in the recording, which over an hour is five minutes.
    assert!(
        frames.abs_diff(4_800) <= 48,
        "a tenth of a second at 44.1 kHz should occupy a tenth of a second of a 48 kHz mix, \
         and occupied {frames} frames",
    );

    // And it is the source's audio in there, not silence of the right length.
    // Measured away from the two ends: a block of constant that starts and
    // stops abruptly is a step at each end, and a windowed sinc rings at a step
    // — that overshoot is the filter behaving correctly, not the level.
    let middle = &mixed[2_000..mixed.len() - 2_000];
    for sample in middle {
        assert!(
            (f64::from(*sample) - 0.5).abs() < 0.01,
            "the converted source should arrive at its own amplitude, and one sample was \
             {sample}",
        );
    }
}

#[test]
fn a_source_at_another_rate_stays_where_the_source_said_it_was() {
    // The reason the conversion is not simply "produce more frames": a
    // converted source has to land at the moment its capture stamped it, and
    // keep landing there packet after packet. A filter that reported its own
    // delay as part of the audio would put the microphone a third of a
    // millisecond late for ever; one that placed by output frames rather than
    // by the source's own span would slide by 8% a second.
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, format(2), Level::UNITY)
        .expect("the mix takes a source at its own rate");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo_at(44_100), Level::UNITY)
        .expect("a source at another rate belongs in the mix");

    // A second of each, in 10 ms packets, both silent except that the
    // microphone is a step from zero to 0.5 at exactly half a second.
    for packet in 0..100_u64 {
        mixer
            .contribute(game, at(millis(packet * 10)), &vec![0.0_f32; 480 * 2])
            .expect("the game contributes");
        let level = if packet >= 50 { 0.5 } else { 0.0 };
        mixer
            .contribute(microphone, at(millis(packet * 10)), &vec![level; 441 * 2])
            .expect("the microphone contributes");
    }

    let (mixed, _) = take_all(&mut mixer);
    let first_loud = mixed
        .chunks_exact(2)
        .position(|frame| frame[0].abs() > 0.25)
        .expect("the step is in the mix");

    // 24,000 frames is half a second of the mix, and it lands there to within a
    // handful of frames — a tenth of a millisecond. The tolerance is tight on
    // purpose: leaving the conversion'''s own delay in would put the step 35
    // frames late, and a tolerance loose enough to allow that would be a test
    // that asserted nothing about the one thing this is here to check.
    assert!(
        first_loud.abs_diff(24_000) < 8,
        "a step half a second into a 44.1 kHz source should be half a second into the mix, \
         and arrived at frame {first_loud}",
    );
}

#[test]
fn the_mix_waits_for_the_slowest_source_but_not_for_ever() {
    // Both halves of the same rule. A frame nobody has been past yet may still
    // gain audio, so the mix cannot emit it — but a source that has stopped
    // altogether must not stop the mix, or a microphone Windows muted takes the
    // whole compatibility track with it.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo, Level::UNITY)
        .expect("a source is added");

    // The game runs for 200 ms; the microphone has covered only the first 50 ms.
    mixer
        .contribute(game, at(0), &steady(0.25, RATE as usize / 5))
        .expect("placed");
    mixer
        .contribute(microphone, at(0), &steady(0.25, RATE as usize / 20))
        .expect("placed");

    let (mixed, _) = take_all(&mut mixer);
    assert_eq!(
        mixed.len() / 2,
        RATE as usize / 20,
        "the mix may only run as far as the source that has covered least"
    );

    // Now the game runs on for another two seconds and the microphone says
    // nothing at all. The mix stops waiting once the microphone is more than
    // MAX_SOURCE_LAG behind, and carries on with the game alone.
    mixer
        .contribute(game, at(millis(200)), &steady(0.25, 2 * RATE as usize))
        .expect("placed");
    let (more, _) = take_all(&mut mixer);

    let reached = (mixed.len() + more.len()) as u64 / 2;
    assert_eq!(
        reached,
        format(2).nanos_to_frames(2 * SECOND + millis(200) - MAX_SOURCE_LAG),
        "the mix should have run to the furthest source less one lag allowance"
    );
    assert!(
        more.iter().all(|sample| (sample - 0.25).abs() < 1e-6),
        "the game's audio should be in the mix at full level, not divided by the number of \
         sources that were declared"
    );
}

#[test]
fn a_source_that_falls_behind_has_its_late_audio_counted_rather_than_misplaced() {
    // The other side of the rule above. Once the mix has passed a moment it
    // cannot go back, so a buffer describing that moment has to be dropped —
    // and *said* to have been dropped, because the alternative is to place it
    // wherever the mix happens to be, which puts one source's audio under
    // another's for the rest of the recording.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo, Level::UNITY)
        .expect("a source is added");

    mixer
        .contribute(game, at(0), &steady(0.25, 2 * RATE as usize))
        .expect("placed");
    let (first, _) = take_all(&mut mixer);
    let emitted_frames = first.len() as u64 / 2;
    assert!(
        emitted_frames > 0,
        "the game's audio should have been emitted"
    );

    // A hundred milliseconds of microphone, from the very start of the
    // recording, arriving after the mix is a second and a half past it.
    let late = RATE as u64 / 10;
    mixer
        .contribute(microphone, at(0), &steady(0.5, late as usize))
        .expect("a late buffer is not an error");

    assert_eq!(mixer.report().late_frames, late);

    // The recording carries on, and what comes out next is the game at the
    // amplitude it was captured at — not the game with a hundred milliseconds of
    // somebody's voice from two seconds ago written over it.
    mixer
        .contribute(game, at(2 * SECOND), &steady(0.25, RATE as usize / 2))
        .expect("placed");
    let (after, _) = take_all(&mut mixer);
    assert!(
        !after.is_empty(),
        "this assertion is vacuous unless the mix produced something"
    );
    assert!(
        after.iter().all(|sample| (sample - 0.25).abs() < 1e-6),
        "the late microphone audio was placed somewhere it does not belong"
    );
}

#[test]
fn a_source_whose_position_jumps_does_not_drag_the_mix_with_it() {
    // A capture reporting a position ten seconds ahead of everything else is a
    // fault, and believing it would do two things: buffer ten seconds of
    // silence, and move the mix's boundary far enough forward that every other
    // source's audio between here and there would arrive late and be dropped.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    mixer
        .contribute(game, at(0), &steady(0.25, 480))
        .expect("placed");
    mixer
        .contribute(game, at(10 * SECOND), &steady(0.25, 480))
        .expect("a jump is reported, not refused");

    assert_eq!(mixer.report().discarded_frames, 480);
    let (mixed, _) = take_all(&mut mixer);
    assert_eq!(
        mixed.len() / 2,
        480,
        "the mix should still be where the believable audio left it"
    );
}

#[test]
fn changing_a_level_changes_the_mix_and_nothing_else() {
    // The third acceptance criterion of issue #29. A level is a property of the
    // mix, so moving one has to be audible there and invisible everywhere else
    // — including in the caller's own buffer, which is still going to its own
    // track.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    let captured = steady(0.4, 480);
    let untouched = captured.clone();

    mixer.contribute(game, at(0), &captured).expect("placed");
    mixer
        .set_level(game, Level::linear(0.5).expect("a real level"))
        .expect("the source belongs to this mix");
    mixer
        .contribute(game, at(millis(10)), &captured)
        .expect("placed");

    let mixed = drain_all(&mut mixer);

    assert_eq!(
        captured, untouched,
        "the mix altered the buffer it was handed, which is the source's own track"
    );
    assert!(
        (mixed[0] - 0.4).abs() < 1e-6,
        "before the change the mix carries the captured amplitude, got {}",
        mixed[0]
    );
    assert!(
        (mixed[960] - 0.2).abs() < 1e-6,
        "after the change the mix carries half of it, got {}",
        mixed[960]
    );
    assert_eq!(
        mixer.level(game),
        Ok(Level::linear(0.5).expect("a real level"))
    );
}

#[test]
fn a_muted_source_contributes_nothing_and_still_lets_the_mix_move() {
    // Muting a source in the mix must not stop the mix: the frames the muted
    // source covers are frames it has had its chance at, so the sources that
    // are not muted can be emitted over them.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo, Level::SILENT)
        .expect("a source is added");

    mixer
        .contribute(game, at(0), &steady(0.25, 480))
        .expect("placed");
    mixer
        .contribute(microphone, at(0), &steady(0.9, 480))
        .expect("placed");

    let (mixed, _) = take_all(&mut mixer);
    assert_eq!(mixed.len() / 2, 480, "the mix advanced over both sources");
    assert!(
        mixed.iter().all(|sample| (sample - 0.25).abs() < 1e-6),
        "a muted source reached the mix"
    );
}

#[test]
fn a_silent_stretch_is_the_length_it_lasted_and_costs_no_samples() {
    // `contribute_silence` is what a `SampleOrigin::SynthesisedSilence` buffer
    // becomes. It has to advance the source exactly as far as the same number of
    // zeroed frames would, or a quiet endpoint would hold the mix up.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo, Level::UNITY)
        .expect("a source is added");

    // Half a second in which the game plays and the microphone is silent.
    mixer
        .contribute(game, at(0), &steady(0.25, RATE as usize / 2))
        .expect("placed");
    mixer
        .contribute_silence(microphone, at(0), u64::from(RATE) / 2)
        .expect("silence is placed like anything else");

    let (mixed, _) = take_all(&mut mixer);
    assert_eq!(
        mixed.len() / 2,
        RATE as usize / 2,
        "silence from one source must not shorten the mix"
    );
    assert!(mixed.iter().all(|sample| (sample - 0.25).abs() < 1e-6));
}

#[test]
fn a_silent_stretch_of_a_source_at_another_rate_lasts_as_long_as_it_really_did() {
    // The counterpart of the test above, and the one place a rate conversion
    // has to reason about frames it never sees: `contribute_silence` is given a
    // count in the *source's* frames, and how far the mix may be emitted to
    // depends on turning it into the time it really covered. Counting 22,050
    // frames of a 44.1 kHz source as 22,050 frames of a 48 kHz mix makes every
    // quiet stretch 8% short, and the mix stops just before the end of each of
    // them for as long as the recording lasts.
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, format(2), Level::UNITY)
        .expect("a source is added");
    let microphone = mixer
        .add_source(AudioSource::Microphone, stereo_at(44_100), Level::UNITY)
        .expect("a source at another rate belongs in the mix");

    // Half a second in which the game plays and the 44.1 kHz microphone is
    // silent. 22,050 of its frames are half a second, whatever the mix's rate.
    mixer
        .contribute(game, at(0), &steady(0.25, RATE as usize / 2))
        .expect("placed");
    mixer
        .contribute_silence(microphone, at(0), 22_050)
        .expect("silence is placed like anything else");

    let (mixed, _) = take_all(&mut mixer);
    assert_eq!(
        mixed.len() / 2,
        RATE as usize / 2,
        "a silent 44.1 kHz source must not hold a 48 kHz mix short of where it reached"
    );
}

#[test]
fn blocks_are_contiguous_and_no_longer_than_one_instalment() {
    // What a muxer relies on, and what keeps the mixer's memory bounded: each
    // block starts exactly where the last one ended, and a caller that stopped
    // collecting gets its backlog in instalments rather than in one allocation.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    mixer
        .contribute(game, at(0), &steady(0.25, RATE as usize / 2))
        .expect("placed");

    let mut expected = BASE;
    let mut frames = 0u64;
    let mut blocks = 0;
    while let Some(block) = mixer.take() {
        assert_eq!(
            block.timestamp().as_nanos(),
            expected,
            "a block started somewhere other than where the last one ended"
        );
        // Written as the number of frames 100 ms is rather than as `MAX_BLOCK`:
        // an assertion phrased in terms of the constant it is checking passes
        // however that constant is changed.
        assert!(
            block.frames() <= 4_800,
            "a block of {} frames is longer than the 100 ms instalment",
            block.frames()
        );
        frames += block.frames() as u64;
        blocks += 1;
        expected = BASE + stereo.frames_to_nanos(frames);
    }

    assert_eq!(frames, u64::from(RATE) / 2);
    assert!(
        blocks >= 5,
        "half a second must arrive in instalments rather than in one allocation, and came in \
         {blocks} block(s)"
    );
    assert_eq!(mixer.report().frames, frames);
}

#[test]
fn samples_that_are_not_a_whole_number_of_frames_are_refused() {
    // The same refusal `clipped-muxer` makes. Mixing them anyway swaps the
    // channels of every frame after the short one, and nothing about the result
    // looks wrong.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    assert_eq!(
        mixer.contribute(game, at(0), &[0.1; 7]),
        Err(MixError::PartialFrame {
            samples: 7,
            channels: 2
        })
    );
    assert_eq!(mixer.report().frames, 0);
    assert!(mixer.take().is_none(), "nothing was accepted");
}

#[test]
fn a_handle_from_another_mix_is_refused() {
    let stereo = format(2);
    let mut one = mixer(2);
    let mut other = mixer(2);
    let stranger = other
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    assert_eq!(
        one.contribute(stranger, at(0), &steady(0.25, 480)),
        Err(MixError::UnknownSource)
    );
    assert_eq!(one.level(stranger), Err(MixError::UnknownSource));
    assert_eq!(
        one.set_level(stranger, Level::SILENT),
        Err(MixError::UnknownSource)
    );
}

#[test]
fn printing_a_block_describes_it_and_never_prints_what_it_contains() {
    // The mix contains the microphone, so this is the same guarantee
    // `CapturedAudio` makes and for the same reason (AGENTS.md section 13): a
    // consumer that writes `tracing::debug!(?block)` must not put somebody's
    // room in a log file.
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");

    // Values that could not occur by accident and must not appear.
    let samples = [0.123_456_79_f32, -0.987_654_3, 0.246_913_58, -0.135_792_47];
    mixer.contribute(game, at(0), &samples).expect("placed");
    let block = mixer.drain().expect("a block comes back");

    let printed = format!("{block:?}");
    for sample in samples {
        let value = format!("{sample}");
        assert!(
            !printed.contains(&value),
            "a sample ({value}) reached a printed block: {printed}"
        );
    }
    assert!(printed.contains("frames: 2"), "{printed}");
}

#[test]
fn printing_the_mixer_does_not_print_the_audio_it_is_holding() {
    let stereo = format(2);
    let mut mixer = mixer(2);
    let game = mixer
        .add_source(AudioSource::Game, stereo, Level::UNITY)
        .expect("a source is added");
    mixer
        .contribute(game, at(0), &[0.123_456_79, -0.987_654_3])
        .expect("placed");

    let printed = format!("{mixer:?}");
    assert!(!printed.contains("0.12345679"), "{printed}");
    assert!(printed.contains("pending_frames: 1"), "{printed}");
}

#[test]
fn a_mix_with_no_sources_produces_nothing_rather_than_silence() {
    // A recording configured with no audio at all must not get a compatibility
    // track full of manufactured silence (AGENTS.md section 54).
    let mut mixer = mixer(2);
    assert!(mixer.take().is_none());
    assert!(mixer.drain().is_none());
    assert_eq!(mixer.report(), MixReport::default());
}
