//! The token that stops the listener trusting whatever arrives.
//!
//! Valve's Game State Integration carries a shared secret **inside the payload**
//! rather than in a header: whoever writes the configuration file chooses a
//! token, and the game puts it in every state it posts. So the token is the one
//! thing this plugin owns that has to survive a restart of the plugin — the
//! game read the configuration file when *it* started, and a plugin that
//! invented a new token at every attach would spend the rest of the match
//! refusing payloads that carry the previous one.
//!
//! # Where it is kept, and why not in the configuration file
//!
//! In Clipped's own directory (`clipped_logging::application_directory`), one
//! file, one line. Not in the game's `.cfg`, which would mean reading Valve's
//! KeyValues format back — a second parser of a format
//! `clipped-game-detection` already parses for Steam's library index, which is
//! the duplication AGENTS.md section 55 exists to prevent. Writing that file is
//! trivial; reading it is not, and the plugin does not need to.
//!
//! It is a generated credential rather than a setting, which is why it does not
//! belong in the configuration API (AGENTS.md section 30): nothing chooses it,
//! nothing shows it, and a user who deletes it gets a new one and a prompt to
//! restart Dota.
//!
//! # What it is worth
//!
//! Any program on this machine that can read files can read the game's
//! configuration file, so the token is not a secret from the machine. It is a
//! secret from everything that can open a socket but cannot read that file —
//! which is exactly the case `docs/privacy.md` names: *"a listening socket
//! bound to loopback is still reachable by every other process on the machine,
//! including a web page in a browser."* A web page can POST to `127.0.0.1`. It
//! cannot read `gamestate_integration_clipped.cfg`.

use core::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The alphabet a generated token is drawn from.
///
/// Lowercase letters and digits with no punctuation, because the token is
/// written into a KeyValues file as a quoted string and into JSON as one: an
/// alphabet with nothing that needs escaping is one fewer way for a file the
/// game parses to be malformed.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// The largest random byte that maps onto [`ALPHABET`] without bias: 252, being
/// 256 less the four values that would otherwise fold onto the first four
/// characters twice. Bytes at or above it are discarded and another is drawn.
const UNBIASED_CEILING: usize = 256 - 256 % ALPHABET.len();

/// How many characters a generated token has.
///
/// 24 characters of a 36-symbol alphabet is a little over 124 bits, which is
/// far beyond guessing by anything that has to make one HTTP request per
/// attempt.
const GENERATED_LENGTH: usize = 24;

/// The shortest and longest token this plugin will use.
const MIN_LENGTH: usize = 16;
/// See [`MIN_LENGTH`].
const MAX_LENGTH: usize = 64;

/// The file the token is remembered in, under Clipped's own directory.
const TOKEN_FILE: &str = "gsi-auth-token";

/// A Game State Integration auth token.
///
/// Deliberately not [`fmt::Debug`]-transparent: the credential does not appear
/// in a formatted structure, so it cannot reach a log line, a panic message or
/// a bug report by accident (AGENTS.md section 13).
#[derive(Clone, PartialEq, Eq)]
pub struct AuthToken(String);

impl AuthToken {
    /// A new token from the operating system's random number generator.
    ///
    /// # Errors
    ///
    /// [`TokenError::NoRandomSource`] if the platform has no random source this
    /// build knows how to ask, which on anything but Windows it does not. A
    /// guessable token would be worse than a refusal: the listener would still
    /// look authenticated and would not be.
    pub fn generate() -> Result<Self, TokenError> {
        let mut token = String::with_capacity(GENERATED_LENGTH);
        let mut bytes = [0_u8; GENERATED_LENGTH];
        while token.len() < GENERATED_LENGTH {
            fill_random(&mut bytes)?;
            for byte in bytes {
                if token.len() == GENERATED_LENGTH {
                    break;
                }
                // Rejection sampling. Folding all 256 values into 36 characters
                // would make four of them very slightly likelier than the other
                // thirty-two — not a difference that matters at this length,
                // and not a difference a reader should have to work out for
                // themselves either.
                if usize::from(byte) < UNBIASED_CEILING {
                    token.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
                }
            }
        }
        Ok(Self(token))
    }

    /// A token read from somewhere, checked.
    ///
    /// # Errors
    ///
    /// [`TokenError::Unusable`] for anything that is not [`MIN_LENGTH`] to
    /// [`MAX_LENGTH`] characters of the alphabet above. A token file that has
    /// been edited, truncated or filled with something else produces a new
    /// token rather than a listener configured with rubbish.
    pub fn parse(text: &str) -> Result<Self, TokenError> {
        let text = text.trim();
        let usable = (MIN_LENGTH..=MAX_LENGTH).contains(&text.len())
            && text.bytes().all(|byte| ALPHABET.contains(&byte));
        if usable {
            Ok(Self(text.to_owned()))
        } else {
            Err(TokenError::Unusable)
        }
    }

    /// The token, for writing into the game's configuration file.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `presented` is this token.
    ///
    /// Compared without an early exit. The attack it forecloses — timing a
    /// remote comparison to recover a secret character by character — is a
    /// stretch against a local socket answering one request at a time, but a
    /// constant-time comparison of two short strings costs nothing and removes
    /// the need to have that argument.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        let expected = self.0.as_bytes();
        let presented = presented.as_bytes();
        let mut difference = u8::from(expected.len() != presented.len());
        for (index, byte) in presented.iter().enumerate() {
            // Indexing modulo the expected length rather than stopping keeps
            // the loop's length a function of the input alone.
            difference |= byte ^ expected[index % expected.len()];
        }
        difference == 0
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthToken(<redacted>)")
    }
}

/// The token this machine uses, generating and remembering one if there is none.
///
/// Returns the token and whether it had to be created, because a new token
/// means the configuration file has to be written again and the game has to be
/// restarted before it will use it.
///
/// # Errors
///
/// [`TokenError`] when there is nowhere to keep a token, when the operating
/// system will not produce one, or when the file cannot be written. None of
/// these is recoverable by trying again, and all of them are reported to the
/// user rather than swallowed (AGENTS.md section 15): a plugin whose listener
/// silently accepted nothing would look exactly like a plugin whose game had
/// no events.
pub fn remembered_token(directory: &Path) -> Result<(AuthToken, bool), TokenError> {
    let path = directory.join(TOKEN_FILE);
    if let Ok(existing) = fs::read_to_string(&path) {
        if let Ok(token) = AuthToken::parse(&existing) {
            return Ok((token, false));
        }
        // A file that is there but unusable is replaced rather than reported.
        // The token is not the user's data — it is a credential this plugin
        // generated — so there is nothing to preserve (AGENTS.md section 56
        // is about the recordings, not about this).
    }

    let token = AuthToken::generate()?;
    fs::create_dir_all(directory).map_err(|source| TokenError::Unwritable {
        path: directory.to_path_buf(),
        source,
    })?;
    fs::write(&path, token.as_str()).map_err(|source| TokenError::Unwritable {
        path: path.clone(),
        source,
    })?;
    Ok((token, true))
}

/// Why there is no usable token.
#[derive(Debug)]
pub enum TokenError {
    /// The platform has no random source this build knows how to ask.
    NoRandomSource,
    /// What was read where a token was expected is not one.
    Unusable,
    /// The token could not be written down, so it could not be used: a token
    /// this plugin forgets on the next run is a game configured to send one
    /// this plugin will refuse.
    Unwritable {
        /// What could not be written.
        path: PathBuf,
        /// Why not.
        source: io::Error,
    },
}

impl fmt::Display for TokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRandomSource => formatter.write_str(
                "this platform has no random source this build can ask, and a guessable Game \
                 State Integration token is worse than none",
            ),
            Self::Unusable => {
                formatter.write_str("that is not a usable Game State Integration token")
            }
            Self::Unwritable { path, source } => write!(
                formatter,
                "the Game State Integration token could not be written to {}: {source}",
                path.display()
            ),
        }
    }
}

impl core::error::Error for TokenError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::NoRandomSource | Self::Unusable => None,
            Self::Unwritable { source, .. } => Some(source),
        }
    }
}

/// Fills `bytes` from the operating system's random number generator.
///
/// `ProcessPrng` is the documented user-mode entry point to Windows' system
/// preferred RNG: it takes a buffer, it cannot fail in any way a caller can act
/// on, and it needs no algorithm handle to be opened and closed first. It is
/// asked for directly rather than through a crate because this is the only
/// randomness in the plugin and the workspace already pins `windows`
/// (AGENTS.md section 10).
#[cfg(windows)]
fn fill_random(bytes: &mut [u8]) -> Result<(), TokenError> {
    // SAFETY: `ProcessPrng` fills the slice it is given and reads nothing else;
    // the binding takes a `&mut [u8]`, so the pointer and the length it is
    // called with are the ones Rust already guarantees agree. It is documented
    // to return non-zero always; the check below treats a zero as a refusal
    // rather than assuming, because a token left as the zeroed buffer would be
    // the same token on every machine.
    let filled = unsafe { windows::Win32::Security::Cryptography::ProcessPrng(bytes) };
    if filled.as_bool() {
        Ok(())
    } else {
        Err(TokenError::NoRandomSource)
    }
}

/// See the Windows implementation above. Clipped is a Windows application
/// (SPEC.md section 3); this arm exists so that the parts of this crate that
/// have nothing to do with Windows still compile and test elsewhere, and it
/// refuses rather than inventing a token from a clock.
#[cfg(not(windows))]
fn fill_random(_bytes: &mut [u8]) -> Result<(), TokenError> {
    Err(TokenError::NoRandomSource)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::scratch_directory;

    #[test]
    fn a_token_is_only_ever_the_alphabet_it_is_written_into_two_formats_with() {
        // The lengths are written out rather than derived from the constants,
        // so that a limit lowered to something guessable is a failing test
        // rather than a test that agrees with the new limit.
        assert!(AuthToken::parse("abc").is_err(), "too short to be a secret");
        assert!(
            AuthToken::parse("abcdefghijklmno").is_err(),
            "fifteen characters is below the sixteen this plugin will accept"
        );
        assert!(
            AuthToken::parse("abcdefghijklmnop").is_ok(),
            "sixteen is not"
        );
        assert!(
            AuthToken::parse(&"a".repeat(65)).is_err(),
            "longer than this plugin will write"
        );
        assert_eq!((MIN_LENGTH, MAX_LENGTH), (16, 64));
        for awkward in [
            "abcdefghijklmnop\"",
            "abcdefghijklmnop\\",
            "abcdefghijklmnop\n1",
            "ABCDEFGHIJKLMNOP",
        ] {
            assert!(
                AuthToken::parse(awkward).is_err(),
                "`{awkward}` would have to be escaped in a KeyValues file or in JSON"
            );
        }
        assert_eq!(
            AuthToken::parse("  abcdefghijklmnop  ")
                .expect("surrounding whitespace is a file's, not a token's")
                .as_str(),
            "abcdefghijklmnop"
        );
    }

    #[test]
    fn comparison_does_not_stop_at_the_first_wrong_character() {
        let token = AuthToken::parse("abcdefghijklmnopqrstuvwx").expect("a well-formed token");
        assert!(token.matches("abcdefghijklmnopqrstuvwx"));
        assert!(!token.matches("abcdefghijklmnopqrstuvwy"));
        assert!(!token.matches("zbcdefghijklmnopqrstuvwx"));
        assert!(!token.matches(""));
        assert!(
            !token.matches("abcdefghijklmnopqrstuvwxabcdefghijklmnopqrstuvwx"),
            "a token repeated must not match: the loop indexes modulo the expected length, and \
             a comparison that ignored the length would accept this"
        );
    }

    #[test]
    fn a_debug_rendering_never_carries_the_credential() {
        let token = AuthToken::parse("abcdefghijklmnopqrstuvwx").expect("a well-formed token");
        assert!(
            !format!("{token:?}").contains("abcdefghijklmnopqrstuvwx"),
            "the token must not reach a log line or a panic message through Debug"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_generated_token_is_remembered_and_reused() {
        // The property the whole module exists for: Dota reads the
        // configuration file when *it* starts, so the token has to outlive the
        // plugin process. A second run that generated a new one would refuse
        // every payload the running game sent.
        let directory = scratch_directory("token");

        let (first, created) = remembered_token(&directory).expect("a token can be made");
        assert!(created, "there was none before");
        let (second, created_again) = remembered_token(&directory).expect("a token can be read");
        assert!(
            !created_again,
            "the second run reuses the first run's token"
        );
        assert_eq!(first.as_str(), second.as_str());

        // And a file that is no longer a token is replaced rather than used.
        fs::write(directory.join(TOKEN_FILE), "nonsense!").expect("the file can be overwritten");
        let (third, created_again) = remembered_token(&directory).expect("a token can be remade");
        assert!(created_again, "an unusable file means a new token");
        assert_ne!(third.as_str(), first.as_str());
    }

    #[cfg(windows)]
    #[test]
    fn two_generated_tokens_differ() {
        let first = AuthToken::generate().expect("the platform has a random source");
        let second = AuthToken::generate().expect("the platform has a random source");
        // Twenty-four, written out for the reason the lengths above are.
        assert_eq!(first.as_str().len(), 24);
        assert_eq!(GENERATED_LENGTH, 24);
        assert_ne!(
            first.as_str(),
            second.as_str(),
            "a token that is the same every time is not a credential"
        );
        assert!(AuthToken::parse(first.as_str()).is_ok());
    }
}
