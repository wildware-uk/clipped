//! What is actually *audible* in the compatibility mix, measured rather than
//! asserted.
//!
//! # Why this file exists separately from the unit tests
//!
//! `src/mix/tests.rs` asserts the mechanics: where a buffer is placed, what a
//! level multiplies, which layouts are refused, how far the mix may run before
//! the slowest source has caught up. A mixer that did all of that and summed
//! *nothing* — emitting silence of exactly the right length, at exactly the
//! right timestamps — would pass every one of them. AGENTS.md section 21 asks
//! for the opposite claim to be tested too, and the only way to make it is to
//! listen.
//!
//! So this file measures energy at a frequency, with the Goertzel filter in
//! `clipped-media-validation` — the same filter, through the same type, that
//! `crates/muxer/tests/multi_track_audio.rs` asserts a finished recording with.
//! The tones are AGENTS.md section 26's own: 440 Hz for the game, 880 Hz for
//! other system audio, 1320 Hz for the microphone.
//!
//! # The two halves of the claim
//!
//! Issue #29's acceptance criterion is a pair, and both halves are here:
//!
//! - **the mix contains every source** — all three tones are in track 1, which
//!   is what makes it the track a naive player should take (SPEC.md section 13);
//! - **every source's own buffer still contains only its own tone**, unchanged
//!   by having been mixed. The mixer takes a shared borrow, so the compiler
//!   already forbids it altering anything; what is measured here is that the
//!   samples which go on to the isolated tracks carry no trace of the mix's
//!   levels, its limiter, or anybody else's audio.
//!
//! # It makes no sound and needs no device
//!
//! Every tone in this file is synthesised into a `Vec<f32>` and read straight
//! back out of the mixer. Nothing here opens an endpoint, renders anything, or
//! runs `ffprobe`, so it runs on a machine with no sound card and makes no noise
//! on a machine with one (AGENTS.md sections 25 and 26). The end-to-end claim —
//! that a *recording* plays correctly in a naive player — needs a session that
//! writes the mix to a file, and belongs with the code that does.

use clipped_audio::{
    AudioFormat, AudioTimestamp, ChannelMask, Level, MixSourceId, Mixer, SampleFormat,
};
use clipped_logging::AudioSource;
use clipped_media_validation::AudioContent;

const RATE: u32 = 48_000;

/// The frequency each source produces (AGENTS.md section 26).
const GAME: f64 = 440.0;
const OTHER_SYSTEM_AUDIO: f64 = 880.0;
const MICROPHONE: f64 = 1320.0;

/// The frequency the clipping test uses, and its third harmonic.
///
/// 997 Hz rather than one of the three above for the reason
/// `tests/system_audio.rs` picks it: it is the frequency digital audio has used
/// for distortion measurements for decades. Here the reason is arithmetic
/// instead — three times 440 Hz is 1320 Hz, so measuring the distortion of a
/// 440 Hz tone would be measuring it at the microphone's frequency.
const OVERDRIVEN: f64 = 997.0;
const THIRD_HARMONIC: f64 = 3.0 * OVERDRIVEN;

/// Long enough for a 250 ms analysis window a quarter of the way in to sit well
/// clear of both ends.
const SECONDS: f64 = 2.0;

/// A 10 ms packet at 48 kHz, which is what WASAPI delivers on the machines this
/// was written against.
const PACKET_FRAMES: usize = 480;

/// A counter reading of the kind a capture really produces.
const BASE: u64 = 31_107_000_000_000_000;

/// Nanoseconds one packet lasts.
const PACKET_NANOS: u64 = 10_000_000;

fn format(channels: u16) -> AudioFormat {
    AudioFormat::new(
        core::num::NonZeroU32::new(RATE).expect("48 kHz is not zero"),
        core::num::NonZeroU16::new(channels).expect("a format has at least one channel"),
        ChannelMask::from_bits(if channels == 2 { 0x3 } else { 0x4 }),
        SampleFormat::Float32,
    )
}

/// `frames` interleaved frames of a sine at `frequency`, the same in every
/// channel.
fn tone(frequency: f64, amplitude: f32, frames: usize, channels: usize) -> Vec<f32> {
    tone_at(RATE, frequency, amplitude, frames, channels)
}

/// The same, for a source whose own sample rate is not the mix's.
fn tone_at(rate: u32, frequency: f64, amplitude: f32, frames: usize, channels: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let phase = 2.0 * std::f64::consts::PI * frequency * frame as f64 / f64::from(rate);
            let sample = amplitude * phase.sin() as f32;
            std::iter::repeat_n(sample, channels)
        })
        .collect()
}

/// One source, its audio, when it starts, and how often it is read.
struct Feed {
    id: MixSourceId,
    samples: Vec<f32>,
    channels: usize,
    /// Frames in one of this source's 10 ms packets, which is its own sample
    /// rate divided by a hundred rather than the mix's: a 44.1 kHz endpoint
    /// hands over 441 frames where a 48 kHz one hands over 480, and both cover
    /// the same ten milliseconds.
    packet_frames: usize,
    /// The packet of the recording this source's first packet is.
    from_packet: usize,
    /// How many recording packets pass between reads of this source.
    ///
    /// One for a source read as fast as the recording produces packets. More
    /// for a source whose thread is scheduled less often, which is what a
    /// second capture on a second thread really looks like — its packets still
    /// carry the positions its own device gave them, they just all arrive at
    /// once. A mixer that appended what it was handed would pile a whole burst
    /// on top of itself.
    every: usize,
}

impl Feed {
    fn new(id: MixSourceId, samples: Vec<f32>, channels: usize) -> Self {
        Self {
            id,
            samples,
            channels,
            packet_frames: PACKET_FRAMES,
            from_packet: 0,
            every: 1,
        }
    }

    /// A source whose own sample rate is not the mix's, so its packets are a
    /// different number of frames for the same ten milliseconds.
    fn at_rate(mut self, rate: u32) -> Self {
        self.packet_frames = rate as usize / 100;
        self
    }

    fn starting_at_second(mut self, second: f64) -> Self {
        self.from_packet = (second * f64::from(RATE)) as usize / PACKET_FRAMES;
        self
    }

    fn read_every(mut self, packets: usize) -> Self {
        self.every = packets;
        self
    }

    /// The recording packets this source hands over when the recording reaches
    /// `packet`.
    fn due(&self, packet: usize) -> core::ops::Range<usize> {
        if (packet + 1) % self.every != 0 {
            return 0..0;
        }
        (packet + 1 - self.every)..(packet + 1)
    }

    /// This source's samples for recording packet `packet`, if it has any.
    fn packet(&self, packet: usize) -> Option<&[f32]> {
        let index = packet.checked_sub(self.from_packet)?;
        let start = index * self.packet_frames * self.channels;
        let end = start + self.packet_frames * self.channels;
        self.samples.get(start..end)
    }
}

/// Feeds every source into the mix in 10 ms packets and returns what came out.
///
/// Deliberately packetised and interleaved with `take`, rather than one
/// contribution per source: that is how a recording arrives, and a mixer that
/// only worked when handed a whole track at once would pass a simpler test and
/// fail on the first real session.
fn run(mixer: &mut Mixer, feeds: &[Feed]) -> Vec<f32> {
    let packets = (SECONDS * f64::from(RATE)) as usize / PACKET_FRAMES;
    let mut mixed = Vec::new();

    for packet in 0..packets {
        for feed in feeds {
            for due in feed.due(packet) {
                let Some(samples) = feed.packet(due) else {
                    continue;
                };
                let at = AudioTimestamp::from_nanos(BASE + due as u64 * PACKET_NANOS);
                mixer
                    .contribute(feed.id, at, samples)
                    .expect("a packet from a registered source is placed");
            }
        }
        while let Some(block) = mixer.take() {
            mixed.extend_from_slice(block.samples());
        }
    }
    while let Some(block) = mixer.drain() {
        mixed.extend_from_slice(block.samples());
    }

    mixed
}

/// Interleaved samples as one channel, which is what "what is on this track"
/// means: a tone panned to one side is still the track's tone.
fn listen(samples: &[f32], channels: usize) -> AudioContent {
    let mono = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    AudioContent::from_samples(mono, RATE)
}

/// The loudest sample in a buffer.
fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

/// The three sources a recording has, as tones.
fn three_sources(mixer: &mut Mixer) -> Vec<Feed> {
    let frames = (SECONDS * f64::from(RATE)) as usize;
    let stereo = format(2);
    let mono = format(1);

    vec![
        Feed::new(
            mixer
                .add_source(AudioSource::Game, stereo, Level::UNITY)
                .expect("a stereo source fits a stereo mix"),
            tone(GAME, 0.30, frames, 2),
            2,
        ),
        Feed::new(
            mixer
                .add_source(AudioSource::OtherSystem, stereo, Level::UNITY)
                .expect("a stereo source fits a stereo mix"),
            tone(OTHER_SYSTEM_AUDIO, 0.20, frames, 2),
            2,
        ),
        // Read a tenth as often as the other two, which is what a second
        // capture on a second thread looks like: its packets carry the
        // positions its device gave them and arrive ten at a time.
        Feed::new(
            mixer
                .add_source(AudioSource::Microphone, mono, Level::UNITY)
                .expect("a mono source spreads into a stereo mix"),
            tone(MICROPHONE, 0.25, frames, 1),
            1,
        )
        .read_every(10),
    ]
}

#[test]
fn every_source_is_audible_in_the_mix_and_none_of_them_is_changed_by_it() {
    // The acceptance criterion of issue #29, both halves of it. Track 1 has to
    // carry everything — that is what makes it safe to hand to a player that
    // takes one track arbitrarily — while the samples that go on to the isolated
    // tracks carry only their own source, unaltered by having been mixed
    // (AGENTS.md section 21).
    let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
    let feeds = three_sources(&mut mixer);
    let pristine: Vec<Vec<f32>> = feeds.iter().map(|feed| feed.samples.clone()).collect();

    let mixed = run(&mut mixer, &feeds);
    assert!(!mixed.is_empty(), "the mix produced nothing at all");

    // Half of the claim: everything is in the mix.
    let mix = listen(&mixed, 2);
    let own = mix.magnitude_at(GAME);
    assert!(own > 0.1, "the game is barely in the mix at all: {own:.4}");
    for tone in [GAME, OTHER_SYSTEM_AUDIO, MICROPHONE] {
        let magnitude = mix.magnitude_at(tone);
        assert!(
            magnitude > own / 2.0,
            "the compatibility mix should carry every source: {tone} Hz measures \
             {magnitude:.4} against {GAME} Hz at {own:.4}"
        );
    }

    // The other half: each source's own audio, untouched, and carrying nothing
    // of anybody else's. Bit-identical first — a mix that altered its inputs
    // would be altering the isolated tracks — and then measured, because
    // "unchanged" and "isolated" are different claims and only one of them is
    // about the mixer's own arithmetic.
    for (feed, before) in feeds.iter().zip(&pristine) {
        assert_eq!(
            &feed.samples, before,
            "the mixer altered a source's own samples"
        );
    }
    for (feed, expected) in feeds.iter().zip([GAME, OTHER_SYSTEM_AUDIO, MICROPHONE]) {
        let track = listen(&feed.samples, feed.channels);
        let (peak, magnitude) = track.dominant_frequency();
        assert!(
            (peak - expected).abs() < 5.0,
            "this source should carry {expected} Hz and the strongest thing on it is \
             {peak:.1} Hz at {magnitude:.4}"
        );

        let own = track.magnitude_at(expected);
        for intruder in [GAME, OTHER_SYSTEM_AUDIO, MICROPHONE] {
            if intruder == expected {
                continue;
            }
            let bleed = track.magnitude_at(intruder);
            assert!(
                bleed * 8.0 < own,
                "{intruder} Hz belongs to another source and must not be audible on this \
                 one, but it measures {bleed:.4} against this source's own {expected} Hz \
                 at {own:.4}"
            );
        }
    }
}

#[test]
fn a_microphone_at_another_rate_is_audible_in_the_mix_and_its_own_track_is_untouched() {
    // The hardware combination this exists for: a 44.1 kHz headset microphone
    // and a 48 kHz render endpoint, which is what a great many machines have.
    // Until issue #30 the mix refused the microphone, and the consequence was
    // not a failed recording but a silent omission — the one track a player
    // that takes a track arbitrarily takes had no voice in it, and the only
    // sign was a log line.
    //
    // Both halves are asserted here, because the second is what makes the first
    // allowed: the microphone is audible in the mix, and the samples that go on
    // to its own track are bit-for-bit the ones the capture produced, at
    // 44.1 kHz, with no conversion anywhere near them (AGENTS.md section 22).
    const MICROPHONE_RATE: u32 = 44_100;

    let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
    let game = Feed::new(
        mixer
            .add_source(AudioSource::Game, format(2), Level::UNITY)
            .expect("a stereo source fits a stereo mix"),
        tone(GAME, 0.30, (SECONDS * f64::from(RATE)) as usize, 2),
        2,
    );
    let microphone = Feed::new(
        mixer
            .add_source(
                AudioSource::Microphone,
                AudioFormat::new(
                    core::num::NonZeroU32::new(MICROPHONE_RATE).expect("44.1 kHz is not zero"),
                    core::num::NonZeroU16::new(1).expect("mono is not zero channels"),
                    ChannelMask::from_bits(0x4),
                    SampleFormat::Float32,
                ),
                Level::UNITY,
            )
            .expect("a source at another rate belongs in the mix"),
        tone_at(
            MICROPHONE_RATE,
            MICROPHONE,
            0.30,
            (SECONDS * f64::from(MICROPHONE_RATE)) as usize,
            1,
        ),
        1,
    )
    .at_rate(MICROPHONE_RATE);

    let pristine = microphone.samples.clone();
    let feeds = [game, microphone];
    let mixed = run(&mut mixer, &feeds);
    assert!(!mixed.is_empty(), "the mix produced nothing at all");

    // In the mix, at its own frequency and at a level comparable with the
    // source that did not need converting. A conversion that had the ratio
    // upside down would put the tone at 1568 Hz or 1112 Hz instead, and the
    // measurement at 1320 Hz would collapse.
    let mix = listen(&mixed, 2);
    let game_tone = mix.magnitude_at(GAME);
    let microphone_tone = mix.magnitude_at(MICROPHONE);
    assert!(
        microphone_tone > game_tone / 2.0,
        "a 44.1 kHz microphone should be as audible in a 48 kHz mix as anything else:          {MICROPHONE} Hz measures {microphone_tone:.4} against {GAME} Hz at {game_tone:.4}"
    );

    // And nothing of it changed on the way. Bit-identical, because the mixer
    // takes a shared borrow and the conversion happens on a copy.
    assert_eq!(
        feeds[1].samples, pristine,
        "the mixer altered the microphone's own samples"
    );
    let track = AudioContent::from_samples(feeds[1].samples.clone(), MICROPHONE_RATE);
    let (peak, magnitude) = track.dominant_frequency();
    assert!(
        (peak - MICROPHONE).abs() < 5.0,
        "the microphone's own track should still be {MICROPHONE} Hz at 44.1 kHz, and the          strongest thing on it is {peak:.1} Hz at {magnitude:.4}"
    );
}

#[test]
fn a_source_that_starts_late_is_mixed_where_it_started() {
    // Placement, measured. A mixer that concatenated what it was given would
    // produce a mix in which the microphone is audible from the first sample,
    // and every structural assertion about lengths and timestamps would still
    // pass.
    let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
    let frames = (SECONDS * f64::from(RATE)) as usize;

    let game = Feed::new(
        mixer
            .add_source(AudioSource::Game, format(2), Level::UNITY)
            .expect("a source is added"),
        tone(GAME, 0.30, frames, 2),
        2,
    );
    let microphone = Feed::new(
        mixer
            .add_source(AudioSource::Microphone, format(1), Level::UNITY)
            .expect("a source is added"),
        tone(MICROPHONE, 0.30, frames / 2, 1),
        1,
    )
    .starting_at_second(1.0)
    .read_every(10);

    let mixed = run(&mut mixer, &[game, microphone]);
    let three_quarters = (0.75 * f64::from(RATE)) as usize * 2;
    assert!(
        mixed.len() > three_quarters,
        "the mix is too short to measure two halves of"
    );

    let before = listen(&mixed[..three_quarters], 2);
    assert!(
        before.magnitude_at(MICROPHONE) * 8.0 < before.magnitude_at(GAME),
        "the microphone starts a second in, but its tone measures {:.4} in the first \
         three quarters of a second of the mix against the game's {:.4}",
        before.magnitude_at(MICROPHONE),
        before.magnitude_at(GAME)
    );

    let after = listen(&mixed[mixed.len() - three_quarters..], 2);
    assert!(
        after.magnitude_at(MICROPHONE) > after.magnitude_at(GAME) / 2.0,
        "the microphone should be in the mix from a second in, and measures {:.4} \
         against the game's {:.4}",
        after.magnitude_at(MICROPHONE),
        after.magnitude_at(GAME)
    );
}

#[test]
fn a_level_change_is_audible_in_the_mix_and_nowhere_else() {
    // The third acceptance criterion. The same three sources mixed twice, with
    // nothing different but the microphone's level: its tone has to be four
    // times quieter in the mix, the other two have to be exactly where they
    // were, and the microphone's own samples have to be identical between the
    // two runs — because they are what its isolated track is made of.
    let measure = |level: Level| {
        let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
        let mut feeds = three_sources(&mut mixer);
        let microphone = feeds[2].id;
        mixer
            .set_level(microphone, level)
            .expect("the source belongs to this mix");
        let mixed = run(&mut mixer, &feeds);
        let own = feeds.pop().expect("three sources").samples;
        (listen(&mixed, 2), own)
    };

    let (loud, own_at_unity) = measure(Level::UNITY);
    // −12 dB, which is a quarter of the amplitude.
    let (quiet, own_when_turned_down) = measure(Level::from_decibels(-12.0).expect("a level"));

    let ratio = loud.magnitude_at(MICROPHONE) / quiet.magnitude_at(MICROPHONE);
    assert!(
        (ratio - 4.0).abs() < 0.4,
        "turning the microphone down 12 dB should make it four times quieter in the mix, \
         and it is {ratio:.2} times"
    );

    // And only in the mix: the other sources are untouched...
    for tone in [GAME, OTHER_SYSTEM_AUDIO] {
        let difference = (loud.magnitude_at(tone) - quiet.magnitude_at(tone)).abs();
        assert!(
            difference < 0.01,
            "{tone} Hz moved by {difference:.4} when the microphone's level changed"
        );
    }
    // ...and so are the samples the microphone's own track is made of.
    assert_eq!(
        own_at_unity, own_when_turned_down,
        "the microphone's level changed the samples going to its own track"
    );
}

#[test]
fn sources_that_sum_past_full_scale_are_held_down_rather_than_clipped() {
    // A mix that distorts when the game is loud and somebody speaks is worse
    // than no mix, so this measures the thing that would go wrong. The same
    // overdriven signal is put through the mixer and through the obvious
    // alternative — sum it and clamp — and the distortion is compared at the
    // third harmonic, which is what a clipped sine produces and a level change
    // does not.
    let frames = (SECONDS * f64::from(RATE)) as usize;
    let captured = tone(OVERDRIVEN, 0.9, frames, 2);

    let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
    let feed = Feed::new(
        mixer
            .add_source(
                AudioSource::Game,
                format(2),
                // Twice the captured amplitude: what several loud sources
                // summing looks like, in one source and with no second
                // frequency to confuse the measurement.
                Level::linear(2.0).expect("a real level"),
            )
            .expect("a source is added"),
        captured.clone(),
        2,
    );

    let mixed = run(&mut mixer, &[feed]);

    assert!(
        peak(&mixed) <= Mixer::CEILING + f32::EPSILON,
        "the mix reached {} against a ceiling of {}",
        peak(&mixed),
        Mixer::CEILING
    );

    let clipped: Vec<f32> = captured
        .iter()
        .map(|sample| (sample * 2.0).clamp(-Mixer::CEILING, Mixer::CEILING))
        .collect();

    let limited = listen(&mixed, 2);
    let clamped = listen(&clipped, 2);

    // The comparison is only worth making if clamping really does distort.
    let clipped_distortion = clamped.magnitude_at(THIRD_HARMONIC);
    assert!(
        clipped_distortion > 0.02,
        "this test compares against a clipped signal, and that one measures only \
         {clipped_distortion:.4} at {THIRD_HARMONIC} Hz"
    );

    let distortion = limited.magnitude_at(THIRD_HARMONIC);
    assert!(
        distortion * 10.0 < clipped_distortion,
        "the mix should distort far less than clipping does: {distortion:.4} at \
         {THIRD_HARMONIC} Hz against clipping's {clipped_distortion:.4}"
    );

    // And it is still the tone that was played, not a quieter something else.
    let (peak_frequency, magnitude) = limited.dominant_frequency();
    assert!(
        (peak_frequency - OVERDRIVEN).abs() < 5.0,
        "the mix's strongest frequency is {peak_frequency:.1} Hz at {magnitude:.4}"
    );
}

#[test]
fn a_source_that_produces_nothing_does_not_take_the_others_with_it() {
    // A microphone Windows had muted, or a capture whose device never came
    // back. The mix cannot emit a frame until every source has had its chance at
    // it, so a source that has stopped would otherwise stop the compatibility
    // track — the one track most people will ever hear — for the rest of the
    // recording.
    let mut mixer = Mixer::new(format(2)).anchored_at(AudioTimestamp::from_nanos(BASE));
    let frames = (SECONDS * f64::from(RATE)) as usize;

    let game = Feed::new(
        mixer
            .add_source(AudioSource::Game, format(2), Level::UNITY)
            .expect("a source is added"),
        tone(GAME, 0.30, frames, 2),
        2,
    );
    let system = Feed::new(
        mixer
            .add_source(AudioSource::OtherSystem, format(2), Level::UNITY)
            .expect("a source is added"),
        tone(OTHER_SYSTEM_AUDIO, 0.20, frames, 2),
        2,
    );
    // Declared, opened, and producing nothing at all for the whole recording.
    let _silent = mixer
        .add_source(AudioSource::Microphone, format(1), Level::UNITY)
        .expect("a source is added");

    let mixed = run(&mut mixer, &[game, system]);

    assert!(
        !mixed.is_empty(),
        "one source producing nothing silenced the whole mix"
    );
    let heard = listen(&mixed, 2);
    for tone in [GAME, OTHER_SYSTEM_AUDIO] {
        assert!(
            heard.magnitude_at(tone) > 0.1,
            "{tone} Hz measures only {:.4} in a mix whose third source was silent",
            heard.magnitude_at(tone)
        );
    }
    assert!(
        heard.magnitude_at(MICROPHONE) * 8.0 < heard.magnitude_at(GAME),
        "a source that produced nothing put {MICROPHONE} Hz in the mix"
    );
}
