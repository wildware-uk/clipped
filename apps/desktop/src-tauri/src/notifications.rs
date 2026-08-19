//! Telling the user something they cannot afford to miss.
//!
//! A notification in Clipped is a **Windows toast**. The tray already carries
//! state and the window already carries sentences, and neither reaches somebody
//! who has closed the window to the notification area and is in a game — which
//! is precisely when the recorder is doing its job and precisely when a failure
//! matters. [`crate::notification_policy`] is where the rules live, including
//! the full list of what is notified and the reasoning for everything that is
//! not; this module is the machinery.
//!
//! # What it does
//!
//! 1. **Registers Clipped's AppUserModelID** under
//!    `HKCU\Software\Classes\AppUserModelId`, so that the toast and the entry on
//!    Windows' own Settings → Notifications page say "Clipped" rather than
//!    `uk.wildware.clipped`. That page is where a user switches Clipped's
//!    notifications off at the operating-system level, and it is no use to
//!    anybody under an identifier they have never seen. Toasts are delivered
//!    with or without this — it was measured both ways, see
//!    `docs/desktop-ui.md` — so a registration that fails costs the name and
//!    nothing else.
//! 2. **Asks the recorder which categories the user wants**, whenever the link
//!    attaches ([`Notifier::refresh`]). They are settings like any other — the
//!    `notifications` section of `settings.json` — and this window may not open
//!    that file, so `get_settings` is how they arrive
//!    ([issue #252](https://github.com/wildware-uk/clipped/issues/252)). Until
//!    the first answer, and for a recorder too old to have them, everything is
//!    on: all four categories are failures, so silence is the wrong way to fail.
//! 3. **Carries an old `notifications.json` into that file and deletes it**
//!    ([`migrate_legacy_switches`]). Before #252 those switches were a second
//!    store in this window's own configuration directory, and a user who
//!    switched a category off there must not silently have it switched back on
//!    (AGENTS.md sections 43 and 56).
//! 4. **Decides and shows.** Every event the recorder link publishes is put to
//!    the policy, and what comes back is a toast with one button on it.
//!
//! # The button
//!
//! Every notification carries an action, because a failure with nothing to do
//! about it is the message AGENTS.md section 45 exists to prevent. The three
//! there are — show the recording in File Explorer, look for a recorder again,
//! open Clipped with the sentence — are all things this build can actually
//! perform, and the policy never offers one that would do nothing.
//!
//! The handler that performs it lives in this process, which has three
//! consequences worth knowing.
//!
//! - Clicking the toast while Clipped is running works whether the toast is on
//!   screen or has fallen into the Action Centre — **provided the notification
//!   object is still alive**, which is [`crate::toast`]'s single
//!   responsibility and the reason this module does not use
//!   `tauri-winrt-notification`.
//! - Clicking one after Clipped has exited does nothing at all: there is no COM
//!   activator registered, deliberately, because one would let Windows start
//!   Clipped from a notification it had no other reason to start it for.
//! - Whether a click reaches [`perform`] has not been verified on a real
//!   desktop. Nothing short of clicking a real toast can verify it, and
//!   `docs/desktop-ui.md` records that as outstanding rather than claiming it
//!   from the presence of a button in the XML.
//!
//! # Threads
//!
//! [`Notifier::consider`] is called from the thread that reads the recorder
//! link's events, and it is the only caller — which is why the policy is owned
//! here rather than shared behind a lock. The one thing that *is* shared is the
//! switches: a `#[tauri::command]` on the window's thread replaces them when
//! somebody saves one, so [`NotificationPreferences`] is behind a lock and
//! nothing else here is. `on_activated` runs on a thread of WinRT's choosing;
//! everything it touches ([`crate::tray::show_window`], [`crate::tray::report`],
//! `RecorderLink`) is safe to call from any thread.
//!
//! [`Notifier::refresh`] makes a blocking `get_settings` call on the event
//! thread, which is allowed for the reason that thread exists: it may block, and
//! the thread drawing the window may not.

use std::collections::BTreeMap;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use clipped_ipc::{RecorderLink, RecorderLinkEvent, RecorderLinkState, SettingsView};
use serde::Deserialize;
use tauri::{AppHandle, Manager as _};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

use crate::notification_policy::{
    Notification, NotificationAction, NotificationCategory, NotificationPolicy,
    NotificationPreferences,
};
use crate::toast::{ToastContent, Toaster};

/// The file the per-category switches lived in before issue #252, in Clipped's
/// own configuration directory rather than the recorder's.
///
/// Read once more, to be carried into `settings.json` and removed. Nothing
/// writes it.
const LEGACY_SETTINGS_FILE: &str = "notifications.json";

/// The only version of that file Clipped ever wrote.
///
/// A file claiming a higher one was written by a build this one does not
/// understand, and is left exactly where it is rather than being flattened into
/// the settings file at whatever this build guesses its fields mean (AGENTS.md
/// section 56). That is the reason the file carried a version at all.
const LEGACY_SETTINGS_VERSION: u32 = 1;

/// The name Windows shows for Clipped's notifications.
const DISPLAY_NAME: &str = "Clipped";

/// The argument the toast's button activates with.
///
/// Anything else — an empty argument, which is what clicking the body of the
/// toast produces — means the user chose the notification rather than the
/// action, and raises the window.
const BUTTON_ARGUMENT: &str = "action";

/// Shows the user the few things worth interrupting them for.
#[derive(Debug)]
pub(crate) struct Notifier {
    /// The Windows toasts, and the objects their buttons call back into.
    toaster: Toaster,
    /// What to show, and what not to. See [`crate::notification_policy`].
    policy: NotificationPolicy,
    /// Which categories the user wants, shared with the window's Save.
    preferences: NotificationPreferences,
    /// Whether the link was attached when the last state arrived.
    ///
    /// The settings are asked for when it *becomes* attached rather than on
    /// every attached state, because the link republishes one every time a
    /// recording starts or stops — and a `get_settings` round trip per recording
    /// would be a question nobody asked.
    attached: bool,
}

/// Prepares notifications, and says what could not be prepared.
///
/// Never fails: a build that cannot register its name can still notify, and
/// refusing to notify at all because of it would trade a cosmetic problem for a
/// silent one. The switches are not read here — there is no recorder to ask yet
/// — so everything is on until the link attaches and [`Notifier::refresh`] has
/// an answer.
pub(crate) fn install(
    app: &AppHandle,
    link: &RecorderLink,
    preferences: NotificationPreferences,
) -> Notifier {
    let app_id = app.config().identifier.clone();

    if let Err(reason) = register_app_user_model_id(&app_id, DISPLAY_NAME) {
        // Not worth the startup notice, which has room for one sentence and
        // should hold the one that changes what the user has to do. Toasts still
        // arrive; they are labelled with the identifier instead of the name.
        eprintln!("clipped: Clipped's notifications will not carry its name: {reason}");
    }

    Notifier {
        toaster: Toaster::new(&app_id),
        policy: NotificationPolicy::new(
            preferences.clone(),
            // "Try again" is only offered where there is something to try. A
            // link with no settings behind it never had a recorder to look for,
            // and `RecorderLink::retry` does nothing to one.
            link.endpoint().is_some(),
            &link.state(),
        ),
        preferences,
        attached: matches!(link.state(), RecorderLinkState::Attached { .. }),
    }
}

impl Notifier {
    /// Puts one event to the policy, and shows whatever comes back.
    pub(crate) fn consider(&mut self, app: &AppHandle, event: &RecorderLinkEvent) {
        if let RecorderLinkEvent::State(state) = event {
            let attached = matches!(state, RecorderLinkState::Attached { .. });
            if attached && !self.attached {
                self.refresh(app);
            }
            self.attached = attached;
        }

        let Some(notification) = self.policy.decide(event) else {
            return;
        };

        if let Err(error) = self.show(app, &notification) {
            // The toast is the surface that reaches somebody in a game, and it
            // has just failed. What it was going to say does not stop being
            // true, so it goes to the window instead — which raises it, and is
            // the more intrusive of the two, but losing a failure notice
            // silently is the one outcome that is not allowed (AGENTS.md
            // sections 15 and 45).
            eprintln!("clipped: a notification could not be shown: {error}");
            crate::tray::report(
                app,
                &format!("{}. {}", notification.title, notification.body),
            );
        }
    }

    /// Asks the recorder which categories the user wants.
    ///
    /// Called when the link attaches, which is the first moment there is
    /// anything to ask and again after every recorder restart. A failure leaves
    /// the switches as they are — the last answer, or everything on — because
    /// the alternative to knowing is telling somebody about a failure, and that
    /// is the direction to fail in.
    ///
    /// A recorder that never attaches is the one gap this leaves: its own
    /// `recorder_unavailable` notification is shown even if it was switched off,
    /// because the switch is in a file only that recorder may open. Keeping a
    /// copy of it here to close that gap is exactly the second store issue #252
    /// removed.
    fn refresh(&self, app: &AppHandle) {
        let Some(link) = app.try_state::<RecorderLink>() else {
            return;
        };

        // The migration first, so that switches somebody moved before #252 are
        // in the settings file before it is read back.
        let view = match migrate_legacy_switches(app, &link) {
            Some(view) => Some(view),
            None => match link.call(&clipped_ipc::Command::GetSettings(
                clipped_ipc::GetSettings::default(),
            )) {
                Ok(clipped_ipc::Reply::Settings { settings }) => Some(settings),
                Ok(_) => {
                    eprintln!("clipped: the recorder answered `get_settings` with something else");
                    None
                }
                Err(error) => {
                    eprintln!("clipped: the notification settings could not be read: {error}");
                    None
                }
            },
        };

        if let Some(view) = view {
            self.preferences.adopt(&view);
        }
    }

    /// Builds and shows the toast.
    fn show(&mut self, app: &AppHandle, notification: &Notification) -> Result<(), String> {
        let action = notification.action.clone();
        let handle = app.clone();

        self.toaster.show(
            ToastContent {
                title: &notification.title,
                body: &notification.body,
                button: notification.action.label(),
                button_argument: BUTTON_ARGUMENT,
            },
            move |chosen| {
                if chosen.as_deref() == Some(BUTTON_ARGUMENT) {
                    perform(&handle, &action);
                } else {
                    // The body of the toast was clicked. The platform convention
                    // is that this activates the application, and doing the
                    // action instead would be a click that did something the
                    // user did not ask for.
                    crate::tray::show_window(&handle);
                }
            },
        )
    }
}

/// Does what the notification offered.
fn perform(app: &AppHandle, action: &NotificationAction) {
    match action {
        NotificationAction::ShowFile { path } => show_file(app, path),
        NotificationAction::RetryRecorder => {
            // The window first: `retry` publishes a state within milliseconds
            // and the window is where the user watches it succeed or fail again.
            // Without it the button would appear to have done nothing.
            crate::tray::show_window(app);
            if let Some(link) = app.try_state::<RecorderLink>() {
                link.retry();
            }
        }
        NotificationAction::OpenClipped { notice } => crate::tray::report(app, notice),
        // The Settings screen is where the hotkey list lives, with the row for
        // the refused combination and the recorder's own sentence beside it
        // (issue #232).
        NotificationAction::OpenHotkeySettings => crate::tray::open_screen(app, "/settings"),
    }
}

/// Opens File Explorer with a recording selected.
///
/// The file is checked first. A recording the user has already moved or deleted
/// would otherwise open an Explorer window on nothing at all, which reads as
/// Clipped being broken rather than as the file being gone (AGENTS.md section
/// 45).
fn show_file(app: &AppHandle, path: &str) {
    if !Path::new(path).exists() {
        crate::tray::report(
            app,
            &format!("The recording is no longer at {path}. It has been moved or deleted."),
        );
        return;
    }

    let Some(argument) = explorer_argument(path) else {
        crate::tray::report(
            app,
            &format!(
                "Clipped could not open File Explorer at {path}, because Explorer cannot be given \
                 a path containing a quotation mark. The recording is there."
            ),
        );
        return;
    };

    if let Err(error) = Command::new("explorer.exe").raw_arg(&argument).spawn() {
        crate::tray::report(
            app,
            &format!("File Explorer could not be opened: {error}. The recording is at {path}."),
        );
    }
}

/// The command line that opens Explorer with one file selected.
///
/// Explorer parses its own command line rather than taking arguments, and it
/// requires the quotation marks to be *inside* the `/select,` argument — which
/// is why this is built by hand and passed with `raw_arg` rather than assembled
/// by [`Command::arg`], whose quoting Explorer reads as a single unfound path.
///
/// [`None`] for a path containing a quotation mark, which cannot be expressed in
/// that form and would otherwise select some other file, or none. Windows
/// permits a quotation mark in a file name, so this is reachable rather than
/// theoretical.
fn explorer_argument(path: &str) -> Option<String> {
    if path.contains('"') {
        return None;
    }

    Some(format!("/select,\"{path}\""))
}

/// A `notifications.json` as the build that shipped issue #110 wrote it.
///
/// Every switch is an [`Option`] rather than a `bool`, unlike the type that
/// read this file before issue #252: what has to be carried across is what the
/// file *said*, and an absent key said nothing. Reading one as `true` would turn
/// three settings the user never touched into three settings they had
/// configured, which is a different file from the one they had.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacySwitches {
    /// The shape of the file, which is the whole reason it carried one.
    version: u32,
    recording_failed: Option<bool>,
    recording_interrupted: Option<bool>,
    recorder_unavailable: Option<bool>,
    hotkey_unavailable: Option<bool>,
}

impl LegacySwitches {
    /// What this file says, as `apply_settings` takes it.
    ///
    /// The keys are [`NotificationCategory::key`], which are the same words the
    /// old file spelled them with — that is what makes this a move rather than a
    /// translation, and why renaming one would silently lose a switch (AGENTS.md
    /// section 43).
    fn changes(&self) -> BTreeMap<String, Option<String>> {
        let mut values = BTreeMap::new();
        for (category, switch) in [
            (NotificationCategory::RecordingFailed, self.recording_failed),
            (
                NotificationCategory::RecordingInterrupted,
                self.recording_interrupted,
            ),
            (
                NotificationCategory::RecorderUnavailable,
                self.recorder_unavailable,
            ),
            (
                NotificationCategory::HotkeyUnavailable,
                self.hotkey_unavailable,
            ),
        ] {
            if let Some(enabled) = switch {
                values.insert(category.key().to_owned(), Some(enabled.to_string()));
            }
        }
        values
    }
}

/// Reads a `notifications.json` this window wrote before issue #252.
///
/// A leading byte-order mark is dropped first. JSON has no such thing and serde
/// rejects it, but this file was only ever edited by hand — there was no screen
/// for it — and both Notepad and `Out-File -Encoding utf8` under Windows
/// PowerShell put one there. That was not a guess: writing the file that way is
/// what the first end-to-end run of issue #110 did, and the notification it was
/// meant to switch off arrived (`docs/desktop-ui.md`). A migration that refused
/// one would lose the switches from precisely the files most likely to have any.
///
/// # Errors
///
/// A sentence for the user when the file is not JSON, is not an object of the
/// expected shape, or says it was written by a later version of Clipped than
/// this one.
fn read_legacy_switches(json: &str) -> Result<LegacySwitches, String> {
    let json = json.strip_prefix('\u{feff}').unwrap_or(json);
    let switches: LegacySwitches =
        serde_json::from_str(json).map_err(|error| format!("it could not be read: {error}"))?;

    if switches.version > LEGACY_SETTINGS_VERSION {
        return Err(format!(
            "it says version {} and this build of Clipped understands version \
             {LEGACY_SETTINGS_VERSION}",
            switches.version
        ));
    }

    Ok(switches)
}

/// Carries an old `notifications.json` into the settings file and deletes it.
///
/// Answers the settings as they now stand when one was migrated, so the caller
/// does not ask again, and [`None`] when there was no file — which is the
/// ordinary case, and every case at all after the first successful run.
///
/// # What it will not do
///
/// **Delete a file it did not manage to save.** The order is read, save,
/// delete, and a failure at any step leaves the file exactly where it is for the
/// next attachment to try again. Deleting first would lose a switch to a
/// recorder that went away mid-migration (AGENTS.md section 56). Applying the
/// same values twice is harmless: they are the values, not a change to them.
///
/// **Guess at a file it cannot read.** One that is not JSON, or that claims a
/// version this build does not know, is left alone and said out loud. There is
/// nowhere better for it to go, and flattening it would destroy whatever it
/// actually held.
fn migrate_legacy_switches(app: &AppHandle, link: &RecorderLink) -> Option<SettingsView> {
    let path = legacy_settings_path(app)?;

    migrate_switches_at(
        &path,
        |request| match link.call(&clipped_ipc::Command::ApplySettings(request)) {
            Ok(clipped_ipc::Reply::Settings { settings }) => Ok(settings),
            Ok(_) => Err("the recorder answered `apply_settings` with something else".to_owned()),
            Err(error) => Err(error.to_string()),
        },
        crate::set_startup_notice,
    )
}

/// The migration itself, with saving left to the caller.
///
/// Separated from [`migrate_legacy_switches`] because everything that can go
/// wrong here is about a *file* — one that is not there, one nobody can read,
/// one that must survive a failed save — and none of it is about Tauri or a
/// named pipe. A test can hand this a directory and a closure; it could not hand
/// it an `AppHandle` and a recorder.
fn migrate_switches_at(
    path: &Path,
    save: impl FnOnce(clipped_ipc::ApplySettings) -> Result<SettingsView, String>,
    notice: impl FnOnce(&str),
) -> Option<SettingsView> {
    let json = match std::fs::read_to_string(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!(
                "clipped: {} could not be read, so the switches in it have not been moved into \
                 your settings: {error}",
                path.display()
            );
            return None;
        }
    };

    let switches = match read_legacy_switches(&json) {
        Ok(switches) => switches,
        Err(reason) => {
            notice(&unmigrated_switches(&path.display().to_string(), &reason));
            return None;
        }
    };

    let view = match save(clipped_ipc::ApplySettings {
        game: None,
        values: switches.changes(),
    }) {
        Ok(view) => view,
        Err(reason) => {
            eprintln!(
                "clipped: your notification switches could not be moved into your settings, so \
                 {} has been left where it is: {reason}",
                path.display()
            );
            return None;
        }
    };

    if let Err(error) = std::fs::remove_file(path) {
        // The switches are saved; only the empty husk is left. Said rather than
        // swallowed, because the next attachment will read and apply it again
        // and somebody editing it would be editing a file nothing reads.
        eprintln!(
            "clipped: your notification switches are now in your settings, but {} could not be \
             removed: {error}",
            path.display()
        );
    }

    Some(view)
}

/// Where the switches were kept before issue #252.
///
/// Clipped's *own* configuration directory — `%APPDATA%\<identifier>` — which is
/// not where the settings file is: that is the recorder's, under
/// `%LOCALAPPDATA%\Clipped`. Two directories for one application's settings was
/// half of what made this a second store.
fn legacy_settings_path(app: &AppHandle) -> Option<PathBuf> {
    match app.path().app_config_dir() {
        Ok(directory) => Some(directory.join(LEGACY_SETTINGS_FILE)),
        Err(error) => {
            eprintln!("clipped: Clipped's configuration directory could not be named: {error}");
            None
        }
    }
}

/// The sentence shown when an old `notifications.json` could not be moved.
///
/// It says the three things that decide what happens next: which file, what is
/// wrong with it, and what Clipped is doing in the meantime — notifying about
/// everything it has not been told otherwise about, so that a file nobody can
/// read is never the reason somebody is not told that nothing is being recorded.
///
/// The useful action AGENTS.md section 45 asks for is the Settings screen, which
/// is where these switches now are: whatever the old file said, they can be set
/// again there, and the file can then be deleted.
fn unmigrated_switches(path: &str, reason: &str) -> String {
    format!(
        "Clipped could not move the notification switches in {path} into your settings: {reason}. \
         That file is no longer read, and it has been left exactly as it is. Every notification is \
         switched on unless you have said otherwise on the Settings screen, which is where these \
         switches now are."
    )
}

/// Tells Windows what Clipped is called.
///
/// The AppUserModelID a toast is shown under is the key Windows files it under:
/// it decides the name on the notification, the entry on the Settings →
/// Notifications page, and which pile the Action Centre groups it into. An
/// unregistered identifier works — the toast is delivered either way — but it is
/// labelled with the identifier itself, and a user cannot be expected to find
/// `uk.wildware.clipped` in a list of applications.
///
/// `HKCU`, so this is per-user and needs no elevation, and one value, so
/// uninstalling leaves at most an empty key behind.
///
/// # Errors
///
/// What Windows said, for the caller to log. This is not worth failing over.
fn register_app_user_model_id(app_id: &str, display_name: &str) -> Result<(), String> {
    let key = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{app_id}"));
    let value: Vec<u16> = display_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes = u32::try_from(std::mem::size_of_val(value.as_slice()))
        .map_err(|_| "the name is too long to store".to_owned())?;

    // SAFETY: `key` and the literal value name are NUL-terminated wide strings
    // that outlive the call, and `value` is a NUL-terminated wide string of
    // exactly `bytes` bytes, which is what `REG_SZ` means. `RegSetKeyValueW`
    // creates the subkey if it is absent and closes the handle it opened.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &key,
            w!("DisplayName"),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            bytes,
        )
    };

    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Windows refused the registration: {status:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_is_selected_rather_than_its_folder_merely_opened() {
        // The quotation marks belong inside the argument. `Command::arg` would
        // put them around the whole of it, and Explorer would open the user's
        // Documents folder instead, which is what "it does nothing" looks like
        // from the outside.
        assert_eq!(
            explorer_argument(r"D:\clips\cs2 2026-08-11.mkv").as_deref(),
            Some(r#"/select,"D:\clips\cs2 2026-08-11.mkv""#)
        );
    }

    #[test]
    fn a_path_explorer_cannot_be_given_is_refused_rather_than_mangled() {
        // Windows allows a quotation mark in a file name. Passing one through
        // would end the quoted argument early and select some other file, or
        // none — and selecting the wrong file is worse than saying so.
        assert_eq!(explorer_argument(r#"D:\clips\the "good" one.mkv"#), None);
    }

    #[test]
    fn a_switch_somebody_turned_off_before_252_is_what_the_migration_sends() {
        // The acceptance criterion: a `notifications.json` written by the build
        // that shipped #110 is migrated with its switches preserved. The keys it
        // sends are the settings file's own, which is what makes this a move.
        let switches = read_legacy_switches(r#"{"version":1,"recording_failed":false}"#)
            .expect("the file the previous build wrote");

        let changes = switches.changes();
        assert_eq!(
            changes.get("recording_failed"),
            Some(&Some("false".to_owned())),
            "the switch that was off has to arrive as off: {changes:?}",
        );
        assert_eq!(
            changes.len(),
            1,
            "a category the file said nothing about must not be written as configured: {changes:?}",
        );
    }

    #[test]
    fn every_category_the_old_file_could_carry_is_one_the_migration_sends() {
        // Walked over `ALL` so that a category the old file had and the
        // migration drops fails here rather than being silently switched back
        // on for whoever had turned it off.
        let json = format!(
            r#"{{"version":1,{}}}"#,
            NotificationCategory::ALL
                .map(|category| format!(r#""{}":false"#, category.key()))
                .join(",")
        );
        let changes = read_legacy_switches(&json)
            .expect("a file with every switch in it")
            .changes();

        for category in NotificationCategory::ALL {
            assert_eq!(
                changes.get(category.key()),
                Some(&Some("false".to_owned())),
                "{} was in the old file and the migration drops it",
                category.key(),
            );
        }
    }

    #[test]
    fn a_file_a_windows_editor_saved_is_still_migrated() {
        // Notepad and Windows PowerShell's `Out-File -Encoding utf8` both write
        // a byte-order mark, which is not JSON. Editing by hand was the *only*
        // way to set these switches, so refusing one would lose the switches
        // from the files most likely to have any.
        let switches = read_legacy_switches("\u{feff}{\"version\":1,\"recording_failed\":false}")
            .expect("a byte-order mark must not cost somebody their switches");

        assert_eq!(switches.recording_failed, Some(false));
    }

    #[test]
    fn a_file_from_a_later_clipped_is_left_alone_rather_than_guessed_at() {
        // The reason that file carried a version. Its fields may mean something
        // else at version 2, and flattening it into the settings would destroy
        // whatever it actually held (AGENTS.md section 56).
        let error = read_legacy_switches(r#"{"version":2,"recording_failed":false}"#)
            .expect_err("version 2 may mean something else by these fields");

        assert!(error.contains("version 2"), "{error}");
        assert!(error.contains("version 1"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_json_is_refused_rather_than_ignored() {
        let error = read_legacy_switches("recording_failed = false")
            .expect_err("an INI file is not a settings file");
        assert!(!error.is_empty(), "the user has to be told what is wrong");
    }

    /// A directory of this test's own, removed when it is dropped.
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn holding(label: &str, json: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "clipped-notifications-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("the temporary directory can be created");
            std::fs::write(path.join(LEGACY_SETTINGS_FILE), json).expect("the file can be written");
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(LEGACY_SETTINGS_FILE)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What the recorder answers a successful `apply_settings` with.
    fn saved() -> SettingsView {
        SettingsView {
            game: None,
            games: Vec::new(),
            file: r"C:\Users\alex\AppData\Local\Clipped\settings.json".to_owned(),
            settings: Vec::new(),
        }
    }

    #[test]
    fn the_switches_are_saved_and_then_the_old_file_is_gone() {
        // Issue #252's third acceptance criterion, end to end over the file: a
        // `notifications.json` from the build that shipped #110 has its switches
        // saved and is removed. Leaving it would leave a second store behind,
        // which is what this issue is about.
        let directory = TestDirectory::holding(
            "migrated",
            r#"{"version":1,"recording_failed":false,"hotkey_unavailable":false}"#,
        );
        let mut sent = None;

        let view = migrate_switches_at(
            &directory.file(),
            |request| {
                sent = Some(request);
                Ok(saved())
            },
            |_| {},
        );

        assert!(view.is_some(), "the settings as they now stand come back");
        let values = sent.expect("the switches were saved").values;
        assert_eq!(
            values.get("recording_failed"),
            Some(&Some("false".to_owned()))
        );
        assert_eq!(
            values.get("hotkey_unavailable"),
            Some(&Some("false".to_owned()))
        );
        assert_eq!(
            values.len(),
            2,
            "only what the file said may be written: {values:?}",
        );
        assert!(
            !directory.file().exists(),
            "the second store is still there after being migrated",
        );
    }

    #[test]
    fn a_save_that_failed_leaves_the_file_for_the_next_attempt() {
        // The order that matters. Deleting first, or deleting anyway, would lose
        // somebody's switches to a recorder that went away mid-migration
        // (AGENTS.md section 56) — and there is nowhere else they exist.
        let directory =
            TestDirectory::holding("unsaved", r#"{"version":1,"recording_failed":false}"#);

        let view = migrate_switches_at(
            &directory.file(),
            |_| Err("there was no recorder listening".to_owned()),
            |_| {},
        );

        assert!(
            view.is_none(),
            "nothing was saved, so there is nothing to draw"
        );
        assert!(
            directory.file().exists(),
            "the switches were not saved and the only copy of them has been deleted",
        );
    }

    #[test]
    fn a_file_from_a_later_clipped_is_neither_saved_nor_deleted() {
        // The reason that file carried a version at all. Its fields may mean
        // something else at version 2, so nothing is guessed and nothing is
        // destroyed.
        let directory =
            TestDirectory::holding("newer", r#"{"version":2,"recording_failed":false}"#);
        let mut asked = false;
        let mut told = None;

        let view = migrate_switches_at(
            &directory.file(),
            |_| {
                asked = true;
                Ok(saved())
            },
            |notice| told = Some(notice.to_owned()),
        );

        // The notice is the whole of what a user gets out of this case, so it
        // is asserted here rather than left to the process-wide one — which is
        // what this used to write, and what made an unrelated test fail when
        // the two ran at once.
        let told = told.expect("a file this build cannot read has to be said out loud");
        assert!(told.contains("version 2"), "{told}");
        assert!(told.contains("understands version 1"), "{told}");
        assert!(
            told.contains("left exactly as it is"),
            "the notice has to say the file was not touched: {told}"
        );

        assert!(view.is_none());
        assert!(
            !asked,
            "a file this build cannot read must not be saved from"
        );
        assert!(directory.file().exists(), "nor deleted");
    }

    #[test]
    fn there_being_no_old_file_is_the_ordinary_case_rather_than_a_failure() {
        // Every launch after the first, and every machine that never had one.
        let directory = TestDirectory::holding("absent", "{}");
        std::fs::remove_file(directory.file()).expect("the file can be removed");
        let mut asked = false;

        let view = migrate_switches_at(
            &directory.file(),
            |_| {
                asked = true;
                Ok(saved())
            },
            |_| {},
        );

        assert!(view.is_none());
        assert!(!asked, "nothing was migrated, so nothing should be saved");
    }

    #[test]
    fn a_file_that_could_not_be_moved_says_which_file_and_what_happens_now() {
        let said = unmigrated_switches(
            r"C:\Users\a\AppData\Roaming\x\notifications.json",
            "it is empty",
        );

        assert!(said.contains("notifications.json"), "{said}");
        assert!(said.contains("it is empty"), "{said}");
        assert!(
            said.contains("switched on"),
            "a user whose file is broken has to know they will still be told: {said}"
        );
        assert!(
            said.contains("Settings"),
            "and where the switches are now, which is the action they can take: {said}"
        );
    }

    #[test]
    fn clipped_registers_itself_under_a_name_a_person_would_recognise() {
        // Cheap, real, and reversible: it writes one HKCU value and reads it
        // back. Without it the toast and the Settings → Notifications entry are
        // labelled `uk.wildware.clipped`.
        let app_id = format!("uk.wildware.clipped.test.{}", std::process::id());
        register_app_user_model_id(&app_id, DISPLAY_NAME).expect("HKCU is writable by its owner");

        let read_back = std::process::Command::new("reg")
            .args([
                "query",
                &format!(r"HKCU\Software\Classes\AppUserModelId\{app_id}"),
                "/v",
                "DisplayName",
            ])
            .output()
            .expect("reg.exe is part of Windows");
        let printed = String::from_utf8_lossy(&read_back.stdout).to_string();

        let removed = std::process::Command::new("reg")
            .args([
                "delete",
                &format!(r"HKCU\Software\Classes\AppUserModelId\{app_id}"),
                "/f",
            ])
            .output();
        assert!(removed.is_ok(), "the test cleans up after itself");

        assert!(
            printed.contains(DISPLAY_NAME),
            "the registration should be readable: {printed}"
        );
    }
}
