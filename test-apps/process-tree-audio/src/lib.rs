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
//! Quietly — about −28 dBFS — and only while a player is running. The waveform,
//! the loop that feeds it to the endpoint and the frequencies a test asks for
//! are `clipped_video_pattern::steady_tone`, which is where they moved when the
//! video subject needed the same continuous tone: two test applications
//! rendering a sine to the same endpoint should be one loop rather than two
//! that drift apart (AGENTS.md section 55). It says why the frequencies are
//! 997 Hz and 1373 Hz rather than a musical note or its harmonic.

pub mod harness;
