//! Which capture backends this build actually contains.
//!
//! [`select`](crate::select) is deliberately a pure function over a candidate
//! list it is handed, so that it can be tested without a display and so that
//! runtime fallback is "call it again with one fewer candidate"
//! ([issue #97](https://github.com/wildware-uk/clipped/issues/97)). Something
//! still has to say what the list *is* for a real recording, and this is it —
//! one place, so that "does this build have a Windows Graphics Capture backend?"
//! has a single answer rather than one per caller.

use crate::{BackendDeclaration, CaptureBackendFactory, CaptureMethod};

/// Every capture backend compiled into this build.
///
/// A `static` rather than a function body so that the list is a fact about the
/// build, visible in one place, with nothing to construct and nothing to
/// initialise lazily. Compiled-out backends are absent rather than present and
/// declining: a non-Windows build has no Windows Graphics Capture backend at
/// all, and [`select`](crate::select) reports "this build registered no capture
/// backends" rather than inventing a reason a backend that does not exist gave.
#[cfg(windows)]
static REGISTERED: &[&dyn CaptureBackendFactory] = &[&crate::windows::WindowsGraphicsCapture];

/// Every capture backend compiled into this build. Empty off Windows: nothing
/// in this workspace captures anything anywhere else, and saying so is more
/// useful than a backend that declines every target.
#[cfg(not(windows))]
static REGISTERED: &[&dyn CaptureBackendFactory] = &[];

/// The backends this build can create, in no particular order.
///
/// Order does not matter here, and relying on it would be a bug waiting to
/// happen: [`select`](crate::select) sorts by
/// `CaptureMethod::preference_rank`, which is checked against
/// [`CaptureMethod::PREFERENCE_ORDER`] while the crate compiles.
#[must_use]
pub fn registered_backends() -> &'static [&'static dyn CaptureBackendFactory] {
    REGISTERED
}

/// The same backends, in the shape [`select`](crate::select) takes.
///
/// A `Vec` because `select` wants `&[&dyn BackendDeclaration]` and the registry
/// holds `&dyn CaptureBackendFactory`; the alternative is a second static list
/// of the same backends, which is a thing that can disagree with the first.
/// This runs once per recording, next to creating a Direct3D device, so the
/// allocation is not worth avoiding.
#[must_use]
pub fn registered_declarations() -> Vec<&'static dyn BackendDeclaration> {
    REGISTERED
        .iter()
        .map(|backend| *backend as &dyn BackendDeclaration)
        .collect()
}

/// The registered backend for `method`, if this build has one.
///
/// This is the step between [`select`](crate::select) and a running capture:
/// selection reports a [`CaptureMethod`], and this turns it back into the
/// factory that implements it.
#[must_use]
pub fn registered_backend(method: CaptureMethod) -> Option<&'static dyn CaptureBackendFactory> {
    REGISTERED
        .iter()
        .copied()
        .find(|backend| backend.method() == method)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_backends_claim_the_same_method() {
        // `select` refuses a candidate list with a duplicate in it, because the
        // report would not say which of the two ran. That check protects
        // callers who build their own list; this one protects the list every
        // recording actually uses, and it fails at `cargo test` rather than at
        // the moment somebody presses record.
        for (index, backend) in registered_backends().iter().enumerate() {
            let duplicate = registered_backends()[..index]
                .iter()
                .any(|earlier| earlier.method() == backend.method());
            assert!(!duplicate, "{} is registered twice", backend.method());
        }
    }

    #[test]
    fn every_registered_backend_can_be_found_by_its_method() {
        for backend in registered_backends() {
            let found = registered_backend(backend.method())
                .expect("a registered backend is findable by its own method");
            assert_eq!(found.method(), backend.method());
        }
    }

    #[test]
    fn declarations_cover_exactly_the_registered_backends() {
        let methods: Vec<CaptureMethod> = registered_backends()
            .iter()
            .map(|backend| backend.method())
            .collect();
        let declared: Vec<CaptureMethod> = registered_declarations()
            .iter()
            .map(|declaration| declaration.method())
            .collect();
        assert_eq!(
            methods, declared,
            "the declarations selection reads and the backends creation uses must be the \
             same set, or a recording can select something it cannot create"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_build_registers_windows_graphics_capture() {
        assert!(
            registered_backend(CaptureMethod::WindowsGraphicsCapture).is_some(),
            "issue #12 added this backend; a Windows build without it is a regression"
        );
        assert!(
            registered_backend(CaptureMethod::DesktopDuplication).is_none(),
            "Desktop Duplication is issue #13 and is not implemented; registering a \
             candidate for it would make selection report a backend that does not exist"
        );
        assert!(
            registered_backend(CaptureMethod::GameCapture).is_none(),
            "AGENTS.md section 34 rules out the usual way of implementing Game Capture"
        );
    }
}
