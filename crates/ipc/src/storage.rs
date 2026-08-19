//! What the library occupies, what a limit would take, and what it would keep,
//! as the Storage screen reads it.
//!
//! # Why this is on the protocol at all
//!
//! SPEC.md section 27 makes storage a product feature: a maximum size, a minimum
//! amount of free disk space and a maximum recording age, with favourites and
//! locked footage never deleted. All of that is measured and acted on —
//! `clipped_library::accounting` walks the library, `clipped_session::cleanup`
//! decides what a limit would take, and the recorder's indexer runs the sweep
//! after every reconciliation.
//!
//! **None of it could be seen from the window.** The measurement is a filesystem
//! walk and a read of the index, and the desktop application may link neither
//! `clipped-library` nor `clipped-storage` and has no file-system permission —
//! the same transport question every library read has had. So the figures
//! reached one caller, `clipped-recorder storage`
//! ([issue #529](https://github.com/wildware-uk/clipped/issues/529)), and a
//! screen SPEC.md section 27 asks for could draw none of them
//! ([issue #95](https://github.com/wildware-uk/clipped/issues/95)).
//!
//! [`StorageReport`] is that measurement, projected onto the wire.
//!
//! # The dry run is the point, not a convenience
//!
//! Setting a maximum usage is the one setting in this application that **deletes
//! somebody's recordings**. A control that silently does that is the failure
//! AGENTS.md section 56 is about, so [`GetStorage::limits`] asks the question the
//! other way round: *given these limits, what would go?* The recorder answers it
//! with `clipped_session::cleanup::preview`, which is the same function the
//! sweep takes its measurement from — so what a window shows before the limit is
//! saved cannot disagree with what happens after it is (AGENTS.md section 55).
//!
//! Nothing here deletes, trashes or moves anything. There is no `apply`: the
//! limits are saved through `apply_settings` like every other setting, and the
//! sweep is what acts on them.
//!
//! # Why the lists are bounded, and what stands in for the rest
//!
//! A library of ten thousand recordings would not fit in a frame, and a screen
//! cannot draw ten thousand rows anyway. Two different bounds, for two different
//! questions:
//!
//! - **What would go**, and **what is filling the drive**, are lists somebody
//!   reads top-down, so they are the first [`MOST_LISTED`] of each with
//!   [`RecordingList::total`] and [`RecordingList::total_bytes`] carrying what
//!   the list does not show. A truncated list that did not say so would read as
//!   the whole answer.
//! - **What is never deleted** is not a list at all. Nobody wants to scroll ten
//!   thousand protected recordings; they want to know that favourites and locked
//!   footage are safe and how much of the disk that accounts for. So it crosses
//!   as [`ProtectedGroup`] — one row per rule, with a count and a size.
//!
//! # Why these are wire types rather than the accounting module's own
//!
//! This crate depends on no other crate of the workspace and may not start to
//! (ADR 0002, `tests/integration/tests/workspace_layering.rs`). `apps/recorder`
//! maps `clipped_session::cleanup::Measurement` onto these, exactly as it maps
//! `clipped_capture::CaptureStatus` onto
//! [`CaptureAccount`](crate::diagnostics::CaptureAccount).
//!
//! # What is deliberately not here
//!
//! **No per-game attribution.** `library_games` already carries what each game
//! occupies and the Home screen already draws it; a second answer to the same
//! question from a second measurement is the two-opinions problem AGENTS.md
//! section 55 warns about.
//!
//! **No retention date on anything.** Nothing configures how long the trash
//! keeps something (SPEC.md section 28's 3, 7 and 30 days), which is why
//! [`TrashedItem::expires_at`](crate::library::TrashedItem::expires_at) is absent
//! from every item this build sends, and a date computed from a policy nobody set
//! would be a screen promising a deletion it cannot keep.

use serde::{Deserialize, Serialize};

/// How many recordings a list in this report names before it stops.
///
/// Enough to fill a panel and to answer "what is filling my drive"; far short of
/// a frame. What is left out is not hidden — [`RecordingList::total`] and
/// [`RecordingList::total_bytes`] are of the whole set, and a window says so.
pub const MOST_LISTED: usize = 25;

/// What a library may occupy, as a window reads and proposes it.
///
/// Every field is optional and absent means **no limit of that kind**, which is
/// what Clipped ships with. That reading is the same in both directions: absent
/// in a [`StorageReport`] is a limit nobody has configured, and absent in a
/// [`GetStorage::limits`] is a limit the window is asking about the removal of.
///
/// Bytes rather than gigabytes, and days rather than a duration, because that is
/// how `settings.json` spells them (`clipped_session::config::storage`) and a
/// second unit on the wire is a second place for a factor of 1024 to be wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageLimits {
    /// What the library may occupy, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_usage_bytes: Option<u64>,
    /// What must stay free on the volume, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_free_space_bytes: Option<u64>,
    /// How old a recording may get, in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_age_days: Option<u64>,
}

impl StorageLimits {
    /// Whether nothing is limited, which is what Clipped ships with.
    #[must_use]
    pub const fn is_unlimited(&self) -> bool {
        self.maximum_usage_bytes.is_none()
            && self.minimum_free_space_bytes.is_none()
            && self.maximum_age_days.is_none()
    }
}

/// Ask what the library occupies and what a limit would do about it.
///
/// # Parameters
///
/// All optional, and an omitted request measures against the limits that are
/// configured — which is what a screen asks when it opens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetStorage {
    /// Limits to judge the measurement against **instead of** the configured
    /// ones.
    ///
    /// This is the dry run: a window about to save a maximum usage sends the
    /// value somebody typed and is told what saving it would delete, before the
    /// setting is written and before the sweep acts on it.
    ///
    /// The whole set is replaced rather than merged, so a field left out is a
    /// limit the proposal does not have. Merging would make "clear this limit"
    /// unexpressible, and a window could not preview a removal.
    ///
    /// Nothing is saved by asking. The limits are written through
    /// `apply_settings` like every other setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<StorageLimits>,
}

/// What one recording occupies, and whether a sweep may take it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageRecording {
    /// The index's own identifier for it, as `library_sessions` reports it.
    pub recording_id: i64,
    /// The file.
    ///
    /// The same path `library_sessions` sends, so a window can match a row here
    /// against the recording it already drew, and it is what "reveal in
    /// Explorer" names. A path inside the user's own profile, so it is redacted
    /// before it reaches a log (`apps/desktop/src/redactPath.ts`).
    pub path: String,
    /// What it occupies, or zero when nothing has measured it.
    pub size_bytes: u64,
    /// When it started, RFC 3339 with an offset. The order a sweep deletes in.
    pub started_at: String,
    /// Why a sweep will not take it, in the words the recorder uses.
    ///
    /// Absent for a recording nothing protects, which is one a sweep may take.
    /// Present is drawn beside the row rather than instead of it: the size still
    /// counts towards the total, and a protected recording is still filling the
    /// drive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_because: Option<String>,
}

/// Some recordings, and the whole set they were taken from.
///
/// The two totals are of everything, and [`recordings`](Self::recordings) is the
/// first [`MOST_LISTED`] of them. A window draws the rows and says how many more
/// there are — a truncated list that did not carry its own total would read as
/// the whole answer, which for "what a limit would delete" is the worst possible
/// thing to be wrong about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingList {
    /// How many recordings there are in all.
    pub total: u64,
    /// What all of them occupy.
    pub total_bytes: u64,
    /// The first [`MOST_LISTED`] of them, in the order the list is about.
    pub recordings: Vec<StorageRecording>,
}

impl RecordingList {
    /// Whether the list names every recording it counted.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.recordings.len() as u64 == self.total
    }
}

/// One rule that keeps recordings out of a sweep, and what it is holding.
///
/// SPEC.md section 27's "never automatically delete" list, as measured state
/// rather than as a sentence on a screen: this is how many recordings that rule
/// is protecting right now and what they occupy. A screen drawing "favourites
/// are protected" with no figure beside it is decorative copy, and a user cannot
/// tell it from a promise nothing keeps (AGENTS.md section 27).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedGroup {
    /// The rule, in the words a person reads, such as `Favourites`.
    ///
    /// Sent rather than derived, for the reason
    /// [`HotkeyBinding::label`](crate::HotkeyBinding::label) is: the vocabulary
    /// of protections lives in `clipped_library::accounting::cleanup`, and a
    /// window keeping its own table of them would show nothing at all for a rule
    /// a newer recorder had added.
    pub label: String,
    /// How many recordings it is protecting.
    pub recordings: u64,
    /// What they occupy.
    pub bytes: u64,
}

/// What one kind of file occupies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryUsage {
    /// The kind, as accounting names it: `recordings`, `trash`, `thumbnails`.
    pub category: String,
    /// What the files of that kind occupy.
    pub bytes: u64,
}

/// What the library occupies, what a limit would take, and what it would keep.
///
/// The reply to `get_storage`. Everything in it is measured: the usage is a walk
/// of the recording and trash directories, the free space is what the volume
/// reports, and the plan is the one the sweep would carry out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageReport {
    /// Where this recorder writes, which is the directory that was measured.
    ///
    /// The directory **in force**, which for the length of a sitting can differ
    /// from the one `settings.json` holds: where automatic recordings go moves
    /// between sittings and never during one
    /// ([issue #609](https://github.com/wildware-uk/clipped/issues/609)). This is
    /// the folder the figures are about, so it is the one in use rather than the
    /// one that was saved — `get_settings` carries the other, and says so on the
    /// row (`SettingEntry::not_yet_in_force`).
    pub recordings_directory: String,
    /// Where deleted media waits, and is measured as part of the usage.
    pub trash_directory: String,
    /// What the library occupies, across every category measured.
    pub usage_bytes: u64,
    /// What each category occupies, largest first.
    ///
    /// A category with nothing in it is left out rather than sent as zero: a
    /// breakdown listing `screenshots: 0` on a machine that has never taken one
    /// is a row about a feature rather than about the disk.
    pub by_category: Vec<CategoryUsage>,
    /// What is free on the volume the recordings are on.
    ///
    /// Measured, not derived: the disk holds other applications' files too, so
    /// this cannot be worked out from the usage above (`clipped_windows::volume`,
    /// [issue #277](https://github.com/wildware-uk/clipped/issues/277)).
    pub free_bytes: u64,
    /// The whole volume, which is what makes the free figure mean something.
    pub capacity_bytes: u64,
    /// The limits the measurement was judged against.
    pub limits: StorageLimits,
    /// Whether those limits came from [`GetStorage::limits`] rather than from
    /// the settings file.
    ///
    /// `true` is a dry run: **nothing has been saved**, and a window has to say
    /// so or it is showing somebody the consequences of a setting they will
    /// believe is already in force.
    pub proposed: bool,
    /// What a sweep would send to the trash under those limits, oldest first.
    ///
    /// Empty for a library inside its limits, and for one with no limits at all.
    /// Not empty is the confirmation a window owes somebody before it saves
    /// them: these recordings, this much.
    pub would_delete: RecordingList,
    /// What would still be over the limit once all of that had gone.
    ///
    /// Zero when the limits would be met. Non-zero means the sweep would run out
    /// of things it is allowed to delete, which is a disk that stays full and is
    /// something somebody has to be told rather than a cleanup that worked.
    pub still_over_limit: u64,
    /// What a sweep would keep, one row per rule.
    ///
    /// Empty on a library where nothing is favourited or locked. Never a reason
    /// to draw nothing: a screen says the rules protect nothing yet, which is a
    /// different thing from a screen that did not ask.
    pub protected: Vec<ProtectedGroup>,
    /// Every recording the index knows, largest first.
    ///
    /// The review path SPEC.md section 27 and
    /// [issue #111](https://github.com/wildware-uk/clipped/issues/111) ask for:
    /// somebody who can see what is filling their drive can act before automatic
    /// cleanup does, which is the whole argument for a limit being something you
    /// set having looked.
    pub largest: RecordingList,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_with_nothing_set_are_the_unlimited_ones_clipped_ships_with() {
        assert!(StorageLimits::default().is_unlimited());
        assert!(!StorageLimits {
            maximum_usage_bytes: Some(250_000_000_000),
            ..StorageLimits::default()
        }
        .is_unlimited());
    }

    #[test]
    fn an_unset_limit_is_left_off_the_wire_rather_than_sent_as_a_zero() {
        // A window reads absent as "no limit". Zero is a real value for
        // `minimum_free_space_bytes` — fill the disk — so the two must not be
        // spelled the same way.
        let json = serde_json::to_string(&StorageLimits {
            minimum_free_space_bytes: Some(0),
            ..StorageLimits::default()
        })
        .expect("limits serialise");

        assert_eq!(json, r#"{"minimum_free_space_bytes":0}"#);
    }

    #[test]
    fn a_request_with_no_parameters_asks_about_the_configured_limits() {
        let asked: GetStorage =
            serde_json::from_value(serde_json::json!({})).expect("every parameter is optional");

        assert_eq!(asked.limits, None);
    }

    #[test]
    fn a_proposal_replaces_the_limits_rather_than_merging_with_them() {
        // The field a proposal leaves out is a limit it does not have, which is
        // how "clear this limit" is expressed at all.
        let asked: GetStorage =
            serde_json::from_value(serde_json::json!({"limits": {"maximum_age_days": 90}}))
                .expect("a proposal parses");

        assert_eq!(
            asked.limits,
            Some(StorageLimits {
                maximum_usage_bytes: None,
                minimum_free_space_bytes: None,
                maximum_age_days: Some(90),
            })
        );
    }

    #[test]
    fn a_list_knows_whether_it_named_everything_it_counted() {
        let listed = RecordingList {
            total: 2,
            total_bytes: 30,
            recordings: vec![StorageRecording {
                recording_id: 1,
                path: r"D:\Clips\one.mkv".to_owned(),
                size_bytes: 20,
                started_at: "2026-08-16T10:00:00+01:00".to_owned(),
                protected_because: None,
            }],
        };

        assert!(!listed.is_complete(), "one of two is not all of them");
        assert!(
            RecordingList::default().is_complete(),
            "none of none is all of them"
        );
    }
}
