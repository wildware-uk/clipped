//! The shared secret between the game's configuration file and the socket.
//!
//! `docs/privacy.md` requires a loopback listener to authenticate what it
//! accepts, because every process on the machine — including a page open in a
//! browser — can post to a loopback port. The token is what makes that check
//! mean something, and a token anybody can predict is not one: it is generated
//! once, when the user installs the configuration, and lives only in the file
//! Counter-Strike reads and in the process that reads it back.
//!
//! It is not a credential. It grants nothing, it is not a Steam token, and
//! learning it lets somebody post fake game states to a plugin on their own
//! machine. It is still generated properly, because the alternative — a
//! timestamp, a process identifier, a hash of the path — is a value another
//! program on the machine can work out, and then the check is decoration.

use windows::Win32::Foundation::NTSTATUS;
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

/// How many random bytes go into a token.
///
/// 128 bits, rendered as 32 hexadecimal characters. Long enough that guessing
/// it is not a strategy, short enough to fit on one line of a configuration
/// file somebody may well read.
const TOKEN_BYTES: usize = 16;

/// A fresh token, from the operating system's random number generator.
///
/// # Errors
///
/// The `NTSTATUS` when Windows will not produce randomness. There is no
/// fallback on purpose: a token from a worse source would be a check that looks
/// like authentication and is not, and this plugin would rather refuse to
/// install than write one (AGENTS.md section 54).
pub fn generate() -> Result<String, NTSTATUS> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    // SAFETY: `BCryptGenRandom` fills the slice it is given and reads nothing
    // else. The slice is a stack array this function owns, its length is what
    // the call is told, and `BCRYPT_USE_SYSTEM_PREFERRED_RNG` is the documented
    // way to ask for the system generator without opening an algorithm handle,
    // so there is no handle to own or close.
    let status = unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return Err(status);
    }
    Ok(bytes.iter().fold(String::new(), |mut token, byte| {
        use core::fmt::Write as _;
        let _ = write!(token, "{byte:02x}");
        token
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_hexadecimal_and_not_the_same_one_twice() {
        let first = generate().expect("Windows produces randomness");
        let second = generate().expect("Windows produces randomness");

        assert_eq!(first.len(), TOKEN_BYTES * 2);
        assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
        assert_ne!(
            first, second,
            "a token that repeats is a token a second reader already knows"
        );
    }
}
