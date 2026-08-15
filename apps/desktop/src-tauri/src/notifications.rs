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
//! 2. **Reads `notifications.json`** from Clipped's configuration directory,
//!    which is the per-category on/off switch until the Settings screen exists
//!    ([issue #51](https://github.com/wildware-uk/clipped/issues/51)). A file
//!    that cannot be read is reported through the startup notice rather than
//!    ignored, and the defaults — everything on — apply.
//! 3. **Decides and shows.** Every event the recorder link publishes is put to
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
//! here rather than shared behind a lock. `on_activated` runs on a thread of
//! WinRT's choosing; everything it touches ([`crate::tray::show_window`],
//! [`crate::tray::report`], `RecorderLink`) is safe to call from any thread.

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use clipped_ipc::{RecorderLink, RecorderLinkEvent};
use tauri::{AppHandle, Manager as _};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

use crate::notification_policy::{
    Notification, NotificationAction, NotificationCategory, NotificationPolicy,
    NotificationSettings, SETTINGS_VERSION,
};
use crate::toast::{ToastContent, Toaster};

/// The file the per-category switches live in, in Clipped's configuration
/// directory.
const SETTINGS_FILE: &str = "notifications.json";

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
}

/// Prepares notifications, and says what could not be prepared.
///
/// Never fails: a build that cannot register its name or read its settings can
/// still notify, and refusing to notify at all because of either would trade a
/// cosmetic problem for a silent one.
pub(crate) fn install(app: &AppHandle, link: &RecorderLink) -> Notifier {
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
            settings(app),
            // "Try again" is only offered where there is something to try. A
            // link with no settings behind it never had a recorder to look for,
            // and `RecorderLink::retry` does nothing to one.
            link.endpoint().is_some(),
            &link.state(),
        ),
    }
}

impl Notifier {
    /// Puts one event to the policy, and shows whatever comes back.
    pub(crate) fn consider(&mut self, app: &AppHandle, event: &RecorderLinkEvent) {
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

/// Which notifications the user wants.
///
/// From `notifications.json` beside Clipped's other configuration. There is no
/// file until somebody writes one, and that is the ordinary case rather than a
/// fault: everything is on by default, because every category is a failure.
///
/// A file that exists and cannot be read is a different matter. Somebody
/// switched something off and it has not taken effect, so it is said through the
/// startup notice — the window asks for that when it mounts — rather than
/// silently ignored (AGENTS.md section 15).
fn settings(app: &AppHandle) -> NotificationSettings {
    let path = match app.path().app_config_dir() {
        Ok(directory) => directory.join(SETTINGS_FILE),
        Err(error) => {
            eprintln!("clipped: Clipped's configuration directory could not be named: {error}");
            return NotificationSettings::default();
        }
    };

    let json = match std::fs::read_to_string(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return NotificationSettings::default()
        }
        Err(error) => {
            crate::set_startup_notice(&unreadable_settings(
                &path.display().to_string(),
                &error.to_string(),
            ));
            return NotificationSettings::default();
        }
    };

    match NotificationSettings::from_json(&json) {
        Ok(settings) => settings,
        Err(reason) => {
            crate::set_startup_notice(&unreadable_settings(&path.display().to_string(), &reason));
            NotificationSettings::default()
        }
    }
}

/// The sentence shown when the notification settings could not be read.
///
/// It says the four things that decide what happens next: which file, what is
/// wrong with it, what Clipped is doing in the meantime — notifying about
/// everything, so that a broken file cannot be the reason a user never hears
/// that nothing is being recorded — and what a file that works looks like.
///
/// The last of those is the useful action AGENTS.md section 45 asks for, and it
/// is written from [`NotificationCategory::ALL`] rather than typed out, so a
/// category added later cannot go unmentioned here. Until the Settings screen
/// exists ([issue #51](https://github.com/wildware-uk/clipped/issues/51)) this
/// file is the only place these switches are, which is exactly why a broken one
/// has to explain itself.
fn unreadable_settings(path: &str, reason: &str) -> String {
    let categories = NotificationCategory::ALL
        .map(NotificationCategory::key)
        .join(", ");

    format!(
        "Clipped could not read its notification settings at {path}: {reason}. Every notification \
         is switched on until that file is corrected or deleted. It holds a JSON object of \
         \"version\": {SETTINGS_VERSION} and any of {categories}, each true or false."
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
    fn an_unreadable_settings_file_says_which_file_and_what_happens_now() {
        let said = unreadable_settings(
            r"C:\Users\a\AppData\Roaming\x\notifications.json",
            "it is empty",
        );

        assert!(said.contains("notifications.json"), "{said}");
        assert!(said.contains("it is empty"), "{said}");
        assert!(
            said.contains("switched on"),
            "a user whose file is broken has to know they will still be told: {said}"
        );
        for category in NotificationCategory::ALL {
            assert!(
                said.contains(category.key()),
                "the notice is the only documentation of this file the user has in front of \
                 them, so it has to name {}: {said}",
                category.key()
            );
        }
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
