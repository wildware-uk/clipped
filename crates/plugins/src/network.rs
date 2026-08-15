//! What a plugin says it will do with the network, in words a user can read.
//!
//! `docs/privacy.md` sets the policy this module implements, and two sentences
//! of it are the whole design:
//!
//! > A plugin declares its network access in its manifest: the class (loopback
//! > or outbound), the destinations, whether it listens or connects, and the
//! > purpose in one line. A plugin that declares nothing is a plugin that is
//! > permitted nothing.
//!
//! # What this can and cannot promise
//!
//! A declaration is **checked, rendered and consented to**; it is not a
//! sandbox. A plugin is a separate process (`crate::process`), and a separate
//! process can call the operating system directly whatever its manifest says.
//! What the process boundary buys is that enforcement is *possible* — a job
//! object or an AppContainer can be put around a child, and
//! [issue #280](https://github.com/wildware-uk/clipped/issues/280) is where that
//! is done — where an in-process plugin could never be held to anything at all.
//!
//! Until then the honest statement, which the plugin manager must show rather
//! than imply the stronger one, is [`NetworkAccess::ENFORCEMENT`]. It is a
//! constant rather than a sentence in a document so that the day enforcement
//! arrives, the wording the user reads changes with it.
//!
//! # Consent is a token, not a flag
//!
//! [`ConsentToken`] is the canonical text of a declaration. Enabling a plugin
//! stores the token; starting one compares the token against the declaration in
//! front of it ([`InstalledPlugin::enable`](crate::InstalledPlugin::enable)).
//! An update that adds outbound access where there was only loopback produces a
//! different token, the comparison fails, and the user is asked again — which is
//! `docs/privacy.md`'s "the consent lapses", implemented as a value rather than
//! as a rule somebody has to remember.

use core::fmt;

use serde::{Deserialize, Serialize};

/// The most a network declaration may say, in bytes of endpoint and purpose.
///
/// Both are shown to a user before they enable a plugin, and a manifest is
/// another program's data: a kilobyte of "purpose" is a plugin drawing its own
/// dialogue box in somebody else's interface.
const MAX_ENDPOINT_BYTES: usize = 128;
const MAX_PURPOSE_BYTES: usize = 120;

/// The most grants one plugin may declare.
///
/// A game integration needs one or two. The bound exists because the list is
/// rendered, and a thousand rows of it is the same interface bomb as a long
/// purpose.
const MAX_GRANTS: usize = 8;

/// Everything a plugin says it will do with the network.
///
/// Empty means it declares nothing, which means it is permitted nothing — the
/// default a manifest gets by leaving the field out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkAccess(Vec<NetworkGrant>);

impl NetworkAccess {
    /// What the host can actually promise about a declaration today, in the
    /// words the plugin manager shows the user.
    ///
    /// See the module documentation: a declaration is checked and consented to,
    /// and it is not a sandbox. `docs/privacy.md` requires that the manager
    /// state which guarantee the user is getting rather than implying the
    /// stronger one, so the wording lives here beside the type it describes and
    /// changes when the guarantee does.
    pub const ENFORCEMENT: &'static str =
        "Clipped shows what a plugin declares and refuses to start one whose declaration has \
         changed since you allowed it. It cannot yet stop a plugin from using the network in \
         ways it did not declare.";

    /// A plugin that declares nothing.
    #[must_use]
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// The grants, in the order they were declared.
    #[must_use]
    pub fn grants(&self) -> &[NetworkGrant] {
        &self.0
    }

    /// Whether the plugin declares no network access at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether anything here leaves the machine.
    ///
    /// The one distinction `docs/privacy.md` treats differently, so it is one
    /// call rather than a filter every caller writes: loopback traffic never
    /// reaches a network adapter, and everything else — including the local
    /// network — does.
    #[must_use]
    pub fn leaves_the_machine(&self) -> bool {
        self.0
            .iter()
            .any(|grant| grant.class == NetworkClass::Outbound)
    }

    /// One plain sentence per grant, for the consent the user is shown.
    ///
    /// Sentences rather than a permissions grid, because `docs/privacy.md` asks
    /// for "listens on 127.0.0.1 for Counter-Strike 2 game state" and not a
    /// table nobody reads.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        self.0.iter().map(NetworkGrant::describe).collect()
    }

    /// The token that records consent to exactly this declaration.
    #[must_use]
    pub fn consent_token(&self) -> ConsentToken {
        ConsentToken::of(self)
    }

    /// Checks the declaration as a whole.
    ///
    /// # Errors
    ///
    /// [`NetworkDeclarationError`] naming the grant and the rule it broke.
    pub(crate) fn validate(&self) -> Result<(), NetworkDeclarationError> {
        if self.0.len() > MAX_GRANTS {
            return Err(NetworkDeclarationError::TooMany {
                declared: self.0.len(),
            });
        }
        for grant in &self.0 {
            grant.validate()?;
        }
        Ok(())
    }
}

/// One thing a plugin says it will do with the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrant {
    /// Whether this stays on the machine.
    pub class: NetworkClass,
    /// Whether the plugin waits to be spoken to, or speaks first.
    pub direction: NetworkDirection,
    /// Where: `127.0.0.1:3212`, `localhost`, `[::1]:2999`, `api.example.com:443`.
    pub endpoint: String,
    /// Why, in one line, in the words the user is shown.
    pub purpose: String,
}

impl NetworkGrant {
    /// One plain sentence describing this grant.
    #[must_use]
    pub fn describe(&self) -> String {
        let verb = match self.direction {
            NetworkDirection::Listen => "Listens on",
            NetworkDirection::Connect => "Connects to",
        };
        let reach = match self.class {
            NetworkClass::Loopback => "this machine only",
            NetworkClass::Outbound => "leaves this machine",
        };
        format!("{verb} {} ({reach}) — {}", self.endpoint, self.purpose)
    }

    /// Checks this grant against the rules in `docs/privacy.md`.
    fn validate(&self) -> Result<(), NetworkDeclarationError> {
        if self.endpoint.is_empty() || self.endpoint.len() > MAX_ENDPOINT_BYTES {
            return Err(NetworkDeclarationError::Endpoint {
                endpoint: self.endpoint.clone(),
                because: "an endpoint is between 1 and 128 bytes",
            });
        }
        if !is_one_plain_line(&self.endpoint) {
            return Err(NetworkDeclarationError::Endpoint {
                endpoint: self.endpoint.clone(),
                because: "an endpoint is one line of printable text",
            });
        }
        if self.purpose.is_empty() || self.purpose.len() > MAX_PURPOSE_BYTES {
            return Err(NetworkDeclarationError::Purpose {
                purpose: self.purpose.clone(),
            });
        }
        if !is_one_plain_line(&self.purpose) {
            return Err(NetworkDeclarationError::Purpose {
                purpose: self.purpose.clone(),
            });
        }

        let host = host_of(&self.endpoint).ok_or_else(|| NetworkDeclarationError::Endpoint {
            endpoint: self.endpoint.clone(),
            because: "an endpoint is `host` or `host:port`, with an IPv6 host in brackets",
        })?;

        // The two mislabels that matter. A declaration is what the user reads,
        // so a grant whose class does not match its endpoint is refused rather
        // than rendered: `docs/privacy.md` calls binding a wildcard address
        // "an outbound-class change wearing a disguise", and an "outbound"
        // grant that never leaves the machine teaches a user to distrust the
        // word the next time they see it.
        match self.class {
            NetworkClass::Loopback if !is_loopback_host(host) => {
                Err(NetworkDeclarationError::NotLoopback {
                    endpoint: self.endpoint.clone(),
                })
            }
            NetworkClass::Outbound if is_loopback_host(host) => {
                Err(NetworkDeclarationError::NotOutbound {
                    endpoint: self.endpoint.clone(),
                })
            }
            _ => Ok(()),
        }
    }
}

/// Whether traffic reaches a network adapter at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkClass {
    /// `127.0.0.1`, `::1` or `localhost`: handled inside the kernel, and never
    /// on a wire. A game posting its state to a port on the same machine.
    Loopback,
    /// Anything else, **including the local network**. There is no trusted LAN
    /// category (`docs/privacy.md`).
    Outbound,
}

impl fmt::Display for NetworkClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Loopback => "loopback",
            Self::Outbound => "outbound",
        })
    }
}

/// Whether the plugin waits to be spoken to, or speaks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkDirection {
    /// It binds a socket and accepts what arrives. A listening socket bound to
    /// loopback is still reachable by every other process on the machine,
    /// including a web page, so `docs/privacy.md` requires one to authenticate
    /// what it accepts.
    Listen,
    /// It opens a connection to somewhere else.
    Connect,
}

impl fmt::Display for NetworkDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Listen => "listen",
            Self::Connect => "connect",
        })
    }
}

/// A user's consent to one exact network declaration.
///
/// The value is the canonical text of the declaration, so it is legible in the
/// settings file that stores it — a person reading their own configuration can
/// see what they agreed to — and comparing two of them is comparing what was
/// declared rather than when it was declared.
///
/// Grants are sorted, so reordering a manifest does not lapse consent, and
/// adding, removing or changing one does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsentToken(String);

impl ConsentToken {
    /// The token for `access`.
    #[must_use]
    pub fn of(access: &NetworkAccess) -> Self {
        if access.is_empty() {
            return Self("no network access".to_owned());
        }
        let mut lines: Vec<String> = access
            .grants()
            .iter()
            .map(|grant| format!("{} {} {}", grant.class, grant.direction, grant.endpoint))
            .collect();
        lines.sort();
        Self(lines.join("; "))
    }

    /// A token read back from somewhere it was stored.
    ///
    /// Named rather than `From<String>` so that the claim is visible: the
    /// caller is saying "this text is a consent somebody gave", and the only
    /// legitimate source is a token this type produced earlier -- a settings
    /// file (`clipped_session::config::plugins`), not a manifest and not
    /// anything a plugin sent.
    ///
    /// Nothing is validated, because there is nothing to validate against: the
    /// whole purpose of the token is to be *compared* with what a plugin
    /// declares now, and text that matches no declaration simply lapses. A
    /// constructor that rejected unfamiliar text would refuse exactly the
    /// tokens written by a build that knew about a grant this one does not.
    #[must_use]
    pub fn from_stored(text: &str) -> Self {
        Self(text.to_owned())
    }

    /// The canonical text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConsentToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a network declaration was refused.
///
/// A manifest that breaks one of these is refused entirely rather than read
/// with the offending grant dropped: a plugin whose declaration was silently
/// edited would be running with the user's consent to something it did not say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkDeclarationError {
    /// More grants than a declaration may carry.
    TooMany {
        /// How many were declared.
        declared: usize,
    },
    /// The endpoint is empty, too long, not one line, or not `host` or
    /// `host:port`.
    Endpoint {
        /// What was declared.
        endpoint: String,
        /// The rule it broke.
        because: &'static str,
    },
    /// The purpose is empty, too long, or not one line of printable text.
    Purpose {
        /// What was declared.
        purpose: String,
    },
    /// Declared loopback, but the endpoint is not on this machine — including
    /// a wildcard address, which is reachable from the local network.
    NotLoopback {
        /// What was declared.
        endpoint: String,
    },
    /// Declared outbound, but the endpoint never leaves the machine.
    NotOutbound {
        /// What was declared.
        endpoint: String,
    },
}

impl fmt::Display for NetworkDeclarationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany { declared } => write!(
                formatter,
                "a plugin may declare at most {MAX_GRANTS} network endpoints, and this one \
                 declares {declared}"
            ),
            Self::Endpoint { endpoint, because } => {
                write!(
                    formatter,
                    "`{endpoint}` is not a usable endpoint: {because}"
                )
            }
            Self::Purpose { purpose } => write!(
                formatter,
                "`{purpose}` is not a usable purpose: it is shown to the user before they enable \
                 the plugin, so it is one line of at most {MAX_PURPOSE_BYTES} bytes"
            ),
            Self::NotLoopback { endpoint } => write!(
                formatter,
                "`{endpoint}` is declared as loopback but is reachable from outside this machine; \
                 a wildcard address in particular is outbound access wearing a disguise"
            ),
            Self::NotOutbound { endpoint } => write!(
                formatter,
                "`{endpoint}` is declared as outbound but never leaves this machine, and a \
                 declaration the user cannot trust is worse than none"
            ),
        }
    }
}

impl core::error::Error for NetworkDeclarationError {}

/// The host part of `host` or `host:port`, with an IPv6 host in brackets.
///
/// Deliberately not a full URL parser: an endpoint in a declaration is what the
/// user is shown and what a reviewer checks, so the accepted shapes are the two
/// a game integration needs and nothing else.
fn host_of(endpoint: &str) -> Option<&str> {
    if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        return (!host.is_empty()).then_some(host);
    }

    let host = match endpoint.split_once(':') {
        // A bare IPv6 address such as `::1` has more than one colon and no
        // brackets; treat the whole string as the host rather than reading `:`
        // as a port separator.
        Some(_) if endpoint.matches(':').count() > 1 => endpoint,
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            host
        }
        None => endpoint,
    };

    (!host.is_empty()).then_some(host)
}

/// Whether a host names this machine and only this machine.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Whether a string is one line of printable text.
///
/// Control characters are refused because both fields are rendered: a newline
/// in a purpose is a plugin adding lines to somebody else's consent dialogue.
fn is_one_plain_line(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(class: NetworkClass, direction: NetworkDirection, endpoint: &str) -> NetworkGrant {
        NetworkGrant {
            class,
            direction,
            endpoint: endpoint.to_owned(),
            purpose: "receives Counter-Strike 2 game state".to_owned(),
        }
    }

    fn access(grants: Vec<NetworkGrant>) -> NetworkAccess {
        NetworkAccess(grants)
    }

    #[test]
    fn a_plugin_that_declares_nothing_says_so_in_its_token() {
        let nothing = NetworkAccess::none();
        assert!(nothing.is_empty());
        assert!(!nothing.leaves_the_machine());
        assert!(nothing.summary().is_empty());
        assert_eq!(nothing.consent_token().as_str(), "no network access");
    }

    #[test]
    fn a_declaration_reads_as_a_sentence_rather_than_a_grid() {
        let declaration = access(vec![grant(
            NetworkClass::Loopback,
            NetworkDirection::Listen,
            "127.0.0.1:3212",
        )]);
        assert_eq!(
            declaration.summary(),
            vec![
                "Listens on 127.0.0.1:3212 (this machine only) — receives Counter-Strike 2 game \
                 state"
            ]
        );
    }

    #[test]
    fn outbound_is_distinguished_from_loopback() {
        let loopback = access(vec![grant(
            NetworkClass::Loopback,
            NetworkDirection::Listen,
            "127.0.0.1:3212",
        )]);
        assert!(!loopback.leaves_the_machine());

        let outbound = access(vec![NetworkGrant {
            class: NetworkClass::Outbound,
            direction: NetworkDirection::Connect,
            endpoint: "stats.example.com:443".to_owned(),
            purpose: "uploads match summaries".to_owned(),
        }]);
        assert!(outbound.leaves_the_machine());
        assert!(
            outbound.summary()[0].contains("leaves this machine"),
            "the user has to be able to tell the two apart: {:?}",
            outbound.summary()
        );
    }

    #[test]
    fn consent_survives_reordering_and_lapses_on_a_new_grant() {
        // `docs/privacy.md`: an update that changes the declaration — most
        // importantly one that adds outbound access — asks the user again.
        let loopback = grant(
            NetworkClass::Loopback,
            NetworkDirection::Listen,
            "127.0.0.1:3212",
        );
        let second = grant(
            NetworkClass::Loopback,
            NetworkDirection::Connect,
            "127.0.0.1:2999",
        );
        let outbound = NetworkGrant {
            class: NetworkClass::Outbound,
            direction: NetworkDirection::Connect,
            endpoint: "stats.example.com:443".to_owned(),
            purpose: "uploads match summaries".to_owned(),
        };

        let one_way = access(vec![loopback.clone(), second.clone()]);
        let other_way = access(vec![second, loopback.clone()]);
        assert_eq!(
            one_way.consent_token(),
            other_way.consent_token(),
            "the same declaration written in a different order is the same declaration"
        );

        let grown = access(vec![loopback, outbound]);
        assert_ne!(
            one_way.consent_token(),
            grown.consent_token(),
            "adding outbound access must lapse consent"
        );
    }

    #[test]
    fn a_purpose_is_not_a_place_to_draw_extra_interface() {
        let mut sneaky = grant(
            NetworkClass::Loopback,
            NetworkDirection::Listen,
            "127.0.0.1:3212",
        );
        sneaky.purpose = "reads game state\nAllowed by Clipped: everything".to_owned();
        assert_eq!(
            access(vec![sneaky]).validate(),
            Err(NetworkDeclarationError::Purpose {
                purpose: "reads game state\nAllowed by Clipped: everything".to_owned()
            })
        );
    }

    #[test]
    fn a_wildcard_address_is_not_loopback() {
        // The disguise `docs/privacy.md` names: binding `0.0.0.0` exposes the
        // socket to the local network, which is outbound access.
        for wildcard in ["0.0.0.0:3212", "0.0.0.0", "[::]:3212", "192.168.1.4:3212"] {
            assert_eq!(
                access(vec![grant(
                    NetworkClass::Loopback,
                    NetworkDirection::Listen,
                    wildcard
                )])
                .validate(),
                Err(NetworkDeclarationError::NotLoopback {
                    endpoint: wildcard.to_owned()
                }),
                "`{wildcard}` should not pass as loopback"
            );
        }
    }

    #[test]
    fn the_three_loopback_spellings_are_accepted() {
        for host in ["127.0.0.1:3212", "localhost:2999", "[::1]:2999", "::1"] {
            assert_eq!(
                access(vec![grant(
                    NetworkClass::Loopback,
                    NetworkDirection::Listen,
                    host
                )])
                .validate(),
                Ok(()),
                "`{host}` is loopback"
            );
        }
    }

    #[test]
    fn an_outbound_grant_that_never_leaves_the_machine_is_refused() {
        assert_eq!(
            access(vec![grant(
                NetworkClass::Outbound,
                NetworkDirection::Connect,
                "127.0.0.1:3212"
            )])
            .validate(),
            Err(NetworkDeclarationError::NotOutbound {
                endpoint: "127.0.0.1:3212".to_owned()
            })
        );
    }

    #[test]
    fn an_endpoint_that_is_not_a_host_and_port_is_refused() {
        for malformed in ["", "127.0.0.1:", "127.0.0.1:http", "[::1", ":3212"] {
            let refusal = access(vec![grant(
                NetworkClass::Loopback,
                NetworkDirection::Listen,
                malformed,
            )])
            .validate()
            .expect_err("malformed endpoints are refused");
            assert!(
                matches!(refusal, NetworkDeclarationError::Endpoint { .. }),
                "`{malformed}` should be refused as an endpoint, and was refused as {refusal}"
            );
        }
    }

    #[test]
    fn a_declaration_is_bounded() {
        let many = (0..MAX_GRANTS + 1)
            .map(|index| {
                grant(
                    NetworkClass::Loopback,
                    NetworkDirection::Listen,
                    &format!("127.0.0.1:{}", 3000 + index),
                )
            })
            .collect();
        assert_eq!(
            access(many).validate(),
            Err(NetworkDeclarationError::TooMany {
                declared: MAX_GRANTS + 1
            })
        );
    }

    #[test]
    fn a_declaration_reads_from_the_manifest_it_was_written_in() {
        let declaration: NetworkAccess = serde_json::from_str(
            r#"[{"class":"loopback","direction":"listen","endpoint":"127.0.0.1:3212",
                 "purpose":"receives Counter-Strike 2 game state"}]"#,
        )
        .expect("a well-formed declaration");
        assert_eq!(declaration.validate(), Ok(()));
        assert_eq!(declaration.grants()[0].class, NetworkClass::Loopback);

        // A field this build does not know is refused rather than ignored: a
        // manifest is a permission document, and a declaration read as weaker
        // than it was written is the one failure that must not be quiet.
        let newer = serde_json::from_str::<NetworkAccess>(
            r#"[{"class":"loopback","direction":"listen","endpoint":"127.0.0.1:3212",
                 "purpose":"reads game state","also_writes_files":true}]"#,
        );
        assert!(newer.is_err(), "an unknown field in a grant is refused");
    }
}
