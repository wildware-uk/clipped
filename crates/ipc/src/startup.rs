//! Whether the recorder starts when this user signs in, as a window reads and
//! changes it.
//!
//! # Why this is on the protocol rather than in the settings
//!
//! It is not configuration. Every other switch on a settings screen is a key in
//! `settings.json` that a recording reads when it starts
//! (`docs/configuration.md`); this one is a value under
//! `HKEY_CURRENT_USER\…\CurrentVersion\Run` that *Windows* reads, once, at
//! sign-in, and that Windows also lists in **Settings → Apps → Startup** with a
//! switch of its own. Putting it in the settings file would mean the file and
//! the registry could disagree, and the registry is the one that decides.
//!
//! # Why the window cannot write it itself
//!
//! The value holds the full path of the executable to run, and the executable
//! to run is the **recorder**. A window that wrote it would be writing the path
//! of a program it does not own, guessed at from its own location — and a
//! guess that is wrong leaves a startup entry pointing at nothing, which fails
//! silently at the next sign-in and nowhere else. The recorder knows its own
//! path, so the recorder writes it
//! ([issue #308](https://github.com/wildware-uk/clipped/issues/308)). The same
//! code answers this command and the `start-at-login` subcommand
//! (`clipped_recorder::start_at_login`, AGENTS.md section 55).
//!
//! # Why a missing executable is reported rather than repaired
//!
//! A Clipped that was moved or reinstalled leaves a value naming a path that no
//! longer exists, so nothing starts at sign-in and nothing says so. Reading the
//! state reports it, and turning the switch on again from the installation the
//! user is looking at is the repair. Repairing it during a *read* would mean
//! opening a settings screen silently rewrote somebody's startup entry, which
//! is the surprising behaviour `docs/privacy.md` exists to refuse.

use serde::{Deserialize, Serialize};

/// Whether the recorder starts at sign-in, and what is arranged.
///
/// The answer to `get_start_at_login` **and** to `set_start_at_login`, for the
/// reason [`SettingsView`](crate::SettingsView) is the answer to both settings
/// commands: a window that drew its own idea of what it had just asked for
/// would show a switch as on when the registry had refused the write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartAtLogin {
    /// Whether Windows has an entry for Clipped under this account.
    ///
    /// The switch's position, and nothing more: an entry that is there but
    /// names an executable that is gone is still `true`, because that is what
    /// Windows will try to run. What is wrong with it is
    /// [`missing_executable`](Self::missing_executable).
    pub enabled: bool,
    /// Where the entry is, spelled the way a registry editor spells it.
    ///
    /// Sent rather than known by the window, for the reason
    /// [`SettingsView::file`](crate::SettingsView) is: a screen that can say
    /// where a thing lives should be told, not keep its own copy of a path that
    /// only the recorder can change.
    pub location: String,
    /// The command line Windows would run, when there is one.
    ///
    /// Absent exactly when [`enabled`](Self::enabled) is `false`. It is the
    /// quoted path of the recorder and the arguments it is started with, which
    /// is worth showing because it is what somebody would otherwise have to
    /// open a registry editor to see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The executable the entry names, when that executable is no longer there.
    ///
    /// Absent when the entry is missing and when it is fine, so its presence is
    /// exactly the case worth acting on: a Clipped that moved. The path is
    /// carried rather than a flag so that a window can name what it looked for
    /// rather than saying only that something is wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_executable: Option<String>,
}

/// Turn starting at login on, or off.
///
/// One boolean rather than two commands, because it is one switch: a window
/// sends the position the user put it in, and gets back the position it is
/// actually in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetStartAtLogin {
    /// `true` writes the entry, `false` removes it.
    ///
    /// Both are idempotent: turning on what is already on rewrites the entry
    /// with this installation's path, which is the repair for a Clipped that
    /// moved, and turning off what was never on is the state being asked for
    /// rather than a failure.
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{SetStartAtLogin, StartAtLogin};

    /// Where the recorder's entry lives, as the recorder reports it. Written
    /// out here rather than imported because `clipped-ipc` deliberately knows
    /// nothing about the registry: this is a string the recorder sends.
    const RUN_VALUE: &str =
        r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run\Clipped Recorder";

    #[test]
    fn an_entry_that_is_not_there_carries_neither_a_command_nor_a_complaint() {
        let state = StartAtLogin {
            enabled: false,
            location: RUN_VALUE.to_owned(),
            command: None,
            missing_executable: None,
        };

        let json = serde_json::to_string(&state).expect("it serialises");
        assert!(
            !json.contains("command") && !json.contains("missing_executable"),
            "an absent entry has nothing to say about a command or an executable: {json}",
        );
        assert_eq!(
            serde_json::from_str::<StartAtLogin>(&json).expect("and deserialises"),
            state,
        );
    }

    #[test]
    fn an_entry_pointing_at_nothing_is_still_on_and_names_what_is_missing() {
        // A Clipped that was moved or reinstalled. The switch is on — Windows
        // will try this at the next sign-in — and it will fail, so both facts
        // have to cross: a window that drew only `enabled` would show a working
        // arrangement, and one that drew only the complaint would offer to turn
        // on something that already is.
        let state = StartAtLogin {
            enabled: true,
            location: RUN_VALUE.to_owned(),
            command: Some(r#""C:\Old\clipped-recorder.exe" serve --watch-for-games"#.to_owned()),
            missing_executable: Some(r"C:\Old\clipped-recorder.exe".to_owned()),
        };

        let json = serde_json::to_string(&state).expect("it serialises");
        let back: StartAtLogin = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, state);
        assert!(
            back.enabled && back.missing_executable.is_some(),
            "on and broken is a state of its own, not a choice between the two",
        );
    }

    #[test]
    fn the_request_carries_where_the_switch_was_put_and_nothing_else() {
        // The whole request, and the only field it will ever have to carry: a
        // window sends where the user put the switch.
        let request: SetStartAtLogin =
            serde_json::from_str(r#"{"enabled": true}"#).expect("the request parses");
        assert!(request.enabled);
        assert_eq!(
            serde_json::to_string(&request).expect("it serialises"),
            r#"{"enabled":true}"#,
        );
    }
}
