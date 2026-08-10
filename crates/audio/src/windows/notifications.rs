//! Learning that the endpoint being captured is no longer the right one.
//!
//! # Why this is not optional
//!
//! A capture client keeps working when its endpoint stops being the default.
//! That is the trap. The user plugs in a headset, Windows moves the default
//! render endpoint to it, every application follows — and this crate carries on
//! capturing the speakers, which now receive nothing. The stream stays healthy,
//! no call fails, and the recording is silent from that moment on. Nothing in
//! `IAudioClient` reports it, because from the client's point of view nothing
//! happened.
//!
//! Device *removal* is the easier half: unplugging the endpoint that is being
//! captured invalidates the client, and the next call returns
//! `AUDCLNT_E_DEVICE_INVALIDATED`. That is handled in `loopback.rs`, and it is
//! handled there as well as here because a device can disappear without anyone
//! being notified in time.
//!
//! So this module registers an `IMMNotificationClient` on the device enumerator
//! and turns the four callbacks that matter into one question the capture asks
//! whenever it is about to read: has the endpoint I am on stopped being the one
//! I should be on?
//!
//! # Threading
//!
//! The callbacks arrive on a thread inside the Windows audio service, not on
//! the capture thread, and Microsoft's documentation is explicit that they must
//! not block and must not call back into the enumerator. So a callback here
//! does exactly one thing: take a mutex held for two field writes, and return.
//! No allocation beyond a device identifier string, no logging, no audio call
//! — the same shape `clipped-capture` uses for `FrameArrived`, and for the same
//! reason.

use std::sync::Mutex;

use windows::core::{implement, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};

use crate::error::AudioError;
use crate::windows::endpoint::{identifier_matches, platform_error};

/// Why the capture should look at the default endpoint again.
///
/// Only the reason is kept, not a queue of them: two notifications before the
/// capture next reads mean one reopen, not two, and which of them arrived
/// second is not information anybody acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndpointChange {
    /// Windows moved the default render endpoint somewhere else.
    DefaultMoved,
    /// The endpoint being captured was disabled, unplugged or otherwise left
    /// the active state.
    CaptureEndpointUnavailable,
    /// The endpoint being captured was removed from the system.
    CaptureEndpointRemoved,
    /// A call on the capture client reported that its device had been
    /// invalidated. Raised by `loopback.rs` rather than by a callback, and kept
    /// here so that every reason a capture reopens is one enumeration.
    CaptureEndpointInvalidated,
}

impl EndpointChange {
    /// The words this reason appears as in a log line.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::DefaultMoved => "the default output device changed",
            Self::CaptureEndpointUnavailable => "the output device being recorded was disabled",
            Self::CaptureEndpointRemoved => "the output device being recorded was unplugged",
            Self::CaptureEndpointInvalidated => "the output device being recorded was invalidated",
        }
    }
}

/// What the callbacks write and the capture reads.
#[derive(Debug, Default)]
struct WatchState {
    /// The pending reason to reopen, if any.
    change: Option<EndpointChange>,
    /// The identifier of the endpoint currently being captured. [`None`] when
    /// no stream is open, in which case a state change to some other device is
    /// nothing to do with this capture.
    captured: Option<String>,
}

/// The one fact the capture thread and the audio service's callback thread
/// share.
#[derive(Debug, Default)]
pub(super) struct EndpointWatch {
    state: Mutex<WatchState>,
}

impl EndpointWatch {
    /// Locks the state, recovering from a poisoned mutex.
    ///
    /// A panic elsewhere while this lock was held would poison it, and the
    /// honest response is to carry on with the state as it was: what is behind
    /// the lock is a flag and a string with no invariant between them, so there
    /// is nothing for poisoning to protect, and refusing to notice device
    /// changes because of it would turn a panic somewhere else into a silent
    /// recording.
    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records which endpoint is being captured, or that none is.
    pub(super) fn set_captured(&self, id: Option<String>) {
        self.lock().captured = id;
    }

    /// Notes that the capture should reopen, keeping the first reason.
    ///
    /// The first rather than the last, because the first is the one that
    /// explains what the user did: a headset being plugged in raises a default
    /// change and then, moments later, a state change on the device that lost
    /// the role, and "the default output device changed" is the useful half.
    pub(super) fn request_reopen(&self, change: EndpointChange) {
        let mut state = self.lock();
        state.change.get_or_insert(change);
    }

    /// Takes the pending reason, if there is one.
    pub(super) fn take_change(&self) -> Option<EndpointChange> {
        self.lock().change.take()
    }
}

/// The COM object Windows calls, and the registration that has to be undone.
///
/// Ownership is the point of this type existing rather than the registration
/// being two loose calls. An `IMMNotificationClient` that is not unregistered
/// keeps being called for the life of the process — after the capture that
/// created it has gone — so the registration is held here and released in
/// [`Drop`], which runs whether the capture stopped cleanly or the thread
/// unwound (AGENTS.md section 58).
#[derive(Debug)]
pub(super) struct EndpointNotifications {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl EndpointNotifications {
    /// Registers `watch` to be updated by the device enumerator.
    pub(super) fn register(
        enumerator: &IMMDeviceEnumerator,
        watch: std::sync::Arc<EndpointWatch>,
    ) -> Result<Self, AudioError> {
        let client: IMMNotificationClient = NotificationClient { watch }.into();
        // SAFETY: `enumerator` is a live `IMMDeviceEnumerator` and `client` is
        // a live `IMMNotificationClient` whose reference is kept in the
        // returned value, so Windows cannot be left calling a freed object.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }
            .map_err(|error| platform_error("subscribing to audio device changes", error))?;

        Ok(Self {
            enumerator: enumerator.clone(),
            client,
        })
    }
}

impl Drop for EndpointNotifications {
    fn drop(&mut self) {
        // SAFETY: `client` is the interface that was registered on
        // `enumerator`, both are still live, and this is the matching
        // unregistration.
        if let Err(error) = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        } {
            // Logged rather than propagated: this runs from a drop, including
            // during an unwind, where there is nowhere to return an error to.
            tracing::warn!(%error, "unsubscribing from audio device changes failed");
        }
    }
}

/// The implementation Windows calls into.
#[implement(IMMNotificationClient)]
struct NotificationClient {
    watch: std::sync::Arc<EndpointWatch>,
}

impl IMMNotificationClient_Impl for NotificationClient_Impl {
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _default: &PCWSTR,
    ) -> windows::core::Result<()> {
        // Only the console render role: that is the role this crate captures
        // (`endpoint.rs`), and reacting to the communications role changing
        // would reopen the stream every time a chat application starts.
        if flow == eRender && role == eConsole {
            self.watch.request_reopen(EndpointChange::DefaultMoved);
        }
        Ok(())
    }

    fn OnDeviceStateChanged(
        &self,
        device: &PCWSTR,
        state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        if state != DEVICE_STATE_ACTIVE && self.concerns_captured_endpoint(device) {
            self.watch
                .request_reopen(EndpointChange::CaptureEndpointUnavailable);
        }
        Ok(())
    }

    fn OnDeviceRemoved(&self, device: &PCWSTR) -> windows::core::Result<()> {
        if self.concerns_captured_endpoint(device) {
            self.watch
                .request_reopen(EndpointChange::CaptureEndpointRemoved);
        }
        Ok(())
    }

    fn OnDeviceAdded(&self, _device: &PCWSTR) -> windows::core::Result<()> {
        // Deliberately nothing. A device appearing only matters if Windows
        // makes it the default, and that arrives as `OnDefaultDeviceChanged`.
        // Reopening for every device that appears would tear the stream down
        // whenever a monitor with speakers woke up.
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        // Deliberately nothing. Properties change for reasons — a name, an
        // icon, a volume — that do not affect which samples arrive.
        Ok(())
    }
}

impl NotificationClient_Impl {
    /// Whether a callback's device identifier is the endpoint being captured.
    fn concerns_captured_endpoint(&self, device: &PCWSTR) -> bool {
        let state = self.watch.lock();
        state
            .captured
            .as_deref()
            .is_some_and(|captured| identifier_matches(device, captured))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use windows::core::ComObject;

    use super::*;

    /// Builds the object Windows would call, without registering it.
    ///
    /// The callbacks are what this module is, so they are tested by calling
    /// them exactly as the audio service does. What is not tested here is
    /// Windows choosing to call them, which needs a device to be unplugged;
    /// `docs/audio-routing.md` records how that was verified by hand.
    fn client(watch: &Arc<EndpointWatch>) -> ComObject<NotificationClient> {
        ComObject::new(NotificationClient {
            watch: Arc::clone(watch),
        })
    }

    #[test]
    fn the_first_reason_is_the_one_kept() {
        // Plugging in a headset raises a default change and then a state change
        // on the device that lost the role. Both mean "reopen"; the first is
        // the one that explains what happened.
        let watch = EndpointWatch::default();
        watch.request_reopen(EndpointChange::DefaultMoved);
        watch.request_reopen(EndpointChange::CaptureEndpointUnavailable);

        assert_eq!(watch.take_change(), Some(EndpointChange::DefaultMoved));
        assert_eq!(
            watch.take_change(),
            None,
            "one reopen, not one per notification"
        );
    }

    #[test]
    fn a_state_change_on_some_other_device_is_not_this_captures_business() {
        // A second sound card being disabled while this capture is on the
        // first must not tear down a healthy stream.
        let watch = Arc::new(EndpointWatch::default());
        watch.set_captured(Some("{0.0.0.00000000}.{captured}".to_owned()));

        let client = client(&watch);

        let other = windows::core::HSTRING::from("{0.0.0.00000000}.{somebody-else}");
        client
            .OnDeviceRemoved(&PCWSTR(other.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), None);

        let captured = windows::core::HSTRING::from("{0.0.0.00000000}.{captured}");
        client
            .OnDeviceRemoved(&PCWSTR(captured.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(
            watch.take_change(),
            Some(EndpointChange::CaptureEndpointRemoved)
        );
    }

    #[test]
    fn only_the_console_render_role_moving_matters() {
        // The communications default moves whenever a headset is used for a
        // call. Recording follows the console role, so this must not reopen.
        let watch = Arc::new(EndpointWatch::default());
        let client = client(&watch);

        let id = windows::core::HSTRING::from("{0.0.0.00000000}.{new-default}");
        client
            .OnDefaultDeviceChanged(
                windows::Win32::Media::Audio::eCapture,
                eConsole,
                &PCWSTR(id.as_ptr()),
            )
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), None, "an input device is not this");

        client
            .OnDefaultDeviceChanged(
                eRender,
                windows::Win32::Media::Audio::eCommunications,
                &PCWSTR(id.as_ptr()),
            )
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), None, "the chat role is not this");

        client
            .OnDefaultDeviceChanged(eRender, eConsole, &PCWSTR(id.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), Some(EndpointChange::DefaultMoved));
    }

    #[test]
    fn a_device_becoming_active_again_is_not_a_reason_to_reopen() {
        let watch = Arc::new(EndpointWatch::default());
        watch.set_captured(Some("{0.0.0.00000000}.{captured}".to_owned()));
        let client = client(&watch);

        let id = windows::core::HSTRING::from("{0.0.0.00000000}.{captured}");
        client
            .OnDeviceStateChanged(&PCWSTR(id.as_ptr()), DEVICE_STATE_ACTIVE)
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), None);

        client
            .OnDeviceStateChanged(
                &PCWSTR(id.as_ptr()),
                windows::Win32::Media::Audio::DEVICE_STATE_UNPLUGGED,
            )
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(
            watch.take_change(),
            Some(EndpointChange::CaptureEndpointUnavailable)
        );
    }
}
