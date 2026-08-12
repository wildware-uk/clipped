//! A controlled test application whose *child* plays a known tone.
//!
//! # Why this exists
//!
//! Process-scoped audio capture is the feature Clipped is built around
//! ([ADR 0003](../../../docs/adr/0003-process-specific-audio-capture.md)), and
//! the thing it must get right is that a game is not one process: a launcher
//! starts it, it starts helpers, and the process that actually renders the
//! sound is often not the one anybody named. A capture that only records the
//! process it was pointed at looks perfect until it meets a game like that.
//!
//! So the subject is deliberately two processes. The parent plays nothing at
//! all; on request it starts a child, and the child is what makes the noise. A
//! test scopes a capture to the parent, asks it for a child, and asserts the
//! child's tone is in the recording — which is issue #26's second acceptance
//! criterion, and cannot be arranged with a single-process generator (AGENTS.md
//! section 26, issue #136).
//!
//! Two of them, started separately, are how *isolation* is asserted: each is
//! its own tree, so a capture scoped to one must contain that one's tone and
//! not the other's.
//!
//! # The protocol
//!
//! Line-based, on standard output, so that a test drives it with no person
//! watching and nothing left running afterwards ([`harness`]):
//!
//! ```text
//! ready pid=1234 role=parent
//! ready pid=1234 role=player frequency=997 amplitude=0.04 rate=48000 channels=2
//! child pid=5678
//! unavailable reason=<why this machine cannot play a tone>
//! stopped
//! ```
//!
//! A run ends when its standard input closes, which is what [`harness::ToneSubject`]
//! does on the way out however the test ended, or after `--seconds` if one was
//! given.
//!
//! # It makes a noise
//!
//! Quietly — [`AMPLITUDE`] is about −28 dBFS — and only while a player is
//! running. [`FREQUENCY`] is 997 Hz for the reason `crates/audio` chose it: it
//! is the frequency digital audio has used for a century of measurements
//! because no instrument plays it, so music playing on the machine puts almost
//! nothing in that bin. A test that asks for two tones at once should pick the
//! second one the same way (`tests/process_loopback_isolation.rs` uses
//! [`SECOND_FREQUENCY`]).

pub mod harness;

#[cfg(windows)]
pub mod tone;

/// The tone a player renders unless it is told otherwise, in hertz.
///
/// 997 Hz: not a musical note, so background music on a developer's machine
/// contributes almost nothing to the bin a test measures. See the module
/// documentation.
pub const FREQUENCY: f32 = 997.0;

/// A second tone for the process tree a test is *not* capturing.
///
/// 1373 Hz is neither a harmonic of [`FREQUENCY`] nor a musical note, so
/// neither tone can be mistaken for the other and nothing on the machine
/// produces either by accident. Both matter: an isolation test asserts that one
/// tone is present *and* that the other is not, and a second frequency that was
/// a harmonic of the first would fail the second half for a reason that has
/// nothing to do with the capture.
pub const SECOND_FREQUENCY: f32 = 1373.0;

/// The peak amplitude of a rendered tone, as a fraction of full scale.
///
/// About −28 dBFS. A Goertzel filter finds a tone far below this; the volume is
/// set by politeness on a machine somebody is using rather than by what the
/// measurement needs.
pub const AMPLITUDE: f32 = 0.04;
