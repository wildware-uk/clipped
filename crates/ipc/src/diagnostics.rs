//! What the recorder knows about how it is capturing and what this machine can
//! encode, as the Diagnostics screen reads it.
//!
//! # Why this is on the protocol at all
//!
//! SPEC.md section 36 asks a recorder to record twelve things and to be able to
//! hand them to somebody with a problem. Eleven of them were inside the recorder
//! process with nothing carrying them to a window: the window reaches the
//! recorder over this protocol, which had commands for recording and nothing for
//! diagnostics, and it reaches its own host through three commands none of which
//! touches the file system. So a screen built to show them
//! (`docs/diagnostics.md`, issue #101) could show one — the path of the recording
//! in progress, which arrives inside a `recording` status — and named the missing
//! work in the other twelve rows.
//!
//! [`Diagnostics`] is what two of those rows read now
//! ([issue #302](https://github.com/wildware-uk/clipped/issues/302)): **which
//! capture backend the recording in progress is using**, and **what this machine
//! can encode**. The rest still name the work that would supply them, because
//! nothing measures them yet — see `docs/diagnostics.md` for which and why.
//!
//! # Why asked for rather than pushed
//!
//! Both answers are properties of the machine and of a recording that has already
//! started, not events. The capability report is settled before any window
//! exists — it is read when the recorder starts a recording, and on a machine
//! whose hardware has not changed it is read from a cache — and a capture
//! backend is chosen in the first moments of a recording. An event announcing
//! either would be published before there was anybody to hear it, which is the
//! same race [`crate::hotkeys`] is asked for rather than pushed to avoid.
//!
//! A window that wants to know whether the *answer* has changed asks again. It
//! is a bounded read of local state on a connection thread, and it is behind a
//! screen somebody opened deliberately.
//!
//! # Why these are wire types rather than the recorder's own
//!
//! This crate depends on no other crate of the workspace and may not start to
//! (ADR 0002, `tests/integration/tests/workspace_layering.rs`): the desktop
//! application links it, and neither the capture engine nor the encoder — which
//! on Windows drags four FFmpeg libraries with it — may come along. So these are
//! wire types, and `apps/recorder` maps `clipped_capture::CaptureStatus` and
//! `clipped_encoder::CapabilityReport` onto them, exactly as it maps
//! `clipped_hotkeys::Registration` onto [`HotkeyBinding`](crate::HotkeyBinding).
//!
//! # What is deliberately not here
//!
//! **No path.** The capability cache lives at
//! `%LOCALAPPDATA%\Clipped\encoder-capabilities.json`, which runs through the
//! user's account name; the terminal report prints it and this does not
//! (AGENTS.md section 13). What crosses instead is
//! [`EncoderAccount::probed`] and [`EncoderAccount::detected_at`], which is
//! the part anybody reading a bug report needs: whether the machine was asked
//! or a stored answer was used, and when that answer was taken.
//!
//! **No window title, no sample, no file contents.** Nothing here comes from
//! anything a user made. A graphics adapter's description — `NVIDIA GeForce RTX
//! 4090` — is hardware rather than user content, which is the judgement
//! `clipped_encoder::Adapter` already records against the same rule.

use serde::{Deserialize, Serialize};

/// The FFmpeg a recorder has loaded, as it reports itself.
///
/// Every field is asked of the loaded libraries rather than taken from the pin
/// in `scripts/fetch-ffmpeg.ps1`: a machine running an FFmpeg of its own —
/// which `.cargo/config.toml` deliberately allows — has to say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegBuild {
    /// FFmpeg's own name for the build, such as
    /// `n8.1.2-34-g9b6c8969e0-20260809`.
    ///
    /// The release tag, the commit and whatever the packager appended, so it
    /// identifies the artefact far more precisely than the library versions do.
    pub identifier: String,
    /// The licence the libraries report for themselves.
    ///
    /// Reported rather than assumed. A substituted GPL build carries the same
    /// version numbers and a different licence, and LGPL v3 section 4(c) is
    /// about what is *displayed*, so a hardcoded "LGPL" here would be the one
    /// thing this must not be.
    pub licence: String,
    /// The loaded `libavformat`, such as `62.12.102`.
    pub avformat: String,
    /// The loaded `libavcodec`.
    pub avcodec: String,
    /// The loaded `libavutil`.
    pub avutil: String,
}

/// What the recorder can say about capture and encoding, right now.
///
/// The reply to `get_diagnostics`. Both halves are answered from state the
/// recorder already holds: nothing here starts a probe, opens an encoder session
/// or touches a capture thread (AGENTS.md sections 17 and 20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    /// How the recording in progress is capturing.
    ///
    /// Absent when nothing is being recorded, because a capture backend is a
    /// property of a recording rather than of the recorder: there is no backend
    /// running between recordings, and reporting the last one used would be a
    /// window saying what *is* happening from a reading of what happened.
    ///
    /// Absent as well for the first moments of a recording that has started and
    /// not yet chosen — see [`CaptureAccount`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureAccount>,
    /// What this machine can encode.
    ///
    /// Never absent. A machine with no hardware encoder at all still has the
    /// software one and still has adapters, and "no encoder was found" is a
    /// report somebody with a problem needs rather than an empty answer.
    pub encoders: EncoderAccount,
    /// Which FFmpeg the recorder actually loaded.
    ///
    /// The recorder logs this at start-up, which answers it for anybody with the
    /// log file. It is here so that it is answerable *without a terminal*, which
    /// is the third acceptance criterion of
    /// [issue #256](https://github.com/wildware-uk/clipped/issues/256) and the
    /// one that was outstanding.
    ///
    /// It is not only a convenience. The DLLs beside the binaries are
    /// replaceable by design — that is the LGPL relinking permission, and
    /// `docs/licensing.md` records the substitution test — so which FFmpeg is
    /// in use is a *run-time* fact. A window that reported the pin would be
    /// reporting what was intended rather than what is loaded.
    ///
    /// Absent from a recorder built before this, which is why it is optional
    /// rather than a field every build must fill.
    ///
    /// Boxed because `Reply` is the largest variant of `Outcome` and five more
    /// strings put it past what clippy's `large_enum_variant` will accept —
    /// every reply of every kind would otherwise carry the space. `serde` sees
    /// through a `Box`, so the wire is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg: Option<Box<FfmpegBuild>>,
    /// What the recording in progress is running with, setting by setting.
    ///
    /// Absent when nothing is being recorded, for the reason
    /// [`capture`](Self::capture) is: these are the answers *one recording* was
    /// started with, not a reading of the settings file. Reporting the file
    /// between recordings would be a window saying what a recording is doing
    /// from a reading of what the next one would do — and the two differ every
    /// time somebody saves a setting while a game is running, which is exactly
    /// when a recording is running (`docs/configuration.md`, "Resolved once,
    /// when the recording starts").
    ///
    /// [Issue #61](https://github.com/wildware-uk/clipped/issues/61)'s third
    /// acceptance criterion; the other half of it is the session record beside
    /// the files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Vec<EffectiveSetting>>,
    /// Why the recording library is not being kept up to date, when it is not.
    ///
    /// Absent is the healthy state, and is what a window should draw nothing
    /// for. Present means recordings are still being made and written correctly
    /// and **none of them is reaching the library**: the Library screen is
    /// empty, the home screen's recent clips are empty, and automatic cleanup is
    /// not running, because all three read the index.
    ///
    /// It is here because that failure is otherwise invisible. On the machine
    /// that raised [issue #738](https://github.com/wildware-uk/clipped/issues/738)
    /// a renumbered migration stopped the library opening for four days, and the
    /// only evidence anywhere was one `WARN` an hour in a log file — so the user
    /// concluded the recorder did not work, while it was recording perfectly
    /// (AGENTS.md section 27).
    ///
    /// The recorder's own sentence, which names the cause. A window that
    /// summarised it would be a second description of a fault it cannot
    /// diagnose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_refusal: Option<String>,
}

/// One setting the recording in progress is running with.
///
/// Not "one setting in the settings file". A recording is built from what its
/// caller asked for — a `clipped-recorder watch` command line, or a
/// `start_recording` — with the settings configured for that game laid over it,
/// so a setting nobody configured keeps the caller's answer and reporting the
/// file's would name a value the encoder is not using
/// (`clipped_session::config::effective`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveSetting {
    /// The key, as `settings.json` spells it: `resolution`, `framerate`,
    /// `codec`, `encoder`, `microphone`, `system_audio`.
    ///
    /// The same names `get_settings` uses, so a window can line a diagnostics
    /// reading up against the settings screen's own rows without a table of its
    /// own.
    ///
    /// Two of the settings a game may override are **not** here, and their
    /// absence is the honest answer rather than a gap: `capture_target`, which
    /// nothing in this build reads, and `replay_window_seconds`, which sizes a
    /// buffer rather than being a property of the recording.
    pub setting: String,
    /// The value, spelled the way the settings file and the command line spell
    /// it — `2560x1440`, `60`, `h264`, `nvenc`, `none`, `name:Shure MV7`.
    pub value: String,
    /// Where this recording's answer came from.
    ///
    /// `default`, `global` or `game` for the three layers of the settings file,
    /// and `request` for a setting the recording asked for itself and no layer
    /// above the shipped default overrode. A window drawing "inherited" from a
    /// value alone would say a recording was following the global settings when
    /// it was following a command line.
    pub source: String,
}

/// How the recording in progress is capturing.
///
/// The three lines SPEC.md section 8 asks for — what was asked for, what is
/// running, and how it got there — from `clipped_capture::CaptureStatus`.
///
/// # When it is absent
///
/// It is published by the recording once its backend is open, which is a few
/// milliseconds into a recording and before the first frame. A `get_diagnostics`
/// that lands in that window gets [`Diagnostics::capture`] absent rather than a
/// guess, for the reason
/// [`RecordingProgress`](https://github.com/wildware-uk/clipped/issues/64)
/// answers `None` before its first frame: "not chosen yet" and "chose this" are
/// different facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAccount {
    /// What the user asked for: `Automatic`, or the method they pinned.
    ///
    /// The words a person reads rather than a machine token, and sent rather
    /// than derived, for the reason
    /// [`HotkeyBinding::label`](crate::HotkeyBinding::label) is: the list of
    /// capture methods has one home, in the recorder, and a window keeping its
    /// own table of names would show nothing at all for a method a newer
    /// recorder had added.
    pub setting: String,
    /// The method this recording started with, such as `Windows Graphics
    /// Capture`.
    pub started_with: String,
    /// The method capturing now.
    ///
    /// Equal to [`started_with`](Self::started_with) unless something in
    /// [`changes`](Self::changes) replaced it. Sent as its own field rather than
    /// left to be read off the last change, because "which backend is running"
    /// is the question and an answer that has to be reconstructed is one a
    /// client can reconstruct wrongly.
    pub current: String,
    /// Every replacement and restart, in the order they happened.
    ///
    /// Empty is the ordinary case and is a measurement rather than an absence:
    /// it says the backend this recording started with is still the one running
    /// and has never been restarted. A restart of the same backend is in here
    /// too, with [`CaptureMethodChange::restart`] set — "it fell over and came
    /// back" is what explains a gap in an otherwise unexplained recording.
    pub changes: Vec<CaptureMethodChange>,
}

/// One replacement or restart of the capture backend, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMethodChange {
    /// The method that was in use before.
    pub from: String,
    /// The method in use after it, equal to [`from`](Self::from) for a restart.
    pub to: String,
    /// Whether this was the same backend starting again rather than a different
    /// one taking over.
    ///
    /// A field rather than a comparison of the two above, so that a client
    /// branches on the recorder's own judgement.
    pub restart: bool,
    /// What made it necessary: `initialisation_failed`, `capture_failed` or
    /// `black_frames`.
    ///
    /// A machine token, kept open the way
    /// [`SessionSummary::end_reason`](crate::SessionSummary::end_reason) is: a
    /// trigger a build has never heard of is shown rather than failing the frame
    /// that carried it, and nothing here branches on it.
    pub trigger: String,
    /// The failure, in the recorder's own words and phrased for this screen.
    ///
    /// It names a capture method, an API failure and a frame format, and never
    /// a window title or a path — `clipped_capture` builds every one of these
    /// sentences out of its own vocabulary (AGENTS.md section 13).
    pub reason: String,
}

/// What this machine can encode, as `clipped-recorder capabilities` reports it.
///
/// Every field is what detection found, never what a recording chose: which
/// encoder a recording *used* is the recording's business and is already in its
/// report. This is the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderAccount {
    /// Whether the machine was asked in this run, rather than a stored answer
    /// still matching the hardware and being used.
    ///
    /// The distinction the terminal report prints and the one a bug report
    /// needs: a stored answer taken before a driver update describes a machine
    /// that no longer exists, and the cache is keyed on the driver version
    /// precisely so that it cannot.
    pub probed: bool,
    /// When a stored answer was originally measured, RFC 3339 with an offset.
    ///
    /// Absent when [`probed`](Self::probed) is `true`, because the answer was
    /// taken just now and [`Diagnostics`] carries no clock of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<String>,
    /// How long the answer took to obtain, cache lookup included.
    ///
    /// Reported rather than assumed, for the reason
    /// `clipped_encoder::Detection::elapsed` exists: the whole argument for
    /// storing an answer is that asking is slow, and a number nobody measures
    /// is how that argument survives being wrong.
    pub elapsed_ms: u64,
    /// The graphics adapters, in the order the system enumerated them.
    pub adapters: Vec<AdapterSummary>,
    /// Every encoder family, in the order SPEC.md section 9 lists them.
    ///
    /// **Including the ones that are not here.** "Clipped did not find your
    /// NVIDIA card" is the report a user with a problem needs, and a list that
    /// left the absent ones out would be indistinguishable from a build that
    /// had never heard of them.
    pub encoders: Vec<EncoderSummary>,
}

/// One graphics adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterSummary {
    /// The model name the driver publishes: `NVIDIA GeForce RTX 4090`.
    ///
    /// Hardware rather than user content, and the single most useful thing in a
    /// bug report about encoding.
    pub description: String,
    /// `nvidia`, `amd`, `intel`, `microsoft` or `other`.
    pub vendor: String,
    /// `own_video_memory`, `shared_video_memory` or `software`.
    pub kind: String,
    /// Bytes of video memory of its own; zero for an adapter that shares the
    /// machine's.
    pub video_memory_bytes: u64,
    /// The driver version, where the adapter reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_version: Option<String>,
    /// Whether this is the adapter a recording creates its graphics device on.
    ///
    /// The reason an encoder can be present, working and unusable: a machine
    /// with a discrete NVIDIA card and integrated AMD graphics has an AMD
    /// encoder no recording on it can hand a frame to, and this is the field
    /// that says which side of that line an adapter is on.
    pub captures: bool,
}

/// One encoder family, and what is known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderSummary {
    /// `nvenc`, `amf`, `quick_sync` or `software`.
    pub encoder: String,
    /// The family in the words a person reads: `NVIDIA NVENC`.
    pub label: String,
    /// Whether a recording made on this machine could use it.
    pub available: bool,
    /// Why it cannot be used, when it cannot.
    ///
    /// Present exactly when [`available`](Self::available) is `false`, and it is
    /// the recorder's own sentence: the runtime is not installed, no adapter of
    /// that vendor is here, or the encoder is real and sits on an adapter no
    /// recording will hand a frame to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// Whether **this build** has a backend proven to encode with it, as
    /// against detecting it.
    ///
    /// The distance between "your machine can do this" and "Clipped can do
    /// this", which a report full of ticks from a build that can use half of
    /// them would hide (AGENTS.md sections 27 and 54).
    pub implemented: bool,
    /// The adapter it runs on, by [`AdapterSummary::description`].
    ///
    /// Absent for the software encoder, which needs no particular adapter, and
    /// for a family that is not here at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Whether an encoder session was ever opened and asked about this family.
    ///
    /// The difference between limits that are published because nobody has
    /// asked and limits that are published because the encoder would not say —
    /// which is the difference between a fresh look being worth suggesting and
    /// being a waste of a session slot.
    ///
    /// **Not a claim about this call.** Answering `get_diagnostics` never opens
    /// a session, because that takes a slot from a game that may be mid-match;
    /// `true` here means the *stored* answer was measured that way, by somebody
    /// running `clipped-recorder capabilities --refresh`. Whether the numbers
    /// beside it came from the machine or from a published table is
    /// [`CodecSummary::inferred`], per codec, which is the finer question and
    /// the one worth drawing.
    pub asked: bool,
    /// What is known about each codec, most efficient first.
    ///
    /// Empty when the family is not available: an encoder that is not there has
    /// no codecs, and a table of limits under it would suggest otherwise.
    pub codecs: Vec<CodecSummary>,
}

/// What is known about one codec on one encoder family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecSummary {
    /// `h264`, `hevc` or `av1`.
    pub codec: String,
    /// Whether the encoder can produce it.
    ///
    /// **Absent is not `false`.** Absent means nothing here knows, which is the
    /// honest answer for HEVC and AV1 on a driver that does not advertise them
    /// and has not been asked; `false` means something was asked and said no.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported: Option<bool>,
    /// The widest picture, where a limit is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<u32>,
    /// The tallest picture, where a limit is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_height: Option<u32>,
    /// Frames a second at 1080p, where a rate is known.
    ///
    /// Quoted at one resolution because that is what most recordings are made
    /// at (SPEC.md section 10) and the ceiling at any other size is this one
    /// scaled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_framerate_1080p: Option<u32>,
    /// Whether any value above came from the family's published limits rather
    /// than from this machine.
    ///
    /// The one thing this report has to make obvious. A limit read from a
    /// vendor's documentation is true of the hardware that documentation covers
    /// and is not a promise about the card in front of the user, and a report
    /// that presented the two alike would be inviting somebody to debug the
    /// wrong thing.
    pub inferred: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterSummary, CaptureAccount, CaptureMethodChange, CodecSummary, Diagnostics,
        EffectiveSetting, EncoderAccount, EncoderSummary,
    };

    fn encoders() -> EncoderAccount {
        EncoderAccount {
            probed: true,
            detected_at: None,
            elapsed_ms: 4,
            adapters: vec![AdapterSummary {
                description: "NVIDIA GeForce RTX 4090".to_owned(),
                vendor: "nvidia".to_owned(),
                kind: "own_video_memory".to_owned(),
                video_memory_bytes: 25_769_803_776,
                driver_version: Some("32.0.15.6094".to_owned()),
                captures: true,
            }],
            encoders: vec![EncoderSummary {
                encoder: "nvenc".to_owned(),
                label: "NVIDIA NVENC".to_owned(),
                available: true,
                unavailable: None,
                implemented: true,
                adapter: Some("NVIDIA GeForce RTX 4090".to_owned()),
                asked: false,
                codecs: vec![CodecSummary {
                    codec: "h264".to_owned(),
                    supported: Some(true),
                    max_width: Some(4096),
                    max_height: Some(4096),
                    max_framerate_1080p: Some(240),
                    inferred: true,
                }],
            }],
        }
    }

    #[test]
    fn a_recorder_with_nothing_recording_leaves_capture_out_rather_than_sending_an_empty_account() {
        let diagnostics = Diagnostics {
            library_refusal: None,
            capture: None,
            encoders: encoders(),
            settings: None,
            // Not the subject here. A recorder that could not ask its libraries
            // answers this way, so it is also the honest fixture (issue #256).
            ffmpeg: None,
        };

        let json = serde_json::to_value(&diagnostics).expect("it serialises");

        assert_eq!(
            json.get("capture"),
            None,
            "an absent capture account must not appear on the wire as anything at all: an empty \
             account would be read as a recording that had chosen no backend"
        );
        assert!(
            json.get("encoders").is_some(),
            "the encoders are never absent"
        );
        assert_eq!(
            json.get("settings"),
            None,
            "the settings a recording is running with belong to a recording, and an empty list              here would be read as a recording running with no settings at all"
        );
    }

    #[test]
    fn a_recordings_settings_carry_the_layer_that_supplied_each_one() {
        // The field the whole reading turns on. A value alone cannot tell a
        // recording following the global settings from one following the
        // command line it was started with, and those are different bugs.
        let diagnostics = Diagnostics {
            library_refusal: None,
            capture: None,
            encoders: encoders(),
            settings: Some(vec![
                EffectiveSetting {
                    setting: "framerate".to_owned(),
                    value: "60".to_owned(),
                    source: "game".to_owned(),
                },
                EffectiveSetting {
                    setting: "codec".to_owned(),
                    value: "h264".to_owned(),
                    source: "request".to_owned(),
                },
            ]),
            // Not the subject here (issue #256).
            ffmpeg: None,
        };

        let json = serde_json::to_value(&diagnostics).expect("it serialises");

        assert_eq!(
            json["settings"],
            serde_json::json!([
                {"setting": "framerate", "value": "60", "source": "game"},
                {"setting": "codec", "value": "h264", "source": "request"},
            ])
        );

        let read: Diagnostics = serde_json::from_value(json).expect("it reads back");
        assert_eq!(read, diagnostics);
    }

    #[test]
    fn an_unchanged_capture_sends_an_empty_change_list_rather_than_leaving_it_out() {
        let account = CaptureAccount {
            setting: "Automatic".to_owned(),
            started_with: "Windows Graphics Capture".to_owned(),
            current: "Windows Graphics Capture".to_owned(),
            changes: Vec::new(),
        };

        let json = serde_json::to_value(&account).expect("it serialises");

        // "nothing has fallen back" is a measurement, and a reader that could
        // not see the empty list would have to guess whether the recorder had
        // looked. `changes` therefore carries no `skip_serializing_if`.
        assert_eq!(
            json.get("changes"),
            Some(&serde_json::json!([])),
            "an unchanged capture must send `changes: []`"
        );
    }

    #[test]
    fn a_codec_nothing_knows_about_is_absent_rather_than_unsupported() {
        let codec = CodecSummary {
            codec: "av1".to_owned(),
            supported: None,
            max_width: None,
            max_height: None,
            max_framerate_1080p: None,
            inferred: false,
        };

        let json = serde_json::to_string(&codec).expect("it serialises");

        assert_eq!(
            json, r#"{"codec":"av1","inferred":false}"#,
            "`supported: false` and \"nobody asked\" are different answers, so an unknown one is \
             absent rather than false"
        );

        let read: CodecSummary = serde_json::from_str(&json).expect("it reads back");
        assert_eq!(read, codec);
    }

    #[test]
    fn a_change_a_newer_recorder_triggered_is_kept_rather_than_failing_the_reply() {
        // `trigger` is an open vocabulary, so a build that has never heard of
        // this one shows it rather than losing the whole account
        // (`docs/ipc.md`, the compatibility policy).
        let json = r#"{"from":"Windows Graphics Capture","to":"Desktop Duplication",
                       "restart":false,"trigger":"driver_reset",
                       "reason":"the display driver was reset"}"#;

        let change: CaptureMethodChange = serde_json::from_str(json).expect("it reads");

        assert_eq!(change.trigger, "driver_reset");
    }

    #[test]
    fn an_account_carrying_a_field_added_later_is_still_read_as_itself() {
        let json = r#"{"probed":false,"detected_at":"2026-08-11T20:14:00+01:00","elapsed_ms":1,
                       "adapters":[],"encoders":[],"power_draw_watts":180}"#;

        let account: EncoderAccount = serde_json::from_str(json).expect("it reads");

        assert!(!account.probed);
        assert_eq!(
            account.detected_at.as_deref(),
            Some("2026-08-11T20:14:00+01:00")
        );
    }
}
