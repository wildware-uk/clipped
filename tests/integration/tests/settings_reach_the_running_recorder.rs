//! Every setting a user can change says what re-reads it once they have.
//!
//! # Why this test exists
//!
//! Four settings have been found frozen at start-up, each separately, each by
//! tripping over it, and each looking correct in review
//! ([issue #648](https://github.com/wildware-uk/clipped/issues/648)):
//!
//! | Setting | Where it was frozen | Fixed by |
//! | --- | --- | --- |
//! | the microphone and every per-game setting | `watch.rs` took `recordings.configuration()` once, when it built the `SessionManager` | PR #608 |
//! | the recording directory | frozen into `AutomaticSettings` before the watcher started, with no setter at all | PR #623 |
//! | the storage limits | `LibraryIndexer::with_storage` uses `Arc::get_mut`, so it works only at construction | PR #647 |
//! | the hotkey bindings | `crate::hotkeys::start` read the configuration once, before the ready line | PR for issue #233 |
//!
//! Review does not catch this, and the reason is worth stating: every one of
//! those reads is *correct where it is written*. `serve` is right to register
//! the hotkeys before the ready line, or a window connecting immediately races
//! registration. The defect is never the read; it is that nothing reads it
//! again, and that fact lives somewhere else, or nowhere.
//!
//! It is also invisible from both ends. The settings file holds the new value,
//! so the file is right. The screen shows the new value, because it draws what
//! `get_settings` returns and that reads the file. Only the thing that *acts*
//! on the setting is stale, and it is a process boundary away. AGENTS.md
//! section 27 calls a control that does nothing worse than an absent one; this
//! is the version that also looks like it worked.
//!
//! # What this checks, and what it cannot
//!
//! It cannot check that a setting is propagated — nothing static can. What it
//! checks is that **somebody answered the question**. Every key a user can
//! change is derived from the source, and every one of them has to appear in
//! [`ANSWERS`] with what re-reads it, or with a deliberate statement that it
//! takes effect at start-up only and why. An unanswered setting fails, which
//! turns "did anyone think about propagation" from a review question — which
//! has now failed four times — into one that has to be answered before the
//! change can land.
//!
//! # Why the list is derived rather than restated
//!
//! Because a hand-maintained copy of the settings is a list that goes stale
//! silently, and a stale list is worse than no list: it reads as an answer.
//! Deriving it means a new `SettingKey`, a new field on `StorageSettings` or
//! `AutomaticSettings`, a new notification switch or a new hotkey action fails
//! this test until its entry is written. That is the same property
//! `tests/integration/tests/process_table_reads.rs`,
//! `tests/integration/tests/disk_space_reads.rs`,
//! `tests/integration/tests/foreground_rules.rs` and
//! `every_undetected_launcher_says_so_in_the_subsystem_document` in
//! `crates/game-detection/src/launcher/mod.rs` are built on.
//!
//! # Why it reads source rather than linking the crates
//!
//! Two of the five lists are **struct fields**, and there is no way to
//! enumerate those at run time. The rest are read the same way for one
//! mechanism rather than two, and this crate deliberately depends on no member
//! of the workspace (`tests/integration/Cargo.toml`), so a setting added to a
//! crate that does not yet compile still fails here — which is when the author
//! is looking. Every read below names the file and the item it reads and fails
//! loudly when that item moves, because a check that has stopped finding its
//! subject must fail rather than pass on nothing
//! (`tests/integration/tests/cited_tests_exist.rs`, AGENTS.md section 54).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// What happens to a setting after `apply_settings` has saved it.
///
/// Four answers, and every one of them carries a sentence. "It is complicated"
/// is not an answer; neither is silence, which is what this file exists to stop
/// being one.
#[derive(Debug, Clone, Copy)]
enum Answer {
    /// Something re-reads it while the recorder runs, and this says what and
    /// where. The sentence has to name the reader, because "it is propagated"
    /// is what all four of the frozen settings looked like.
    ReadAgain(&'static str),
    /// It takes effect only when the recorder starts, deliberately, and this
    /// says why that is acceptable — which has to be a reason the user is not
    /// left with a control that silently needs a restart (AGENTS.md section
    /// 27).
    ///
    /// **Nothing uses this today, and that is the finding rather than an
    /// oversight**: every setting below either reaches the running recorder or
    /// is declared as one nothing acts on. It stays because the answer has to
    /// exist before it is needed — a guard offering only the answers somebody
    /// could already give would push the next start-up-only setting towards a
    /// wrong one (issue #648).
    #[expect(
        dead_code,
        reason = "the answer a start-up-only setting has to be able to give; see above"
    )]
    AtStartUp(&'static str),
    /// Nothing in this build acts on it at all, and this says where that is
    /// declared to the user. A setting nothing reads is a different defect from
    /// a setting read once, and it must not be able to hide inside this list as
    /// though it were propagated.
    NothingActsOnIt(&'static str),
    /// Not a setting a user can change, and this says what it is instead. The
    /// derivation reads struct fields, and a struct holds things that are not
    /// settings; each one still has to be named as such rather than quietly
    /// skipped.
    NotAUserSetting(&'static str),
}

/// Every user-changeable setting, and what re-reads it after `apply_settings`.
///
/// Keyed by the type the setting is derived from and its name in the settings
/// file, so that two settings cannot share an entry and a failure names
/// something a reader can find.
const ANSWERS: &[(&str, Answer)] = &[
    // ---- The per-game settings -------------------------------------------
    //
    // All of these are resolved when a recording starts and never re-read while
    // it runs, which is deliberate (issue #61): a recording's settings belong to
    // the moment it started. What matters here is the *next* recording.
    (
        "SettingKey::capture_target",
        Answer::NothingActsOnIt(
            "every recording captures the game's own window, whatever this says. \
             `apps/recorder/src/settings.rs::applies` declares it, so the settings screen \
             draws the sentence instead of a control rather than offering a dead one \
             (AGENTS.md section 27, issue #61).",
        ),
    ),
    (
        "SettingKey::resolution",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::framerate",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::codec",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::encoder",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::microphone",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::system_audio",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    (
        "SettingKey::replay_window_seconds",
        Answer::ReadAgain(SETTINGS_FILE_PER_RECORDING),
    ),
    // ---- The storage section ---------------------------------------------
    (
        "StorageSettings::maximum_usage",
        Answer::ReadAgain(STORAGE_SWEEP),
    ),
    (
        "StorageSettings::minimum_free_space",
        Answer::ReadAgain(STORAGE_SWEEP),
    ),
    (
        "StorageSettings::maximum_age_days",
        Answer::ReadAgain(STORAGE_SWEEP),
    ),
    (
        "StorageSettings::trash_directory",
        Answer::ReadAgain(
            "the storage sweep, through the same push the limits take: it reads the whole \
             `StorageSettings` out of `LibraryIndexer`'s mutex each run and asks \
             `clipped_session::cleanup::trash_directory` where the trash is \
             (`apps/recorder/src/library.rs`). No settings command carries this one — it is \
             changed by editing `settings.json` — and `apply_settings` reloads the file before \
             it saves, so a hand edit reaches the sweep at the next save from the window and \
             at the next start otherwise.",
        ),
    ),
    (
        "StorageSettings::recording_directory",
        Answer::ReadAgain(
            "the automatic recorder, at the end of each sitting: `apply_settings` saves it and \
             `AutomaticRecorder::close_active` applies it to the `AutomaticSettings` the \
             running session manager holds, so it moves between sittings and never during one \
             (`apps/recorder/src/watch.rs`, PR #623, issue #609). A save made mid-sitting says \
             so on screen through `SettingEntry::not_yet_in_force` rather than looking as \
             though it had taken effect.",
        ),
    ),
    (
        "StorageSettings::unknown",
        Answer::NotAUserSetting(
            "keys a newer Clipped wrote that this build does not understand, kept by \
             `crates/session/src/config/storage.rs` so that writing the file back does not \
             delete them (AGENTS.md section 56). Nothing in this build reads them, because by \
             definition it cannot.",
        ),
    ),
    // ---- What an automatic recording is made with ------------------------
    (
        "AutomaticSettings::directory",
        Answer::ReadAgain(
            "where `StorageSettings::recording_directory` lands, and the same answer: \
             `AutomaticRecorder::close_active` calls `AutomaticSettings::set_directory` on the \
             settings the running session manager holds (PR #623, issue #609). This field is \
             the one that was frozen — it had no setter at all — so it is the reason \
             `AutomaticSettings` is derived here rather than only `StorageSettings`.",
        ),
    ),
    (
        "AutomaticSettings::restart_grace",
        Answer::NotAUserSetting(AUTOMATIC_TIMING),
    ),
    (
        "AutomaticSettings::suspend_gap",
        Answer::NotAUserSetting(AUTOMATIC_TIMING),
    ),
    (
        "AutomaticSettings::recording_restart_delay",
        Answer::NotAUserSetting(AUTOMATIC_TIMING),
    ),
    (
        "AutomaticSettings::max_recordings_per_session",
        Answer::NotAUserSetting(AUTOMATIC_TIMING),
    ),
    // ---- The notification switches ---------------------------------------
    (
        "NotificationCategory::recording_failed",
        Answer::ReadAgain(NOTIFICATION_POLICY),
    ),
    (
        "NotificationCategory::recording_interrupted",
        Answer::ReadAgain(NOTIFICATION_POLICY),
    ),
    (
        "NotificationCategory::recorder_unavailable",
        Answer::ReadAgain(NOTIFICATION_POLICY),
    ),
    (
        "NotificationCategory::hotkey_unavailable",
        Answer::ReadAgain(NOTIFICATION_POLICY),
    ),
    // ---- The hotkeys -----------------------------------------------------
    (
        "HotkeyAction::save_replay",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::add_bookmark",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::take_screenshot",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::toggle_recording",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::mute_microphone",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::toggle_microphone",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
    (
        "HotkeyAction::open_overlay",
        Answer::ReadAgain(HOTKEY_REBIND),
    ),
];

/// The answer the eight per-game settings share.
const SETTINGS_FILE_PER_RECORDING: &str =
    "the next recording, on both paths. A recording started from the window resolves it from \
     `RecorderService`'s shared `SettingsFile` as it starts, and the automatic recorder — which \
     holds a copy, because its session manager owns what per-game settings are resolved from — \
     compares `SettingsFile::generation` on every watcher pass and takes a fresh configuration \
     when it has moved (`apps/recorder/src/watch.rs`, PR #608, issue #51). Nothing re-reads it \
     while a recording runs, which is issue #61 and deliberate: what a recording is made with \
     belongs to the moment it started.";

/// The answer the three storage limits share.
const STORAGE_SWEEP: &str =
    "the storage sweep, after every reconciliation. `apply_settings` pushes the saved \
     `StorageSettings` to `LibraryIndexer::set_storage`, which writes them into the mutex the \
     sweep reads each run (`apps/recorder/src/serve.rs`, `apps/recorder/src/library.rs`, PR \
     #647, issue #95). It was `Arc::get_mut` and therefore construction-only before that, which \
     is how a limit that deletes footage came to need a restart.";

/// The answer the four automatic-recording timings share.
const AUTOMATIC_TIMING: &str =
    "no key in `settings.json` carries it and no settings command sets it. These are the \
     constants in `crates/session/src/automatic/settings.rs` that decide when a sitting ends \
     and how often a game may be recorded again; only tests vary them, through the `with_*` \
     builders. A user-facing control for any of them would need an entry here saying what \
     re-reads it.";

/// The answer the four notification switches share.
const NOTIFICATION_POLICY: &str =
    "the desktop application, at the moment it decides whether to show a toast \
     (`apps/desktop/src-tauri/src/notification_policy.rs`, issue #252). It is the one group of \
     settings whose reader is the window rather than the recorder, and it reads the answer to \
     `get_settings` rather than a copy taken at start-up.";

/// The answer the seven hotkey actions share.
const HOTKEY_REBIND: &str =
    "the running hotkey service, during the `apply_settings` that saved it. \
     `RecorderService::rebind_hotkeys` calls `RegisteredHotkeys::apply`, which posts the \
     rebind to the thread that owns the message loop — `RegisterHotKey` is bound to that \
     thread — and waits for Windows to answer (`apps/recorder/src/hotkeys.rs`, issue #233). \
     `get_hotkeys` is then read out of the live registration rather than a stored report, so \
     there is no second copy to go stale.";

/// The repository root, found from this crate's manifest.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the repository root resolves")
}

/// A source file of this repository, as text.
fn source(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path))
        .unwrap_or_else(|error| panic!("{path} is part of this repository: {error}"))
}

/// The brace-matched block that follows `marker`, or a failure naming what has
/// moved.
///
/// Brace matching rather than a pattern per item, for the reason
/// `apps/desktop/src/settingsConformance.test.ts` matches braces: the blocks
/// read below are `match` arms and struct bodies, and a regular expression over
/// them would break on a line wrap `cargo fmt` decided differently — which
/// would look like a setting being unanswered rather than like this file
/// needing an edit.
fn block_after<'a>(path: &str, text: &'a str, marker: &str) -> &'a str {
    let at = text.find(marker).unwrap_or_else(|| {
        panic!(
            "{path} no longer contains `{marker}`. It is one of the lists of settings this test \
             derives its question from, so renaming or reshaping it means deciding what asks \
             the question instead (issue #648)"
        )
    });
    let open = text[at..]
        .find('{')
        .unwrap_or_else(|| panic!("{path} has no block after `{marker}`"))
        + at;

    let mut depth = 0_usize;
    for (offset, character) in text[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("the block after `{marker}` in {path} is not closed")
}

/// Every double-quoted string in a fragment of Rust, which for the `match`
/// blocks read here is exactly the set of keys.
fn quoted(path: &str, marker: &str, fragment: &str) -> Vec<String> {
    let found: Vec<String> = fragment
        .split('"')
        .skip(1)
        .step_by(2)
        .map(ToOwned::to_owned)
        .collect();
    assert!(
        !found.is_empty(),
        "`{marker}` in {path} was read as naming no settings at all, which is not what it says",
    );
    found
}

/// The field names of a struct, which is the only way to enumerate them: a
/// field is not something a running program can list.
fn fields_of(path: &str, text: &str, marker: &str) -> Vec<String> {
    let found: Vec<String> = block_after(path, text, marker)
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//") && !line.starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_owned())
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character == '_')
        })
        .collect();
    assert!(
        !found.is_empty(),
        "`{marker}` in {path} was read as having no fields, which is not what it says",
    );
    found
}

/// Every setting a user can change, qualified by where it is declared.
fn every_setting(root: &Path) -> Vec<String> {
    let mut settings = Vec::new();

    let value = source(root, "crates/session/src/config/value.rs");
    let marker = "pub const fn name(self) -> &'static str";
    for key in quoted(
        "crates/session/src/config/value.rs",
        marker,
        block_after("crates/session/src/config/value.rs", &value, marker),
    ) {
        settings.push(format!("SettingKey::{key}"));
    }

    let storage = source(root, "crates/session/src/config/storage.rs");
    for field in fields_of(
        "crates/session/src/config/storage.rs",
        &storage,
        "pub struct StorageSettings {",
    ) {
        settings.push(format!("StorageSettings::{field}"));
    }

    let automatic = source(root, "crates/session/src/automatic/settings.rs");
    for field in fields_of(
        "crates/session/src/automatic/settings.rs",
        &automatic,
        "pub struct AutomaticSettings {",
    ) {
        settings.push(format!("AutomaticSettings::{field}"));
    }

    let notifications = source(root, "crates/session/src/config/notifications.rs");
    let marker = "pub const fn key(self) -> &'static str";
    for key in quoted(
        "crates/session/src/config/notifications.rs",
        marker,
        block_after(
            "crates/session/src/config/notifications.rs",
            &notifications,
            marker,
        ),
    ) {
        settings.push(format!("NotificationCategory::{key}"));
    }

    let actions = source(root, "crates/hotkeys/src/action.rs");
    let marker = "pub const fn name(self) -> &'static str";
    for key in quoted(
        "crates/hotkeys/src/action.rs",
        marker,
        block_after("crates/hotkeys/src/action.rs", &actions, marker),
    ) {
        settings.push(format!("HotkeyAction::{key}"));
    }

    settings
}

#[test]
fn every_user_changeable_setting_says_what_re_reads_it_after_a_save() {
    let declared: Vec<String> = every_setting(&repository());
    let answered: BTreeSet<&str> = ANSWERS.iter().map(|(setting, _)| *setting).collect();

    let unanswered: Vec<&String> = declared
        .iter()
        .filter(|setting| !answered.contains(setting.as_str()))
        .collect();

    assert!(
        unanswered.is_empty(),
        "these settings have no entry in ANSWERS, so nobody has said what happens to them when \
         a user changes one: {unanswered:#?}\n\nAdd each of them to ANSWERS in \
         tests/integration/tests/settings_reach_the_running_recorder.rs with one of four \
         answers, and the sentence that goes with it:\n\n  \
         Answer::ReadAgain      — name what re-reads it while the recorder runs, and where. \
         Not \"it is propagated\": all four of the settings that turned out to be frozen \
         looked propagated.\n  \
         Answer::AtStartUp      — say why taking effect only at the next start is acceptable, \
         and what tells the user so. A control that silently needs a restart is worse than no \
         control (AGENTS.md section 27).\n  \
         Answer::NothingActsOnIt — say where the fact that nothing reads it is declared to the \
         user, so it cannot be drawn as a working control.\n  \
         Answer::NotAUserSetting — say what it is instead, for a struct field that is not \
         something a user changes.\n\nThis is issue #648: four settings have been found frozen \
         at start-up, each separately, each after somebody had already reviewed the change \
         that froze it. The question is cheap to answer while writing the setting and \
         expensive to answer after somebody's microphone, storage limit or hotkey has quietly \
         stopped working."
    );

    let declared: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
    let stale: Vec<&&str> = answered
        .iter()
        .filter(|setting| !declared.contains(*setting))
        .collect();
    assert!(
        stale.is_empty(),
        "ANSWERS names settings that no longer exist: {stale:#?}. A stale entry is worse than \
         no list at all — it is an answer to a question nobody is asking any more, and it makes \
         the list look complete while a real setting goes unanswered. Remove it, or correct the \
         spelling if the setting was renamed."
    );
}

#[test]
fn every_answer_says_something_a_reader_can_check() {
    // An entry whose sentence is "yes" would satisfy the test above and answer
    // nothing, which is how a guard becomes a formality. The bar is low on
    // purpose — this cannot judge an argument — but a one-word answer is not an
    // argument, and every real one names a file, a type or an issue.
    for (setting, answer) in ANSWERS {
        let reason = match answer {
            Answer::ReadAgain(reason)
            | Answer::AtStartUp(reason)
            | Answer::NothingActsOnIt(reason)
            | Answer::NotAUserSetting(reason) => *reason,
        };
        assert!(
            reason.len() >= 80,
            "{setting} is answered with {reason:?}, which is too short to be an answer. Say what \
             re-reads it and where, so that the next person can check rather than believe."
        );
        assert!(
            reason.contains(".rs") || reason.contains("issue #") || reason.contains("PR #"),
            "{setting} is answered without naming a file or an issue: {reason:?}. An answer \
             nobody can follow is one nobody can find has gone stale."
        );
    }
}
