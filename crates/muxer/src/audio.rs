//! The audio side of a recording: what the tracks are, what order they are in,
//! and how captured samples become packets.
//!
//! Multi-track audio is the product (SPEC.md section 11), and AGENTS.md section
//! 21 states the rule the whole design answers to: **sources the user expects to
//! stay separate are never silently combined**. A recording therefore carries
//! one track per source, each named so that a player and an editor can tell them
//! apart without a sidecar, in an order that does not depend on the order a
//! caller happened to configure them in.
//!
//! # What this module decides
//!
//! - **The vocabulary.** [`AudioSource`] is the closed set of things a track can
//!   be, and it — not the caller — supplies the name written into the container.
//!   Two recordings made on two machines call the microphone track the same
//!   thing, which is what makes a library, an editor and a support log able to
//!   talk about "the Game track" at all.
//! - **The order.** [`AudioSource::ordering_rank`] fixes it: the compatibility
//!   mix first (SPEC.md section 13), then game, other system audio, microphone,
//!   voice chat, and per-application tracks in the order they were configured.
//!   [`AudioSource::SystemAudio`] — everything the machine played, on one track,
//!   which is what a recording that could not separate the game from the rest
//!   produces — takes the game's place, because a recording has the pair or the
//!   undivided track and never both.
//!   [`RecordingLayout`](crate::RecordingLayout) inserts by that rank, so a
//!   caller that declares the microphone before the game still gets the standard
//!   file.
//! - **The storage format.** [`RECORDING_AUDIO_CODEC`]: uncompressed 16-bit PCM.
//!   `docs/muxing.md` records why, and what it costs.
//! - **What a silent source produces.** Nothing. See
//!   [`AudioTrackWriter`](AudioTrackWriter#a-source-that-produces-nothing).
//!
//! # What this module does not decide
//!
//! Which sources a recording has, and what goes into the compatibility mix.
//! Routing is configuration ([issue #33](https://github.com/wildware-uk/clipped/issues/33))
//! and the mix is [issue #29](https://github.com/wildware-uk/clipped/issues/29);
//! both live above the muxer, which is handed a set of tracks and told what to
//! put in each.

use core::fmt;
use core::time::Duration;

use crate::error::MuxError;
use crate::packet::{EncodedPacket, PacketTimestamp};
use crate::track::{AudioCodec, AudioTrack, TrackId};
use crate::writer::MkvWriter;

/// What a recording's audio tracks are stored as.
///
/// Uncompressed 16-bit PCM. The reasoning is in `docs/muxing.md`; in short,
/// nothing in Clipped can encode audio yet, the bitrate is small beside the
/// video it sits next to, and an archival recording that has never been through
/// a lossy encoder is the one an editor should be given. It is a constant rather
/// than a literal in three places so that the decision has one name and changing
/// it is one edit.
pub const RECORDING_AUDIO_CODEC: AudioCodec = AudioCodec::PcmS16Le;

/// How much audio goes into one packet.
///
/// Twenty milliseconds: the length Windows loopback capture delivers at, short
/// enough that audio interleaves with video rather than arriving in lumps, and
/// long enough that a recording is not made of a hundred thousand blocks a
/// minute. A capture buffer longer than this is split; one shorter is written as
/// it is, because holding it back to fill a packet would mean audio waiting in
/// this process for the next buffer — and what waits in memory is what a killed
/// recorder loses (AGENTS.md section 17).
const PACKET: Duration = Duration::from_millis(20);

/// Where a recording's audio track came from.
///
/// The name written into the container comes from here rather than from the
/// caller, so that every recording Clipped makes labels its tracks the same way.
/// [`Application`](Self::Application) is the exception, because a track carrying
/// Discord has to say "Discord" (SPEC.md section 11).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioSource {
    /// Everything the user chose to hear, mixed together (SPEC.md section 13).
    ///
    /// It exists because some players pick one audio track from a multi-track
    /// file arbitrarily, and a recording that sounds wrong when it is opened
    /// casually is a recording people conclude is broken. It is first, and it
    /// carries the default-track flag.
    CompatibilityMix,
    /// Everything the machine played, undivided.
    ///
    /// **Not [`OtherSystemAudio`](Self::OtherSystemAudio) and not
    /// [`Game`](Self::Game).** SPEC.md section 11 defines the other-system track
    /// as all system audio *minus* the game's process tree, so a track carrying
    /// the game as well is not that track under a different name: somebody who
    /// mutes "Other System Audio" expecting the game to stay audible would lose
    /// the game, which is the failure AGENTS.md section 21 exists to prevent.
    /// Calling it "Game" would be the same lie the other way round.
    ///
    /// Two recordings produce it. A capture with no process to scope to — a
    /// monitor, or a window whose process has gone — has nothing to separate, so
    /// one endpoint capture is the whole of its system audio. And a machine that
    /// **cannot** scope a capture to a process at all, which is every Windows 10
    /// build below 20348, records this instead of the pair it was asked for
    /// (`AudioError::ProcessLoopbackUnavailable`, ADR 0003,
    /// [issue #604](https://github.com/wildware-uk/clipped/issues/604)). In both
    /// cases the name is the honest one: an editor shows a track that says it
    /// holds everything, and it does.
    ///
    /// It never appears beside [`Game`](Self::Game) or
    /// [`OtherSystemAudio`](Self::OtherSystemAudio). A recording either
    /// separated the two or it did not.
    SystemAudio,
    /// The detected game's process tree.
    Game,
    /// Everything the machine played that was not the game.
    OtherSystemAudio,
    /// The selected input device.
    Microphone,
    /// A voice chat application, where one has been routed to its own track.
    VoiceChat,
    /// One application, on a track of its own.
    Application {
        /// What the track is called: the application's name, as the user knows
        /// it — `Spotify`, not `spotify.exe`.
        name: String,
    },
}

impl AudioSource {
    /// A track for one application, named as the user knows it.
    #[must_use]
    pub fn application(name: impl Into<String>) -> Self {
        Self::Application { name: name.into() }
    }

    /// The name the container carries for this source, which is what an editor
    /// shows instead of `Audio 3`.
    #[must_use]
    pub fn track_name(&self) -> &str {
        match self {
            Self::CompatibilityMix => "Compatibility Mix",
            Self::SystemAudio => "System Audio",
            Self::Game => "Game",
            Self::OtherSystemAudio => "Other System Audio",
            Self::Microphone => "Microphone",
            Self::VoiceChat => "Voice Chat",
            Self::Application { name } => name,
        }
    }

    /// Where this source sits among a recording's audio tracks.
    ///
    /// Lower comes first. The compatibility mix leads because a player that
    /// takes the first audio track it finds has to find the one that sounds
    /// right (SPEC.md section 13); the rest follow SPEC.md section 11's own
    /// listing, and application tracks come last in the order they were
    /// configured, because there can be any number of them and nothing
    /// distinguishes one from another.
    ///
    /// This is the whole of "deterministic track ordering": two recordings
    /// configured with the same sources have their tracks in the same order
    /// whatever order the sources were declared in, so a saved editor project or
    /// a support instruction that says "track 3 is the microphone" keeps being
    /// true.
    #[must_use]
    pub const fn ordering_rank(&self) -> u8 {
        match self {
            Self::CompatibilityMix => 0,
            // One rank for the system side however it was captured. A recording
            // has either the pair or the undivided track and never both, so
            // sharing the first of the two ranks with `Game` costs nothing and
            // keeps the microphone in the same place in every file.
            Self::SystemAudio | Self::Game => 1,
            Self::OtherSystemAudio => 2,
            Self::Microphone => 3,
            Self::VoiceChat => 4,
            Self::Application { .. } => 5,
        }
    }

    /// Whether a player should select this track on its own.
    ///
    /// The compatibility mix, and nothing else.
    #[must_use]
    pub const fn is_default_track(&self) -> bool {
        matches!(self, Self::CompatibilityMix)
    }
}

impl fmt::Display for AudioSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.track_name())
    }
}

/// Turns captured samples into packets on one audio track.
///
/// This is the path between a capture and a file. `clipped-audio` hands out
/// interleaved `f32` samples in `[-1.0, 1.0]` with a timestamp on the
/// recording's clock (`docs/audio-routing.md`); a container wants packets of
/// coded bytes. Converting and cutting them up is the same arithmetic for every
/// track and every source, so it is written once here rather than in each caller
/// that has audio to write (AGENTS.md section 55).
///
/// One of these per track. It holds no audio between calls — a buffer handed in
/// is written before the call returns — so a recording never has sound waiting
/// in this process for more sound to arrive.
///
/// ```no_run
/// # use clipped_muxer::{
/// #     AudioSource, AudioTrack, AudioTrackWriter, MkvWriter, PacketTimestamp,
/// #     RecordingLayout, VideoCodec, VideoTrack,
/// # };
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let samples: Vec<f32> = Vec::new();
/// # let path = std::path::Path::new("recording.mkv");
/// let microphone = AudioTrack::for_source(AudioSource::Microphone, 48_000, 1);
/// let layout = RecordingLayout::new(VideoTrack::new(VideoCodec::H264, 1920, 1080))
///     .with_audio_track(microphone.clone());
/// let track = layout
///     .audio_track_for(&AudioSource::Microphone)
///     .expect("the track was just declared");
///
/// let mut writer = MkvWriter::create(path, &layout)?;
/// let mut audio = AudioTrackWriter::new(track, &microphone)?;
/// audio.write_samples(&mut writer, PacketTimestamp::from_nanos(0), &samples)?;
/// # Ok(())
/// # }
/// ```
///
/// # A source that produces nothing
///
/// Nothing is written, and nothing is invented. A track whose source never
/// produced a sample is declared in the header — Matroska fixes the track list
/// there, so it could not appear later even if the source woke up — and holds no
/// packets. [`RecordingSummary::audio_tracks_without_packets`](crate::RecordingSummary::audio_tracks_without_packets)
/// counts those tracks and [`MkvWriter::finish`] names them in the log, because
/// an empty track is the visible symptom of a muted microphone, a device that
/// was never opened or routing that pointed at an application that was not
/// running, and a recorder that says nothing about it leaves the user to
/// discover it in an editor.
///
/// Filling such a track with manufactured silence is deliberately *not* done
/// here. A capture is the only thing that knows the difference between "the
/// device produced silence" and "there was no device", and `clipped-audio`
/// already synthesises silence to keep a track the length of its recording
/// (`docs/audio-routing.md`); a muxer that invented more would be writing audio
/// nobody captured.
#[derive(Debug)]
pub struct AudioTrackWriter {
    track: TrackId,
    sample_rate: u32,
    channels: u16,
    /// The coded bytes of the packet being written, reused for the whole
    /// recording: this runs for every buffer of every track for hours, and
    /// allocating per packet on that path is what AGENTS.md section 18 asks
    /// against.
    bytes: Vec<u8>,
}

impl AudioTrackWriter {
    /// Prepares to write samples into `track`, which must be the audio track
    /// `declared` describes.
    ///
    /// The declaration is taken rather than a sample rate and a channel count so
    /// that the format samples are written in cannot drift from the format the
    /// container was told about. A track described as 48 kHz stereo and fed
    /// mono buffers produces a file that plays at half speed on one channel,
    /// and nothing about it looks wrong until somebody listens.
    ///
    /// # Errors
    ///
    /// [`MuxError::InvalidTrack`] for the video track, for a track whose codec
    /// is not [`RECORDING_AUDIO_CODEC`] — this writer produces uncompressed
    /// samples and cannot pretend they are Opus — and for a declaration with no
    /// sampling rate or no channels.
    pub fn new(track: TrackId, declared: &AudioTrack) -> Result<Self, MuxError> {
        if track == TrackId::Video {
            return Err(MuxError::InvalidTrack {
                track,
                reason: "captured audio samples cannot be written to the video track",
            });
        }
        if declared.codec() != RECORDING_AUDIO_CODEC {
            return Err(MuxError::InvalidTrack {
                track,
                reason: "this writer converts captured samples to uncompressed PCM, and the \
                         track was declared in a codec whose packets have to come from an \
                         encoder",
            });
        }
        if declared.sample_rate() == 0 {
            return Err(MuxError::InvalidTrack {
                track,
                reason: "samples cannot be timed on a track with no sampling rate",
            });
        }
        if declared.channels() == 0 {
            return Err(MuxError::InvalidTrack {
                track,
                reason: "interleaved samples cannot be split up for a track with no channels",
            });
        }

        Ok(Self {
            track,
            sample_rate: declared.sample_rate(),
            channels: declared.channels(),
            bytes: Vec::new(),
        })
    }

    /// The track these samples go to.
    #[must_use]
    pub const fn track(&self) -> TrackId {
        self.track
    }

    /// Writes `samples` — interleaved, `channels` per frame — as audio starting
    /// at `at`.
    ///
    /// A buffer longer than [`PACKET`] is split, and each piece is timed from
    /// its own offset within the buffer rather than from the piece before it, so
    /// a rate that does not divide evenly into nanoseconds rounds once per
    /// packet instead of accumulating across a session.
    ///
    /// An empty slice writes nothing and is not an error: a capture that has
    /// nothing to report yet is the ordinary case, not a failure.
    ///
    /// # Errors
    ///
    /// [`MuxError::PartialAudioFrame`] when the slice does not hold a whole
    /// number of frames, which means the caller's channel count and this
    /// track's have diverged; and whatever [`MkvWriter::write_packet`] reports —
    /// for a recorder running for hours, usually the disk filling up.
    pub fn write_samples(
        &mut self,
        writer: &mut MkvWriter,
        at: PacketTimestamp,
        samples: &[f32],
    ) -> Result<(), MuxError> {
        let channels = usize::from(self.channels);
        if samples.len() % channels != 0 {
            return Err(MuxError::PartialAudioFrame {
                track: self.track,
                samples: samples.len(),
                channels: self.channels,
            });
        }

        let frames_per_packet = frames_per_packet(self.sample_rate);
        for (first_frame, frames) in packets(samples.len() / channels, frames_per_packet) {
            let from = first_frame * channels;
            encode_pcm_s16le(&mut self.bytes, &samples[from..from + frames * channels]);

            writer.write_packet(
                &EncodedPacket::new(
                    self.track,
                    PacketTimestamp::from_nanos(
                        at.as_nanos()
                            .saturating_add(nanos_for(first_frame, self.sample_rate)),
                    ),
                    &self.bytes,
                )
                .with_duration(Duration::from_nanos(
                    nanos_for(frames, self.sample_rate).unsigned_abs(),
                ))
                // Every PCM packet stands on its own: there is nothing to
                // decode, so a player can start at any of them.
                .with_keyframe(true),
            )?;
        }

        Ok(())
    }
}

/// How many frames of audio fit in one packet at `sample_rate`.
///
/// At least one, so that a rate below 50 Hz — which no capture produces, but
/// which a caller can describe — cannot ask for packets of no frames and loop
/// forever.
fn frames_per_packet(sample_rate: u32) -> usize {
    let frames = u64::from(sample_rate) * PACKET.as_millis() as u64 / 1000;
    usize::try_from(frames).unwrap_or(usize::MAX).max(1)
}

/// Splits `frames` into packets of at most `per_packet`, as
/// `(first frame, frames in this packet)`.
fn packets(frames: usize, per_packet: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..frames)
        .step_by(per_packet.max(1))
        .map(move |first| (first, per_packet.min(frames - first)))
}

/// How long `frames` last at `sample_rate`, in nanoseconds.
///
/// Computed in 128 bits: a session running for days at 48 kHz passes the point
/// where a frame count multiplied by a billion overflows 64, and a wrapped
/// timestamp is a recording with a packet at the far end of the timeline.
fn nanos_for(frames: usize, sample_rate: u32) -> i64 {
    let nanos = frames as i128 * 1_000_000_000 / i128::from(sample_rate);
    i64::try_from(nanos).unwrap_or(i64::MAX)
}

/// Converts interleaved `f32` samples to the bytes a PCM track stores, replacing
/// whatever `bytes` held.
///
/// Signed 16-bit little-endian, which is what [`RECORDING_AUDIO_CODEC`] names
/// and what Matroska's `BitDepth` for the track says a player will find.
///
/// Two details that are audible when they are wrong:
///
/// - **The scale is 32,767 rather than 32,768.** Full scale in either direction
///   is representable, and `1.0 * 32768` is not.
/// - **Samples are clamped, not wrapped.** `clipped-audio` produces `[-1.0,
///   1.0]`, but a mix of several sources (issue #29) can sum past it, and a
///   sample that wrapped would turn the loudest instant of a recording into a
///   full-scale click in the opposite direction (AGENTS.md section 21,
///   "clipping").
fn encode_pcm_s16le(bytes: &mut Vec<u8>, samples: &[f32]) {
    bytes.clear();
    bytes.reserve(samples.len() * 2);
    for sample in samples {
        let scaled = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        bytes.extend_from_slice(&scaled.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compatibility_mix_leads_and_the_rest_follow_the_spec_listing() {
        // The order two recordings have to agree on. Asserted as a sequence
        // rather than rank by rank, because what matters is the arrangement.
        let mut sources = [
            AudioSource::application("Spotify"),
            AudioSource::Microphone,
            AudioSource::VoiceChat,
            AudioSource::Game,
            AudioSource::CompatibilityMix,
            AudioSource::OtherSystemAudio,
        ];
        sources.sort_by_key(AudioSource::ordering_rank);

        let names: Vec<_> = sources.iter().map(AudioSource::track_name).collect();
        assert_eq!(
            names,
            [
                "Compatibility Mix",
                "Game",
                "Other System Audio",
                "Microphone",
                "Voice Chat",
                "Spotify",
            ]
        );
    }

    #[test]
    fn an_undivided_system_track_takes_the_place_the_game_track_would_have_had() {
        // The layout of a recording made on a machine that cannot scope a
        // capture to a process (issue #604). The microphone has to stay where it
        // is: a support instruction that says "track 3 is the microphone" is
        // true of a recording with the pair, and has to stay true of one
        // without it.
        let mut sources = [
            AudioSource::Microphone,
            AudioSource::CompatibilityMix,
            AudioSource::SystemAudio,
        ];
        sources.sort_by_key(AudioSource::ordering_rank);

        let names: Vec<_> = sources.iter().map(AudioSource::track_name).collect();
        assert_eq!(names, ["Compatibility Mix", "System Audio", "Microphone"]);
        assert_ne!(
            AudioSource::SystemAudio.track_name(),
            AudioSource::OtherSystemAudio.track_name(),
            "everything the machine played and everything-except-the-game are \
             different claims, and a track that makes the wrong one is worse than a \
             refusal (SPEC.md section 11)"
        );
    }

    #[test]
    fn only_the_compatibility_mix_is_the_track_a_player_picks() {
        // Two default tracks is a file whose player chooses between them, which
        // is the situation the flag exists to end (SPEC.md section 13).
        assert!(AudioSource::CompatibilityMix.is_default_track());
        for source in [
            AudioSource::SystemAudio,
            AudioSource::Game,
            AudioSource::OtherSystemAudio,
            AudioSource::Microphone,
            AudioSource::VoiceChat,
            AudioSource::application("Discord"),
        ] {
            assert!(!source.is_default_track(), "{source} claimed the flag");
        }
    }

    #[test]
    fn samples_become_signed_little_endian_bytes_at_full_scale() {
        let mut bytes = Vec::new();
        encode_pcm_s16le(&mut bytes, &[0.0, 1.0, -1.0, 0.5]);

        assert_eq!(
            bytes,
            [
                0x00, 0x00, // silence
                0xff, 0x7f, // +32767
                0x01, 0x80, // -32767
                0x00, 0x40, // +16383.5, rounded away from zero
            ]
        );
    }

    #[test]
    fn a_sample_past_full_scale_is_clamped_rather_than_wrapped() {
        // A mix that summed past full scale (issue #29). Wrapping turns the
        // loudest instant of a recording into a click of the opposite sign,
        // which is the failure this arm exists for.
        let mut bytes = Vec::new();
        encode_pcm_s16le(&mut bytes, &[1.5, -1.5, f32::INFINITY, f32::NEG_INFINITY]);

        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(samples, [32_767, -32_767, 32_767, -32_767]);
    }

    #[test]
    fn a_buffer_longer_than_one_packet_is_split_and_nothing_is_lost() {
        // 50 ms at 48 kHz: two full 20 ms packets and a 10 ms remainder.
        let per_packet = frames_per_packet(48_000);
        assert_eq!(per_packet, 960);

        let split: Vec<_> = packets(2400, per_packet).collect();
        assert_eq!(split, [(0, 960), (960, 960), (1920, 480)]);
        assert_eq!(
            split.iter().map(|(_, frames)| frames).sum::<usize>(),
            2400,
            "every frame handed in has to be written exactly once"
        );

        // And a buffer shorter than a packet is written as it stands rather than
        // held back until more audio arrives.
        assert_eq!(packets(96, per_packet).collect::<Vec<_>>(), [(0, 96)]);
        assert_eq!(packets(0, per_packet).count(), 0);
    }

    #[test]
    fn each_packet_is_timed_from_its_own_offset_rather_than_from_the_one_before() {
        // 44.1 kHz is the rate that catches accumulation: 20 ms is 882 frames,
        // which is exactly 20 ms, but a rate whose packet length is not a whole
        // number of nanoseconds would drift if each packet were timed by adding
        // to its predecessor. A quarter of a second in, the offset must still be
        // the true one.
        assert_eq!(nanos_for(882, 44_100), 20_000_000);
        assert_eq!(nanos_for(11_025, 44_100), 250_000_000);
        assert_eq!(nanos_for(960, 48_000), 20_000_000);
        assert_eq!(nanos_for(0, 48_000), 0);

        // An hour of audio, where a 64-bit product of frames and a billion would
        // still be fine but a session lasting days would not.
        assert_eq!(nanos_for(48_000 * 3600, 48_000), 3_600_000_000_000);
    }

    #[test]
    fn a_track_this_writer_cannot_produce_packets_for_is_refused() {
        // Handing PCM to a track declared as Opus writes bytes no decoder can
        // read, into a file that opens and looks healthy.
        let opus = AudioTrack::new(AudioCodec::Opus, 48_000, 2).with_codec_private([1, 2, 3]);
        let error = AudioTrackWriter::new(TrackId::Audio(0), &opus)
            .expect_err("only PCM tracks can be fed captured samples");
        assert!(
            matches!(error, MuxError::InvalidTrack { .. }),
            "unexpected error: {error}"
        );

        let error = AudioTrackWriter::new(
            TrackId::Video,
            &AudioTrack::new(RECORDING_AUDIO_CODEC, 48_000, 2),
        )
        .expect_err("audio does not go to the video track");
        assert!(
            matches!(error, MuxError::InvalidTrack { .. }),
            "unexpected error: {error}"
        );

        for broken in [
            AudioTrack::new(RECORDING_AUDIO_CODEC, 0, 2),
            AudioTrack::new(RECORDING_AUDIO_CODEC, 48_000, 0),
        ] {
            assert!(
                AudioTrackWriter::new(TrackId::Audio(0), &broken).is_err(),
                "a track with no rate or no channels cannot be written to"
            );
        }
    }
}
