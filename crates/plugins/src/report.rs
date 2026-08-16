//! What a plugin says while it is running, and how it becomes an event.
//!
//! One JSON object per line, in both directions: the host writes
//! [`HostCommand`]s to the plugin's standard input and reads [`PluginReport`]s
//! from its standard output. That is the whole wire. It is deliberately the
//! plainest thing that works, because a plugin is a program somebody else
//! wrote, possibly in a language that has never heard of Rust, and the cost of
//! writing one should be a `println!`.
//!
//! ```text
//! host  → {"command":"attach","contract":1,"session":{…}}
//! plugin→ {"report":"hello","contract":1}
//! plugin→ {"report":"event","kind":"kill","ago_ns":480000000,"precision_ns":0,"confidence":1.0}
//! plugin→ {"report":"alive"}
//! host  → {"command":"detach"}
//! ```
//!
//! # A plugin never says when, on the session's timeline
//!
//! It says **how long ago**, in nanoseconds, measured against its own clock.
//!
//! This is the one design decision in this module that is not obvious, so it is
//! worth the paragraph. An event's position in a recording is the whole of its
//! usefulness (`docs/plugin-api.md`), and the session's timeline is the
//! capture clock's — which a separate process does not have. The two ways to
//! bridge that are a shared wall clock or a duration, and a duration wins on
//! every count: two processes reading the same wall clock disagree by whatever
//! NTP did to it in between, a clock step during a session moves every
//! subsequent event, and a plugin whose machine's time zone changed reports
//! events an hour into the future. A duration measured inside one process
//! against one monotonic clock has none of those failure modes.
//!
//! So the host reads its own clock at the moment the report arrives, subtracts
//! the plugin's `ago_ns`, and that is [`EventTiming::at`]. The same number is
//! the event's [`latency`](EventTiming::latency) — how much later than the
//! moment it describes the report arrived — because that is precisely what it
//! measures. One number from the plugin fills in both, and neither can be a
//! claim the plugin did not make: a plugin reporting an event the instant it
//! hears about it sends `ago_ns: 0` and gets an event at the moment it was
//! heard, which is honest, rather than an event at a moment it guessed.
//!
//! # A plugin never says who it is, either
//!
//! [`EventSource`] is stamped by the host from the manifest
//! ([`PluginId::as_source`](crate::PluginId::as_source)). There is no `source`
//! field on the wire, so a plugin cannot attribute a mark on a timeline to
//! `clipped`, or to another plugin, or to a game it is not integrating. That is
//! not a check that can be forgotten; it is a field that does not exist.

use core::fmt;
use core::time::Duration;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use clipped_events::{
    Confidence, EventKind, EventPayload, EventSource, EventTime, EventTiming, GameEvent,
    InvalidConfidence, PayloadTooLarge,
};

use crate::manifest::{ContractVersion, ObservedProcess, CONTRACT};

/// What the host tells a plugin.
///
/// `attach` is written once, immediately after the process starts. `detach` is
/// written when the session ends, and is followed by the plugin's standard
/// input being closed — so a plugin that ignores commands entirely still learns
/// that it is finished, by reading end of file, and a plugin whose host has
/// died learns the same thing the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HostCommand {
    /// A session this plugin supports has started.
    Attach {
        /// The contract version the host speaks.
        contract: ContractVersion,
        /// What it is attached to.
        session: SessionDetails,
    },
    /// The session has ended. Finish and exit.
    Detach,
}

impl HostCommand {
    /// The command as the line it is written as, newline included.
    ///
    /// # Panics
    ///
    /// Never in practice: the type serialises to a JSON object with no map keys
    /// that can fail, and a failure here would be a bug in this crate rather
    /// than anything a caller can act on.
    #[must_use]
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("a host command always serialises");
        line.push('\n');
        line
    }
}

/// What a plugin is told about the session it is attached to.
///
/// The session's own identifier and the process it is about, and nothing else:
/// not where recordings are being written, not the window title, not the
/// command line. A plugin needs to find the game's own interface — a log
/// directory under the executable, a port the game opens — and everything
/// beyond that is somebody's private machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionDetails {
    /// The session this plugin is attached to, for the plugin's own logging.
    pub session: String,
    /// The game process that started.
    pub process: ObservedProcess,
}

/// What a plugin says.
///
/// Unknown fields are ignored, and a report this build cannot read at all is a
/// protocol fault it counts rather than a reason to stop — the compatibility
/// policy `docs/ipc.md` sets out for the control protocol, applied to the same
/// kind of problem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "report", rename_all = "snake_case")]
pub enum PluginReport {
    /// The first thing a plugin says: which contract version it speaks.
    ///
    /// The manifest says the same thing, and both are checked, because they can
    /// disagree — a manifest is edited by hand and an executable is replaced by
    /// an update. The manifest is what the user consented to; this is what is
    /// actually running.
    Hello {
        /// The contract version the plugin speaks.
        contract: ContractVersion,
    },
    /// Something happened in the game.
    Event(ReportedEvent),
    /// Nothing has happened, and the plugin is still there.
    ///
    /// A plugin with nothing to report must say this at least as often as the
    /// host's silence timeout, or it is treated as hung and stopped
    /// (`crate::supervision`). A game can easily go a minute without an event,
    /// and the alternative — a host that assumes silence means health — cannot
    /// tell a quiet plugin from a deadlocked one.
    Alive,
    /// Something is wrong that the user can act on: a game's integration file
    /// is missing, a port is taken, a permission was refused.
    ///
    /// Reported rather than logged and forgotten, because an integration that
    /// silently never works is worse than one that says why (AGENTS.md section
    /// 45).
    Problem {
        /// What is wrong, in one line, in the words the user is shown.
        message: String,
    },
}

/// The most a plugin's problem message may carry.
///
/// It is shown to the user, so it is bounded for the same reason a manifest's
/// fields are (`crate::manifest`).
pub const MAX_PROBLEM_BYTES: usize = 240;

/// An event as a plugin reports it: what, how long ago, and how sure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportedEvent {
    /// What happened, as a standard tag or a namespaced custom name.
    pub kind: EventKind,
    /// How long before this report the event happened, in nanoseconds,
    /// measured on the plugin's own clock.
    ///
    /// Zero — the default — means "just now", which is the honest answer for a
    /// plugin that reports what it hears as it hears it.
    #[serde(default)]
    pub ago_ns: u64,
    /// How far either side of that moment the truth may lie, in nanoseconds.
    ///
    /// Required, and deliberately so: zero is the claim "I timed this exactly",
    /// and a plugin that never made that claim must not start making it by
    /// leaving a field out (`crates/events`).
    pub precision_ns: u64,
    /// How sure the plugin is that it happened at all, between 0 and 1.
    pub confidence: f32,
    /// The game's own vocabulary: `weapon`, `headshot`, `championKilled`.
    /// Nothing above the plugin interprets it.
    #[serde(default)]
    pub data: Map<String, Value>,
}

impl ReportedEvent {
    /// Turns a report into an event on the session's timeline.
    ///
    /// `source` is the plugin's identifier, stamped by the host. `received` is
    /// where the host's own clock was when the report arrived — see
    /// [`SessionTimeline`].
    ///
    /// # Errors
    ///
    /// [`ReportRefused`] when the plugin said something a producer may not say.
    /// Every one of these refuses **one event**; the plugin keeps running, and
    /// `crate::supervision` decides what a plugin that keeps doing it is worth.
    pub fn into_event(
        self,
        source: &EventSource,
        received: EventTime,
    ) -> Result<GameEvent, ReportRefused> {
        // The vocabulary rule, at the producer boundary. `crates/events` reads
        // an unknown unnamespaced kind out of a database as `Unrecognised` and
        // keeps it, because refusing there would delete a mark from a user's
        // timeline. Here the opposite applies: an unnamespaced name this build
        // does not define is a plugin claiming a word in the project's own
        // vocabulary, and the answer is a namespaced custom name. A plugin is
        // live code that can be told; a stored event is not.
        if let EventKind::Unrecognised(tag) = &self.kind {
            return Err(ReportRefused::UnknownKind { tag: tag.clone() });
        }

        // And a plugin may not label a mark as the *user's* own.
        // `EventKind::UserLabelled` is a name somebody typed into this
        // application (ADR 0010), and a timeline that cannot tell one of those
        // from a plugin's claim is a timeline where an integration can put words
        // in a user's mouth. This module's own documentation says a plugin
        // cannot attribute a mark to anybody else because `source` is a field
        // that does not exist on the wire — `kind` *is* a field a plugin
        // controls, so this is the check that makes the same promise of it.
        if let EventKind::UserLabelled(label) = &self.kind {
            return Err(ReportRefused::NotAPluginsToGive {
                tag: label.to_string(),
            });
        }

        let ago = Duration::from_nanos(self.ago_ns);
        let timing = EventTiming::new(
            received.saturating_sub(ago),
            Duration::from_nanos(self.precision_ns),
        )
        .reported_late_by(ago);

        let confidence = Confidence::new(self.confidence)
            .map_err(|source| ReportRefused::Confidence { source })?;
        let payload =
            EventPayload::new(self.data).map_err(|source| ReportRefused::Payload { source })?;

        Ok(GameEvent::new(self.kind, timing, source.clone(), confidence).with_data(payload))
    }
}

/// Why one reported event was refused.
///
/// Each of these is a plugin bug, and each names what the plugin author has to
/// change. None of them is a reason to lose the plugin: an integration that
/// reports one malformed event a match is still reporting the others.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportRefused {
    /// A kind with no namespace that this build does not define.
    UnknownKind {
        /// What the plugin sent.
        tag: String,
    },
    /// A kind reserved for something other than a plugin: a label the user
    /// typed, or a mark one of this application's own subsystems made.
    NotAPluginsToGive {
        /// What the plugin sent.
        tag: String,
    },
    /// A confidence outside 0 to 1.
    Confidence {
        /// The refusal from `crates/events`.
        source: InvalidConfidence,
    },
    /// A payload over `clipped_events::MAX_PAYLOAD_BYTES`.
    Payload {
        /// The refusal from `crates/events`.
        source: PayloadTooLarge,
    },
}

impl fmt::Display for ReportRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind { tag } => write!(
                formatter,
                "`{tag}` is not an event kind this build defines, and a plugin may not claim a \
                 name in the project's vocabulary: a plugin's own event is namespaced, as in \
                 `my-plugin.{tag}`"
            ),
            Self::NotAPluginsToGive { tag } => write!(
                formatter,
                "`{tag}` is a label the user gives an event, not one a plugin gives itself: a                  plugin's own event is namespaced, as in `my-plugin.something`"
            ),
            Self::Confidence { source } => write!(formatter, "{source}"),
            Self::Payload { source } => write!(formatter, "{source}"),
        }
    }
}

impl core::error::Error for ReportRefused {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::UnknownKind { .. } | Self::NotAPluginsToGive { .. } => None,
            Self::Confidence { source } => Some(source),
            Self::Payload { source } => Some(source),
        }
    }
}

/// The one place a plugin's timing becomes a position on a session's timeline.
///
/// # What it is
///
/// A session's timeline starts at the timestamp of the first video frame it
/// keeps (`docs/av-sync.md`). This holds the reading of *this process's*
/// monotonic clock at that moment, so that a report arriving later can be
/// placed on it: [`at`](Self::at) is the subtraction, written once, and
/// `docs/plugin-api.md` requires it to happen in exactly one named place.
///
/// # One per session, and it outlives any one recording
///
/// The name is load-bearing. A session that writes several files keeps **this**
/// timeline across all of them, so the second file's events are stamped from
/// the same zero as the first's and the second file occupies a span at a
/// positive offset. [Issue
/// #338](https://github.com/wildware-uk/clipped/issues/338) says the session
/// stamps every event through *one* of these, and that is why.
///
/// Building a fresh one per recording would compile, would never trip an
/// assertion, and would put every event of the second file into the first,
/// because `clipped_library::events` places a moment by asking which segment of
/// a single axis contains it. The wording here used to say "a recording's
/// timeline", which invited exactly that.
///
/// # The error it carries, stated
///
/// [`Instant`] is not the capture clock. Whoever attaches a plugin takes an
/// `Instant` beside the capture epoch, and the two readings are separated by
/// however long that takes — microseconds, and constant for the session, so
/// every event in a session is shifted by the same small amount rather than
/// drifting. That is comfortably inside the precision any of the three planned
/// integrations can claim: Game State Integration is posted on a configurable
/// interval measured in tens of milliseconds.
///
/// It is a duplication of the session's timeline all the same, which is what
/// [issue #253](https://github.com/wildware-uk/clipped/issues/253) exists to
/// end: a shared time crate that `clipped-capture` and this one both name would
/// let a session hand over its capture epoch rather than a second reading of a
/// second clock. Until then this is the third copy `crates/events` warned about,
/// and it is bounded the same way — one conversion, in one function, named.
#[derive(Debug, Clone, Copy)]
pub struct SessionTimeline {
    epoch: Instant,
}

impl SessionTimeline {
    /// A timeline whose zero is `epoch`.
    ///
    /// `epoch` is a reading of this process's monotonic clock taken as close as
    /// possible to the **session's** first kept frame -- that is, the first
    /// frame of its first recording -- and held for the whole session. A
    /// session that starts a second recording does not build a second timeline;
    /// see the type documentation for what that would break.
    #[must_use]
    pub const fn starting_at(epoch: Instant) -> Self {
        Self { epoch }
    }

    /// A timeline starting now.
    #[must_use]
    pub fn starting_now() -> Self {
        Self::starting_at(Instant::now())
    }

    /// Where `moment` sits on the session's timeline.
    ///
    /// Saturating at the limits of an `i64` of nanoseconds, which is 292 years
    /// either side of the epoch.
    #[must_use]
    pub fn at(&self, moment: Instant) -> EventTime {
        if moment >= self.epoch {
            let elapsed = moment.duration_since(self.epoch);
            EventTime::ZERO.saturating_add(elapsed)
        } else {
            // A moment before the epoch is normal rather than a fault: a plugin
            // attached to a game that was already running can describe the match
            // it joined, and `EventTime` is signed for exactly that reason.
            let before = self.epoch.duration_since(moment);
            EventTime::ZERO.saturating_sub(before)
        }
    }

    /// Where the session's timeline is now.
    #[must_use]
    pub fn now(&self) -> EventTime {
        self.at(Instant::now())
    }
}

/// Reads one line a plugin wrote.
///
/// # Errors
///
/// The `serde_json` failure, for the caller to count and log. A line that
/// cannot be read is not a reason to stop a plugin on its own — a plugin that
/// prints its own diagnostics on standard output produces one per line — and
/// `crate::supervision` owns the budget for how many is too many.
pub fn read_report(line: &str) -> Result<PluginReport, serde_json::Error> {
    serde_json::from_str(line)
}

/// Reads one line the host wrote.
///
/// The plugin's side of the wire, for a plugin written in Rust. A plugin
/// written in anything else parses the same object itself.
///
/// # Errors
///
/// The `serde_json` failure. A plugin that cannot read a command from its host
/// should say so and exit rather than guess.
pub fn read_command(line: &str) -> Result<HostCommand, serde_json::Error> {
    serde_json::from_str(line)
}

/// Writes one report as the line a plugin sends, newline included.
///
/// This is the plugin's side of the wire, and it is public because the plugins
/// in this repository use it: a plugin written in Rust should not be hand-
/// building JSON that this crate is about to parse. A plugin written in
/// anything else prints the same object itself.
///
/// # Panics
///
/// Never in practice, as [`HostCommand::to_line`].
#[must_use]
pub fn write_report(report: &PluginReport) -> String {
    let mut line = serde_json::to_string(report).expect("a plugin report always serialises");
    line.push('\n');
    line
}

/// The plugin's side of the handshake: the report a plugin sends first.
#[must_use]
pub fn hello() -> PluginReport {
    PluginReport::Hello { contract: CONTRACT }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use clipped_events::CustomName;

    use super::*;

    fn source() -> EventSource {
        EventSource::plugin("counter-strike-2").expect("a valid identifier")
    }

    fn reported(kind: EventKind) -> ReportedEvent {
        ReportedEvent {
            kind,
            ago_ns: 0,
            precision_ns: 0,
            confidence: 1.0,
            data: Map::new(),
        }
    }

    #[test]
    fn a_report_becomes_an_event_attributed_to_the_plugin() {
        let received = EventTime::from_media_nanos(61_500_000_000);
        let event = reported(EventKind::Kill)
            .into_event(&source(), received)
            .expect("a well-formed report");

        assert_eq!(event.kind(), &EventKind::Kill);
        assert_eq!(event.timing().at(), received);
        assert_eq!(event.source().as_str(), "counter-strike-2");
    }

    #[test]
    fn how_long_ago_becomes_both_the_moment_and_the_latency() {
        // The whole of the timing decision in one assertion: the event is
        // placed where it happened, and how late the report was is kept as a
        // separate, explicit fact rather than moving it.
        let received = EventTime::from_media_nanos(61_500_000_000);
        let mut report = reported(EventKind::Kill);
        report.ago_ns = 480_000_000;
        report.precision_ns = 100_000_000;

        let event = report
            .into_event(&source(), received)
            .expect("a well-formed report");
        assert_eq!(
            event.timing().at(),
            EventTime::from_media_nanos(61_020_000_000)
        );
        assert_eq!(event.timing().latency(), Duration::from_millis(480));
        assert_eq!(event.timing().precision(), Duration::from_millis(100));
        assert_eq!(event.timing().observed(), received);
    }

    #[test]
    fn a_plugin_cannot_put_a_mark_on_the_timeline_as_the_user() {
        // ADR 0010 gives a name somebody typed its own kind. A plugin sending
        // one would be an integration writing on a user's timeline in the
        // user's own hand, which is the same failure this module prevents for
        // `source` by leaving that field off the wire entirely — `kind` is a
        // field a plugin does control, so it takes a check instead.
        let label = clipped_events::UserLabel::new("clutch").expect("a valid label");
        let refusal = reported(EventKind::UserLabelled(label))
            .into_event(&source(), EventTime::ZERO)
            .expect_err("a user's label is not a plugin's to give");

        assert_eq!(
            refusal,
            ReportRefused::NotAPluginsToGive {
                tag: "clutch".to_owned()
            }
        );
        assert!(
            refusal.to_string().contains("my-plugin."),
            "the message should show the plugin author what to send instead: {refusal}"
        );
    }

    #[test]
    fn a_plugin_cannot_claim_a_word_in_the_projects_vocabulary() {
        // `crates/events` reads an unknown unnamespaced kind out of a database
        // and keeps it. A live plugin sending one is refused, because that is
        // the plugin inventing a standard event rather than an older build
        // meeting a newer one.
        let refusal = reported(EventKind::Unrecognised("kill_streak".to_owned()))
            .into_event(&source(), EventTime::ZERO)
            .expect_err("an unnamespaced unknown kind is refused");
        assert_eq!(
            refusal,
            ReportRefused::UnknownKind {
                tag: "kill_streak".to_owned()
            }
        );
        assert!(
            refusal.to_string().contains("my-plugin.kill_streak"),
            "the message should show the plugin author what to send instead: {refusal}"
        );

        // The namespaced form of the same idea is accepted, and keeps its name.
        let name = CustomName::new("acme-cs2.kill_streak").expect("a namespaced custom name");
        let event = reported(EventKind::Custom(name))
            .into_event(&source(), EventTime::ZERO)
            .expect("a custom event is what the rule asks for");
        assert_eq!(event.kind().as_str(), "acme-cs2.kill_streak");
    }

    #[test]
    fn a_confidence_or_payload_a_producer_may_not_send_refuses_one_event() {
        let mut nonsense = reported(EventKind::Kill);
        nonsense.confidence = 1.5;
        assert!(matches!(
            nonsense.into_event(&source(), EventTime::ZERO),
            Err(ReportRefused::Confidence { .. })
        ));

        let mut oversize = reported(EventKind::Kill);
        oversize.data.insert(
            "blob".to_owned(),
            json!("x".repeat(clipped_events::MAX_PAYLOAD_BYTES)),
        );
        assert!(matches!(
            oversize.into_event(&source(), EventTime::ZERO),
            Err(ReportRefused::Payload { .. })
        ));
    }

    #[test]
    fn there_is_no_way_for_a_plugin_to_send_a_source_at_all() {
        // Not "a source is overwritten": the field does not exist on the wire,
        // so a plugin cannot attribute a mark to `clipped` or to another
        // plugin. An attempt to send one is an unknown field, ignored.
        let line = r#"{"report":"event","kind":"kill","precision_ns":0,"confidence":1.0,
                       "source":"clipped"}"#;
        let PluginReport::Event(event) = read_report(line).expect("the line reads") else {
            panic!("expected an event report");
        };
        let event = event
            .into_event(&source(), EventTime::ZERO)
            .expect("a well-formed report");
        assert_eq!(
            event.source().as_str(),
            "counter-strike-2",
            "the source is the manifest's, and nothing the plugin sent"
        );
    }

    #[test]
    fn the_wire_is_one_json_object_per_line_in_both_directions() {
        let attach = HostCommand::Attach {
            contract: CONTRACT,
            session: SessionDetails {
                session: "2026-08-11-cs2".to_owned(),
                process: ObservedProcess::new("cs2.exe", 4242),
            },
        };
        let line = attach.to_line();
        assert!(line.ends_with('\n'), "a command is a line");
        assert_eq!(
            serde_json::from_str::<HostCommand>(line.trim_end()).expect("it reads back"),
            attach
        );

        assert_eq!(
            write_report(&hello()),
            "{\"report\":\"hello\",\"contract\":1}\n"
        );
        assert_eq!(
            read_report(r#"{"report":"alive"}"#).expect("it reads"),
            PluginReport::Alive
        );
        assert!(
            !read_report(r#"{"command":"detach"}"#)
                .expect_err("a command is not a report")
                .to_string()
                .is_empty(),
            "reading a command as a report should say what was wrong with it"
        );
    }

    #[test]
    fn an_event_line_is_the_shape_a_plugin_author_writes_by_hand() {
        let line = r#"{"report":"event","kind":"kill","ago_ns":480000000,"precision_ns":0,
                       "confidence":1.0,"data":{"weapon":"ak47"}}"#;
        let PluginReport::Event(event) = read_report(line).expect("the line reads") else {
            panic!("expected an event report");
        };
        assert_eq!(event.kind, EventKind::Kill);
        assert_eq!(event.ago_ns, 480_000_000);
        assert_eq!(event.data["weapon"], json!("ak47"));
    }

    #[test]
    fn a_precision_a_plugin_did_not_state_is_refused_rather_than_read_as_exact() {
        let refusal = read_report(r#"{"report":"event","kind":"kill","confidence":1.0}"#)
            .expect_err("precision is required");
        assert!(
            refusal.to_string().contains("precision_ns"),
            "the message should name the field: {refusal}"
        );
    }

    #[test]
    fn a_moment_on_the_timeline_is_measured_from_the_epoch() {
        let epoch = Instant::now();
        let timeline = SessionTimeline::starting_at(epoch);
        assert_eq!(timeline.at(epoch), EventTime::ZERO);
        assert_eq!(
            timeline.at(epoch + Duration::from_secs(61)),
            EventTime::from_media_nanos(61_000_000_000)
        );
        assert_eq!(
            timeline.at(epoch - Duration::from_millis(250)),
            EventTime::from_media_nanos(-250_000_000),
            "a plugin attached to a game that was already running can describe something \
             earlier than the first frame"
        );
    }
}
