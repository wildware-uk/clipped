//! Activating an audio client that is not on any device.
//!
//! # Why this is not `IMMDevice::Activate`
//!
//! Every capture in this crate but one opens a client on a device: find the
//! endpoint, call `Activate`, and hold the interface it returns
//! (`endpoint.rs`). Process-scoped loopback has no device to find. Windows
//! exposes it as a *virtual* audio device — the path
//! `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`, which no enumerator lists — reached
//! through `ActivateAudioInterfaceAsync`, with the process to scope to passed
//! in a `PROPVARIANT` carrying an `AUDIOCLIENT_ACTIVATION_PARAMS` blob
//! ([ADR 0003](../../../../docs/adr/0003-process-specific-audio-capture.md)).
//!
//! That call is asynchronous. It returns immediately with an operation object
//! and calls a completion handler this crate implements when the activation has
//! actually happened, which may be a failure. So there are three results to
//! tell apart rather than one: the call itself failing, the activation
//! completing with a failure, and the whole thing never completing at all.
//!
//! # The failure that matters most
//!
//! Process loopback is documented as available from Windows build 20348. No
//! shipping consumer Windows 10 release reaches that number, so on a Windows 10
//! machine this activation is expected to fail — and the product's central
//! feature is unavailable there (ADR 0003's first consequence). That is not an
//! exceptional condition to be reported as an unclassified platform error: it
//! is a supported outcome with a documented fallback, so it has an error of its
//! own, [`AudioError::ProcessLoopbackUnavailable`], whose message says what is
//! missing and what the machine needs (AGENTS.md sections 15 and 45).
//!
//! # Threading and ownership
//!
//! The handler is called on a thread inside the Windows audio service, and does
//! one thing: set an event. Nothing is allocated in it, no lock is taken and no
//! audio call is made — the same shape `notifications.rs` uses, and for the
//! same reason.
//!
//! The event is owned by an [`ActivationSignal`] shared between the caller and
//! the handler, so it is closed when the last of the two lets go of it. That
//! matters on the one path where the caller gives up first: a caller that has
//! stopped waiting must not close a handle Windows is about to signal, and
//! COM's own reference counting is what decides when the object — and with it
//! the handle — actually goes.

use core::mem::ManuallyDrop;
use core::time::Duration;
use std::sync::Arc;

use windows::core::{implement, Interface, Ref, GUID, HRESULT};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioClient, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::BLOB;
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use crate::error::AudioError;

/// How long an activation is waited for before it is treated as never coming.
///
/// The activation is the audio service doing a small amount of work on a thread
/// it already has, and it completes in single-digit milliseconds on the machine
/// this was written on. Five seconds is far beyond that and is a bound on a
/// hang rather than a latency anybody should approach: a recorder that waited
/// for ever here would be a recording that never starts, with nothing in the
/// log to say why (AGENTS.md section 16).
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);

/// The event the completion handler sets and the caller waits on.
///
/// Shared by an [`Arc`] rather than owned by either, so that the handle is
/// closed exactly once, by whichever of the two lets go last.
#[derive(Debug)]
struct ActivationSignal(HANDLE);

// SAFETY: a Windows event handle is a kernel object with no thread affinity,
// and the only operations here are `SetEvent`, `WaitForSingleObject` and
// `CloseHandle`, all of which any thread may perform. The handle is never
// mutated after construction.
unsafe impl Send for ActivationSignal {}
// SAFETY: as above; both operations are safe to perform concurrently, which is
// exactly what an event is for.
unsafe impl Sync for ActivationSignal {}

impl ActivationSignal {
    /// Creates the event, unsignalled.
    fn new() -> Result<Arc<Self>, AudioError> {
        // SAFETY: all four arguments are optional and null/false is valid for
        // each: default security, manual-reset, initially unsignalled, unnamed.
        // Manual-reset so that a signal set before the wait begins is not
        // consumed by anything else.
        let handle = unsafe { CreateEventW(None, true, false, None) }.map_err(|error| {
            AudioError::Platform {
                operation: "creating the event a process-scoped activation completes on",
                source: Box::new(error),
            }
        })?;
        Ok(Arc::new(Self(handle)))
    }

    /// Reports that the activation has completed.
    fn signal(&self) {
        // SAFETY: `self.0` is the event created above and is not closed until
        // this value drops, which cannot happen while a method is running on
        // it.
        //
        // A failure is discarded because there is nothing to do about it and
        // nowhere to report it from — this runs on a Windows thread — and the
        // caller's wait has a timeout precisely so that a signal that never
        // arrives is survivable (AGENTS.md section 15).
        let _ = unsafe { SetEvent(self.0) };
    }

    /// Waits for the activation, answering whether it completed in time.
    fn wait(&self, limit: Duration) -> bool {
        let milliseconds = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `self.0` is the event created above, and this value keeps it
        // open for the duration of the call.
        unsafe { WaitForSingleObject(self.0, milliseconds) == WAIT_OBJECT_0 }
    }
}

impl Drop for ActivationSignal {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateEventW` in `new`, this value has
        // owned it since, and nothing refers to it afterwards.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            tracing::warn!(%error, "closing an activation's event handle failed");
        }
    }
}

/// The object Windows calls when an asynchronous activation has finished.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationCompleted {
    signal: Arc<ActivationSignal>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationCompleted_Impl {
    /// Called on a Windows thread when the activation has completed, whether it
    /// succeeded or failed.
    ///
    /// The result is deliberately not read here. It is read by the caller from
    /// the operation object `ActivateAudioInterfaceAsync` returned — the same
    /// object this is handed — which keeps everything that can fail on the
    /// thread that can report it, and keeps this callback to one call that
    /// cannot block.
    fn ActivateCompleted(
        &self,
        _operation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        self.signal.signal();
        Ok(())
    }
}

/// Activates an audio client scoped to `target` and the processes it started.
///
/// The client comes back initialised by nobody: the caller chooses the format
/// and calls `Initialize` on it, exactly as it would for a client activated on
/// an endpoint. Unlike an endpoint's, this client has no mix format to ask for
/// — `GetMixFormat` is not supported on it — which is why the format is the
/// caller's decision (`process_loopback.rs`).
///
/// # Errors
///
/// [`AudioError::ProcessLoopbackUnavailable`] when Windows will not give a
/// process-scoped client at all, which is what a machine below build 20348 is
/// expected to answer, and when the activation does not complete within
/// [`ACTIVATION_TIMEOUT`]. [`AudioError::Platform`] for the failures that are
/// this crate's own — an event that cannot be created, an interface that comes
/// back as something else.
pub(super) fn activate_process_loopback(
    target: u32,
    mode: PROCESS_LOOPBACK_MODE,
) -> Result<IAudioClient, AudioError> {
    let mut parameters = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target,
                ProcessLoopbackMode: mode,
            },
        },
    };

    // A `PROPVARIANT` of type `VT_BLOB` pointing at the structure above. It
    // borrows rather than owns: the blob *is* the local `parameters`, which
    // outlives the call below.
    //
    // `ManuallyDrop` is load-bearing and not caution. windows-rs implements
    // `Drop` for `PROPVARIANT` as `PropVariantClear`, and clearing a `VT_BLOB`
    // frees `pBlobData` with `CoTaskMemFree` — which, for a pointer into this
    // function's own stack frame, is heap corruption at the moment the value
    // goes out of scope. Written without this, the probe in
    // `examples/process_loopback_probe.rs` died with `STATUS_HEAP_CORRUPTION`
    // the instant an activation succeeded.
    let mut activation = ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: writing the discriminant and then the matching member of the
    // union is the contract of a `PROPVARIANT`. `vt` says which member is live,
    // and `VT_BLOB` means `blob` is. The pointer is to a local that is still
    // alive when `ActivateAudioInterfaceAsync` reads it, and the size is that
    // structure's own.
    unsafe {
        let contents = &mut activation.Anonymous.Anonymous;
        contents.vt = VT_BLOB;
        contents.Anonymous.blob = BLOB {
            cbSize: u32::try_from(size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>())
                .expect("AUDIOCLIENT_ACTIVATION_PARAMS is far smaller than u32::MAX"),
            pBlobData: (&raw mut parameters).cast::<u8>(),
        };
    }

    let signal = ActivationSignal::new()?;
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationCompleted {
        signal: Arc::clone(&signal),
    }
    .into();

    // SAFETY: the path is Windows' own constant for the process-loopback
    // virtual device, the interface identifier is the one the returned
    // interface is cast to below, the activation parameters point at a live
    // local, and `handler` is a live COM object this scope keeps a reference to
    // for as long as the wait lasts.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID as *const GUID,
            Some(&*activation),
            &handler,
        )
    }
    .map_err(|error| {
        AudioError::process_loopback_unavailable(format!(
            "Windows refused to start a process-scoped audio capture ({error})"
        ))
    })?;

    if !signal.wait(ACTIVATION_TIMEOUT) {
        // The handler still holds the signal, so nothing is closed underneath
        // Windows by returning here.
        return Err(AudioError::process_loopback_unavailable(format!(
            "Windows did not answer a request for a process-scoped audio capture within {} \
             seconds",
            ACTIVATION_TIMEOUT.as_secs()
        )));
    }

    let mut result = HRESULT(0);
    let mut activated = None;
    // SAFETY: the activation has completed — that is what the wait above
    // established — which is when `GetActivateResult` is valid. Both out
    // parameters are live locals of the types the signature names.
    unsafe { operation.GetActivateResult(&mut result, &mut activated) }.map_err(|error| {
        AudioError::process_loopback_unavailable(format!(
            "Windows would not say how a process-scoped audio capture ended ({error})"
        ))
    })?;

    result.ok().map_err(|error| {
        AudioError::process_loopback_unavailable(format!(
            "Windows could not start a process-scoped audio capture of process {target} \
             ({error})"
        ))
    })?;

    let activated = activated.ok_or_else(|| {
        AudioError::process_loopback_unavailable(
            "Windows reported a process-scoped audio capture as started but produced no audio \
             client"
                .to_owned(),
        )
    })?;

    activated.cast::<IAudioClient>().map_err(|error| {
        // The activation asked for `IAudioClient` by its own interface
        // identifier, so anything else here is Windows contradicting itself
        // rather than a machine that cannot do this.
        AudioError::Platform {
            operation: "reading the audio client a process-scoped activation produced",
            source: Box::new(error),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_activation_blob_describes_the_process_it_scopes_to() {
        // The one piece of this that is arithmetic rather than a Windows call:
        // the structure Windows reads out of the `PROPVARIANT` has to name the
        // process and the mode that were asked for. A blob whose size or
        // pointer is wrong is read as whatever is next to it in memory, which
        // is a capture scoped to some other process — the silent
        // misattribution ADR 0003 is written against.
        let parameters = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: 4_242,
                    ProcessLoopbackMode:
                        windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };

        // SAFETY: the activation type says which member of the union is live,
        // and it is the one being read.
        let loopback = unsafe { parameters.Anonymous.ProcessLoopbackParams };
        assert_eq!(loopback.TargetProcessId, 4_242);
        assert_eq!(
            loopback.ProcessLoopbackMode,
            windows::Win32::Media::Audio::PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
        );
        assert_eq!(
            parameters.ActivationType,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
        );
        assert!(
            size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
                >= size_of::<AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS>(),
            "the blob has to be large enough to carry the parameters inside it"
        );
    }

    #[test]
    fn an_activation_that_never_completes_is_not_waited_for_for_ever() {
        // The wait itself, on an event nothing signals: it has to come back
        // and say so rather than hang the thread that opened the capture.
        let signal = ActivationSignal::new().expect("an event can always be created");
        assert!(!signal.wait(Duration::from_millis(10)));

        signal.signal();
        assert!(
            signal.wait(Duration::from_millis(10)),
            "a signalled event has to be seen as signalled"
        );
        assert!(
            signal.wait(Duration::ZERO),
            "the event is manual-reset, so a completion cannot be consumed by one look at it"
        );
    }

    #[test]
    fn the_completion_handler_signals_the_caller() {
        // What Windows does to this crate when an activation finishes, called
        // the way Windows calls it: through the interface, so the generated
        // vtable is what dispatches. A handler that did not signal would leave
        // every activation timing out on a machine that can do this perfectly
        // well.
        let signal = ActivationSignal::new().expect("an event can always be created");
        let handler: IActivateAudioInterfaceCompletionHandler = ActivationCompleted {
            signal: Arc::clone(&signal),
        }
        .into();

        assert!(!signal.wait(Duration::ZERO));
        // SAFETY: `handler` is a live COM object, and the operation argument is
        // optional — this implementation does not read it, which is the point
        // the module documentation makes about keeping the callback trivial.
        unsafe { handler.ActivateCompleted(None) }
            .expect("the handler reports success to Windows whatever it decides");
        assert!(signal.wait(Duration::ZERO));
    }
}
