//! What the rate conversion does to a signal, measured rather than asserted to
//! exist.
//!
//! A converter that returned the right *number* of frames and filled them with
//! noise would satisfy any test that only counted them, and the whole reason
//! this module is not `crate::resample` is what it does to the samples. So
//! every test here measures the output: the amplitude and frequency of a tone
//! that went through it, the energy that arrived anywhere it should not have,
//! and how flat a constant stayed.

use super::*;

/// The two rates the ordinary case is between: a 44.1 kHz microphone and a
/// 48 kHz render endpoint on the same machine.
const CD: u32 = 44_100;
const DVD: u32 = 48_000;

/// A mono sine of `hertz` at `rate`, `frames` long, starting at phase zero.
fn tone(hertz: f64, rate: u32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|frame| {
            let phase = core::f64::consts::TAU * hertz * frame as f64 / f64::from(rate);
            phase.sin() as f32
        })
        .collect()
}

/// The amplitude of whatever is at `hertz` in `samples`.
///
/// The same question `clipped_media_validation::Tone` answers about a decoded
/// track, asked here by correlating against the frequency directly: that crate
/// is a test dependency of the integration tests and not of this one, and a
/// unit test that needed the media tools installed would stop running on a
/// hosted runner. One bin rather than a spectrum, because every assertion here
/// is about one frequency at a time.
fn amplitude_at(samples: &[f32], hertz: f64, rate: u32) -> f64 {
    let step = core::f64::consts::TAU * hertz / f64::from(rate);
    let (mut real, mut imaginary) = (0.0_f64, 0.0_f64);
    for (index, sample) in samples.iter().enumerate() {
        let phase = step * index as f64;
        real += f64::from(*sample) * phase.cos();
        imaginary -= f64::from(*sample) * phase.sin();
    }
    2.0 * real.hypot(imaginary) / samples.len() as f64
}

/// Everything in `samples` that is *not* the tone at `hertz`, as a fraction of
/// that tone's own amplitude.
///
/// A single-frequency measurement can only find an artefact somebody predicted
/// the frequency of. This finds the rest: it fits the tone that is supposed to
/// be there, subtracts it, and reports what the converter added, wherever in
/// the spectrum it landed.
fn everything_but_the_tone(samples: &[f32], hertz: f64, rate: u32) -> f64 {
    let step = core::f64::consts::TAU * hertz / f64::from(rate);
    let (mut real, mut imaginary) = (0.0_f64, 0.0_f64);
    for (index, sample) in samples.iter().enumerate() {
        let phase = step * index as f64;
        real += f64::from(*sample) * phase.cos();
        imaginary -= f64::from(*sample) * phase.sin();
    }
    let amplitude = 2.0 * real.hypot(imaginary) / samples.len() as f64;
    let offset = imaginary.atan2(real);

    let residual: f64 = samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let fitted = amplitude * (step * index as f64 + offset).cos();
            (f64::from(*sample) - fitted).powi(2)
        })
        .sum::<f64>()
        / samples.len() as f64;

    residual.sqrt() / (amplitude / core::f64::consts::SQRT_2)
}

/// Pushes `input` through in packets of `packet` samples, as a capture would.
fn convert(converter: &mut RateConverter, input: &[f32], packet: usize) -> Vec<f32> {
    let mut output = Vec::new();
    let mut block = Vec::new();
    for chunk in input.chunks(packet) {
        converter.process(chunk, &mut block);
        output.extend_from_slice(&block);
    }
    output
}

#[test]
fn a_tone_keeps_its_frequency_and_its_amplitude_across_a_rate_change() {
    let mut converter = RateConverter::new(CD, DVD, 1);
    // A second of 1 kHz: high enough that the window's transition band is not
    // what is being measured, low enough to be squarely in the passband.
    let input = tone(1_000.0, CD, CD as usize);
    let output = convert(&mut converter, &input, 441);

    // Ten frames either way for the fractional position the conversion carries
    // and the filter's own delay.
    let expected = DVD as usize;
    assert!(
        output.len().abs_diff(expected) < 10,
        "a second in should be about a second out, not {} frames",
        output.len(),
    );

    // The head is the fade-in from the silence the filter starts with, and the
    // tail is the frames it has not been given the future of yet.
    let steady = &output[1_000..output.len() - 1_000];
    let amplitude = amplitude_at(steady, 1_000.0, DVD);
    assert!(
        (amplitude - 1.0).abs() < 0.01,
        "a unit-amplitude tone should come out at unit amplitude, not {amplitude}",
    );
}

#[test]
fn a_tone_is_not_accompanied_by_the_image_a_cruder_interpolator_leaves() {
    // The case the module documentation names: 10 kHz from 44.1 kHz to 48 kHz.
    // Linear interpolation puts a spurious tone at 48000 - (44100 - 10000) =
    // 13.9 kHz about 23 dB below the original. This is the assertion that
    // separates this module from `crate::resample`, and the one that fails if
    // the windowed sinc is ever quietly replaced with something cheaper.
    let mut converter = RateConverter::new(CD, DVD, 1);
    let input = tone(10_000.0, CD, CD as usize);
    let output = convert(&mut converter, &input, 441);
    let steady = &output[2_000..output.len() - 2_000];

    let wanted = amplitude_at(steady, 10_000.0, DVD);
    let image = amplitude_at(steady, 13_900.0, DVD);
    assert!(
        wanted > 0.9,
        "the tone itself should survive the conversion, and measured {wanted}",
    );
    assert!(
        image < wanted / 1_000.0,
        "the image at 13.9 kHz should be 60 dB down, and measured {image} against {wanted}",
    );
}

#[test]
fn content_above_the_output_nyquist_is_removed_rather_than_folded_back() {
    // Down in rate, 48 kHz into a 32 kHz mix — a rate Windows will present, and
    // one whose transition band a filter of this length can actually fit. A
    // 22 kHz tone is under the input's Nyquist frequency (24 kHz) and over the
    // output's (16 kHz), so the output cannot represent it. What a converter
    // that only interpolated would do is not lose it but *move* it: it would
    // arrive at 32000 - 22000 = 10 kHz, a tone that was never played, in the
    // middle of the audible band. Following the cutoff down to the lower of the
    // two rates is what prevents that, and this is the assertion that the
    // cutoff is not simply fixed at 1.0.
    let mut converter = RateConverter::new(DVD, 32_000, 1);
    let input = tone(22_000.0, DVD, DVD as usize);
    let output = convert(&mut converter, &input, 480);
    let steady = &output[2_000..output.len() - 2_000];

    let alias = amplitude_at(steady, 10_000.0, 32_000);
    assert!(
        alias < 0.001,
        "a tone above the output's Nyquist frequency should be filtered out rather than \
         folded back to 10 kHz, which measured {alias}",
    );
}

#[test]
fn the_passband_is_flat_across_everything_a_person_can_hear() {
    // The number this filter is chosen by, and the one worth quoting: how much
    // of the source survives the conversion. `TAPS` decides how narrow the
    // transition band between the passband and the stopband is, so it decides
    // where the response starts falling — and a converter whose response
    // sagged at 10 kHz would be dulling every microphone it touched.
    //
    // `--nocapture` prints the response, which is where the figures in
    // `docs/audio-routing.md` come from.
    let mut converter = RateConverter::new(CD, DVD, 1);
    let mut worst: f64 = 0.0;
    for hertz in [100.0, 1_000.0, 5_000.0, 10_000.0, 15_000.0, 18_000.0] {
        let input = tone(hertz, CD, CD as usize);
        let output = convert(&mut converter, &input, 441);
        let steady = &output[4_000..output.len() - 4_000];
        let amplitude = amplitude_at(steady, hertz, DVD);
        let decibels = 20.0 * amplitude.log10();
        println!("44.1 kHz to 48 kHz at {hertz:>8.0} Hz: {decibels:+.3} dB");
        worst = worst.max(decibels.abs());
        converter.reset();
    }
    assert!(
        worst < 0.1,
        "the passband should be flat to a tenth of a decibel up to 18 kHz, and the worst \
         point was {worst:.3} dB out",
    );
}

#[test]
fn a_converted_tone_arrives_with_nothing_beside_it() {
    // The measurement no single-frequency assertion can make: how much of the
    // output is not the tone that went in, wherever it landed. It is what the
    // table's fractional-position interpolation is for — taking the taps from
    // the nearer of 256 rows rather than interpolating between two of them is a
    // timing jitter, which spreads a tone into spurs all over the spectrum, and
    // none of them is at a frequency anybody would have thought to look at.
    // This measures −103 dB as written and −56 dB with the interpolation taken
    // out, which is the whole reason the interpolation is there.
    let mut converter = RateConverter::new(CD, DVD, 1);
    let input = tone(10_000.0, CD, CD as usize);
    let output = convert(&mut converter, &input, 441);
    let steady = &output[4_000..output.len() - 4_000];

    let residue = everything_but_the_tone(steady, 10_000.0, DVD);
    let decibels = 20.0 * residue.log10();
    println!("everything but the 10 kHz tone: {decibels:.1} dB");
    assert!(
        residue < 0.001,
        "the conversion should add nothing within 60 dB of the tone, and added {decibels:.1} dB",
    );
}

#[test]
fn a_constant_comes_out_constant_at_every_fractional_position() {
    // Both directions, because they fail differently and only one of them fails
    // loudly. Going up in rate the taps sum to within two parts in a million of
    // unity before they are normalised at all, so this measures the
    // interpolation between fractional positions. Going *down* they sum to the
    // reciprocal of the cutoff — 1.5 at these rates — so a converter that
    // skipped the normalisation would hand the mix a source 3.5 dB too loud,
    // and this is where that is caught.
    for (from, to) in [(CD, DVD), (DVD, 32_000)] {
        let mut converter = RateConverter::new(from, to, 1);
        let input = vec![0.5_f32; from as usize];
        let output = convert(&mut converter, &input, (from / 100) as usize);
        let steady = &output[1_000..output.len() - 1_000];

        for sample in steady {
            assert!(
                (f64::from(*sample) - 0.5).abs() < 1e-4,
                "a constant should stay constant from {from} Hz to {to} Hz, and one sample \
                 was {sample}",
            );
        }
    }
}

#[test]
fn channels_are_converted_independently() {
    // Interleaving is the easiest thing in a resampler to get wrong, and the
    // symptom — the left channel's audio arriving in the right — is one nothing
    // else in this crate would catch.
    let mut converter = RateConverter::new(CD, DVD, 2);
    let left = tone(1_000.0, CD, CD as usize);
    let right = tone(5_000.0, CD, CD as usize);
    let input: Vec<f32> = left
        .iter()
        .zip(&right)
        .flat_map(|(left, right)| [*left, *right])
        .collect();
    let output = convert(&mut converter, &input, 882);

    let steady = &output[4_000..output.len() - 4_000];
    let left: Vec<f32> = steady.iter().step_by(2).copied().collect();
    let right: Vec<f32> = steady.iter().skip(1).step_by(2).copied().collect();

    assert!(
        amplitude_at(&left, 1_000.0, DVD) > 0.9,
        "the left channel's own tone"
    );
    assert!(
        amplitude_at(&left, 5_000.0, DVD) < 0.01,
        "the right channel's tone must not be in the left",
    );
    assert!(
        amplitude_at(&right, 5_000.0, DVD) > 0.9,
        "the right channel's own tone"
    );
    assert!(
        amplitude_at(&right, 1_000.0, DVD) < 0.01,
        "the left channel's tone must not be in the right",
    );
}

#[test]
fn the_frame_count_tracks_the_ratio_over_a_long_run_rather_than_its_own_rounding() {
    // Ten minutes of 10 ms packets. Rounding each packet to a whole number of
    // output frames independently would lose about a frame per packet at this
    // ratio, which is nine seconds over the ten minutes; carrying the
    // fractional position is what makes the total right.
    let mut converter = RateConverter::new(CD, DVD, 1);
    let packet = (CD / 100) as usize;
    let packets = 60_000_u64;
    let mut produced = 0_u64;
    let mut block = Vec::new();
    let silence = vec![0.0_f32; packet];
    for _ in 0..packets {
        produced += converter.process(&silence, &mut block) as u64;
    }

    let expected = packets * u64::from(DVD) / 100;
    assert!(
        produced.abs_diff(expected) < 64,
        "{produced} frames out of {expected} expected after ten minutes",
    );
}

#[test]
fn packets_far_shorter_than_the_filter_convert_to_the_same_thing() {
    // A capture is not obliged to hand over a round ten milliseconds, and
    // WASAPI does not: a packet can be a handful of frames, which is far fewer
    // than the filter's own width. Nothing about the answer may depend on how
    // the same second of audio was divided into calls — the same second in
    // three-frame packets and in 10 ms packets has to produce the same number
    // of frames and the same tone, or the conversion is losing or inventing
    // audio at packet boundaries.
    let mut converter = RateConverter::new(CD, DVD, 1);
    let input = tone(1_000.0, CD, CD as usize);
    let dribbled = convert(&mut converter, &input, 3);

    converter.reset();
    let ordinary = convert(&mut converter, &input, 441);

    assert!(
        dribbled.len().abs_diff(ordinary.len()) < 8,
        "a second delivered three frames at a time produced {} frames against {} for the \
         same second delivered in packets",
        dribbled.len(),
        ordinary.len(),
    );
    let amplitude = amplitude_at(&dribbled[2_000..dribbled.len() - 2_000], 1_000.0, DVD);
    assert!(
        (amplitude - 1.0).abs() < 0.01,
        "and it is still the tone, at {amplitude}",
    );
}

#[test]
fn a_reset_starts_the_next_packet_from_silence_rather_than_from_stale_samples() {
    let mut converter = RateConverter::new(CD, DVD, 1);
    let loud = vec![1.0_f32; 4_410];
    let mut block = Vec::new();
    converter.process(&loud, &mut block);
    converter.reset();

    // Nothing of the loud packet may survive the reset into the silence that
    // follows it, or a gap in a capture would be filled with the sound before
    // it smeared across the join.
    let quiet = vec![0.0_f32; 4_410];
    converter.process(&quiet, &mut block);
    for sample in &block {
        assert!(
            sample.abs() < 1e-6,
            "silence after a reset should be silent, and one sample was {sample}",
        );
    }
}
