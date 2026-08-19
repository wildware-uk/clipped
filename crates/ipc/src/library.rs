//! Reading the recording library across the process boundary.
//!
//! The desktop application cannot read the library index itself. It has no
//! file-system permission to open `library.db`, and it may not link
//! `clipped-library`: `tests/integration/tests/workspace_layering.rs` permits
//! `apps/desktop/src-tauri` exactly one member of the workspace, this crate,
//! which is what keeps the recording engine out of the window's process
//! ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)). So the
//! window asks the recorder, and these are the messages it asks with
//! ([issue #301](https://github.com/wildware-uk/clipped/issues/301)).
//!
//! # These are copies, not views
//!
//! Everything here crossed a process boundary and can be stale by the time it is
//! drawn, exactly as [`crate::status`] is. Nothing in a window is derived from
//! one of these after the fact.
//!
//! # What is deliberately not here
//!
//! - **Writing.** Favouriting, tagging and deleting are writes and belong with
//!   the screens that perform them
//!   ([issue #58](https://github.com/wildware-uk/clipped/issues/58),
//!   [issue #94](https://github.com/wildware-uk/clipped/issues/94)). This is the
//!   read.
//! - **Thumbnails and waveforms.** A picture is bytes rather than a row, and a
//!   page of rows is not where it belongs: a library page carries twenty-five
//!   sittings and a picture is 20 kB, so putting one on each row would be a
//!   page that no longer fits in a frame. They are [`crate::preview`] instead —
//!   their own command, asked one recording at a time as a row is drawn
//!   ([issue #448](https://github.com/wildware-uk/clipped/issues/448) took that
//!   decision; [#57](https://github.com/wildware-uk/clipped/issues/57) and
//!   [#66](https://github.com/wildware-uk/clipped/issues/66) are what draw
//!   them).
//!
//! # `missing_since` is the field this exists for
//!
//! A recording whose file has gone is *listed*, carrying
//! [`LibraryRecording::missing_since`]. A library screen has to say "this file
//! has gone" rather than draw a broken tile (AGENTS.md section 27), and it can
//! only do that if the fact crosses the boundary. Omitting the row would leave
//! the window unable to tell a deleted recording from one that never existed.
//!
//! # Identifiers are numbers, and that has a ceiling
//!
//! [`LibraryRecording::recording_id`] is the index's own `INTEGER PRIMARY KEY`,
//! carried as a JSON number. JSON numbers are `f64` in every browser, so
//! identifiers stay exact up to 2^53 — nine thousand million million
//! recordings. A library reaches the ceiling of the disk it is on some time
//! before that.

use serde::{Deserialize, Serialize};

/// Asking for the game events of one recording.
///
/// Placed events, not stored ones: the offset is into *that recording's file*,
/// which is what a timeline draws at and a player seeks to. Turning a moment on
/// a session's timeline into one of those needs the recording's span, which the
/// window has no way to know, so the recorder does it
/// ([issue #329](https://github.com/wildware-uk/clipped/issues/329)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEvents {
    /// The recording to draw a lane for, as the library identifies it.
    pub recording: String,
}

/// One mark on a recording's timeline.
///
/// # Two rules this shape keeps
///
/// **`kind` is not a closed vocabulary on the wire.** A kind added after the
/// window shipped, and a plugin's namespaced custom name, must both arrive and
/// both be drawn -- a protocol validating against a list would delete exactly
/// the marks that need to survive. It is the string
/// `clipped_events::EventKind::as_str` produces, and nothing here checks it
/// against anything.
///
/// **The payload does not travel.** A plugin's own detail can be kilobytes and
/// nothing above the plugin interprets it, so there is no reason for it to
/// cross this boundary for a mark two pixels wide. It stays in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryEventMark {
    /// The recording this mark is on, as the library identifies it.
    pub recording: String,
    /// How far into that recording's file the event is, in nanoseconds.
    pub at: i64,
    /// What happened, as an open string.
    pub kind: String,
    /// Who reported it: a plugin's identifier, or `clipped` for the parts of
    /// the application that report events themselves.
    pub source: String,
}

/// The marks of one recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryEventLane {
    /// The marks, earliest first.
    ///
    /// **Empty means none, and is not the same as not having asked.** A window
    /// that cannot tell those apart has to guess whether to draw an empty lane
    /// or say nothing is known, and the two are different things to show
    /// somebody (AGENTS.md section 27). This field is always present, so
    /// receiving the reply at all is the answer to "was it asked".
    pub marks: Vec<LibraryEventMark>,
}

/// Which page of the library to read.
///
/// Every field is optional: a window opening on the newest sessions sends no
/// parameters at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySessions {
    /// How many sessions to answer with.
    ///
    /// The recorder clamps this to what one frame can carry, so a caller that
    /// asks for the whole library gets a page and a cursor rather than a
    /// refusal. Absent means the recorder's own page size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Continue after the session this cursor names.
    ///
    /// It is [`LibrarySessionPage::next_cursor`] from the previous reply and is
    /// opaque: the only thing a caller may do with one is send it back. A cursor
    /// the recorder cannot read starts at the newest session rather than
    /// refusing, because a window may have kept one across a restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Only the sessions this search query selects.
    ///
    /// The language is `docs/search.md` — `game:cs2 tag:clutch -favourite` and
    /// the rest — parsed by the recorder. A query that does not parse is refused
    /// with [`ErrorCode::InvalidParameters`](crate::ErrorCode::InvalidParameters)
    /// carrying the position and what was expected there, so a search box can
    /// show what is wrong with what was typed rather than an empty result set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// One page of the library, newest session first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibrarySessionPage {
    /// The sessions on this page.
    ///
    /// Empty means the library holds no more sessions matching the request —
    /// which is a different thing from the library being unreadable, and the
    /// two must stay different: an unreadable library is
    /// [`ErrorCode::LibraryUnavailable`](crate::ErrorCode::LibraryUnavailable)
    /// and says why.
    pub sessions: Vec<LibrarySession>,
    /// The cursor for the page after this one.
    ///
    /// Absent at the end of the library. It is present only when a further
    /// session was actually found, so a caller stops on this rather than on an
    /// empty page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// One sitting, and what it produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibrarySession {
    /// The identifier the recorder generated, shared by the sidecar and the
    /// files.
    pub session_id: String,
    /// The catalogue's identifier for the game.
    ///
    /// Absent for a sitting the catalogue would not attribute — it reported a
    /// tie and the recording was filed under no game rather than under a guess
    /// (`docs/sessions.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    /// The game's name as the catalogue knew it when it was played.
    ///
    /// Absent for the same reason. What to call that group on screen is the
    /// screen's decision, not the protocol's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_name: Option<String>,
    /// When the sitting started, RFC 3339 with the offset it was recorded in.
    pub started_at: String,
    /// When it ended. Absent for a sitting that has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// Why it ended: `game-exited`, `system-resumed`, `recorder-stopping`, or
    /// `recording-ended` for a sitting that was one recording somebody asked for
    /// (`docs/sessions.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// Whether the user favourited the sitting itself (SPEC.md section 29).
    pub favourite: bool,
    /// Whether the user locked the sitting against automatic cleanup.
    ///
    /// A locked sitting protects every recording in it, so a screen drawing a
    /// padlock against a recording reads that recording's
    /// [`LibraryRecording::protected`] rather than working the cascade out from
    /// this (issue #472).
    ///
    /// Defaulted, so a page from a recorder older than the lock column parses
    /// as "nothing is locked" rather than failing: that build has no locks, and
    /// a window refusing its library would be a worse answer than an open
    /// padlock.
    #[serde(default)]
    pub locked: bool,
    /// The files it recorded, in the order they were recorded.
    pub recordings: Vec<LibraryRecording>,
    /// The clips cut from it.
    pub clips: Vec<LibraryClip>,
}

/// One media file a sitting produced.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryRecording {
    /// The index's own identifier for it.
    pub recording_id: i64,
    /// Its ordinal within the sitting, as the sidecar recorded it.
    pub session_index: u32,
    /// The file. A window cannot open it; it is what a "reveal in Explorer" and
    /// a support request name.
    pub path: String,
    /// When it started, RFC 3339 with an offset.
    pub started_at: String,
    /// When it ended. Absent for one that was still running when the library was
    /// last reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// What became of it: `recorded`, `no-window` or `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Why it ended, and only meaningful for `recorded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
    /// How long it runs for. Absent for a recording that produced no file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// The encoded picture width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// The encoded picture height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// What the file occupied when it was last seen.
    ///
    /// Kept while [`Self::missing_since`] is set, so a drive coming back needs
    /// no re-measurement — but a screen must not add it into a total meanwhile,
    /// because that space is not being used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// When the library first found the file gone.
    ///
    /// Absent while the file is there. Present is the state a screen has to
    /// *say* rather than fail on (AGENTS.md section 27).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_since: Option<String>,
    /// Whether the user favourited it.
    pub favourite: bool,
    /// Whether the user locked this recording itself.
    ///
    /// Its own lock only, which is what a *control* is drawn from: a recording
    /// inside a locked sitting has nothing of its own to release (issue #472).
    #[serde(default)]
    pub locked: bool,
    /// Whether automatic cleanup will leave it alone.
    ///
    /// [`Self::locked`] or its sitting's lock, worked out by the recorder so
    /// that the cascade has one expression rather than one here and one in
    /// every window. This is what a padlock is drawn from.
    #[serde(default)]
    pub protected: bool,
    /// The tags on it, alphabetically.
    pub tags: Vec<String>,
}

/// One clip cut from a sitting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryClip {
    /// The index's own identifier for it.
    pub clip_id: i64,
    /// The file, when there is one.
    ///
    /// **Absent for a clip nothing has exported yet**, which is the normal
    /// state of a generated highlight: it is a range of a recording until
    /// somebody asks for a file, and asking is what makes one. A window has to
    /// draw that clip — it is a clip the user made — with whatever it offers
    /// instead of "reveal in Explorer".
    ///
    /// Absent is **not** [`Self::missing_since`]. No path is "there is no file
    /// yet"; `missing_since` is "there was one and it has gone". A screen that
    /// conflated them would tell somebody a highlight had been lost when
    /// nothing had ever been written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// What it is called, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// When it was made, RFC 3339 with an offset.
    pub created_at: String,
    /// How long it runs for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// What the file occupies, with the same caveat as a recording's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// When the library first found the file gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_since: Option<String>,
    /// Whether the user favourited it.
    ///
    /// There is no `locked` beside it, deliberately: automatic cleanup deletes
    /// recordings and never clips, so a lock here would be a mark nothing
    /// consults (`clipped_library::locks`).
    pub favourite: bool,
    /// The tags on it, alphabetically.
    pub tags: Vec<String>,
}

/// What the library holds for one game.
///
/// SPEC.md section 17's games view: four numbers under a name, plus how many of
/// the files behind them could not be found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryGame {
    /// The catalogue's identifier, or absent for the sittings nothing was
    /// attributed to. There is one such row at most, and it is last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    /// The name as the catalogue knew it when the game was last played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When the first sitting of this game was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
    /// When the most recent one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_played_at: Option<String>,
    /// Sittings recorded.
    pub sessions: u64,
    /// Recordings that are not in the trash.
    pub recordings: u64,
    /// Clips that are not in the trash.
    pub clips: u64,
    /// Sessions, recordings and clips the user has favourited.
    pub favourites: u64,
    /// What the files that are still there occupy.
    ///
    /// A missing file contributes nothing: the space it is not occupying is not
    /// being used, and a library reporting 83 GB of files nobody can find would
    /// be a lie (`docs/library.md`).
    pub bytes: u64,
    /// Recordings and clips whose file could not be found when the library was
    /// last reconciled.
    pub missing: u64,
}

/// Asking for one clip's edit document.
///
/// The document is what a clip *is* — which recordings to play, which parts of
/// them, in which order, how loud each track is and what is drawn over the
/// picture — and it is stored as text in a column of the library index
/// (`docs/editing.md`, "Where a document lives"). The window cannot read that
/// column: it has no file-system permission and may not link
/// `clipped-library` or `clipped-edit`, which
/// `tests/integration/tests/workspace_layering.rs` asserts. So it asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryClipDocument {
    /// The clip to open, as the library identifies it.
    ///
    /// A string carrying [`LibraryClip::clip_id`], for the reason
    /// [`LibraryEvents::recording`] is one: the identifier a window was given
    /// travels back exactly as it arrived, rather than being turned into a
    /// number and back on the way.
    pub clip: String,
}

/// One clip's edit document, as text.
///
/// # Why text and not a parsed document
///
/// `docs/editing.md` settles it: "a document crosses the IPC boundary as the
/// same JSON, rather than being converted into a second representation for the
/// desktop application". Restating the model here would be a second
/// implementation of it (AGENTS.md section 55) that could disagree with
/// `crates/edit` — and this crate may not link `clipped-edit` to prevent that,
/// because both are at layer 0 of README.md's dependency table.
///
/// So this carries the string, and the window's own reader
/// (`apps/desktop/src/editor/document.ts`) is what parses it.
///
/// # What the text is guaranteed to be
///
/// **The version this recorder writes**, always. A stored document older than
/// the recorder's build is converted before it is sent, and
/// [`Self::converted_from`] says so; one *newer* than the recorder's build is
/// refused rather than sent, because a recorder that cannot read a document
/// cannot honestly hand it to a window either.
///
/// That is what makes the window's own refusal of an older document correct
/// rather than a limitation: it never sees one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipDocument {
    /// The clip this is, echoed from the request.
    ///
    /// Echoed so that a window which asked about two clips can tell the
    /// answers apart without holding the question, which is what
    /// [`LibraryEventMark::recording`] is for on a mark.
    pub clip: String,
    /// The document itself, as `clipped_edit::EditDocument::write` writes it.
    pub document: String,
    /// The format the *stored* text was in, when it was older than this one.
    ///
    /// Absent is the ordinary case: the stored text was already the version
    /// this build writes and nothing was converted.
    ///
    /// **Present does not mean anything was written.** Reading converts in
    /// memory only; the stored text is still the older one, and stays that way
    /// until somebody saves. It is here because a window that knows the clip it
    /// is showing came from an older format can say so, and because the same
    /// number comes back from a save as
    /// [`ClipDocumentSaved::superseded`] — the moment the original is actually
    /// kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converted_from: Option<u32>,
    /// Whether the library held no document for this clip and this is a
    /// starting one.
    ///
    /// True for a saved replay: a clip made before there was an editor is a
    /// file and a window of a recording, with `clips.edit` still NULL. The
    /// recorder builds the document that means "this recording, this span, no
    /// edits" rather than leaving the window to invent one, because two builds
    /// inventing it separately would disagree about what an unedited clip is.
    ///
    /// **Nothing has been stored.** The clip's row is untouched until a save,
    /// so a user who opens a replay and closes it again has changed nothing.
    pub synthesised: bool,
}

/// Storing an edited document against a clip.
///
/// The other direction of [`LibraryClipDocument`], and the one with the
/// data-safety rule attached. Nothing here touches a recording: a clip is
/// metadata over recordings that are never modified, moved, truncated or
/// re-encoded because somebody edited one (AGENTS.md sections 56 and 57).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveClipDocument {
    /// The clip to store it against, as the library identifies it.
    pub clip: String,
    /// The document, as the window has it.
    ///
    /// Validated before anything is written — `crates/edit` validates on every
    /// read *and* every write — so text that is not a document this build can
    /// read is refused and the stored one is left exactly as it was. A window
    /// cannot corrupt a clip by sending nonsense; it can only fail to save.
    pub document: String,
}

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipDocumentSaved {
    /// The clip, echoed from the request.
    pub clip: String,
    /// The format the text this save replaced was in, when it was older than
    /// the one now stored and was therefore **kept**.
    ///
    /// This is `docs/editing.md`'s rule paid: "the caller decides whether to
    /// store the result, and must keep the original when it does". Present
    /// means a copy of the older text is in the index beside the new one and
    /// will not be overwritten by a later save. Absent means there was nothing
    /// older to keep — either the stored text was already this version, or
    /// there was no stored text at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_that_asks_for_nothing_in_particular_reaches_the_wire_as_an_empty_object() {
        // What a window opening on the newest sessions sends. A `limit` that
        // serialised as null would be a page size nobody chose.
        let json = serde_json::to_string(&LibrarySessions::default()).expect("it serialises");
        assert_eq!(json, "{}");
        assert_eq!(
            serde_json::from_str::<LibrarySessions>("{}").expect("and deserialises"),
            LibrarySessions::default()
        );
    }

    #[test]
    fn a_recording_that_is_present_says_nothing_about_being_missing() {
        // The absence is the state: `missing_since` present means gone, and a
        // null on the wire would make every recording look like it carried the
        // field.
        let recording = LibraryRecording {
            recording_id: 1,
            session_index: 1,
            path: r"D:\clips\session.mkv".to_owned(),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            ..LibraryRecording::default()
        };

        let json = serde_json::to_string(&recording).expect("it serialises");
        assert!(
            !json.contains("missing_since"),
            "a recording that is there should not carry a null: {json}"
        );
        assert_eq!(
            serde_json::from_str::<LibraryRecording>(&json).expect("and deserialises"),
            recording
        );
    }

    #[test]
    fn a_recording_whose_file_has_gone_carries_when_it_went_and_what_it_was() {
        let recording = LibraryRecording {
            recording_id: 7,
            session_index: 2,
            path: r"D:\clips\gone.mkv".to_owned(),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            size_bytes: Some(4_812_009),
            missing_since: Some("2026-08-12T09:00:00+01:00".to_owned()),
            ..LibraryRecording::default()
        };

        let json = serde_json::to_string(&recording).expect("it serialises");
        let back: LibraryRecording = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(
            back.missing_since.as_deref(),
            Some("2026-08-12T09:00:00+01:00")
        );
        assert_eq!(
            back.size_bytes,
            Some(4_812_009),
            "the size it had has to survive, or a drive coming back needs re-measuring"
        );
    }

    #[test]
    fn an_empty_page_is_a_page_rather_than_a_missing_field() {
        // The distinction issue #305's last acceptance criterion turns on: an
        // empty library and a library that could not be read must not look the
        // same. This is the empty one, and it is a successful reply.
        let json = serde_json::to_string(&LibrarySessionPage::default()).expect("it serialises");
        assert_eq!(json, r#"{"sessions":[]}"#);
    }

    #[test]
    fn a_page_round_trips_with_everything_a_session_can_carry() {
        let page = LibrarySessionPage {
            sessions: vec![LibrarySession {
                session_id: "cs2-20260811-201400".to_owned(),
                game_id: Some("cs2".to_owned()),
                game_name: Some("Counter-Strike 2".to_owned()),
                started_at: "2026-08-11T20:14:00+01:00".to_owned(),
                ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
                end_reason: Some("game-exited".to_owned()),
                favourite: true,
                locked: false,
                recordings: vec![LibraryRecording {
                    recording_id: 12,
                    session_index: 1,
                    path: r"D:\clips\session.mkv".to_owned(),
                    started_at: "2026-08-11T20:14:00+01:00".to_owned(),
                    ended_at: Some("2026-08-11T22:03:00+01:00".to_owned()),
                    outcome: Some("recorded".to_owned()),
                    end_reason: Some("target-lost".to_owned()),
                    duration_seconds: Some(6_540.0),
                    width: Some(2560),
                    height: Some(1440),
                    size_bytes: Some(9_812_009_112),
                    missing_since: None,
                    favourite: false,
                    locked: true,
                    protected: true,
                    tags: vec!["clutch".to_owned()],
                }],
                clips: vec![LibraryClip {
                    clip_id: 3,
                    path: Some(r"D:\clips\ace.mkv".to_owned()),
                    title: Some("Ace on Mirage".to_owned()),
                    created_at: "2026-08-11T21:02:00+01:00".to_owned(),
                    duration_seconds: Some(30.0),
                    size_bytes: Some(48_120_091),
                    missing_since: None,
                    favourite: true,
                    tags: vec![],
                }],
            }],
            next_cursor: Some("2026-08-11T20:14:00+01:00|cs2-20260811-201400".to_owned()),
        };

        let json = serde_json::to_string(&page).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<LibrarySessionPage>(&json).expect("and deserialises"),
            page
        );
    }

    #[test]
    fn a_field_added_after_this_build_was_compiled_is_ignored_rather_than_fatal() {
        // The additive half of the compatibility policy in docs/ipc.md, for
        // these payloads as much as for a status.
        let session: LibrarySession = serde_json::from_str(
            r#"{"session_id":"s-1","started_at":"2026-08-11T20:14:00+01:00","favourite":false,
                "recordings":[],"clips":[],"invented_later":{"anything":1}}"#,
        )
        .expect("an unknown field must not break a known message");
        assert_eq!(session.session_id, "s-1");
    }

    #[test]
    fn an_unattributed_sitting_carries_no_game_rather_than_an_empty_one() {
        // The catalogue refused to guess which game it was, and so does this:
        // an empty string here would be a game called "".
        let session = LibrarySession {
            session_id: "unattributed-20260811-201400".to_owned(),
            started_at: "2026-08-11T20:14:00+01:00".to_owned(),
            ..LibrarySession::default()
        };
        let json = serde_json::to_string(&session).expect("it serialises");
        assert!(!json.contains("game_id"), "{json}");
        assert!(!json.contains("game_name"), "{json}");
    }
}

/// Asking what is in the trash.
///
/// No paging: the trash is what a user deleted and has not emptied, bounded by
/// the retention period rather than by the size of the library. A library with
/// ten thousand sittings in it can have an empty trash, and one that needs
/// paging is one whose owner has bigger problems than a screen — if that turns
/// out to be wrong, a cursor is added the way [`LibrarySessions`] has one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryTrash {}

/// One thing waiting in the trash.
///
/// The window may not link `clipped-library`, so this is the projection of
/// `clipped_library::trash::TrashEntry` that a screen needs: what it was, when
/// it went, when it expires, and what it would cost to keep or free
/// ([issue #450](https://github.com/wildware-uk/clipped/issues/450)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashedItem {
    /// `recording` or `clip`, which is what a screen groups by.
    pub kind: String,
    /// The library's own identifier for it, which `restore_from_trash` names.
    pub id: i64,
    /// Where the file is **now**, inside the trash.
    ///
    /// Absent for an item that has no file: a clip nothing has exported is a
    /// range of a recording, and deleting it deletes the clip rather than a
    /// file ([issue #593](https://github.com/wildware-uk/clipped/issues/593)).
    /// Absent rather than `null` or `""`, because an empty string is a file
    /// name a window would try to open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Where it was, and where restoring puts it back.
    ///
    /// The one a person recognises: a file's name inside the trash is not what
    /// they deleted, and a screen that showed only that would be asking them to
    /// identify their own recording by a name they have never seen.
    ///
    /// Absent for the same reason [`Self::path`] is: an item that never had a
    /// file was never anywhere, and a screen names it by what it is instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    /// When it was deleted, RFC 3339 with an offset.
    pub deleted_at: String,
    /// When it will be removed for good, RFC 3339, or absent when the retention
    /// period is not known here.
    ///
    /// Absent rather than computed in the window: how long the trash keeps
    /// things is the recorder's setting, and a screen that worked it out itself
    /// would be a second answer that could disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// What it occupied when the index last saw it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    /// How many clips were cut from this recording, and are now pointing at a
    /// file that is not where they think it is.
    ///
    /// Zero for a clip. A screen says so before somebody empties the trash,
    /// because that is the moment those clips stop being recoverable.
    pub dependent_clips: u32,
}

/// What is in the trash, and what emptying it would take.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashListing {
    /// Everything in it, newest deletion first.
    pub items: Vec<TrashedItem>,
    /// How many there are, which is half of what `empty_trash` confirms.
    pub total_items: u64,
    /// What they occupy, which is the other half.
    ///
    /// The pair travels because `empty_trash` takes it back: a window that
    /// showed "3 recordings, 12 GB" and then emptied a trash that had gained a
    /// fourth is refused rather than deleting something nobody saw
    /// (`clipped_library::trash::EmptyTrash`).
    pub total_bytes: u64,
    /// Where the trash directory is, so that a screen can say where the files
    /// went rather than only that they went.
    pub directory: String,
}

/// Putting one thing back where it was.
///
/// Named by kind and identifier, which is what a listing gave the window. A
/// path would be the wrong key: the file inside the trash is not the thing the
/// index knows about, and two recordings can have had the same name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFromTrash {
    /// `recording` or `clip`, as [`TrashedItem::kind`] gave it.
    pub kind: String,
    /// The library's own identifier for it.
    pub id: i64,
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoredItem {
    /// Which one it was.
    pub kind: String,
    /// Its identifier.
    pub id: i64,
    /// Where the file is now, which is where the index now points.
    ///
    /// Absent for an item that has no file — a clip nothing has exported —
    /// which comes back with none, exactly as it went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether there was a file to move back.
    ///
    /// `false` for something whose media had already gone before it was
    /// deleted, and for something that never had any: the row returns to the
    /// library and reports itself missing or fileless, which is the truth
    /// rather than a row with no explanation.
    pub file_restored: bool,
    /// Whether it had to go somewhere other than where it came from, because
    /// something else was there.
    pub renamed: bool,
}

/// Emptying the trash, confirmed against the listing it was shown.
///
/// **Both numbers are required and both are checked.** They are the listing the
/// user was looking at when they pressed the button: if the trash has gained an
/// item since, the recorder refuses rather than deleting something nobody saw
/// (`clipped_library::trash::EmptyTrash`). That is why this is not a boolean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmptyTrash {
    /// How many things the listing held.
    pub items: u64,
    /// What the listing said they occupied.
    pub bytes: u64,
}

/// What emptying the trash destroyed, and what it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashEmptied {
    /// How many things were destroyed.
    pub removed: u64,
    /// What the volume got back.
    pub reclaimed_bytes: u64,
    /// What could not be destroyed, each saying why.
    ///
    /// Always sent. A file a virus scanner had open is a real outcome, and the
    /// next sweep tries it again; a window that showed only the count would say
    /// the trash is empty when it is not.
    pub refused: Vec<String>,
}

/// Marking one thing a favourite, or clearing the mark.
///
/// [Issue #58](https://github.com/wildware-uk/clipped/issues/58). Every read in
/// this protocol has carried `favourite` since the library screen was built, the
/// database has carried `favourited_at` since its first migration, and
/// `clipped_library::favourites` has known how to write one — and there was no
/// way to *ask*. This is that.
///
/// # Why the target takes two fields
///
/// A sitting is addressed by the identifier the recorder generated, which is
/// text; a recording and a clip are addressed by the integer key the index gave
/// them. That is the schema, not a choice made here, and one field pretending to
/// be both would be a number written as a string half the time.
///
/// So exactly one of them is meaningful, and which one is decided by [`Self::kind`]:
///
/// | `kind` | Addressed by |
/// | --- | --- |
/// | `session` | [`Self::session_id`], and [`Self::id`] is ignored |
/// | `recording` | [`Self::id`] |
/// | `clip` | [`Self::id`] |
///
/// A request whose target is not filled in is refused, naming the field that was
/// missing, rather than silently favouriting whatever row happens to be at zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetFavourite {
    /// `session`, `recording` or `clip`.
    pub kind: String,
    /// The sitting's own identifier, for `session`.
    pub session_id: String,
    /// The library's integer identifier, for `recording` and `clip`.
    pub id: i64,
    /// Whether it should be a favourite afterwards.
    ///
    /// The state to be in, not a toggle. Two windows open on the same library
    /// would disagree about what a toggle means, and a screen that has just
    /// drawn a star already knows which way it points.
    pub favourite: bool,
}

/// Locking one thing against automatic cleanup, or unlocking it.
///
/// [Issue #472](https://github.com/wildware-uk/clipped/issues/472). The same
/// target shape as [`SetFavourite`], and for the same reason — a sitting is
/// addressed by text and a recording by an integer — but a shorter vocabulary:
/// a clip cannot be locked, because automatic cleanup deletes recordings and a
/// mark nothing consults is worse than no mark at all
/// (`clipped_library::locks`).
///
/// # What it protects against
///
/// Automatic cleanup, and nothing else. A locked recording is deleted by a
/// manual delete exactly as an unlocked one is. That is the decision recorded on
/// the issue, and a window must not imply otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetLock {
    /// `session` or `recording`.
    pub kind: String,
    /// The sitting's own identifier, for `session`.
    pub session_id: String,
    /// The library's integer identifier, for `recording`.
    pub id: i64,
    /// Whether it should be locked afterwards.
    ///
    /// The state to be in rather than a toggle, for the reason
    /// [`SetFavourite::favourite`] is.
    pub locked: bool,
}

/// What the lock is now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockMark {
    /// Which thing it was, echoed so a window can match the reply to the row.
    pub kind: String,
    /// The sitting's identifier, for `session`.
    pub session_id: String,
    /// The integer identifier, for `recording`.
    pub id: i64,
    /// Whether it has a lock **of its own** now.
    ///
    /// Read back after the write, so a request against a row that has gone
    /// comes back `false` rather than as an acknowledgement.
    pub locked: bool,
    /// Whether automatic cleanup will leave it alone.
    ///
    /// Not the same question as [`Self::locked`], and this is the one a padlock
    /// on a screen should be drawn from: a recording inside a locked sitting is
    /// protected without having a lock of its own. Always `true` when
    /// [`Self::locked`] is; also `true` for a recording whose sitting is
    /// locked. For a `session` target the two are the same.
    pub protected: bool,
    /// Whether this request is what changed it.
    pub changed: bool,
}

/// What the mark is now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FavouriteMark {
    /// Which thing it was, echoed so a window can match the reply to the row.
    pub kind: String,
    /// The sitting's identifier, for `session`.
    pub session_id: String,
    /// The integer identifier, for `recording` and `clip`.
    pub id: i64,
    /// Whether it is a favourite now.
    ///
    /// The state after the write rather than what was asked for. They are the
    /// same unless the row has gone, and a screen should draw what is true.
    pub favourite: bool,
    /// Whether this request is what changed it.
    ///
    /// `false` for a star that was already full. Worth sending: it is the
    /// difference between "you did that" and "that was already so", and a
    /// second click on a full star must not look like a failure.
    pub changed: bool,
}
