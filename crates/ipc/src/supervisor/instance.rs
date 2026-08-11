//! One of something, per sign-in session.
//!
//! The recorder does not need this: its endpoint already is its single-instance
//! token, because `FILE_FLAG_FIRST_PIPE_INSTANCE` makes the second `serve` on a
//! name fail rather than share it (`transport/windows.rs`). The desktop
//! application has no equivalent — nothing about opening a window stops a second
//! window opening — and a second window would be a second supervisor, with a
//! restart budget of its own, deciding independently whether the recorder needs
//! replacing.
//!
//! # Why a named mutex
//!
//! A mutex is a kernel object whose handles the kernel closes when a process
//! ends, *however* it ends. That is the property that matters: a UI killed from
//! Task Manager, or one that crashed in its webview, must not lock the next
//! launch out. A lock file has the opposite behaviour — it survives the process
//! that made it — and would need a liveness check, which is a second mechanism
//! with a race in it.
//!
//! It is created in the `Local\` namespace, which Windows scopes to the sign-in
//! session, for the same reason the endpoint carries a session discriminator:
//! two people signed in at once, one at the keyboard and one over Remote
//! Desktop, are two users each entitled to their own window and their own
//! recorder (`transport.rs`).
//!
//! # What this deliberately does not do
//!
//! It does not bring the existing window to the front. A second launch finds the
//! name taken, says so and exits; making the first window appear needs a channel
//! to it and a window to raise, and belongs with the tray icon
//! ([issue #50](https://github.com/wildware-uk/clipped/issues/50)) rather than
//! here. What matters for a supervisor is only that the second launch starts no
//! competing recorder, and exiting achieves that completely.

use std::fmt;

/// A claim on a name, released when this value is dropped or the process ends.
#[derive(Debug)]
pub struct SingleInstance {
    name: String,
    /// The mutex object itself.
    ///
    /// Never read, and deliberately so: it is held for its lifetime rather than
    /// for its value, and closing it is what releases the claim.
    #[cfg(windows)]
    #[expect(
        dead_code,
        reason = "the handle is the claim; dropping it is what releases the name"
    )]
    handle: OwnedMutex,
}

impl SingleInstance {
    /// The name that was claimed, without the namespace prefix.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// What happened when a name was claimed.
#[derive(Debug)]
pub enum InstanceClaim {
    /// This process holds the name. Keep the value alive for as long as the
    /// claim should last.
    Claimed(SingleInstance),
    /// Another process in this sign-in session holds it already.
    AlreadyRunning,
}

impl InstanceClaim {
    /// Whether this process is the one holding the name.
    #[must_use]
    pub const fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed(_))
    }
}

/// Why a name could not be claimed.
///
/// Distinct from [`InstanceClaim::AlreadyRunning`], which is not a failure: it
/// is the answer the mechanism exists to give.
#[derive(Debug)]
pub enum InstanceError {
    /// The name would not be a usable object name.
    InvalidName {
        /// What was asked for.
        name: String,
        /// Why it was refused.
        reason: String,
    },
    /// Windows refused to create the object.
    Platform(std::io::Error),
    /// This build has no single-instance mechanism, because it is not a Windows
    /// build.
    Unsupported,
}

impl fmt::Display for InstanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(
                    formatter,
                    "`{name}` is not a usable instance name: {reason}"
                )
            }
            Self::Platform(error) => {
                write!(formatter, "the instance name could not be claimed: {error}")
            }
            Self::Unsupported => formatter.write_str(
                "this build has no single-instance mechanism; it is not a Windows build",
            ),
        }
    }
}

impl std::error::Error for InstanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Platform(error) => Some(error),
            _ => None,
        }
    }
}

/// The longest instance name accepted.
///
/// Windows allows 260 characters for an object name including its namespace
/// prefix. This leaves room for `Local\` with a wide margin.
const MAX_NAME_LENGTH: usize = 200;

/// Claims `name` for this process, for as long as the returned value lives.
///
/// The name is placed in the session-local namespace, so it collides only with
/// another process signed in as the same user in the same session.
///
/// # Errors
///
/// [`InstanceError::InvalidName`] if the name is empty, too long, or contains
/// anything but ASCII letters, digits, `-`, `_` and `.` — the same character
/// set an [`Endpoint`](crate::Endpoint) name allows, and refused for the same
/// reason: a backslash would move the object into a namespace the caller did
/// not ask for. [`InstanceError::Platform`] if Windows refused, and
/// [`InstanceError::Unsupported`] off Windows.
pub fn claim_instance(name: &str) -> Result<InstanceClaim, InstanceError> {
    validate(name)?;
    platform_claim(name)
}

/// Refuses a name that is not one.
fn validate(name: &str) -> Result<(), InstanceError> {
    if name.is_empty() || name.len() > MAX_NAME_LENGTH {
        return Err(InstanceError::InvalidName {
            name: name.to_owned(),
            reason: format!("a name must be 1 to {MAX_NAME_LENGTH} characters"),
        });
    }

    if let Some(bad) = name
        .chars()
        .find(|character| !matches!(character, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.'))
    {
        return Err(InstanceError::InvalidName {
            name: name.to_owned(),
            reason: format!(
                "`{bad}` is not allowed; a name may contain letters, digits, `-`, `_` and `.`"
            ),
        });
    }

    Ok(())
}

#[cfg(windows)]
mod windows_impl {
    use std::io;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    use super::{InstanceClaim, InstanceError, SingleInstance};

    /// A mutex handle, closed exactly once when dropped.
    #[derive(Debug)]
    pub(super) struct OwnedMutex(HANDLE);

    // SAFETY: a Windows kernel handle is a process-wide value with no thread
    // affinity, and nothing here does anything with it but close it. The claim
    // is a property of the process, so it has to be movable between threads for
    // a caller to hold it anywhere useful.
    unsafe impl Send for OwnedMutex {}
    // SAFETY: as above, and `OwnedMutex` exposes no operation at all, so shared
    // references cannot race.
    unsafe impl Sync for OwnedMutex {}

    impl Drop for OwnedMutex {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateMutexW`, is owned solely by
            // this value, and is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Creates the session-local mutex, or reports that it exists.
    pub(super) fn claim(name: &str) -> Result<InstanceClaim, InstanceError> {
        let qualified: Vec<u16> = format!("Local\\{name}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `qualified` is a NUL-terminated wide string that outlives the
        // call. `None` for the security attributes asks for the default
        // descriptor, which for the session-local namespace is already
        // unreachable from another session; `false` asks not to take ownership,
        // because the object's existence is the signal and nothing here ever
        // waits on it. The handle returned is taken over by `OwnedMutex`.
        let handle =
            unsafe { CreateMutexW(None, false, PCWSTR(qualified.as_ptr())) }.map_err(|error| {
                InstanceError::Platform(io::Error::from_raw_os_error(error.code().0))
            })?;

        // SAFETY: reading the calling thread's last error immediately after the
        // call that set it. `CreateMutexW` succeeds either way and sets this to
        // ERROR_ALREADY_EXISTS when it opened an existing object rather than
        // creating one, which is the only way to tell the two apart.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let owned = OwnedMutex(handle);

        if existed {
            // Closed at once by dropping `owned`: holding a second handle to
            // somebody else's object would keep it alive after they exited,
            // which would lock out the launch after this one.
            drop(owned);
            return Ok(InstanceClaim::AlreadyRunning);
        }

        Ok(InstanceClaim::Claimed(SingleInstance {
            name: name.to_owned(),
            handle: owned,
        }))
    }
}

#[cfg(windows)]
use windows_impl::OwnedMutex;

#[cfg(windows)]
fn platform_claim(name: &str) -> Result<InstanceClaim, InstanceError> {
    windows_impl::claim(name)
}

#[cfg(not(windows))]
fn platform_claim(_name: &str) -> Result<InstanceClaim, InstanceError> {
    Err(InstanceError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name no other test and no running application will have.
    fn unique_name(label: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        format!(
            "clipped-instance-test.{label}.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    }

    #[test]
    fn a_name_nobody_holds_is_claimed() {
        let name = unique_name("free");
        let claim = claim_instance(&name).expect("claiming works");
        assert!(claim.is_claimed(), "nothing else holds {name}");
    }

    #[cfg(windows)]
    #[test]
    fn the_second_claim_on_a_name_is_told_something_else_holds_it() {
        let name = unique_name("taken");
        let first = claim_instance(&name).expect("claiming works");
        assert!(first.is_claimed());

        let second = claim_instance(&name).expect("claiming works");
        assert!(
            matches!(second, InstanceClaim::AlreadyRunning),
            "a second claim on a held name must not succeed"
        );
    }

    #[cfg(windows)]
    #[test]
    fn releasing_a_claim_lets_the_next_one_through() {
        // The property a lock file would not have: the claim ends with the
        // holder, so a window that crashed does not lock the next launch out.
        // Dropping stands in for the process ending, since Windows closes a
        // dead process's handles the same way.
        let name = unique_name("released");

        let first = claim_instance(&name).expect("claiming works");
        assert!(first.is_claimed());
        drop(first);

        let second = claim_instance(&name).expect("claiming works");
        assert!(
            second.is_claimed(),
            "a released name must be claimable again"
        );
    }

    #[test]
    fn a_name_that_could_reach_another_namespace_is_refused() {
        for name in [
            r"Global\clipped-desktop",
            r"..\..\clipped",
            "clipped desktop",
            "",
        ] {
            let error = claim_instance(name).expect_err("that should be refused");
            assert!(
                matches!(error, InstanceError::InvalidName { .. }),
                "unexpected error for `{name}`: {error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_claim_remembers_the_name_it_holds() {
        let name = unique_name("named");
        match claim_instance(&name).expect("claiming works") {
            InstanceClaim::Claimed(instance) => assert_eq!(instance.name(), name),
            InstanceClaim::AlreadyRunning => panic!("nothing else holds {name}"),
        }
    }
}
