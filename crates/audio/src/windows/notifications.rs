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
//! happened. The same is true on the input side: the default microphone moves
//! to the headset that was just plugged in while the recording keeps listening
//! to a desk microphone nobody is talking into.
//!
//! Device *removal* is the easier half: unplugging the endpoint that is being
//! captured invalidates the client, and the next call returns
//! `AUDCLNT_E_DEVICE_INVALIDATED`. That is handled in `endpoint_capture.rs`,
//! and it is handled there as well as here because a device can disappear
//! without anyone being notified in time.
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
    eConsole, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
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
    /// Windows moved the default endpoint this capture follows somewhere else.
    DefaultMoved,
    /// The endpoint being captured was disabled, unplugged or otherwise left
    /// the active state.
    CaptureEndpointUnavailable,
    /// The endpoint being captured was removed from the system.
    CaptureEndpointRemoved,
    /// A call on the capture client reported that its device had been
    /// invalidated. Raised by `endpoint_capture.rs` rather than by a callback,
    /// and kept here so that every reason a capture reopens is one enumeration.
    CaptureEndpointInvalidated,
}

impl EndpointChange {
    /// The words this reason appears as in a log line.
    ///
    /// Which device is meant is the `source` and `device` fields of the line
    /// this appears on, so the reason itself does not name one: the same four
    /// things happen to a pair of speakers and to a microphone.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::DefaultMoved => "the default device changed",
            Self::CaptureEndpointUnavailable => "the device being recorded was disabled",
            Self::CaptureEndpointRemoved => "the device being recorded was unplugged",
            Self::CaptureEndpointInvalidated => "the device being recorded was invalidated",
        }
    }
}

/// What the callbacks write and the capture reads.
#[derive(Debug, Default)]
struct WatchState {
    /// The pending reason to reopen, if any.
    change: Option<EndpointChange>,
    /// The identifier of the endpoint this capture is on, set from the moment
    /// the endpoint is chosen rather than from the moment its stream is
    /// running: opening one takes long enough to be unplugged during, and a
    /// notification that arrives while this is [`None`] is discarded as being
    /// about somebody else's device. [`None`] when the capture has no endpoint
    /// at all, which is a state it waits through rather than fails on.
    captured: Option<String>,
}

/// The one fact the capture thread and the audio service's callback thread
/// share.
///
/// Which notifications matter is fixed when the capture opens - the flow its
/// endpoints are on, and whether it follows the default at all - so those two
/// are plain fields rather than shared state, and a callback reads them without
/// taking the lock.
#[derive(Debug)]
pub(super) struct EndpointWatch {
    /// The side of the audio stack this capture's endpoints are on. A default
    /// microphone moving is not a reason to reopen system audio.
    flow: EDataFlow,
    /// Whether the default endpoint moving concerns this capture. It does not
    /// when the user chose a particular device: a chosen microphone being
    /// unplugged must not move the recording onto whatever Windows promoted.
    follows_default: bool,
    state: Mutex<WatchState>,
}

impl EndpointWatch {
    /// A watch for one capture's endpoints.
    pub(super) fn new(flow: EDataFlow, follows_default: bool) -> Self {
        Self {
            flow,
            follows_default,
            state: Mutex::new(WatchState::default()),
        }
    }

    /// Whether a default-device notification is about the endpoint this capture
    /// follows.
    fn concerns_default(&self, flow: EDataFlow, role: ERole) -> bool {
        // Only the console role: that is the role this crate records
        // (`endpoint.rs`), and reacting to the communications role changing
        // would reopen the stream every time a chat application started.
        self.follows_default && flow == self.flow && role == eConsole
    }

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
        if self.watch.concerns_default(flow, role) {
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
    use windows::Win32::Media::Audio::{eCapture, eRender};

    use super::*;

    /// Builds the object Windows would call, without registering it.
    ///
    /// The callbacks are what this module is, so they are tested by calling
    /// them exactly as the audio service does. What is *not* covered anywhere
    /// in this crate is Windows choosing to call them: that needs a device to
    /// be unplugged on a real machine, it has not been done, and until it has,
    /// the registration itself — `RegisterEndpointNotificationCallback`
    /// succeeding and Windows dispatching through the generated vtable — is
    /// unverified. `docs/audio-routing.md` gives the procedure and
    /// [issue #141](https://github.com/wildware-uk/clipped/issues/141) tracks
    /// carrying it out.
    fn client(watch: &Arc<EndpointWatch>) -> ComObject<NotificationClient> {
        ComObject::new(NotificationClient {
            watch: Arc::clone(watch),
        })
    }

    /// A watch for a capture that follows the default output device, which is
    /// what system audio capture uses.
    fn following_the_default_output() -> EndpointWatch {
        EndpointWatch::new(eRender, true)
    }

    #[test]
    fn the_first_reason_is_the_one_kept() {
        // Plugging in a headset raises a default change and then a state change
        // on the device that lost the role. Both mean "reopen"; the first is
        // the one that explains what happened.
        let watch = following_the_default_output();
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
        let watch = Arc::new(following_the_default_output());
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
    fn only_the_console_role_on_this_captures_own_side_moving_matters() {
        // The communications default moves whenever a headset is used for a
        // call. Recording follows the console role, so this must not reopen.
        // Nor must the default *microphone* moving reopen system audio: the
        // two captures share this code and would otherwise each tear down for
        // the other's device.
        let watch = Arc::new(following_the_default_output());
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
    fn the_default_microphone_moving_is_a_microphone_captures_business_only() {
        // The other side of the same rule, from the microphone's point of view.
        let watch = Arc::new(EndpointWatch::new(eCapture, true));
        let client = client(&watch);

        let id = windows::core::HSTRING::from("{0.0.1.00000000}.{new-default}");
        client
            .OnDefaultDeviceChanged(eRender, eConsole, &PCWSTR(id.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(
            watch.take_change(),
            None,
            "somebody plugging in speakers is not a reason to reopen the microphone"
        );

        client
            .OnDefaultDeviceChanged(eCapture, eConsole, &PCWSTR(id.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), Some(EndpointChange::DefaultMoved));
    }

    #[test]
    fn a_capture_on_a_chosen_device_does_not_follow_the_default_anywhere() {
        // The user picked a microphone. Windows promoting the webcam's
        // microphone to default - which unplugging the chosen one does - must
        // not move the recording onto it: a track that silently became a
        // different microphone is worse than a silent one. The chosen device
        // going away is still noticed, because that is what the capture waits
        // for.
        let watch = Arc::new(EndpointWatch::new(eCapture, false));
        watch.set_captured(Some("{0.0.1.00000000}.{chosen}".to_owned()));
        let client = client(&watch);

        let id = windows::core::HSTRING::from("{0.0.1.00000000}.{promoted}");
        client
            .OnDefaultDeviceChanged(eCapture, eConsole, &PCWSTR(id.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(watch.take_change(), None);

        let chosen = windows::core::HSTRING::from("{0.0.1.00000000}.{chosen}");
        client
            .OnDeviceRemoved(&PCWSTR(chosen.as_ptr()))
            .expect("the callback reports success to Windows whatever it decides");
        assert_eq!(
            watch.take_change(),
            Some(EndpointChange::CaptureEndpointRemoved)
        );
    }

    #[test]
    fn a_device_becoming_active_again_is_not_a_reason_to_reopen() {
        let watch = Arc::new(following_the_default_output());
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
