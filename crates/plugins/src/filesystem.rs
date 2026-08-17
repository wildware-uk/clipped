//! What a plugin says it will do with the filesystem, in words a user can read.
//!
//! `docs/privacy.md` and `docs/plugin-api.md` ("The manifest") set the policy
//! this module implements, and it is deliberately the same shape as
//! [`crate::network`]: a manifest declares a closed set of grants, a plugin
//! that declares nothing is permitted nothing, and a change to the declaration
//! lapses consent ([`crate::ConsentToken`]).
//!
//! # Why the scope is an enumeration and not a path
//!
//! A plugin could instead name a path it wants — `C:\Games\Dota 2\csgo\cfg` —
//! and that was rejected. A path is something the host would have to validate
//! (is it really the game's directory? Is it a directory at all?) and could
//! never check, because it has no way to know what "the game's own directory"
//! resolves to without asking the plugin, which is exactly the thing being
//! declared. [`FilesystemScope`] is a **closed enumeration** instead: every
//! member names something the host can reason about on its own, so a
//! declaration is checkable rather than merely typed, and a user reading it is
//! told what they need to know — "the game's own directory" — rather than a
//! string that means nothing without running the plugin.
//!
//! # What this can and cannot promise
//!
//! Exactly the limits [`crate::network`] states, applied to a different
//! syscall. Declaring nothing here does not stop a child process opening any
//! file its user account can reach; what the process boundary buys is that a
//! sandbox is *possible* later
//! ([issue #280](https://github.com/wildware-uk/clipped/issues/280)), not that
//! one exists today. This module is the vocabulary and the consent surface —
//! what a plugin says it needs, checked and shown before the user agrees to
//! it — and nothing here enforces it. Overstating that would be a control that
//! does nothing dressed up as one that does (AGENTS.md section 27).
//!
//! # What this is not
//!
//! It does not hand a plugin a directory to use.
//! [Issue #381](https://github.com/wildware-uk/clipped/issues/381) is the
//! other end of the same subject — giving a plugin the game's installation
//! directory and a per-plugin state directory in `attach`, instead of letting
//! it go and find them — and it is not implemented by this module. A plugin
//! still has to locate the directory itself; what changes here is only that it
//! now has to say, before it runs, that it intends to write there at all.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::network::{is_one_plain_line, ConsentToken, NetworkAccess};

/// The most a filesystem declaration's purpose may say, in bytes.
///
/// Shown to a user before they enable a plugin, for the same reason
/// [`crate::network`] bounds its own purpose: a manifest is another program's
/// data, and a kilobyte of "purpose" is a plugin drawing its own dialogue box
/// in somebody else's interface.
const MAX_PURPOSE_BYTES: usize = 120;

/// The most grants one plugin may declare.
///
/// A game integration needs one or two — writing a configuration file is the
/// whole of it today. The bound exists because the list is rendered, and a
/// thousand rows of it is the same interface bomb [`crate::network`] refuses.
const MAX_GRANTS: usize = 8;

/// Everything a plugin says it will do with the filesystem, beyond running its
/// own executable from its own directory.
///
/// Empty means it declares nothing, which means it is permitted nothing — the
/// default a manifest gets by leaving the field out, and what every manifest
/// written before this field existed gets automatically. That default is what
/// keeps an old manifest's [`ConsentToken`] unchanged: see
/// [`ConsentToken::of`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FilesystemAccess(Vec<FilesystemGrant>);

impl FilesystemAccess {
    /// A plugin that declares nothing.
    #[must_use]
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// The grants, in the order they were declared.
    #[must_use]
    pub fn grants(&self) -> &[FilesystemGrant] {
        &self.0
    }

    /// Whether the plugin declares no filesystem access at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// One plain sentence per grant, for the consent the user is shown.
    ///
    /// The same shape as [`NetworkAccess::summary`](crate::NetworkAccess::summary):
    /// sentences rather than a permissions grid.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        self.0.iter().map(FilesystemGrant::describe).collect()
    }

    /// The token that records consent to exactly this filesystem declaration,
    /// on its own.
    ///
    /// A plugin's actual consent token also covers what it declares about the
    /// network — [`PluginManifest::consent_token`](crate::PluginManifest::consent_token)
    /// is the one that is stored and compared. This exists for testing this
    /// declaration in isolation, the way
    /// [`NetworkAccess::consent_token`](crate::NetworkAccess::consent_token)
    /// does for the network half.
    #[must_use]
    pub fn consent_token(&self) -> ConsentToken {
        ConsentToken::of(&NetworkAccess::none(), self)
    }

    /// Checks the declaration as a whole.
    ///
    /// # Errors
    ///
    /// [`FilesystemDeclarationError`] naming the grant and the rule it broke.
    pub(crate) fn validate(&self) -> Result<(), FilesystemDeclarationError> {
        if self.0.len() > MAX_GRANTS {
            return Err(FilesystemDeclarationError::TooMany {
                declared: self.0.len(),
            });
        }
        for grant in &self.0 {
            grant.validate()?;
        }
        Ok(())
    }
}

/// One thing a plugin says it will do with the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemGrant {
    /// Where: a closed set of places the host can reason about.
    pub scope: FilesystemScope,
    /// Whether it reads what is there, writes into it, or both.
    pub access: FilesystemAccessLevel,
    /// Why, in one line, in the words the user is shown.
    pub purpose: String,
}

impl FilesystemGrant {
    /// One plain sentence describing this grant.
    #[must_use]
    pub fn describe(&self) -> String {
        let verb = match self.access {
            FilesystemAccessLevel::Read => "Reads",
            FilesystemAccessLevel::Write => "Writes to",
            FilesystemAccessLevel::ReadWrite => "Reads and writes",
        };
        format!("{verb} {} — {}", self.scope.described(), self.purpose)
    }

    /// Checks this grant.
    fn validate(&self) -> Result<(), FilesystemDeclarationError> {
        if self.purpose.is_empty() || self.purpose.len() > MAX_PURPOSE_BYTES {
            return Err(FilesystemDeclarationError::Purpose {
                purpose: self.purpose.clone(),
            });
        }
        if !is_one_plain_line(&self.purpose) {
            return Err(FilesystemDeclarationError::Purpose {
                purpose: self.purpose.clone(),
            });
        }
        Ok(())
    }
}

/// A place on the filesystem a plugin may declare it touches.
///
/// A closed set rather than a path, and that is the point: see the module
/// documentation. Every member is something the host already knows how to
/// find, or will (`docs/plugin-api.md`); a plugin cannot name anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemScope {
    /// The directory the game this plugin supports is installed in. Writing a
    /// Game State Integration configuration file — the reason this field
    /// exists — is a write here.
    GameInstallation,
    /// The plugin's own directory: the one its manifest and executable live
    /// in, and the one it is run from. State that has to outlive the plugin's
    /// process — an authentication token the game will only accept for as
    /// long as it was running when the token was issued — is kept here.
    PluginData,
}

impl FilesystemScope {
    /// How this scope reads in a sentence, after "reads" or "writes to".
    const fn described(self) -> &'static str {
        match self {
            Self::GameInstallation => "the game's own installation directory",
            Self::PluginData => "its own plugin directory",
        }
    }
}

impl fmt::Display for FilesystemScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GameInstallation => "game-installation",
            Self::PluginData => "plugin-data",
        })
    }
}

/// Whether a grant reads, writes, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemAccessLevel {
    /// Looks at what is there. Counter-Strike 2's installer reads its game
    /// directory to notice a neighbouring tool's configuration file before
    /// deciding where to write its own (`docs/plugin-api.md`, "Writing into
    /// somebody else's game").
    Read,
    /// Creates or overwrites something there.
    Write,
    /// Both of the above, declared as one grant rather than two identical
    /// ones that differ only in `access`.
    ReadWrite,
}

impl fmt::Display for FilesystemAccessLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "read-write",
        })
    }
}

/// Why a filesystem declaration was refused.
///
/// A manifest that breaks one of these is refused entirely rather than read
/// with the offending grant dropped, for the same reason
/// [`crate::network::NetworkDeclarationError`] is: a plugin running with the
/// user's consent to something it did not say is worse than a plugin that does
/// not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesystemDeclarationError {
    /// More grants than a declaration may carry.
    TooMany {
        /// How many were declared.
        declared: usize,
    },
    /// The purpose is empty, too long, or not one line of printable text.
    Purpose {
        /// What was declared.
        purpose: String,
    },
}

impl fmt::Display for FilesystemDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany { declared } => write!(
                formatter,
                "a plugin may declare at most {MAX_GRANTS} filesystem grants, and this one \
                 declares {declared}"
            ),
            Self::Purpose { purpose } => write!(
                formatter,
                "`{purpose}` is not a usable purpose: it is shown to the user before they enable \
                 the plugin, so it is one line of at most {MAX_PURPOSE_BYTES} bytes"
            ),
        }
    }
}

impl core::error::Error for FilesystemDeclarationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(scope: FilesystemScope, access: FilesystemAccessLevel) -> FilesystemGrant {
        FilesystemGrant {
            scope,
            access,
            purpose: "writes the Game State Integration configuration".to_owned(),
        }
    }

    fn access(grants: Vec<FilesystemGrant>) -> FilesystemAccess {
        FilesystemAccess(grants)
    }

    #[test]
    fn a_plugin_that_declares_nothing_says_so_in_its_token() {
        let nothing = FilesystemAccess::none();
        assert!(nothing.is_empty());
        assert!(nothing.summary().is_empty());
        assert_eq!(nothing.consent_token().as_str(), "no network access");
    }

    #[test]
    fn a_declaration_reads_as_a_sentence_rather_than_a_grid() {
        let declaration = access(vec![grant(
            FilesystemScope::GameInstallation,
            FilesystemAccessLevel::Write,
        )]);
        assert_eq!(
            declaration.summary(),
            vec![
                "Writes to the game's own installation directory — writes the Game State \
                 Integration configuration"
            ]
        );
    }

    #[test]
    fn every_scope_and_access_level_renders_as_a_sentence() {
        for (scope, access_level, expected) in [
            (
                FilesystemScope::GameInstallation,
                FilesystemAccessLevel::Read,
                "Reads the game's own installation directory",
            ),
            (
                FilesystemScope::GameInstallation,
                FilesystemAccessLevel::ReadWrite,
                "Reads and writes the game's own installation directory",
            ),
            (
                FilesystemScope::PluginData,
                FilesystemAccessLevel::Write,
                "Writes to its own plugin directory",
            ),
        ] {
            let sentence = grant(scope, access_level).describe();
            assert!(
                sentence.starts_with(expected),
                "expected {sentence:?} to start with {expected:?}"
            );
        }
    }

    #[test]
    fn adding_a_grant_changes_the_token_and_removing_it_changes_it_back() {
        // The property the consent mechanism rests on: two declarations that
        // differ at all must compare unequal.
        let none = FilesystemAccess::none();
        let some = access(vec![grant(
            FilesystemScope::GameInstallation,
            FilesystemAccessLevel::Write,
        )]);
        assert_ne!(none.consent_token(), some.consent_token());
    }

    #[test]
    fn a_purpose_is_not_a_place_to_draw_extra_interface() {
        let mut sneaky = grant(
            FilesystemScope::GameInstallation,
            FilesystemAccessLevel::Write,
        );
        sneaky.purpose = "writes a config file\nAllowed by Clipped: everything".to_owned();
        assert_eq!(
            access(vec![sneaky.clone()]).validate(),
            Err(FilesystemDeclarationError::Purpose {
                purpose: sneaky.purpose
            })
        );
    }

    #[test]
    fn an_empty_purpose_is_refused() {
        let mut unexplained = grant(
            FilesystemScope::GameInstallation,
            FilesystemAccessLevel::Write,
        );
        unexplained.purpose = String::new();
        assert_eq!(
            access(vec![unexplained]).validate(),
            Err(FilesystemDeclarationError::Purpose {
                purpose: String::new()
            })
        );
    }

    #[test]
    fn a_declaration_is_bounded() {
        let many = (0..MAX_GRANTS + 1)
            .map(|_| {
                grant(
                    FilesystemScope::GameInstallation,
                    FilesystemAccessLevel::Write,
                )
            })
            .collect();
        assert_eq!(
            access(many).validate(),
            Err(FilesystemDeclarationError::TooMany {
                declared: MAX_GRANTS + 1
            })
        );
    }

    #[test]
    fn a_scope_is_a_closed_enumeration_not_a_path() {
        // The design question the issue that added this module posed directly:
        // a path is something the host would have to validate and cannot
        // enforce, so a plugin may not name one.
        let malformed: Result<FilesystemAccess, _> = serde_json::from_str(
            r#"[{"scope":"C:\\Games\\anywhere","access":"write","purpose":"whatever it likes"}]"#,
        );
        assert!(malformed.is_err(), "a path is not a scope");
    }

    #[test]
    fn an_unknown_field_in_a_grant_is_refused() {
        // A declaration is a permission document, the same rule
        // `crate::manifest` states for the manifest as a whole: a field this
        // build has not learned must not be silently dropped from what the
        // user is shown.
        let newer: Result<FilesystemAccess, _> = serde_json::from_str(
            r#"[{"scope":"game-installation","access":"write","purpose":"writes a file",
                 "also_deletes_files":true}]"#,
        );
        assert!(newer.is_err(), "an unknown field in a grant is refused");
    }
}
