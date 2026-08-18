//! The game's own audio: one process tree, captured on its own.
//!
//! # What it is
//!
//! Everything one process tree plays, and nothing else — the track SPEC.md
//! section 11 calls "Game", and the reason Clipped exists rather than a
//! recorder that writes one mixed stream
//! ([ADR 0003](../../../../docs/adr/0003-process-specific-audio-capture.md)).
//! Windows scopes a capture client to a process and the processes it started
//! through `ActivateAudioInterfaceAsync`, and it offers **both sides**:
//! `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` for what the tree
//! played, and `..._EXCLUDE_...` for everything the machine played except it.
//! `activation.rs` is that call, [`TreeScope`] is which side, and this is what
//! a recording does with them.
//!
//! A session needs both or neither ([issue
//! #27](https://github.com/wildware-uk/clipped/issues/27)): a recording with a
//! `Game` track scoped to the tree *and* a system track that is the whole
//! endpoint has the game's audio on two tracks, which is worse than one honest
//! track and a note saying separation was unavailable (ADR 0003).
//!
//! And it needs them scoped to the **same process**, which is what
//! [`ProcessLoopbackCapture::open_pair`] is for. Two captures opened
//! separately each resolve a surviving process tree from their own
//! [`ProcessTree`], on their own schedule, so a game whose launcher exits
//! partway through a recording can leave them on different trees for the rest
//! of it — and a process in one and not the other is a process whose audio is
//! on both tracks or on neither. `decide_scope` is the rule that stops that,
//! and it agrees through one atomic rather than a lock, because both sides are
//! read on capture threads (AGENTS.md section 20).
//!
//! Everything after the activation is `endpoint_capture.rs`: the same packet
//! loop, the same timeline that keeps a track as long as its recording, the
//! same conversion to `f32`, the same handling of a stream that fails
//! (AGENTS.md section 55). Three things are genuinely different, and they are
//! what this file is.
//!
//! # 1. Nobody says what shape the audio is
//!
//! A process-scoped client has no endpoint, so it has no mix format:
//! `GetMixFormat` is not available on it and the format is the *caller's*
//! choice, which the audio engine then converts into. The choice made here is
//! the default output endpoint's rate and channel count, as 32-bit float —
//! because the game track has to sit in the same file as the system-audio and
//! microphone tracks, and this crate has no resampler
//! ([issue #30](https://github.com/wildware-uk/clipped/issues/30)). A capture
//! that quietly asked for 44.1 kHz would produce a track nothing could mux
//! beside the others.
//!
//! If the audio engine refuses that shape — or the machine has no output device
//! to take it from — [`FALLBACK_RATE`] stereo is asked for instead, and the
//! accepted format is fixed for the life of the capture so that a stream
//! reopened halfway through a recording cannot change it.
//!
//! # 2. The target is a tree, and Windows only takes one root
//!
//! `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS` names **one** process, and Windows
//! includes what that process started. `clipped_windows::ProcessTree`
//! (issue #25) tracks the same membership from this side, with handles rather
//! than remembered identifiers, and is used here for the two questions the
//! activation cannot answer:
//!
//! - **Has the game gone?** An empty tree is the game and everything it started
//!   having exited, which is what [`ProcessLoopbackCapture::target_is_running`]
//!   reports. The capture does not end itself over it — a recording is worth
//!   more than the audio it is missing (AGENTS.md section 17) — the including
//!   side's track simply becomes silence. The **excluding** side's does not:
//!   excluding an empty set is everything the machine plays, which is exactly
//!   what that track is for when somebody closes the game and keeps recording
//!   with a browser still playing. `opens_against_an_empty_tree` is that
//!   asymmetry, and the measurement it rests on
//!   ([issue #563](https://github.com/wildware-uk/clipped/issues/563)).
//! - **Has the process the activation names gone, while the game lives on?**
//!   Some titles re-execute themselves and exit the process that was launched.
//!   The tree survives that (its members are pinned by handles, and a dead
//!   member is kept as a ghost while anything descends from it), so the capture
//!   re-scopes onto a surviving member rather than recording silence for the
//!   rest of the session.
//!
//! What that re-scoping cannot do is cover *several* surviving members at once:
//! one client, one root. When more than one member survives the process the
//! capture was scoped to, the members outside the new root's own subtree are
//! not captured, and a `warn` says so by name.
//! [Issue #311](https://github.com/wildware-uk/clipped/issues/311) is the way
//! out — several clients mixed into one track — and it needs the mixing stage
//! from [issue #29](https://github.com/wildware-uk/clipped/issues/29).
//!
//! # 3. It may not be available at all
//!
//! Process loopback is documented from Windows build 20348, which no shipping
//! Windows 10 release reaches. [`AudioError::ProcessLoopbackUnavailable`] is
//! what a machine below that floor produces, and the documented answer is a
//! single system-audio track with the separation explicitly stated as
//! unavailable rather than a track labelled "Game" that is really everything
//! (ADR 0003's second consequence, `docs/audio-routing.md`).

use core::num::{NonZeroU16, NonZeroU32};
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;
use std::sync::Arc;

use clipped_windows::{ProcessTree, WindowsError};
use windows::Win32::Media::Audio::{
    eRender, IAudioClient, IMMDeviceEnumerator, AUDCLNT_E_UNSUPPORTED_FORMAT,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, PROCESS_LOOPBACK_MODE,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;

use crate::error::{AudioError, Capture};
use crate::format::{AudioFormat, ChannelMask, SampleFormat};
use crate::windows::activation::activate_process_loopback;
use crate::windows::endpoint::{
    default_endpoint, platform_error, EndpointIdentity, MixFormat, SourceKind,
};
use crate::windows::endpoint_capture::{
    create_wake_event, CaptureSource, CaptureStats, EndpointCapture, PositionTrust, Reopen,
    StreamParts, Wake, BUFFER_DURATION,
};

/// The sample rate asked for when the default output endpoint cannot supply
/// one.
///
/// 48 kHz is what the Windows audio engine mixes at on every machine this has
/// been seen on, so it is the shape least likely to be converted twice.
const FALLBACK_RATE: u32 = 48_000;

/// The channel count asked for beside [`FALLBACK_RATE`].
const FALLBACK_CHANNELS: u16 = 2;

/// The speaker positions of a stereo pair: front left and front right.
const STEREO_MASK: u32 = 0x3;

/// How the capture is described in a log line while it is being opened.
///
/// A process-scoped client is not on a device, so the `device` field of the
/// shared engine's log lines would otherwise be empty for it. It names the
/// *side* as well as the tree, because a recording runs both at once and a
/// pair of lines that describe themselves identically is a pair of lines
/// nobody can tell apart (issue #27).
fn describe(root: u32, scope: TreeScope) -> String {
    match scope {
        TreeScope::Only => format!("the game's process tree, rooted at process {root}"),
        TreeScope::Except => {
            format!("everything except the game's process tree, rooted at process {root}")
        }
    }
}

/// Which side of a process tree a capture takes.
///
/// Windows offers both, and a session needs **both or neither**: a recording
/// with a `Game` track scoped to the tree and a system track that is the whole
/// endpoint has the game's audio on two tracks, which is worse than having one
/// track and saying so (`clipped_session::audio`, ADR 0003).
///
/// The two are the same activation with one constant changed, which is why they
/// are one type rather than two capture implementations
/// ([issue #27](https://github.com/wildware-uk/clipped/issues/27)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TreeScope {
    /// Everything the tree played, and nothing else. The game's own track.
    Only,
    /// Everything the machine played **except** the tree. The other-system-audio
    /// track that can sit beside a game track without doubling it.
    Except,
}

impl TreeScope {
    /// The Windows constant for this side.
    const fn mode(self) -> PROCESS_LOOPBACK_MODE {
        match self {
            Self::Only => PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            Self::Except => PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        }
    }

    /// How this reads in a log line.
    pub(super) const fn log_value(self) -> &'static str {
        match self {
            Self::Only => "the process tree",
            Self::Except => "everything but the process tree",
        }
    }

    /// How this side reads in a stream's identifier.
    ///
    /// Short and stable, because it ends up in the `device` identifier of the
    /// engine's log lines rather than in prose.
    pub(super) const fn identity_word(self) -> &'static str {
        match self {
            Self::Only => "only",
            Self::Except => "except",
        }
    }

    /// Which track's `audio_source` this side's log lines carry.
    ///
    /// The two sides go to two different tracks, so they are two different
    /// kinds even though they are one activation with one constant changed:
    /// `docs/logging.md`'s closed list of `audio_source` values exists so that
    /// somebody reading a user's log months later can filter to one track, and
    /// a pair that both said `game` would make that impossible in exactly the
    /// recording it matters for.
    pub(super) const fn kind(self) -> SourceKind {
        match self {
            Self::Only => SourceKind::GameAudio,
            Self::Except => SourceKind::OtherSystemAudio,
        }
    }
}

/// What a capture should do about the process its activation names.
///
/// Separated from the tree, the atomic and the reopen so that the rule can be
/// tested on a machine with no audio device, no game and no process table
/// (`decide_scope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rescope {
    /// The activation still names a living member. Nothing to do.
    Stay,
    /// This capture had followed the other side onto a process it could not
    /// then see, and can now see it. Still nothing to do to the stream — but
    /// this capture may lead again.
    CaughtUp,
    /// The other side of the pair has already chosen; take its choice.
    Follow(u32),
    /// This capture chose, and has published the choice for the other side.
    Lead(u32),
    /// The game and everything it started have exited.
    Ended,
}

/// Which process both sides of a pair should be scoped to next.
///
/// Windows takes one root per activation, and the two sides of one recording
/// have to name the **same** root or the recording is wrong in one of two ways:
/// scoped to different trees, a process in one but not the other has its audio
/// on both tracks, or on neither. Nothing forces agreement on its own — the two
/// captures run on two threads, each with its own [`ProcessTree`] reading the
/// process table on its own schedule, so a root that exits while two children
/// live can be resolved differently by each of them, and neither ever
/// reconsiders once it is scoped to something alive.
///
/// `scoping` is what they agree through: one `u32` naming the process the pair
/// is scoped to, read and written with no lock, which is what makes this safe
/// to call from a capture thread (AGENTS.md section 20).
///
/// The rule, in order:
///
/// 1. If the cell no longer says what this capture is scoped to, the other side
///    has moved. **Follow it**, even if this capture's own tree has not yet
///    caught up with the process it moved to: being briefly scoped to something
///    this side cannot see is a track that is briefly silent, while disagreeing
///    is a track with the wrong audio on it.
/// 2. A capture that is following does not lead. That is what makes this
///    terminate: without it, two captures whose trees disagree about a process
///    would each keep moving away from the other's choice.
/// 3. Otherwise, a capture whose activation names a dead process picks the
///    lowest-numbered living member and publishes it — and if the other side
///    published first, in the window between the read and the write, that one
///    wins.
///
/// `members` must be in ascending order, which is what
/// [`ProcessTree::members`] documents.
fn decide_scope(scoping: &AtomicU32, scoped_to: u32, following: bool, members: &[u32]) -> Rescope {
    // Relaxed throughout: the process identifier is the whole message. Nothing
    // else is published alongside it, so there is no other write for an
    // acquire-release pair to order against.
    let agreed = scoping.load(Ordering::Relaxed);
    if agreed != scoped_to {
        return Rescope::Follow(agreed);
    }

    let alive = members.binary_search(&scoped_to).is_ok();
    if following {
        return if alive {
            Rescope::CaughtUp
        } else {
            Rescope::Stay
        };
    }
    if alive {
        return Rescope::Stay;
    }

    let Some(&successor) = members.first() else {
        return Rescope::Ended;
    };
    match scoping.compare_exchange(scoped_to, successor, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => Rescope::Lead(successor),
        Err(theirs) => Rescope::Follow(theirs),
    }
}

/// Which processes a capture is scoped to, and how that is kept current.
///
/// The [`CaptureSource`] arm of a process-scoped capture. It owns the tree, the
/// format that was accepted, and the decision to re-scope; the engine owns
/// everything that happens to the samples afterwards.
#[derive(Debug)]
pub(super) struct ProcessLoopbackSource {
    /// The game, as the session identified it. Never changes: it is what the
    /// tree is rooted at and what the track is named after.
    root: u32,
    /// The process the current activation names, which is [`Self::root`] until
    /// that process exits with descendants still running.
    scoped_to: u32,
    /// Which side of the tree this capture takes.
    scope: TreeScope,
    /// The process both sides of a pair are scoped to, shared with the other
    /// side when there is one (`decide_scope`, issue #27).
    ///
    /// A capture opened on its own owns this alone, so it always agrees with
    /// itself and behaves exactly as it did before there was a pair.
    scoping: Arc<AtomicU32>,
    /// Whether [`Self::scoped_to`] came from the other side of the pair rather
    /// than from this capture's own tree, and has not yet been seen alive here.
    ///
    /// What stops two captures whose trees disagree from moving away from each
    /// other for ever; see `decide_scope`.
    following: bool,
    /// The membership this capture is really about, maintained live
    /// (issue #25).
    tree: ProcessTree,
    /// The format the audio engine accepted, fixed at the first successful
    /// open. A track's shape may not change underneath a muxer that has already
    /// written a stream header.
    format: Option<AudioFormat>,
    /// A pending reason for the engine to throw the stream away and activate
    /// again.
    change: Option<Reopen>,
    /// Whether the tree has been observed to be empty, so that the game ending
    /// is logged once rather than on every refresh.
    ended: bool,
    /// How this capture is named in a log line.
    description: String,
}

impl ProcessLoopbackSource {
    /// Builds a source scoped to `root` and everything it started.
    ///
    /// `scoping` is the cell this capture agrees with the other side of its
    /// pair through, and is shared with it when there is one. It must already
    /// hold `root`.
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessUnavailable`] when the process cannot be followed:
    /// it has already exited, or it runs at a higher integrity level than
    /// Clipped. Either way there is no tree to scope a capture to, so it is a
    /// failure rather than an empty capture.
    pub(super) fn new(
        root: u32,
        scope: TreeScope,
        scoping: Arc<AtomicU32>,
    ) -> Result<Self, AudioError> {
        let tree = ProcessTree::rooted_at(root).map_err(|error| match error {
            WindowsError::ProcessUnavailable { process_id } => {
                AudioError::ProcessUnavailable { process_id }
            }
            other => AudioError::Platform {
                operation: "reading the processes a game consists of",
                source: Box::new(other),
            },
        })?;

        Ok(Self {
            root,
            scoped_to: root,
            scope,
            scoping,
            following: false,
            tree,
            format: None,
            change: None,
            ended: false,
            description: describe(root, scope),
        })
    }

    /// How this capture is named in a log line.
    pub(super) fn description(&self) -> &str {
        &self.description
    }

    /// Which track this capture's log lines belong to.
    pub(super) const fn kind(&self) -> SourceKind {
        self.scope.kind()
    }

    /// The process the current activation is scoped to.
    pub(super) fn scoped_to(&self) -> u32 {
        self.scoped_to
    }

    /// How many processes of the game are running.
    pub(super) fn members(&self) -> usize {
        self.tree.members().len()
    }

    /// Brings the tree up to date and decides whether the activation has to
    /// move.
    ///
    /// Called between reads, on the capture thread, from the shared engine.
    /// `ProcessTree::refresh` reads the process table at most once a second and
    /// costs about 25 ns inside that window, so calling it on every read is the
    /// intended use (`docs/audio-routing.md`).
    pub(super) fn take_change(&mut self) -> Option<Reopen> {
        match self.tree.refresh() {
            Ok(change) => {
                if !change.refused().is_empty() {
                    // Not an error and not membership: these are processes
                    // Windows will not let Clipped open, which in practice are
                    // a game's anti-cheat or crash-reporting services. Their
                    // audio — if they make any — is not in this track, and the
                    // only way anybody finds out is a log line.
                    tracing::debug!(
                        audio_source = %self.scope.kind().audio_source(),
                        refused = ?change.refused(),
                        "some of the game's processes cannot be opened, so they are not part \
                         of the tree this track is scoped to"
                    );
                }
            }
            Err(error) => {
                // The process table could not be read. Membership is left
                // exactly as it was and the next read tries again; a scan that
                // failed is not a game that has exited.
                tracing::warn!(
                    %error,
                    audio_source = %self.scope.kind().audio_source(),
                    "could not read which processes the game consists of; the capture stays \
                     scoped where it is"
                );
                return self.change.take();
            }
        }

        self.consider_rescoping();
        self.change.take()
    }

    /// Decides whether the process the activation names is still the right one.
    ///
    /// The decision itself is `decide_scope`, which is where the rule and the
    /// reasoning are; this is what it costs the stream.
    fn consider_rescoping(&mut self) {
        let source = self.scope.kind().audio_source();
        match decide_scope(
            &self.scoping,
            self.scoped_to,
            self.following,
            self.tree.members(),
        ) {
            Rescope::Stay => {}
            Rescope::CaughtUp => {
                // The process the other side chose is now a member here too.
                // Nothing happens to the stream — it is already scoped there —
                // but this capture may lead the next move.
                self.following = false;
            }
            Rescope::Ended => {
                // The game and everything it started have gone. The capture is
                // left exactly as it is: the track becomes silence of the right
                // length, because that is what a stream with nothing rendering
                // into it produces, and stopping is the caller's decision
                // rather than this crate's (AGENTS.md section 17).
                if !self.ended {
                    self.ended = true;
                    tracing::info!(
                        audio_source = %source,
                        root = self.root,
                        // Not the same outcome for the two sides, and both are
                        // now measured rather than guessed at (issue #563,
                        // `opens_against_an_empty_tree`). The including side
                        // has nothing left to include, so its track is silence.
                        // The excluding side excludes a set with nothing in it,
                        // which on Windows 11 build 26200 is everything the
                        // machine plays — for the stream that is already open
                        // and for any stream reopened afterwards.
                        this_track_now = match self.scope {
                            TreeScope::Only => "is silence from here",
                            TreeScope::Except =>
                                "excludes a tree with nothing left in it, so it is everything \
                                 the machine plays from here",
                        },
                        "the game and every process it started have exited"
                    );
                }
            }
            Rescope::Lead(successor) => {
                let members = self.tree.members();
                if members.len() > 1 {
                    // One activation, one root. A game that leaves two
                    // unrelated processes behind cannot be captured by one
                    // client, and saying so is better than a track that quietly
                    // lost half the game.
                    tracing::warn!(
                        audio_source = %source,
                        scoping_to = successor,
                        members = ?members,
                        "the process this capture was scoped to has exited and more than one \
                         process of the game is still running. Windows scopes a capture to one \
                         process tree, so audio from a process that did not descend from the \
                         one named here is not in this track (issue #311)"
                    );
                } else {
                    tracing::info!(
                        audio_source = %source,
                        scoping_to = successor,
                        "the process this capture was scoped to has exited and the game is \
                         still running, so the capture is re-scoping onto what is left of it"
                    );
                }
                self.rescope_to(successor, false);
            }
            Rescope::Follow(successor) => {
                tracing::info!(
                    audio_source = %source,
                    scoping_to = successor,
                    "the other side of this recording's process-loopback pair re-scoped, so \
                     this capture is following it onto the same process; both sides have to \
                     name one tree or the same audio lands on two tracks (issue #27)"
                );
                self.rescope_to(successor, true);
            }
        }
    }

    /// Moves the activation onto `successor` and asks the engine to reopen.
    ///
    /// `followed` records where the choice came from, which is what stops two
    /// captures whose trees disagree from chasing each other; see
    /// `decide_scope`.
    fn rescope_to(&mut self, successor: u32, followed: bool) {
        self.scoped_to = successor;
        self.following = followed;
        self.description = format!(
            "{}, captured through process {successor}",
            describe(self.root, self.scope)
        );
        self.change = Some(Reopen {
            reason: "the process this capture's audio was scoped through exited",
            // Paced by processes exiting rather than by a call that fails on
            // every look, so this cannot become a loop.
            from_failed_call: false,
        });
    }

    /// Activates and initialises a client scoped to the tree.
    ///
    /// [`None`] when there is nothing to *include*: every process of the game
    /// has exited, so the including side has no audio to ask Windows for. The
    /// engine treats that as a state to wait through, exactly as it waits
    /// through an unplugged microphone.
    ///
    /// The **excluding** side is opened anyway; [`opens_against_an_empty_tree`]
    /// is why.
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessLoopbackUnavailable`] when Windows will not give a
    /// process-scoped client, and [`AudioError::UnsupportedFormat`] when the
    /// audio engine refuses every shape this crate knows how to ask for.
    pub(super) fn open_stream(
        &mut self,
        enumerator: &IMMDeviceEnumerator,
    ) -> Result<Option<StreamParts>, AudioError> {
        let emptied = self.tree.members().is_empty();
        if emptied && !opens_against_an_empty_tree(self.scope) {
            return Ok(None);
        }
        if emptied {
            // Not on a packet path: `open_stream` runs when a stream is being
            // activated, which is once at the start and once per reopen
            // (AGENTS.md section 20). It is worth a line, because a track whose
            // meaning changed halfway through a recording and said nothing is
            // exactly what AGENTS.md section 35 exists to prevent — and because
            // the identifier below names a process that has exited, which is
            // the one thing a reader of this log would otherwise call a bug.
            tracing::info!(
                audio_source = %self.scope.kind().audio_source(),
                scoped_to = self.scoped_to,
                "the game's processes have all exited, so this capture is being reopened \
                 excluding a tree with nothing in it; measured on Windows 11 build 26200, that \
                 is everything the machine plays, which is what this track is for (issue #563)"
            );
        }

        let client = activate_process_loopback(self.scoped_to, self.scope.mode())?;

        let candidates = match self.format {
            // A reopen mid-recording: the shape is already decided, and asking
            // for anything else would change a track's format underneath
            // whatever is writing it.
            Some(format) => vec![format],
            None => candidate_formats(enumerator),
        };

        let (client, format, wake) = initialise(client, &candidates, self.scoped_to, self.scope)?;
        self.format = Some(format);

        Ok(Some(StreamParts {
            kind: self.scope.kind(),
            identity: EndpointIdentity {
                // The side is part of the identity: a recording runs both, and
                // two streams sharing one identifier is two streams a log
                // cannot tell apart (issue #27).
                id: format!(
                    "process-loopback:{}:{}",
                    self.scope.identity_word(),
                    self.scoped_to
                ),
                name: self.description.clone(),
            },
            format,
            client,
            wake,
            // There is no device and so no mute switch: what Windows mutes is
            // an endpoint, and this capture is not on one.
            mute: None,
            // Whether a process-scoped client fills the performance-counter
            // position of a packet is not documented, and a track timed from a
            // number that is not a counter reading would be discarded whole.
            // The first packet decides (`endpoint_capture.rs`).
            positions: PositionTrust::Unverified,
        }))
    }
}

/// Whether a stream is still worth activating once the game's tree is empty.
///
/// The two sides answer differently, and the asymmetry is the whole of
/// [issue #563](https://github.com/wildware-uk/clipped/issues/563).
///
/// **The including side: no.** There is nothing left to include, so an
/// activation would produce a stream of zeroes. The engine synthesises silence
/// of exactly the same length for nothing, and the track is silence either way.
///
/// **The excluding side: yes.** Excluding an empty set is *everything the
/// machine plays* — which is precisely what that track is for, and precisely
/// the case a user hits when they close the game and keep recording while a
/// browser or a voice call is still playing. Refusing here gave that track
/// silence for the rest of the recording from the first reopen onwards, and a
/// reopen is what an endpoint change or a re-scope does.
///
/// # What Windows does with an identifier that no longer names a process
///
/// Undocumented, so it was measured rather than reasoned about (AGENTS.md
/// section 54). On **Windows 11 Pro build 26200**, with a 997 Hz tone playing
/// from the measuring process and a `cmd.exe` that plays nothing as the game:
///
/// | Activation | Result | 997 Hz measured |
/// | --- | --- | --- |
/// | exclude, live identifier | activate, initialise and start all succeed | 0.02687 |
/// | exclude, identifier of a process that has exited | all succeed | 0.02690 |
/// | exclude, identifier that has never existed | all succeed | 0.02717 |
/// | include, identifier of a process that has exited | all succeed | 0.00000 |
/// | either side, identifier `0` | activation refused, `E_INVALIDARG` | — |
///
/// So an exclude-mode activation against a dead identifier is not a special
/// case to Windows at all: it excludes a tree with no members, which is
/// everything. The numbers are the same measurement the live baseline gives,
/// and the include row is the control that says the filter is really being
/// applied rather than every activation returning the endpoint.
///
/// That is why this is one condition rather than a fall back to
/// [`SystemAudioCapture`](super::SystemAudioCapture) on the whole endpoint,
/// which was the other candidate: it is the same activation, the same client
/// and the same format, so there is no changeover to make seamless and nothing
/// that could double if the tree became non-empty again.
///
/// # What it costs
///
/// Windows reuses process identifiers, and once the tree is empty
/// `ProcessTree` has released the handle that was pinning this one — that is
/// what makes an empty tree empty. So a reopen long afterwards can name an
/// identifier some unrelated process has since been given, and that process's
/// audio would then be missing from this track. It is bounded — one tree's
/// audio absent, where the alternative is the whole track silent — and it is
/// the same exposure [issue #311](https://github.com/wildware-uk/clipped/issues/311)
/// already describes for a game that leaves two trees behind. Closing it needs
/// `clipped-windows` to lend out the handle that pins an identifier, which is
/// a change to that crate's API rather than to this one.
const fn opens_against_an_empty_tree(scope: TreeScope) -> bool {
    match scope {
        TreeScope::Only => false,
        TreeScope::Except => true,
    }
}

/// The formats to ask the audio engine for, in the order they are tried.
///
/// The default output endpoint's rate and channel count first, as 32-bit float,
/// so that the game track can sit beside the system-audio track in one file
/// without resampling. Then a plain 48 kHz stereo, for a machine with no output
/// device — which is a machine that plays nothing, but whose games can still be
/// recorded — and for an engine that refuses the first.
fn candidate_formats(enumerator: &IMMDeviceEnumerator) -> Vec<AudioFormat> {
    let fallback = AudioFormat::new(
        NonZeroU32::new(FALLBACK_RATE).expect("48 kHz is not zero"),
        NonZeroU16::new(FALLBACK_CHANNELS).expect("stereo is not zero channels"),
        ChannelMask::from_bits(STEREO_MASK),
        SampleFormat::Float32,
    );

    let Some(preferred) = endpoint_shape(enumerator) else {
        return vec![fallback];
    };
    if preferred.is_interchangeable_with(&fallback) {
        return vec![fallback];
    }
    vec![preferred, fallback]
}

/// The rate and channel count the default output endpoint mixes at, as 32-bit
/// float.
///
/// [`None`] when there is no output device or Windows will not describe it,
/// both of which are ordinary on a machine with no sound card: there is then no
/// endpoint shape to match and the fallback is as good an answer as any.
fn endpoint_shape(enumerator: &IMMDeviceEnumerator) -> Option<AudioFormat> {
    let device = default_endpoint(enumerator, eRender).ok()??;
    // SAFETY: `device` is a live `IMMDevice`; windows-rs infers the interface
    // identifier from the return type, so the activation cannot ask for one
    // interface and be typed as another.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    let mix = MixFormat::of(&client).ok()?.audio();

    Some(AudioFormat::new(
        mix.sample_rate(),
        mix.channels(),
        mix.channel_mask(),
        // Whatever the endpoint presents, this capture asks the engine for
        // floats: it is the format the engine mixes in, so it is the one
        // conversion that cannot lose anything.
        SampleFormat::Float32,
    ))
}

/// Initialises the client with the first format the audio engine accepts.
///
/// Answers the client, the format that was accepted and how the capture thread
/// will wait on it. The client is returned rather than mutated in place because
/// `IAudioClient::Initialize` may be called only once on one: every retry —
/// another format, or the fall back to polling — activates a fresh client.
fn initialise(
    client: IAudioClient,
    candidates: &[AudioFormat],
    target: u32,
    scope: TreeScope,
) -> Result<(IAudioClient, AudioFormat, Wake), AudioError> {
    let mut client = client;
    let mut refused = Vec::new();

    for (attempt, format) in candidates.iter().enumerate() {
        if attempt > 0 {
            // A client whose `Initialize` failed is spent, whatever it failed
            // for.
            // The same side of the tree the first attempt asked for: a
            // retry that quietly swapped to the other one would record the
            // opposite of what the caller asked for.
            client = activate_process_loopback(target, scope.mode())?;
        }

        match initialise_with(&client, *format) {
            Ok(wake) => {
                tracing::debug!(
                    audio_source = %scope.kind().audio_source(),
                    format = %format,
                    wake = ?wake,
                    // Which side of the tree, because a recording with both
                    // will open two of these and the log has to say which is
                    // which (issue #27).
                    scope = scope.log_value(),
                    "the audio engine accepted this shape for a process-scoped capture"
                );
                return Ok((client, *format, wake));
            }
            Err(error) if error.code() == AUDCLNT_E_UNSUPPORTED_FORMAT => {
                refused.push(format.to_string());
            }
            Err(error) => {
                return Err(platform_error(
                    "opening a process-scoped audio capture stream",
                    error,
                ))
            }
        }
    }

    Err(AudioError::unsupported_format(format!(
        "the Windows audio engine would not capture the game's audio as {}",
        refused.join(" or ")
    )))
}

/// One attempt at initialising a client with one format.
///
/// Event-driven first, and polling if the audio engine refuses that, exactly as
/// an endpoint stream does (`endpoint_capture.rs`): a recording with the audio
/// read on a timer is better than one without the audio. The second attempt
/// needs a second client, which is the caller's business, so a refusal comes
/// back as the error it was.
fn initialise_with(client: &IAudioClient, format: AudioFormat) -> windows::core::Result<Wake> {
    let wave = wave_format(format);
    // SAFETY: `WAVEFORMATEXTENSIBLE` starts with the `WAVEFORMATEX` every
    // format begins with, at offset zero, so this is the pointer Windows
    // expects. It is taken from the whole structure rather than from the field,
    // because both are `#[repr(packed)]` and a reference to a field of a packed
    // structure is undefined behaviour.
    let raw = (&raw const wave).cast::<WAVEFORMATEX>();

    // SAFETY: `client` is a live, uninitialised `IAudioClient` from
    // `activate_process_loopback`, `raw` points at a live format that outlives
    // the call, and the flags are the ones Microsoft documents as required for
    // process loopback: shared mode, loopback, and a buffer duration.
    let event_driven = unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            SourceKind::GameAudio.stream_flags() | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_DURATION,
            0,
            raw,
            None,
        )
    };

    event_driven.and_then(|()| create_wake_event(client))
}

/// The `WAVEFORMATEXTENSIBLE` describing `format` as 32-bit float.
///
/// Extensible rather than a plain `WAVEFORMATEX` because it is the only form
/// that states which speaker each channel is, and a surround game whose
/// channels were unlabelled would be muxed by guesswork.
fn wave_format(format: AudioFormat) -> WAVEFORMATEXTENSIBLE {
    let channels = format.channels().get();
    let bits = u16::try_from(SampleFormat::Float32.bytes_per_sample() * 8)
        .expect("a sample is far narrower than u16::MAX bits");
    let block = channels * bits / 8;
    let rate = format.sample_rate().get();

    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: u16::try_from(size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>())
                .expect("the extensible tail is 22 bytes"),
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: bits,
        },
        dwChannelMask: mask_for(format),
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// The speaker positions to ask for.
///
/// An endpoint whose mix format had no extensible part says nothing about
/// speaker positions, and asking the audio engine for a stereo stream with no
/// positions at all is asking it to guess. Stereo is filled in because it is
/// the only layout that has one obvious answer; anything else is passed through
/// as it was, including zero.
fn mask_for(format: AudioFormat) -> u32 {
    match (format.channel_mask().bits(), format.channels().get()) {
        (0, FALLBACK_CHANNELS) => STEREO_MASK,
        (mask, _) => mask,
    }
}

/// A capture of everything one process tree plays.
///
/// See `endpoint_capture.rs` for the threading and ownership rules — a capture
/// is read on a thread the caller supplies, and reading is the only thing that
/// changes it — and `docs/audio-routing.md` for how the tree is resolved and
/// what happens when the game's processes come and go.
///
/// # Example
///
/// ```no_run
/// use core::time::Duration;
///
/// use clipped_audio::windows::ProcessLoopbackCapture;
/// use clipped_audio::Capture;
///
/// # let game_process = 0_u32;
/// let mut capture = ProcessLoopbackCapture::open(game_process)?;
/// while capture.target_is_running() {
///     match capture.read(Duration::from_millis(100))? {
///         Capture::Samples(audio) => { /* audio.samples() is the game, and only the game */ }
///         Capture::Idle | Capture::FormatChanged(_) => {}
///     }
/// }
///
/// // Hand over what the audio engine still holds rather than losing the last
/// // fraction of a second of the recording.
/// capture.finish();
/// while let Ok(Capture::Samples(audio)) = capture.read(Duration::from_millis(100)) {
///     let _ = audio;
/// }
/// # Ok::<(), clipped_audio::AudioError>(())
/// ```
#[derive(Debug)]
pub struct ProcessLoopbackCapture {
    capture: EndpointCapture,
    root: u32,
    /// The cell this capture agrees its scope through, held here as well as in
    /// the [`ProcessLoopbackSource`] inside `capture` so that a caller can ask
    /// **which** agreement it is without the engine lending out its source.
    ///
    /// The same `Arc`, not a second one: [`Self::open_scoped`] clones the one it
    /// hands to the source. Its identity is the whole of what
    /// [`Self::scope_agreement`] reports, and the value in it belongs to
    /// `decide_scope` on the capture thread.
    scoping: Arc<AtomicU32>,
}

/// Which agreement a process-scoped capture follows.
///
/// The two sides of one [`ProcessLoopbackCapture::open_pair`] return the same
/// one; captures opened separately do not, however alike they look otherwise.
/// That is the whole of what it says, and it is what lets a caller assert it
/// opened the pair rather than two captures that happen to name the same
/// process ([issue #581](https://github.com/wildware-uk/clipped/issues/581)).
///
/// It deliberately does not expose the process the pair is scoped to: that is
/// [`ProcessLoopbackCapture::scoped_to`], it changes on the capture thread, and
/// a second route to it would be a second answer to the same question
/// (AGENTS.md section 55).
#[derive(Clone)]
pub struct ScopeAgreement(Arc<AtomicU32>);

impl PartialEq for ScopeAgreement {
    /// Whether these are the same agreement, not whether they say the same
    /// thing.
    ///
    /// Two captures opened separately hold two cells, and a moment after either
    /// is opened both cells say the same process — which is exactly the
    /// arrangement this type exists to tell apart. So it is the identity of the
    /// cell that is compared and never its contents.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ScopeAgreement {}

impl core::fmt::Debug for ScopeAgreement {
    /// The cell's address, because that is what equality is over. Printing only
    /// the process it names would make two distinct agreements look identical
    /// in the message of the assertion that just told them apart.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ScopeAgreement")
            .field("cell", &Arc::as_ptr(&self.0))
            .field("scoped_to", &self.0.load(Ordering::Relaxed))
            .finish()
    }
}

impl ProcessLoopbackCapture {
    /// Opens a capture of everything `root_process` and the processes it
    /// started are playing.
    ///
    /// `root_process` should be the game itself rather than the launcher that
    /// started it: a tree rooted at Steam would put its notification chime into
    /// the track named after the game (`docs/audio-routing.md`).
    ///
    /// # Errors
    ///
    /// [`AudioError::ProcessLoopbackUnavailable`] when this machine cannot
    /// capture a process tree at all, which is expected below Windows build
    /// 20348. The documented answer is to record one system-audio track with
    /// [`SystemAudioCapture`](super::SystemAudioCapture) and to say that
    /// per-source separation is unavailable, rather than to label everything
    /// the machine plays "Game" (ADR 0003).
    ///
    /// [`AudioError::ProcessUnavailable`] when the process cannot be followed —
    /// it has exited, or it runs at a higher integrity level than Clipped —
    /// [`AudioError::UnsupportedFormat`] when the audio engine refuses every
    /// shape this crate asks for, and [`AudioError::Platform`] when Windows
    /// refuses something outright.
    pub fn open(root_process: u32) -> Result<Self, AudioError> {
        Self::open_scoped(
            root_process,
            TreeScope::Only,
            Self::own_scoping(root_process),
        )
    }

    /// Everything the machine played **except** `root_process` and its tree.
    ///
    /// The other half of the model, and the one that makes a `Game` track
    /// possible: with only the including side, a recording that also captures
    /// the whole endpoint has the game's audio on two tracks
    /// ([issue #27](https://github.com/wildware-uk/clipped/issues/27)).
    ///
    /// **A recording should open [`open_pair`](Self::open_pair) rather than
    /// this and [`open`](Self::open) separately.** Two captures opened here can
    /// re-scope onto different processes of the same game, which puts that
    /// game's audio back onto two tracks by another route; `open_pair` is the
    /// same two captures with one agreement between them. This is for a caller
    /// that wants the excluding side alone.
    ///
    /// It outlives the tree it excludes. Once every process of the game has
    /// gone the set being excluded is empty, and this capture carries
    /// everything the machine plays — including across a reopen, which is the
    /// case that used to give the track silence for the rest of the recording
    /// (`opens_against_an_empty_tree`, issue #563). A tree that is *already*
    /// empty when this is called is still an error, because there is then no
    /// recording in progress to protect and the caller has a better answer:
    /// `SystemAudioCapture` and a note that per-source separation was
    /// unavailable (`docs/audio-routing.md`).
    ///
    /// # Errors
    ///
    /// Exactly [`open`](Self::open)'s: this is the same activation with one
    /// constant changed, so a machine that cannot do one cannot do the other.
    pub fn open_excluding(root_process: u32) -> Result<Self, AudioError> {
        Self::open_scoped(
            root_process,
            TreeScope::Except,
            Self::own_scoping(root_process),
        )
    }

    /// Both sides of one tree, opened together and kept scoped to one process.
    ///
    /// **This is what a recording opens** — `clipped_session::audio::open`,
    /// which is the only caller in the workspace that opens both sides, and
    /// which for a week opened them separately while this documentation said
    /// otherwise ([issue
    /// #581](https://github.com/wildware-uk/clipped/issues/581)).
    /// `open` and `open_excluding` on their
    /// own are two independent captures, and a recording that opens both of
    /// them separately has a defect that only appears when a game's launcher
    /// exits partway through: each capture then resolves the surviving tree
    /// from its **own** `ProcessTree`, on its own schedule, and there is
    /// nothing to stop them landing on different processes. A process in one
    /// side's tree and not the other's has its audio on both tracks — the
    /// doubling `open_excluding` exists to prevent — or on neither.
    ///
    /// The pair shares one cell naming the process both are scoped to, so that
    /// a re-scope by either side moves the other; see `decide_scope` for the
    /// rule and why it terminates. The two captures are otherwise independent
    /// and are meant to be read on a thread each, which is what a session does
    /// with them.
    ///
    /// Returned in track order: everything the tree played, then everything
    /// else.
    ///
    /// # Errors
    ///
    /// [`open`](Self::open)'s, for either side. A machine that cannot activate
    /// one side cannot activate the other, so a failure here means neither
    /// capture exists and the caller takes the documented fallback — one
    /// system-audio track, with the separation stated as unavailable (ADR
    /// 0003).
    pub fn open_pair(root_process: u32) -> Result<(Self, Self), AudioError> {
        let scoping = Self::own_scoping(root_process);
        let only = Self::open_scoped(root_process, TreeScope::Only, Arc::clone(&scoping))?;
        // A failure here drops `only`, which releases its client: both sides or
        // neither, at the point they are opened as well as in what they record.
        let except = Self::open_scoped(root_process, TreeScope::Except, scoping)?;
        Ok((only, except))
    }

    /// The agreement cell of a capture that has nobody to agree with.
    ///
    /// A lone capture always reads back what it wrote, so it behaves exactly as
    /// it did before pairs existed.
    fn own_scoping(root_process: u32) -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(root_process))
    }

    /// [`open`](Self::open) and [`open_excluding`](Self::open_excluding), which
    /// differ only in which side of the tree they take.
    fn open_scoped(
        root_process: u32,
        scope: TreeScope,
        scoping: Arc<AtomicU32>,
    ) -> Result<Self, AudioError> {
        let source = ProcessLoopbackSource::new(root_process, scope, Arc::clone(&scoping))?;
        let capture = EndpointCapture::open(CaptureSource::ProcessTree(source))?
            // The tree was built a moment ago and had a member in it, so there
            // is only one way to be here: every process of the game exited
            // between the two calls.
            .ok_or(AudioError::ProcessUnavailable {
                process_id: root_process,
            })?;

        Ok(Self {
            capture,
            root: root_process,
            scoping,
        })
    }

    /// Which agreement this capture follows.
    ///
    /// Equal for the two sides of one [`open_pair`](Self::open_pair) and for
    /// nothing else, so it is how a caller checks it opened the pair rather
    /// than two captures that merely name the same process — a distinction
    /// nothing can make from the outside otherwise, because two captures opened
    /// separately are identical in every other observable way until a game's
    /// launcher exits partway through a recording and they disagree about which
    /// process to follow ([issue
    /// #581](https://github.com/wildware-uk/clipped/issues/581)).
    ///
    /// Reading it is free and takes no lock; it is [`Clone`] so that a caller
    /// holding one capture at a time can compare the two.
    #[must_use]
    pub fn scope_agreement(&self) -> ScopeAgreement {
        ScopeAgreement(Arc::clone(&self.scoping))
    }

    /// The shape of every buffer this capture produces.
    ///
    /// Chosen when the capture is opened — see the module documentation — and
    /// fixed for its life, including across a re-scoping onto another process
    /// of the same game.
    #[must_use]
    pub fn format(&self) -> AudioFormat {
        self.capture.format()
    }

    /// The process this capture was opened for.
    #[must_use]
    pub fn root_process(&self) -> u32 {
        self.root
    }

    /// The process the current activation is scoped to.
    ///
    /// [`Self::root_process`] unless that process has exited with descendants
    /// still running, in which case it is the surviving member the capture
    /// re-scoped onto.
    #[must_use]
    pub fn scoped_to(&self) -> u32 {
        self.capture
            .tree_state()
            .map_or(self.root, |state| state.scoped_to)
    }

    /// Whether any process of the game is still running.
    ///
    /// `false` once the game and everything it started have exited, which is
    /// how a caller knows there is nothing left to capture. The capture does not
    /// stop itself: the track keeps its place on the timeline until the caller
    /// stops it, because a recording is worth more than the audio it is
    /// missing.
    ///
    /// It says nothing about whether *this* capture still has audio to give.
    /// An including capture's track is silence from here; an excluding one's is
    /// everything the machine plays, because the set it excludes is now empty
    /// (`opens_against_an_empty_tree`, issue #563).
    #[must_use]
    pub fn target_is_running(&self) -> bool {
        self.capture
            .tree_state()
            .is_some_and(|state| state.members > 0)
    }

    /// What this capture has produced so far.
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.capture.stats()
    }

    /// Reads the next block of the game's audio, waiting up to `timeout` for
    /// one.
    ///
    /// Consecutive buffers are exactly contiguous, whatever the game's
    /// processes did in between: periods it played nothing come back as
    /// [`SampleOrigin::SynthesisedSilence`](crate::SampleOrigin::SynthesisedSilence)
    /// of the right length rather than as nothing at all.
    ///
    /// # Errors
    ///
    /// [`AudioError::NotOpen`] after [`close`](Self::close), and after a drain
    /// started by [`finish`](Self::finish) has handed over everything it had.
    /// Failures of the capture itself are not errors: they are handled, logged,
    /// and reported through [`Capture`].
    pub fn read(&mut self, timeout: Duration) -> Result<Capture<'_>, AudioError> {
        self.capture.read(timeout)
    }

    /// Ends the capture by handing over what the audio engine still holds.
    ///
    /// The audio engine keeps up to 200 ms of captured audio for this stream. A
    /// capture that is simply closed loses it, which is the last fraction of a
    /// second before the user stopped recording — often the part they pressed
    /// the key for. After this, [`read`](Self::read) returns the packets that
    /// were queued and then reports [`AudioError::NotOpen`]; nothing is
    /// reopened and no further silence is synthesised.
    ///
    /// **This does not close anything by itself**, and the reading is not
    /// optional. A caller that calls this and then [`close`](Self::close)
    /// without reading in between has thrown the audio away exactly as a bare
    /// close would — which is what `clipped-session` did for as long as this
    /// method existed, so every track of every recording lost its tail while
    /// the one capture that could drain looked as though it did
    /// ([issue #320](https://github.com/wildware-uk/clipped/issues/320)).
    ///
    /// It never waits for the client: a drain reads what is queued and stops,
    /// so a game that has exited ends it on the first look.
    pub fn finish(&mut self) {
        self.capture.begin_drain();
    }

    /// Stops capturing and releases the client, discarding anything not yet
    /// collected.
    ///
    /// [`finish`](Self::finish) is the ordinary way to end a recording; this is
    /// for a caller that wants the capture gone now. Idempotent, and does the
    /// same thing as dropping the capture.
    pub fn close(&mut self) {
        self.capture.close();
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicBool;
    use std::io::Write as _;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::time::Instant;

    use super::*;
    use crate::buffer::SampleOrigin;
    use crate::windows::endpoint_capture::testing::{skipped, suppressed, Contiguity};
    use crate::windows::notifications::EndpointChange;

    /// How long a test waits for Windows to start or end a process.
    ///
    /// Generous on purpose: it bounds a hang rather than asserting a latency.
    const PATIENCE: Duration = Duration::from_secs(20);

    /// How far a track's length may sit from the time that passed while it was
    /// read.
    ///
    /// A track is as long as the *device* says: what a read hands over is what
    /// the audio engine had captured by the moment it was asked for it, and the
    /// engine holds up to [`BUFFER_DURATION`] that nobody has asked for yet. So
    /// a length measured across an interval is the time that interval took, give
    /// or take how much the engine happened to be holding at each end of it.
    ///
    /// Every length below is asserted against a *measured* elapsed time plus
    /// this, and never against the duration a `sleep` or a read loop was asked
    /// for. `std::thread::sleep` is a floor rather than a duration, and a read
    /// loop leaves when its deadline has passed rather than when it arrives; a
    /// thread descheduled on a shared runner comes back long after either, and
    /// the audio engine goes on capturing the whole time it is away. A bound
    /// written against the nominal duration therefore fails on a busy machine
    /// with nothing having regressed — which
    /// [issue #387](https://github.com/wildware-uk/clipped/issues/387) is about,
    /// and which `stopping_a_capture_hands_over_the_audio_the_engine_was_still_holding`
    /// did on a commit that changed an icon (AGENTS.md section 25).
    ///
    /// Two other tests in this module failed in that same run and are **not**
    /// this: `a_process_tree_that_plays_nothing_still_produces_a_track_of_the_right_length`
    /// failed its *lower* bound with 0.1 s of track in 1.2 s of reading
    /// ([issue #425](https://github.com/wildware-uk/clipped/issues/425)), and
    /// `a_game_that_re_executes_itself_is_followed_onto_the_process_that_survived`
    /// failed on one nanosecond of contiguity in a helper this does not touch
    /// ([issue #424](https://github.com/wildware-uk/clipped/issues/424)). Three
    /// tests failing together is not three tests failing for one reason, and
    /// reading it that way would have closed #387 over two live defects.
    const ENGINE_BACKLOG: Duration = Duration::from_nanos(BUFFER_DURATION as u64 * 100);

    /// How long the consumer stops reading for in the drain test.
    ///
    /// Long enough that what the drain has to produce is still bounded from
    /// below once [`ENGINE_BACKLOG`] is allowed for either side of it. The 150 ms
    /// this stalled for previously is shorter than the engine's own buffer, so
    /// the only honest lower bound on it would have been zero.
    const STALL: Duration = Duration::from_millis(500);

    /// What a stretch of reading produced, and how long it really took.
    #[derive(Debug)]
    struct Reading {
        /// Every buffer that arrived, checked contiguous with the one before it
        /// as it did.
        timeline: Contiguity,
        /// Of those frames, the ones the client delivered — as opposed to the
        /// ones this crate invented to cover a period the client said nothing
        /// about.
        from_the_client: u64,
        /// The wall-clock time this really occupied. See [`ENGINE_BACKLOG`] for
        /// why it is measured rather than assumed.
        elapsed: Duration,
    }

    impl Reading {
        /// Asserts that the audio is as long as the time it was read over.
        ///
        /// The property every downstream stage depends on: a second of
        /// recording is a second of audio, contiguous, whether or not the tree
        /// played anything in it. A capture that only produced samples while the
        /// game made a noise would slide against the video by exactly the amount
        /// of quiet in the recording.
        fn assert_as_long_as_it_took(&self, what: &str) {
            let took = self.elapsed.as_secs_f64();
            let slack = ENGINE_BACKLOG.as_secs_f64();
            let seconds = self.timeline.seconds();
            assert!(
                (took - slack..=took + slack).contains(&seconds),
                "{what}: {took:.3} s passed, so there should be about {took:.3} s of audio, \
                 give or take the {slack:.3} s the audio engine holds; got {seconds:.3} s"
            );
        }

        /// Asserts that the audio covers *at least* the time it was read over.
        ///
        /// The one-sided form, for a drain that follows a deliberate stall.
        /// What would mean audio was lost there is a drain **shorter** than the
        /// period the reader was away for. A longer one means the engine was
        /// holding more than [`ENGINE_BACKLOG`] describes, which is what a
        /// machine under load does to it and not a defect: CI has produced
        /// 0.793 s of drain against a 0.500 s stall, so an engine holding
        /// 0.293 s where 0.200 s was allowed for (#387).
        ///
        /// Deliberately not the wider tolerance that would also silence the
        /// two-sided assertion everywhere else. A stall is the one place a long
        /// drain is the expected outcome, because the reader was made slow on
        /// purpose; everywhere else both directions are still suspicious.
        fn assert_covered_at_least_as_long_as_it_took(&self, what: &str) {
            let took = self.elapsed.as_secs_f64();
            let slack = ENGINE_BACKLOG.as_secs_f64();
            let seconds = self.timeline.seconds();
            assert!(
                seconds >= took - slack,
                "{what}: {took:.3} s passed, so the drain has to cover at least that much, \
                 give or take the {slack:.3} s the audio engine holds; got {seconds:.3} s, \
                 which is a drain that lost some of the period the reader was away for"
            );
        }
    }

    /// Opens a capture of a process tree, or reports why this machine cannot.
    ///
    /// **The tests that open through here make no sound.** They capture a
    /// process tree that renders nothing — this test process, or a `cmd.exe`
    /// the test started — so what they read is silence, and they are exempt
    /// from the "does this machine want quiet" check every test that plays a
    /// tone begins with. One test in this module is *not* exempt and does not
    /// come through here:
    /// `a_track_of_everything_but_the_game_is_still_everything_once_the_game_has_gone`
    /// has to play something to have anything to measure, so it asks
    /// [`suppressed`] first. What they
    /// do need is a machine whose Windows can scope a capture to a process at
    /// all; where it cannot, they skip loudly rather than failing.
    ///
    /// **They are not a local-only suite.** This comment used to say a GitHub
    /// runner cannot scope a capture and that they skip there, which is false
    /// and cost somebody an afternoon
    /// ([issue #441](https://github.com/wildware-uk/clipped/issues/441)): the
    /// CI failures behind
    /// [#341](https://github.com/wildware-uk/clipped/issues/341),
    /// [#387](https://github.com/wildware-uk/clipped/issues/387) and
    /// [#425](https://github.com/wildware-uk/clipped/issues/425) all quote
    /// measured track lengths, which a skipped test cannot produce. Whether a
    /// given runner can scope a capture is a property of that machine, and the
    /// skip below is what finds out — it is not a statement about where these
    /// run.
    fn open(root: u32) -> Option<ProcessLoopbackCapture> {
        match ProcessLoopbackCapture::open(root) {
            Ok(capture) => Some(capture),
            Err(error @ AudioError::ProcessLoopbackUnavailable { .. }) => {
                skipped(&format!("{error}"));
                None
            }
            Err(error) => {
                skipped(&format!(
                    "a process-scoped capture could not be opened here: {error}"
                ));
                None
            }
        }
    }

    /// Reads for at least `duration`, returning what arrived and how long it
    /// took.
    ///
    /// Answers the frames handed over — asserting on the way that every buffer
    /// was exactly contiguous with the one before it, which is the property
    /// everything downstream depends on — how many of those frames the client
    /// produced rather than this crate inventing them, and the elapsed time the
    /// length is judged against.
    ///
    /// The elapsed time is not `duration`. This leaves when the deadline has
    /// *passed*, which is one read later at best and however long the thread was
    /// descheduled for at worst (see [`ENGINE_BACKLOG`]).
    fn read_for(capture: &mut ProcessLoopbackCapture, duration: Duration) -> Reading {
        let mut timeline = Contiguity::new(capture.format());
        let mut from_the_client = 0u64;
        let started = Instant::now();
        let until = started + duration;

        // Reading stops on the clock; **collecting does not**.
        //
        // A read hands over at most one silence instalment -- 100 ms, so that
        // the memory this crate uses stays fixed however long a consumer stalls
        // (`crate::timeline`'s `SILENCE_CHUNK`). A reader that is descheduled
        // therefore does not lose the audio for the period it was away: it is
        // *owed*, and arrives on the reads that follow.
        //
        // Stopping on the clock alone measured how often this thread was
        // scheduled rather than what the capture produced. CI failed with
        // 0.100 s of track against 1.200 s of reading -- exactly one instalment,
        // which is a loop that went round once
        // ([issue #425](https://github.com/wildware-uk/clipped/issues/425)).
        // Draining what is still owed is what makes the assertions below
        // statements about the capture rather than about the machine.
        let collect = |capture: &mut ProcessLoopbackCapture,
                       timeline: &mut Contiguity,
                       from_the_client: &mut u64| {
            match capture
                .read(Duration::from_millis(100))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        *from_the_client += samples.frames() as u64;
                    }
                    timeline.accept(&samples);
                }
                Capture::Idle | Capture::FormatChanged(_) => {}
            }
        };

        while Instant::now() < until {
            collect(capture, &mut timeline, &mut from_the_client);
        }
        let elapsed = started.elapsed();

        // Bounded, so that a capture which genuinely stopped producing fails
        // the assertion rather than hanging here. Covering the window needs
        // about one read per instalment, and ten times that leaves room for
        // every one of them to have been a wasted trip.
        let attempts = (elapsed.as_millis() / 100 + 1) * 10;
        for _ in 0..attempts {
            if timeline.seconds() >= elapsed.as_secs_f64() {
                break;
            }
            collect(capture, &mut timeline, &mut from_the_client);
        }

        Reading {
            timeline,
            from_the_client,
            elapsed,
        }
    }

    /// A process that does nothing until its standard input closes, and the
    /// descendants it starts when told to.
    ///
    /// The same fixture `crates/windows/tests/process_tree.rs` uses, and for
    /// the same reasons: the descendants appear only when the test says so, and
    /// killing the first one leaves the others running with a parent identifier
    /// that names nothing — which is a game that re-executed itself and let go
    /// of the process it was launched as.
    struct Chain {
        root: Child,
        input: Option<ChildStdin>,
    }

    impl Chain {
        fn start() -> Self {
            let mut root = Command::new("cmd.exe")
                // `set /p` reads one line from standard input; what follows the
                // `&` runs once it has one, and `more` then holds that input
                // open until it is closed.
                .args(["/c", "set /p go= & cmd.exe /c more"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("cmd.exe is on every Windows installation");
            let input = root.stdin.take().expect("standard input was piped");
            Self {
                root,
                input: Some(input),
            }
        }

        fn root_pid(&self) -> u32 {
            self.root.id()
        }

        /// Makes the root start the two processes below it.
        fn start_descendants(&mut self) {
            let input = self.input.as_mut().expect("the chain is still open");
            input
                .write_all(b"go\r\n")
                .expect("the root is still reading");
            input.flush().expect("the root is still reading");
        }

        /// Ends the root, leaving its descendants running and orphaned.
        fn kill_root(&mut self) {
            self.root.kill().expect("the root is this test's own child");
            self.root.wait().expect("the root is this test's own child");
        }
    }

    impl Drop for Chain {
        fn drop(&mut self) {
            // Both halves, in this order. Killing the root ends only the root —
            // Windows does not end a process's children with it — and closing
            // the input is what ends the two below it. A test that failed early
            // must not leave processes behind on a machine that is shared.
            let _ = self.root.kill();
            let _ = self.root.wait();
            self.input = None;
        }
    }

    /// Waits until `condition` holds, reading the capture meanwhile.
    ///
    /// Reading is what makes the capture notice anything: the process tree is
    /// refreshed between reads, on the reading thread, and never behind one.
    fn read_until(
        capture: &mut ProcessLoopbackCapture,
        condition: impl Fn(&ProcessLoopbackCapture) -> bool,
    ) -> bool {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if condition(capture) {
                return true;
            }
            let _ = capture.read(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_process_tree_that_plays_nothing_still_produces_a_track_of_the_right_length() {
        // The property every downstream stage depends on, against the real
        // activation rather than against the timeline in isolation: a second of
        // reading is a second of audio, contiguous, whether or not the tree
        // played anything. A capture that only produced samples while the game
        // made a noise would slide against the video by exactly the amount of
        // quiet in the recording.
        let Some(mut capture) = open(std::process::id()) else {
            return;
        };

        let reading = read_for(&mut capture, Duration::from_millis(1_200));
        reading.assert_as_long_as_it_took("reading a tree that plays nothing");

        // And the length has to come from the client rather than from silence
        // invented to cover a client that produced nothing, or this would pass
        // just as well on a capture that never delivered a packet.
        //
        // That is a claim about the platform, and it is the one measured
        // difference between this and endpoint loopback: an endpoint delivers
        // nothing at all while it is quiet, whereas a process-scoped client on
        // Windows 11 build 26200 delivers silent packets continuously — 48,000
        // frames a second from a tree playing nothing, with no silence
        // synthesised at all (`docs/audio-routing.md`). A machine where that
        // stopped holding would fail here rather than quietly changing what
        // this crate is built on.
        assert!(
            reading.from_the_client > 0,
            "every one of the {} frames was silence this crate invented, and the client              delivered nothing",
            reading.timeline.frames
        );
        assert!(
            capture.target_is_running(),
            "this test process is a member of its own process tree"
        );
        assert_eq!(capture.scoped_to(), std::process::id());
    }

    #[test]
    fn stopping_a_capture_hands_over_the_audio_the_engine_was_still_holding() {
        // Issue #26's third scope item. The audio engine holds captured audio
        // nobody has asked for yet; a capture that is simply closed throws it
        // away, which is the last fraction of a second before somebody stopped
        // recording.
        //
        // The consumer stops reading for long enough to leave a real backlog,
        // and the drain then has to produce it — after the client has been
        // stopped, which is the part that makes this a drain rather than a
        // longer read.
        let Some(mut capture) = open(std::process::id()) else {
            return;
        };
        let format = capture.format();

        read_for(&mut capture, Duration::from_millis(300));

        // Measured, not assumed: `sleep` guarantees only that it does not
        // return early, and the engine goes on capturing for however long this
        // thread is really away (see `ENGINE_BACKLOG`).
        let stall_began = Instant::now();
        std::thread::sleep(STALL);
        let stalled = stall_began.elapsed();

        let before = capture.stats().frames;
        capture.finish();

        let mut timeline = Contiguity::new(format);
        let mut from_the_client = 0u64;
        loop {
            match capture.read(Duration::from_millis(100)) {
                Ok(Capture::Samples(samples)) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        from_the_client += samples.frames() as u64;
                    }
                    timeline.accept(&samples);
                }
                Ok(Capture::Idle | Capture::FormatChanged(_)) => break,
                // The drain has handed over everything and closed itself, which
                // is how a caller knows there is no more.
                Err(AudioError::NotOpen) => break,
                Err(error) => panic!("a drain does not fail: {error}"),
            }
        }
        let drain = Reading {
            timeline,
            from_the_client,
            elapsed: stalled,
        };

        assert!(
            drain.timeline.frames > 0,
            "a {stalled:.3?} stall leaves audio in the engine, and stopping the capture must \
             hand it over rather than lose it"
        );
        // Not just *a* length: the audio the engine was holding. The rest of
        // what a drain hands over is silence covering the gap between where the
        // recording had got to and where the surviving packets sit, and a drain
        // that had lost the packets and kept only the gap would still be the
        // right length — measured on Windows 11 build 26200, a stall of any
        // length from 50 ms to 1.5 s leaves the same 30 ms of real audio in the
        // engine and the rest of the drain is that silence.
        assert!(
            drain.from_the_client > 0,
            "every one of the {} frames the drain handed over was silence this crate invented; \
             what a drain is for is the audio the engine had captured and not been asked for",
            drain.timeline.frames
        );
        // And the recording does not lose the period the reader was away for:
        // the drain covers it, however long it really was — which is the whole
        // property here, so it is asserted in that direction only. See
        // `assert_covered_at_least_as_long_as_it_took`.
        drain.assert_covered_at_least_as_long_as_it_took("draining after a stall");
        assert_eq!(
            capture.stats().frames - before,
            drain.timeline.frames,
            "everything the drain handed over is on the same timeline as the recording"
        );
        assert!(
            matches!(
                capture.read(Duration::from_millis(10)),
                Err(AudioError::NotOpen)
            ),
            "a capture that has finished draining is closed"
        );
    }

    #[test]
    fn the_game_exiting_is_noticed_and_does_not_end_the_recording() {
        // Issue #26's second scope item, and AGENTS.md section 17: a recording
        // is worth more than the audio it is missing. When the last of the
        // game's processes goes, the caller is told — that is how a session
        // knows to stop — and the track carries on as silence of exactly the
        // right length rather than stopping mid-recording or failing a read.
        let mut chain = Chain::start();
        let Some(mut capture) = open(chain.root_pid()) else {
            return;
        };

        assert!(
            capture.target_is_running(),
            "the process the capture was opened for is running"
        );
        read_for(&mut capture, Duration::from_millis(200));

        chain.kill_root();
        drop(chain);

        assert!(
            read_until(&mut capture, |capture| !capture.target_is_running()),
            "the capture has to notice that every process of the game has exited"
        );

        let after = read_for(&mut capture, Duration::from_millis(600));
        after.assert_as_long_as_it_took(
            "the track has to keep its place on the timeline after the game exits",
        );
    }

    #[test]
    fn a_game_that_re_executes_itself_is_followed_onto_the_process_that_survived() {
        // The case a single-root API cannot express. Windows scopes a capture
        // to one process and its children; some titles start, spawn the process
        // that really is the game, and exit. Recording silence for the rest of
        // the session because the process named in the activation is gone would
        // be a bug nobody notices until they open the file.
        let mut chain = Chain::start();
        let root = chain.root_pid();
        let Some(mut capture) = open(root) else {
            return;
        };

        chain.start_descendants();
        // The tree reads the process table once a second, so the descendants
        // are members within an interval of appearing.
        assert!(
            read_until(&mut capture, |capture| capture.target_is_running()),
            "the chain is running"
        );
        read_for(&mut capture, Duration::from_millis(1_100));

        chain.kill_root();

        assert!(
            read_until(&mut capture, |capture| capture.scoped_to() != root),
            "the capture has to re-scope onto a process of the game that is still running"
        );
        assert!(
            capture.target_is_running(),
            "the game is still running: only the process it was launched as has gone"
        );
        assert_eq!(
            capture.root_process(),
            root,
            "the track is still the track of the game it was opened for"
        );
        assert!(
            capture.stats().endpoint_changes > 0,
            "re-scoping means activating a new client, which is a change worth counting"
        );

        // And the recording carries on across it, contiguously.
        let after = read_for(&mut capture, Duration::from_millis(600));
        after.assert_as_long_as_it_took("reading after a re-scoping");
        assert!(
            after.from_the_client > 0,
            "the client activated on the surviving process has to be delivering packets, and              everything after the re-scoping was invented silence"
        );
    }

    fn stereo(rate: u32, mask: u32) -> AudioFormat {
        AudioFormat::new(
            NonZeroU32::new(rate).expect("a rate is not zero"),
            NonZeroU16::new(2).expect("stereo is not zero channels"),
            ChannelMask::from_bits(mask),
            SampleFormat::Float32,
        )
    }

    #[test]
    fn the_format_asked_for_is_the_one_the_wave_format_describes() {
        // The structure Windows is handed *is* the request: a mismatch between
        // it and the `AudioFormat` this crate then converts by would be read as
        // noise rather than as audio, because every packet would be
        // deinterleaved with the wrong stride.
        let format = AudioFormat::new(
            NonZeroU32::new(44_100).expect("a rate is not zero"),
            NonZeroU16::new(6).expect("5.1 is not zero channels"),
            ChannelMask::from_bits(0x3f),
            SampleFormat::Float32,
        );
        let wave = wave_format(format);

        assert_eq!({ wave.Format.nSamplesPerSec }, 44_100);
        assert_eq!({ wave.Format.nChannels }, 6);
        assert_eq!({ wave.Format.wBitsPerSample }, 32);
        assert_eq!({ wave.Format.nBlockAlign }, 24, "six 32-bit samples");
        assert_eq!({ wave.Format.nAvgBytesPerSec }, 44_100 * 24);
        assert_eq!({ wave.dwChannelMask }, 0x3f);
        assert_eq!({ wave.SubFormat }, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
        assert_eq!(
            { wave.Format.cbSize },
            22,
            "the tail Windows reads a subformat and a channel mask out of"
        );
    }

    #[test]
    fn a_stereo_endpoint_that_named_no_speakers_is_asked_for_the_obvious_pair() {
        // An endpoint with no extensible part reports a mask of zero, and
        // passing that on asks the audio engine to guess which speaker each
        // channel is.
        assert_eq!(mask_for(stereo(48_000, 0)), STEREO_MASK);
        assert_eq!(
            mask_for(stereo(48_000, 0x603)),
            0x603,
            "a mask the endpoint did state is passed through untouched"
        );

        let five_one = AudioFormat::new(
            NonZeroU32::new(48_000).expect("a rate is not zero"),
            NonZeroU16::new(6).expect("5.1 is not zero channels"),
            ChannelMask::from_bits(0),
            SampleFormat::Float32,
        );
        assert_eq!(
            mask_for(five_one),
            0,
            "there is no obvious layout for six unlabelled channels, so none is invented"
        );
    }

    #[test]
    fn a_capture_cannot_be_scoped_to_a_process_that_cannot_be_followed() {
        // 0 is the system idle process: the one identifier that cannot be
        // opened on any machine, which makes it the only deterministic negative
        // case available (AGENTS.md section 25). It needs no audio hardware, so
        // it runs in the pull-request CI job.
        let error = ProcessLoopbackSource::new(0, TreeScope::Only, Arc::new(AtomicU32::new(0)))
            .expect_err("the idle process cannot be followed");

        assert!(
            matches!(error, AudioError::ProcessUnavailable { process_id: 0 }),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("no longer running"),
            "the message has to say what is wrong in words a user can act on: {error}"
        );
    }

    #[test]
    fn the_unavailable_error_says_what_the_machine_needs_and_what_happens_instead() {
        // ADR 0003's second consequence: below the Windows floor this feature
        // is unavailable and the fallback is a supported mode. A user whose
        // tracks all came out identical has to be able to find out why from the
        // message rather than from an HRESULT (AGENTS.md sections 15 and 45).
        let error = AudioError::process_loopback_unavailable("E_NOTIMPL".to_owned());
        let message = error.to_string();

        assert!(message.contains("20348"), "{message}");
        assert!(message.contains("E_NOTIMPL"), "{message}");
        assert!(
            message.contains("System Audio"),
            "the message has to name the fallback, and to name it as the track an \
             editor will show rather than as a description: {message}"
        );
        assert!(
            message.contains("instead"),
            "and it has to say that this is what happened, not what could happen. It \
             said \"Clipped can still record\" for as long as nothing implemented it \
             (issue #604): {message}"
        );
    }

    #[test]
    fn the_description_names_the_process_a_track_is_scoped_to() {
        // It is the `device` field of every log line the shared engine writes
        // about this capture, and "which game" is the only thing that makes
        // those lines worth reading when two captures are running.
        assert_eq!(
            describe(4_242, TreeScope::Only),
            "the game's process tree, rooted at process 4242"
        );
    }

    #[test]
    fn the_two_sides_of_one_tree_do_not_describe_themselves_identically() {
        // A recording runs both at once, and every line the shared engine
        // writes about either of them carries `describe` as its `device` field.
        // Two captures that name themselves the same way is a log in which the
        // one question worth asking — which track did this audio go to — cannot
        // be answered (AGENTS.md section 35).
        let only = describe(4_242, TreeScope::Only);
        let except = describe(4_242, TreeScope::Except);

        assert_ne!(only, except, "the two sides have to be tellable apart");
        assert!(
            except.contains("except"),
            "the excluding side has to say so in words: {except}"
        );
        assert_eq!(
            TreeScope::Only.kind().audio_source().to_string(),
            "game",
            "the including side is the game's track"
        );
        assert_eq!(
            TreeScope::Except.kind().audio_source().to_string(),
            "other_system",
            "the excluding side is the other-system-audio track, not a second game track"
        );
    }

    /// The rule `decide_scope` is written against, with none of the machinery.
    ///
    /// Two captures, one shared cell, and a list of members each — which is
    /// everything the decision depends on. No audio device, no game, no process
    /// table.
    mod agreeing_on_one_tree {
        use super::*;

        /// A pair, as a session opens it: both sides scoped to the same root
        /// and neither following the other.
        struct Pair {
            scoping: Arc<AtomicU32>,
            /// `(scoped_to, following)` for the including and excluding sides.
            only: (u32, bool),
            except: (u32, bool),
        }

        impl Pair {
            fn rooted_at(root: u32) -> Self {
                Self {
                    scoping: Arc::new(AtomicU32::new(root)),
                    only: (root, false),
                    except: (root, false),
                }
            }

            /// Applies one refresh of the including side against `members`.
            fn only_sees(&mut self, members: &[u32]) -> Rescope {
                let decision = decide_scope(&self.scoping, self.only.0, self.only.1, members);
                apply(decision, &mut self.only);
                decision
            }

            /// Applies one refresh of the excluding side against `members`.
            fn except_sees(&mut self, members: &[u32]) -> Rescope {
                let decision = decide_scope(&self.scoping, self.except.0, self.except.1, members);
                apply(decision, &mut self.except);
                decision
            }
        }

        /// What `consider_rescoping` does with a decision, in the two fields
        /// the decision depends on.
        fn apply(decision: Rescope, side: &mut (u32, bool)) {
            match decision {
                Rescope::Stay | Rescope::Ended => {}
                Rescope::CaughtUp => side.1 = false,
                Rescope::Lead(successor) => *side = (successor, false),
                Rescope::Follow(successor) => *side = (successor, true),
            }
        }

        #[test]
        fn a_capture_on_its_own_is_unaffected_by_any_of_this() {
            // `open` and `open_excluding` each own their cell, so the agreement
            // machinery has to be invisible to them: a lone capture re-scopes
            // when the process it names dies and does nothing otherwise,
            // exactly as it did before pairs existed.
            let scoping = Arc::new(AtomicU32::new(4_242));

            assert_eq!(
                decide_scope(&scoping, 4_242, false, &[4_242, 4_243]),
                Rescope::Stay,
                "the process it names is alive, so nothing moves"
            );
            assert_eq!(
                decide_scope(&scoping, 4_242, false, &[4_243, 4_244]),
                Rescope::Lead(4_243),
                "the process it names has gone, so it takes the lowest survivor"
            );
            assert_eq!(
                decide_scope(&scoping, 4_243, false, &[]),
                Rescope::Ended,
                "nothing of the game is left"
            );
        }

        #[test]
        fn the_side_that_notices_first_decides_for_both() {
            // The defect this exists for. The launcher exits leaving two
            // children; each side refreshes its own tree on its own schedule,
            // so they can see different memberships. Without the shared cell
            // the including side scopes to 100 and the excluding side to 200,
            // and everything 100's subtree plays is then on *both* tracks —
            // the doubling `open_excluding` exists to prevent (issue #27).
            let mut pair = Pair::rooted_at(1);

            // The including side refreshes first and sees both children.
            assert_eq!(pair.only_sees(&[100, 200]), Rescope::Lead(100));

            // The excluding side refreshes a moment later, by which time 100
            // has gone as well. Left to itself it would take 200.
            assert_eq!(pair.except_sees(&[200]), Rescope::Follow(100));

            assert_eq!(
                pair.only.0, pair.except.0,
                "both sides have to name one process or the same audio lands on two tracks"
            );
        }

        #[test]
        fn a_side_whose_own_tree_still_looks_healthy_follows_the_pair_anyway() {
            // The case the shared cell exists for, on its own. This capture's
            // tree still lists the process it is scoped to as alive, so nothing
            // about its own state says anything is wrong — and the other side
            // has already moved. A capture that consulted only its own tree
            // would stay here, and the two would then be scoped to two
            // different trees for the rest of the recording, which is the same
            // audio on both tracks or on neither (issue #27).
            let scoping = Arc::new(AtomicU32::new(200));

            assert_eq!(
                decide_scope(&scoping, 100, false, &[100, 200]),
                Rescope::Follow(200),
                "the pair's choice outranks a healthy-looking tree of this side's own"
            );
        }

        #[test]
        fn two_sides_deciding_at_once_do_not_both_lead() {
            // What the compare-exchange buys, and the only thing that does:
            // both sides look while the cell still names the dead root, so
            // neither sees the other's move, and both go on to publish. One
            // wins and the other is told the winner's process.
            //
            // Publishing with a plain `store` would still *converge* — the
            // loser would notice on its next refresh and follow — so what this
            // catches is the reopen in between: a stream thrown away and
            // activated again, which is a gap in one of the two tracks that a
            // recording did not have to have.
            //
            // Threads rather than two calls in a row, because the window is
            // between the load and the write inside one call and cannot be
            // reached from a single thread. The assertion holds whether or not
            // a given round happens to race, so this cannot fail spuriously;
            // 500 rounds behind a spin barrier is what makes it race often
            // enough to catch the regression.
            const ROUNDS: usize = 500;

            for _ in 0..ROUNDS {
                let scoping = Arc::new(AtomicU32::new(1));
                let ready = Arc::new(AtomicU32::new(0));
                let leads = Arc::new(AtomicU32::new(0));

                // The two sides see different survivors, which is the whole
                // reason they have to agree: left alone, one would take 100 and
                // the other 200.
                let sides: [Vec<u32>; 2] = [vec![100, 200], vec![200]];
                let racing: Vec<_> = sides
                    .into_iter()
                    .map(|members| {
                        let scoping = Arc::clone(&scoping);
                        let ready = Arc::clone(&ready);
                        let leads = Arc::clone(&leads);
                        std::thread::spawn(move || {
                            ready.fetch_add(1, Ordering::SeqCst);
                            while ready.load(Ordering::SeqCst) < 2 {
                                core::hint::spin_loop();
                            }
                            if matches!(
                                decide_scope(&scoping, 1, false, &members),
                                Rescope::Lead(_)
                            ) {
                                leads.fetch_add(1, Ordering::SeqCst);
                            }
                        })
                    })
                    .collect();

                for side in racing {
                    side.join().expect("deciding a scope cannot panic");
                }

                assert!(
                    leads.load(Ordering::SeqCst) <= 1,
                    "both sides led the same move, so both reopened their stream and one of \
                     them did so for nothing"
                );
            }
        }

        #[test]
        fn a_follower_does_not_lead_back_and_the_pair_settles() {
            // What makes the rule terminate. The excluding side follows onto a
            // process its own tree has never listed — a tree up to a rescan
            // interval stale, or a process Windows refused it. Without the
            // "a follower does not lead" rule it would immediately publish its
            // own choice, the including side would follow that, and the two
            // would chase each other for the rest of the recording, reopening a
            // stream each time.
            let mut pair = Pair::rooted_at(1);

            assert_eq!(pair.only_sees(&[100, 200]), Rescope::Lead(100));
            assert_eq!(pair.except_sees(&[200]), Rescope::Follow(100));

            // Ten more refreshes of the excluding side, still never seeing 100.
            for _ in 0..10 {
                assert_eq!(
                    pair.except_sees(&[200]),
                    Rescope::Stay,
                    "a follower stays where the pair agreed rather than publishing its own"
                );
            }
            assert_eq!(pair.only_sees(&[100]), Rescope::Stay);
            assert_eq!(pair.scoping.load(Ordering::Relaxed), 100);
        }

        #[test]
        fn a_follower_that_catches_up_can_lead_the_next_move() {
            // The other half of that rule: `following` is a state to leave, not
            // a demotion. Once the excluding side can see the process the pair
            // agreed on, it is as entitled as the other to notice the next
            // death first — which matters because it is the side that goes on
            // recording after the game exits, and a side that could never lead
            // would leave the pair waiting on a capture that has stopped
            // caring.
            let mut pair = Pair::rooted_at(1);

            assert_eq!(pair.only_sees(&[100, 200]), Rescope::Lead(100));
            assert_eq!(pair.except_sees(&[200]), Rescope::Follow(100));
            assert_eq!(
                pair.except_sees(&[100, 200]),
                Rescope::CaughtUp,
                "its tree now lists the process the pair agreed on"
            );

            // 100 dies. This time the excluding side notices first.
            assert_eq!(pair.except_sees(&[200]), Rescope::Lead(200));
            assert_eq!(pair.only_sees(&[100, 200]), Rescope::Follow(200));
            assert_eq!(
                pair.only.0, pair.except.0,
                "the pair is still on one process"
            );
        }

        #[test]
        fn the_game_ending_does_not_split_the_pair() {
            // Both sides see the tree empty. Neither publishes anything, so the
            // cell still names the last process they agreed on and a side that
            // refreshes late does not find a disagreement to act on.
            let mut pair = Pair::rooted_at(1);

            assert_eq!(pair.only_sees(&[]), Rescope::Ended);
            assert_eq!(pair.except_sees(&[]), Rescope::Ended);
            assert_eq!(pair.scoping.load(Ordering::Relaxed), 1);
            assert_eq!(pair.only.0, pair.except.0);
        }
    }

    #[test]
    fn the_other_side_of_a_process_tree_can_be_captured_too() {
        // Issue #27's whole point. A session needs *both* sides or neither: a
        // recording with a `Game` track scoped to the tree and a system track
        // that is the whole endpoint has the game's audio on two tracks, which
        // is worse than one honest track (ADR 0003).
        //
        // What this proves is that Windows accepts the excluding activation on
        // this machine and hands back a client that initialises — the thing
        // that was not known, because nothing had ever asked for it. What is on
        // the two tracks is a routing question and belongs with the session
        // (#33).
        let root = std::process::id();

        let including = match ProcessLoopbackCapture::open(root) {
            Ok(capture) => capture,
            Err(error) => {
                skipped(&format!("this machine will not scope a capture: {error}"));
                return;
            }
        };
        let excluding = ProcessLoopbackCapture::open_excluding(root)
            .expect("a machine that can include a tree can exclude one: same activation");

        // Both are real captures of the same shape. A machine that gave the
        // excluding side a different format would give a session two tracks it
        // could not mix, which is worth knowing here rather than in a muxer.
        assert_eq!(
            including.format().sample_rate(),
            excluding.format().sample_rate(),
            "the two sides of one tree are captured at one rate"
        );

        let mut excluding = excluding;
        let mut including = including;
        including.close();
        excluding.close();
    }

    #[test]
    fn a_pair_agrees_through_one_cell_and_two_captures_opened_separately_do_not() {
        // What `ScopeAgreement` claims, measured on real captures rather than
        // assumed from reading `open_pair`. It is what a caller checks it
        // opened the pair with — `clipped_session::audio::open` does, because
        // for a week it did not open the pair at all and every test in this
        // file passed regardless (issue #581) — so an agreement that reported
        // "same" for two independent captures would make that check useless
        // and this one is what stops it.
        let root = std::process::id();

        let (game, other) = match ProcessLoopbackCapture::open_pair(root) {
            Ok(pair) => pair,
            Err(error) => {
                skipped(&format!("this machine will not scope a capture: {error}"));
                return;
            }
        };
        assert_eq!(
            game.scope_agreement(),
            other.scope_agreement(),
            "the two sides of one pair re-scope through one cell, which is the whole of what \
             `open_pair` adds over opening them separately"
        );

        let alone = ProcessLoopbackCapture::open(root)
            .expect("a machine that scoped a pair scopes one side of it");
        assert_ne!(
            alone.scope_agreement(),
            game.scope_agreement(),
            "a capture opened on its own owns its cell. An agreement that compared equal here \
             would say every capture is paired with every other, and the session's check that a \
             recording opened the pair would pass on the code that did not"
        );

        let mut game = game;
        let mut other = other;
        let mut alone = alone;
        game.close();
        other.close();
        alone.close();
    }

    #[test]
    fn the_two_sides_answer_differently_when_there_is_nothing_left_to_scope_to() {
        // The rule itself, with no machine involved: an empty tree stops the
        // *including* side and not the excluding one. Everything else in
        // `open_stream` is a Windows call; this is the decision, and it is the
        // whole of issue #563 in one line.
        assert!(
            !opens_against_an_empty_tree(TreeScope::Only),
            "there is nothing left to include, so there is no stream worth activating"
        );
        assert!(
            opens_against_an_empty_tree(TreeScope::Except),
            "excluding an empty set is everything the machine plays, which is what the \
             other-system-audio track is for"
        );
    }

    /// A 997 Hz tone rendered by *this* process, for as long as it is held.
    ///
    /// This process is not a member of the game's tree in the tests below — it
    /// is the tree's grandparent — so what it plays is by definition the
    /// complement the excluding side is supposed to carry. Playing it here
    /// rather than from a third program is the same choice
    /// `tests/audio/track_isolation.rs` makes and for the same reason: another
    /// process would add a process without adding a claim.
    ///
    /// `crates/audio/tests/system_audio.rs` renders a tone the same way. It
    /// cannot be shared with this one — that is an integration test, and its
    /// helpers are not reachable from the crate's own unit tests, which is
    /// where this has to live because forcing a reopen needs
    /// [`EndpointCapture::simulate_endpoint_change`]. A single renderer for the
    /// whole workspace belongs in `tests/media`, beside the Goertzel filter
    /// that measures what it played; that is a change to a crate several people
    /// are working in and it is not made here.
    struct TonePlayer {
        running: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TonePlayer {
        /// Starts rendering, or explains why this machine cannot.
        fn start() -> Result<Self, String> {
            let running = Arc::new(AtomicBool::new(true));
            let (ready, started) = std::sync::mpsc::channel();
            let thread = std::thread::spawn({
                let running = Arc::clone(&running);
                move || render_tone(&running, &ready)
            });

            match started.recv() {
                Ok(Ok(())) => Ok(Self {
                    running,
                    thread: Some(thread),
                }),
                Ok(Err(reason)) => Err(reason),
                Err(_) => Err("the render thread stopped before it reported anything".to_owned()),
            }
        }
    }

    impl Drop for TonePlayer {
        fn drop(&mut self) {
            // A test that panicked must not leave a tone sounding on somebody's
            // speakers.
            self.running.store(false, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The render thread's body: the endpoint's own mix format, filled with a
    /// sine, until it is told to stop.
    ///
    /// Reports through `ready` whether it got as far as playing, so a machine
    /// with no usable output endpoint skips rather than hangs.
    fn render_tone(running: &AtomicBool, ready: &std::sync::mpsc::Sender<Result<(), String>>) {
        use windows::Win32::Media::Audio::{
            eConsole, IAudioRenderClient, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CoIncrementMTAUsage};

        // SAFETY: as `apartment.rs` explains — the reference is never given
        // back, which is what makes it safe to take from a thread that exits.
        if let Err(error) = unsafe { CoIncrementMTAUsage() } {
            let _ = ready.send(Err(format!("COM is unavailable: {error}")));
            return;
        }

        let prepared = (|| -> Result<_, String> {
            let failed = |what: &str, error: &dyn core::fmt::Display| format!("{what}: {error}");
            // SAFETY: `MMDeviceEnumerator` is the class identifier for
            // `IMMDeviceEnumerator`, which is the return type.
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .map_err(|error| failed("the device enumerator", &error))?;
            // SAFETY: both arguments are values of the enumerations named.
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                .map_err(|error| failed("the default output device", &error))?;
            // SAFETY: `device` is live; the interface comes from the return
            // type.
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
                .map_err(|error| failed("activating a render client", &error))?;
            // SAFETY: `client` is live and uninitialised, which is when
            // `GetMixFormat` is valid.
            let mix = MixFormat::of(&client).map_err(|error| failed("the mix format", &error))?;
            let format = mix.audio();
            // SAFETY: `mix` owns a live `WAVEFORMATEX` for the whole call, and
            // it is the format Windows itself reported for this endpoint.
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    0,
                    BUFFER_DURATION,
                    0,
                    mix.as_ptr(),
                    None,
                )
            }
            .map_err(|error| failed("initialising the render stream", &error))?;
            // SAFETY: `client` is initialised, which is when `GetService` is
            // valid.
            let render: IAudioRenderClient = unsafe { client.GetService() }
                .map_err(|error| failed("the render service", &error))?;
            // SAFETY: `client` is initialised.
            let frames = unsafe { client.GetBufferSize() }
                .map_err(|error| failed("the render buffer size", &error))?;
            Ok((client, render, frames, format))
        })();

        let (client, render, buffer_frames, format) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = ready.send(Err(format!(
                    "the default output device would not take a render stream: {error}"
                )));
                return;
            }
        };
        if format.endpoint_samples() != SampleFormat::Float32 {
            let _ = ready.send(Err(format!(
                "this endpoint presents {} samples, and this renderer only knows how to write \
                 floats; writing anything else would be full-scale noise on somebody's speakers",
                format.endpoint_samples()
            )));
            return;
        }

        // SAFETY: `client` is initialised and not started.
        if let Err(error) = unsafe { client.Start() } {
            let _ = ready.send(Err(format!("the render stream would not start: {error}")));
            return;
        }
        let _ = ready.send(Ok(()));

        let channels = format.channels().get() as usize;
        let rate = format.sample_rate().get() as f32;
        let step = 2.0 * core::f32::consts::PI * NEIGHBOUR as f32 / rate;
        let mut phase = 0.0f32;

        while running.load(Ordering::Relaxed) {
            // SAFETY: `client` is a started `IAudioClient`.
            let Ok(padding) = (unsafe { client.GetCurrentPadding() }) else {
                break;
            };
            let free = buffer_frames.saturating_sub(padding);
            if free == 0 {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            // SAFETY: `free` is at most the free space `GetCurrentPadding` just
            // reported, which is what `GetBuffer` requires. The pointer is
            // valid for `free * channels` floats until `ReleaseBuffer`, which
            // is called below before anything else happens.
            let Ok(buffer) = (unsafe { render.GetBuffer(free) }) else {
                break;
            };
            // SAFETY: as above, and the endpoint was checked to present floats.
            let samples = unsafe {
                std::slice::from_raw_parts_mut(buffer.cast::<f32>(), free as usize * channels)
            };
            for frame in samples.chunks_exact_mut(channels) {
                frame.fill(phase.sin() * NEIGHBOUR_AMPLITUDE);
                phase = (phase + step) % (2.0 * core::f32::consts::PI);
            }
            // SAFETY: `free` is the frame count `GetBuffer` was asked for and
            // the buffer has been written in full, so no silence flag is
            // needed.
            if unsafe { render.ReleaseBuffer(free, 0) }.is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        // SAFETY: `client` was started; stopping it is what releases the
        // endpoint.
        let _ = unsafe { client.Stop() };
    }

    /// The tone this process holds while the tests below run.
    ///
    /// 997 Hz and not the more obvious 440 for the reason
    /// `crates/audio/tests/system_audio.rs` gives: 440 Hz is a musical A, and
    /// music playing on the machine while the suite runs puts energy in exactly
    /// that bin.
    const NEIGHBOUR: f64 = 997.0;

    /// How loud it is played: about −28 dBFS, which is audible and not
    /// startling.
    const NEIGHBOUR_AMPLITUDE: f32 = 0.04;

    /// How far apart the two tracks' measurements of [`NEIGHBOUR`] have to be.
    ///
    /// The same rejection threshold `tests/audio/track_isolation.rs` uses
    /// (`Tone::DEFAULT_RATIO`): eight times, about 18 dB. Two streams that were
    /// really the same endpoint are nowhere near that far apart.
    const REJECTION: f64 = 8.0;

    /// What one side of the pair produced over a stretch of reading.
    struct Heard {
        /// Every sample handed over, one channel of it.
        mono: Vec<f32>,
        /// How many frames the client delivered, as opposed to ones this crate
        /// invented to cover a client that produced nothing.
        from_the_client: u64,
        /// The rate the samples above are at.
        rate: u32,
    }

    impl Heard {
        /// How much energy sits at `frequency`.
        ///
        /// Through `clipped_media_validation::AudioContent`, which is the same
        /// filter `tests/audio/track_isolation.rs` and
        /// `crates/muxer/tests/multi_track_audio.rs` measure a finished
        /// recording with, rather than a second one written here (AGENTS.md
        /// section 55).
        fn magnitude_at(&self, frequency: f64) -> f64 {
            clipped_media_validation::AudioContent::from_samples(self.mono.clone(), self.rate)
                .magnitude_at(frequency)
        }
    }

    /// Reads both sides of a pair for `duration`, at the same time.
    ///
    /// A thread each, which is how a session reads them and is not a detail
    /// here. Reading one and then the other leaves the second unread for as
    /// long as the first takes; the engine holds 200 ms and the rest of the
    /// outage comes back as synthesised silence at the *front* of the track,
    /// which is where a fixed analysis window then lands. A first attempt at
    /// this test read them in turn and measured 0.00000 at 997 Hz on a track
    /// whose peak amplitude was the tone's own 0.04 — the audio was there, and
    /// the window was looking at the hole left by not reading.
    fn listen_to_both(
        first: &mut ProcessLoopbackCapture,
        second: &mut ProcessLoopbackCapture,
        duration: Duration,
    ) -> (Heard, Heard) {
        std::thread::scope(|scope| {
            let one = scope.spawn(|| listen(first, duration));
            let other = scope.spawn(|| listen(second, duration));
            (
                one.join().expect("a reading thread does not panic"),
                other.join().expect("a reading thread does not panic"),
            )
        })
    }

    /// Reads `capture` for `duration`, keeping the samples.
    fn listen(capture: &mut ProcessLoopbackCapture, duration: Duration) -> Heard {
        let format = capture.format();
        let channels = format.channels().get() as usize;
        let mut mono = Vec::new();
        let mut from_the_client = 0u64;
        let until = Instant::now() + duration;

        while Instant::now() < until {
            match capture
                .read(Duration::from_millis(50))
                .expect("a healthy capture does not fail")
            {
                Capture::Samples(samples) => {
                    if samples.origin() == SampleOrigin::Endpoint {
                        from_the_client += samples.frames() as u64;
                    }
                    mono.extend(samples.samples().iter().step_by(channels));
                }
                Capture::Idle | Capture::FormatChanged(_) => {}
            }
        }

        Heard {
            mono,
            from_the_client,
            rate: format.sample_rate().get(),
        }
    }

    #[test]
    fn a_track_of_everything_but_the_game_is_still_everything_once_the_game_has_gone() {
        // Issue #563. `open_stream` used to refuse an empty tree on both sides,
        // so the first reopen after the game exited left the
        // other-system-audio track as synthesised silence for the rest of the
        // recording — and a reopen is what an endpoint change or a re-scope
        // does. The case it cost is the ordinary one: somebody closes the game,
        // keeps recording, and a browser or a voice call is still playing.
        //
        // Both halves are asserted, because only the pair of them says the
        // change was made in the right place:
        //
        // - the **excluding** side has to carry this process's tone after the
        //   reopen. That is the fix.
        // - the **including** side must not. A build that simply stopped
        //   refusing an empty tree on both sides would put everything the
        //   machine plays into the track labelled with the game's name, which
        //   is precisely ADR 0003's cardinal sin: muting the game in an editor
        //   would not silence it.
        //
        // It makes a sound, so it asks first.
        if suppressed() {
            return;
        }
        let tone = match TonePlayer::start() {
            Ok(playing) => playing,
            Err(reason) => {
                skipped(&format!("this machine cannot play a tone: {reason}"));
                return;
            }
        };

        // The game: a process tree that plays nothing at all.
        let mut chain = Chain::start();
        let root = chain.root_pid();
        let (mut game_track, mut other_track) = match ProcessLoopbackCapture::open_pair(root) {
            Ok(pair) => pair,
            Err(error) => {
                skipped(&format!("this machine will not scope a capture: {error}"));
                return;
            }
        };

        // While the game is running, the tone this process holds belongs to the
        // excluding side and to nothing else. Asserted first so that a failure
        // after the game exits is about the game having exited rather than
        // about a machine whose two sides were never separated at all.
        let (before_game, before_other) = listen_to_both(
            &mut game_track,
            &mut other_track,
            Duration::from_millis(700),
        );
        assert_isolated(
            "while the game was running",
            &before_other,
            &before_game,
            &tone,
        );

        // The game and everything it started exit.
        chain.kill_root();
        drop(chain);
        for capture in [&mut game_track, &mut other_track] {
            assert!(
                read_until(capture, |capture| !capture.target_is_running()),
                "both sides have to notice that every process of the game has exited"
            );
        }

        // And something asks for a stream to be activated again. A user plugs
        // in a headset, or a call on the client reports its device invalidated;
        // this is the second step of that path, and everything after it — the
        // stream torn down, `open_stream` called again, the outage covered with
        // silence — is the code a real endpoint change runs
        // (`EndpointCapture::simulate_endpoint_change`).
        game_track
            .capture
            .simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);
        other_track
            .capture
            .simulate_endpoint_change(EndpointChange::CaptureEndpointRemoved);

        // The change is acted on at the top of the next read, and one converted
        // packet from the stream that has just been thrown away may still be
        // waiting to be handed over — 480 frames of it, one WASAPI packet, was
        // exactly what a first attempt at this test measured on the game's side
        // and mistook for the reopened stream. So a moment of reading is
        // discarded either side of the change, and the window measured below is
        // the new stream and nothing else.
        listen_to_both(
            &mut game_track,
            &mut other_track,
            Duration::from_millis(400),
        );

        let (after_game, after_other) = listen_to_both(
            &mut game_track,
            &mut other_track,
            Duration::from_millis(1_500),
        );

        assert!(
            after_other.from_the_client > 0,
            "after the game exited and the stream was reopened, the other-system-audio track has \
             to be coming from a client. It measured {} frames from the client, which is a track \
             made entirely of silence this crate invented — the whole of issue #563",
            after_other.from_the_client
        );
        assert_eq!(
            after_game.from_the_client, 0,
            "the game's track must not start delivering audio once the game has gone. A tree \
             with nothing in it plays nothing, and a client that hands over anything here is a \
             `Game` track carrying the whole machine (ADR 0003)"
        );
        assert_isolated(
            "after the game exited and both streams were reopened",
            &after_other,
            &after_game,
            &tone,
        );
    }

    /// Asserts that `carrying` holds [`NEIGHBOUR`] and `absent` does not.
    ///
    /// The rejection is the assertion. "The other-system track is not empty"
    /// would pass just as well on a build that copied the endpoint into every
    /// track, which is the failure this whole file exists to prevent.
    fn assert_isolated(when: &str, carrying: &Heard, absent: &Heard, tone: &TonePlayer) {
        // The tone has to still be playing, or both measurements below are
        // zero and the test would pass by measuring nothing.
        assert!(
            tone.running.load(Ordering::Relaxed),
            "the tone stopped playing part way through, so nothing measured here means anything"
        );

        let present = carrying.magnitude_at(NEIGHBOUR);
        let rejected = absent.magnitude_at(NEIGHBOUR);
        let _ = writeln!(
            std::io::stderr(),
            "{when}: other_system {NEIGHBOUR} Hz {present:.5} ({} client frames, {} samples, peak \
             {:.5}), game {NEIGHBOUR} Hz {rejected:.5} ({} client frames, {} samples, peak {:.5})",
            carrying.from_the_client,
            carrying.mono.len(),
            carrying.mono.iter().fold(0.0f32, |a, s| a.max(s.abs())),
            absent.from_the_client,
            absent.mono.len(),
            absent.mono.iter().fold(0.0f32, |a, s| a.max(s.abs())),
        );
        assert!(
            present > rejected * REJECTION,
            "{when}, the tone this process was playing measured {present:.5} on the \
             other-system-audio track and {rejected:.5} on the game's, which is {:.1} times \
             apart against a threshold of {REJECTION:.0}. The tone belongs to the complement of \
             the game's tree and to nothing else",
            present / rejected.max(f64::MIN_POSITIVE)
        );
        assert!(
            present > f64::from(NEIGHBOUR_AMPLITUDE) / 8.0,
            "{when}, the other-system-audio track measured {present:.5} at {NEIGHBOUR} Hz, which \
             is a track that is not carrying the tone at all. Two tracks that are both silent \
             pass a ratio and prove nothing"
        );
    }
}
