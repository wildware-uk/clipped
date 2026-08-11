//! The WMI notification subscription, which is how this watcher avoids polling.
//!
//! # Why WMI
//!
//! Windows offers three ways to learn that a process started without asking
//! repeatedly, and only one of them is available to an application that is not
//! elevated:
//!
//! | Mechanism | Latency | Cost | Needs |
//! | --- | --- | --- | --- |
//! | `__InstanceCreationEvent WITHIN n` over `Win32_Process` | up to `n` seconds | one process-table comparison per `n` seconds, inside the WMI service | nothing |
//! | `Win32_ProcessStartTrace` (WMI over ETW) | milliseconds | none until something happens | administrator |
//! | An ETW kernel session (`NT Kernel Logger`) | milliseconds | none until something happens | administrator, and a session other tools compete for |
//!
//! Clipped runs as the user, unelevated, because a recorder that demands
//! administrator rights is a recorder people run once. That rules the second
//! and third rows out as the *primary* source — a mechanism that only works for
//! some users is not a detection strategy — and leaves the first, which works
//! for everybody at the cost of a bounded delay and of work done in a service
//! rather than here.
//!
//! What that buys is the thing the ticket asks for: this process does not poll.
//! It blocks inside `IEnumWbemClassObject::Next` and is woken when something
//! happens. The polling that remains is the WMI service comparing the process
//! table with itself once per interval, and `docs/game-detection.md` records
//! what that costs, measured rather than assumed.
//!
//! # The fallback
//!
//! WMI is a service, and services stop. Its repository can be corrupted, group
//! policy can refuse the connection, and `winmgmt` can be restarted underneath
//! a working subscription. Every one of those ends with no events arriving,
//! which for a recorder means silently never recording again — so failure is
//! explicit here: subscription failure at start, and loss afterwards, both fall
//! back to [`super::snapshot::poll`] (AGENTS.md section 16).

use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{Arc, OnceLock};

use windows::core::{w, Interface, BSTR, HRESULT, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoIncrementMTAUsage, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VariantClear, VARENUM, VARIANT, VT_BSTR, VT_I4, VT_UNKNOWN};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_S_TIMEDOUT,
};

use super::super::config::WatchConfig;
use super::super::error::SourceError;
use super::super::process::ProcessSnapshot;
use super::super::source::{SourceEvent, SourceMessage};
use super::stop::Stop;

/// The namespace `Win32_Process` lives in.
const NAMESPACE: &str = r"ROOT\CIMV2";

/// Which half of the process lifetime a subscription watches.
///
/// Two subscriptions rather than one over `__InstanceOperationEvent`, which
/// would cover both in a single query. That query also delivers
/// `__InstanceModificationEvent`, and a `Win32_Process` instance changes
/// whenever its working set does — which is to say constantly, for every
/// process on the machine. One query would be cheaper in the service and
/// ruinous everywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// A process appeared: `__InstanceCreationEvent`.
    Started,
    /// A process disappeared: `__InstanceDeletionEvent`.
    Exited,
}

impl Change {
    /// The WQL query that subscribes to this half.
    ///
    /// `WITHIN` is how often WMI is allowed to look, and it is the floor under
    /// detection latency. It is expressed in whole seconds because that is the
    /// smallest interval the design is willing to ask a service to do work at
    /// (see [`WatchConfig::notification_interval`]).
    fn query(self, seconds: u32) -> String {
        let class = match self {
            Self::Started => "__InstanceCreationEvent",
            Self::Exited => "__InstanceDeletionEvent",
        };
        format!("SELECT * FROM {class} WITHIN {seconds} WHERE TargetInstance ISA 'Win32_Process'")
    }

    /// The name used in diagnostics and in the pumping thread's name.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Started => "creation",
            Self::Exited => "deletion",
        }
    }
}

/// A live subscription: the enumerator, and the connection that must outlive it.
struct Subscription {
    /// Held only to keep the connection open. Releasing it invalidates the
    /// enumerator built from it, so it is not dead weight even though nothing
    /// calls it again.
    _services: IWbemServices,
    enumerator: IEnumWbemClassObject,
}

/// Subscribes and pumps events until `stop` is raised or the source is lost.
///
/// Reports the outcome of the subscription itself through `ready`, once, so
/// that the watcher can fall back synchronously if WMI is unavailable rather
/// than discovering it the first time a game fails to be noticed.
///
/// # Threading
///
/// Everything COM in this crate happens on the thread that calls this and
/// nowhere else: the connection, the enumerator and every interface pointer are
/// created here, used here and dropped here. That is deliberate — it means
/// there is no marshalling question to answer and no apartment to reason about
/// beyond the one this thread joins.
pub(crate) fn run(
    change: Change,
    config: WatchConfig,
    events: &Sender<SourceMessage>,
    ready: &SyncSender<Result<(), SourceError>>,
    stop: &Arc<Stop>,
) {
    let subscription = match subscribe(change, config) {
        Ok(subscription) => {
            let _ = ready.send(Ok(()));
            subscription
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    pump(&subscription, change, config, events, stop);
}

/// Connects to WMI and starts one notification query.
fn subscribe(change: Change, config: WatchConfig) -> Result<Subscription, SourceError> {
    ensure_apartment()?;

    // SAFETY: `WbemLocator` is a class identifier constant, there is no
    // aggregation, and the requested interface matches the binding's type. The
    // returned pointer is owned by `locator` and released when it drops.
    let locator: IWbemLocator =
        unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| SourceError::new("CoCreateInstance(WbemLocator)", error))?;

    // SAFETY: every string argument is a live `BSTR` for the duration of the
    // call. The empty ones are null `BSTR`s, which is how this API is told to
    // use the current user's credentials and the default locale and authority.
    let services = unsafe {
        locator.ConnectServer(
            &BSTR::from(NAMESPACE),
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )
    }
    .map_err(|error| SourceError::new("IWbemLocator::ConnectServer", error))?;

    // Without this the proxy identifies the caller but cannot act as them, and
    // the query fails with access denied on a machine where the account is not
    // an administrator — which is every machine this is designed for.
    //
    // SAFETY: the proxy is a live interface this scope owns; the principal name
    // and authentication info are null, which selects the defaults for the
    // current user; every other argument is a constant from the SDK.
    unsafe {
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    }
    .map_err(|error| SourceError::new("CoSetProxyBlanket", error))?;

    let query = change.query(config.notification_interval_seconds());

    // `RETURN_IMMEDIATELY` with `FORWARD_ONLY` is what makes this
    // semi-synchronous: the call returns an enumerator straight away and the
    // waiting happens in `Next`, where it can be given a timeout. The
    // alternative — `ExecNotificationQueryAsync` with an object sink — needs an
    // `IWbemObjectSink` implementation and an unsecured apartment to receive
    // callbacks on, for the same events.
    //
    // SAFETY: both strings are live `BSTR`s for the duration of the call, the
    // flags are SDK constants, and there is no context object.
    let enumerator = unsafe {
        services.ExecNotificationQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query.as_str()),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )
    }
    .map_err(|error| {
        SourceError::new(
            match change {
                Change::Started => "ExecNotificationQuery(__InstanceCreationEvent)",
                Change::Exited => "ExecNotificationQuery(__InstanceDeletionEvent)",
            },
            error,
        )
    })?;

    tracing::debug!(
        change = change.as_str(),
        interval_seconds = config.notification_interval_seconds(),
        "subscribed to WMI process notifications"
    );

    Ok(Subscription {
        _services: services,
        enumerator,
    })
}

/// Blocks on the enumerator, forwarding events, until told to stop.
fn pump(
    subscription: &Subscription,
    change: Change,
    config: WatchConfig,
    events: &Sender<SourceMessage>,
    stop: &Arc<Stop>,
) {
    // The timeout is not a poll interval: it is how long this thread is willing
    // to be un-interruptible. A COM call that is blocking cannot be cancelled,
    // so shutdown waits for the current `Next` to return, and this bounds that
    // wait. Each expiry costs one atomic read, which is why it is measured in
    // seconds rather than milliseconds.
    let timeout = i32::try_from(config.notification_interval_seconds().saturating_mul(1_000))
        .unwrap_or(i32::MAX);

    while !stop.is_raised() {
        let mut received: [Option<IWbemClassObject>; 1] = [None];
        let mut returned = 0_u32;

        // SAFETY: `received` is a live array of exactly the length the call is
        // told about, and `returned` is a live local. The call writes at most
        // one interface pointer, which this scope then owns.
        let status = unsafe {
            subscription
                .enumerator
                .Next(timeout, &mut received, &mut returned)
        };

        if status == HRESULT(WBEM_S_TIMEDOUT.0) {
            continue;
        }
        if status.is_err() {
            let error = SourceError::new(
                "IEnumWbemClassObject::Next",
                windows::core::Error::from_hresult(status),
            );
            tracing::warn!(
                change = change.as_str(),
                error = %error,
                "WMI process notifications stopped"
            );
            let _ = events.send(SourceMessage::Lost(error));
            return;
        }

        let Some(event) = received[0].take().filter(|_| returned > 0) else {
            continue;
        };

        let Some(event) = read_event(&event, change) else {
            // An event whose `TargetInstance` has no `ProcessId` is not a
            // process this watcher can say anything about. Dropped rather than
            // guessed at, and counted only in a debug line, because the one
            // machine that produces them would otherwise produce a warning per
            // second.
            tracing::debug!(
                change = change.as_str(),
                "a WMI process event carried no usable instance"
            );
            continue;
        };

        if events.send(SourceMessage::Event(event)).is_err() {
            return;
        }
    }
}

/// Turns one notification into a source event.
fn read_event(event: &IWbemClassObject, change: Change) -> Option<SourceEvent> {
    let instance = Variant::read(event, w!("TargetInstance"))?.as_object()?;
    let pid = Variant::read(&instance, w!("ProcessId"))?.as_u32()?;

    match change {
        Change::Exited => Some(SourceEvent::Exited { pid }),
        Change::Started => {
            let parent_pid = Variant::read(&instance, w!("ParentProcessId"))?.as_u32()?;
            // Null for processes this account cannot open, which is normal for
            // anything at a higher integrity level. `Name` is in the instance
            // either way.
            let path = Variant::read(&instance, w!("ExecutablePath"))?.as_string();
            let name = Variant::read(&instance, w!("Name"))?
                .as_string()
                .unwrap_or_default();

            Some(SourceEvent::Started(ProcessSnapshot::new(
                pid,
                parent_pid,
                path.map(Into::into),
                &name,
            )))
        }
    }
}

/// A `VARIANT` that clears itself.
///
/// `IWbemClassObject::Get` fills a caller-owned `VARIANT`, and the caller owns
/// whatever is inside it — a `BSTR` to free, or an interface to release. The
/// raw type has no destructor, so every early return in the reads above would
/// otherwise be a leak of a string or a reference, once per process event, for
/// as long as Clipped runs.
struct Variant(VARIANT);

impl Variant {
    /// Reads `name` from `object`, or [`None`] if it has no such property.
    fn read(object: &IWbemClassObject, name: PCWSTR) -> Option<Self> {
        let mut value = VARIANT::default();

        // SAFETY: `name` is a static null-terminated wide string, `value` is a
        // live zeroed `VARIANT` of the type the signature names, and the two
        // optional out parameters are declined. On success the call has filled
        // `value`, which this `Variant` now owns and clears in `Drop`.
        unsafe { object.Get(name, 0, &mut value, None, None) }.ok()?;

        Some(Self(value))
    }

    /// The variant's type tag.
    fn kind(&self) -> VARENUM {
        // SAFETY: reading `vt` is defined for any initialised `VARIANT`; it is
        // the discriminant that says which arm of the union below is live, and
        // it is not itself part of that union.
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// The value as an unsigned 32-bit number.
    ///
    /// WMI reports `uint32` properties — `ProcessId`, `ParentProcessId` — as
    /// `VT_I4`, because `VARIANT` has no unsigned 32-bit arm that automation
    /// clients are expected to handle. The bits are the identifier; the sign
    /// the tag implies is an artefact of the transport.
    fn as_u32(&self) -> Option<u32> {
        if self.kind() != VT_I4 {
            return None;
        }

        // SAFETY: the tag says `VT_I4`, so `lVal` is the live arm.
        let signed = unsafe { self.0.Anonymous.Anonymous.Anonymous.lVal };

        // The cast keeps the bits and changes only how they are read, which is
        // the whole point: the value is an identifier that was posted through a
        // signed field.
        #[allow(
            clippy::cast_sign_loss,
            reason = "the bits are an identifier, not a number"
        )]
        Some(signed as u32)
    }

    /// The value as a string, or [`None`] when it is null or empty.
    fn as_string(&self) -> Option<String> {
        if self.kind() != VT_BSTR {
            return None;
        }

        // SAFETY: the tag says `VT_BSTR`, so `bstrVal` is the live arm. The
        // borrow ends before this value is cleared, and `to_string` copies.
        let text = unsafe { self.0.Anonymous.Anonymous.Anonymous.bstrVal.to_string() };
        (!text.is_empty()).then_some(text)
    }

    /// The value as the embedded object it points at.
    ///
    /// This is how a notification carries the process it is about:
    /// `TargetInstance` is a whole `Win32_Process` instance inside the event.
    fn as_object(&self) -> Option<IWbemClassObject> {
        if self.kind() != VT_UNKNOWN {
            return None;
        }

        // SAFETY: the tag says `VT_UNKNOWN`, so `punkVal` is the live arm.
        // Cloning takes a reference of its own, so the returned interface stays
        // valid after this variant is cleared.
        let unknown = unsafe { self.0.Anonymous.Anonymous.Anonymous.punkVal.as_ref() }?.clone();
        unknown.cast::<IWbemClassObject>().ok()
    }
}

impl Drop for Variant {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a `VARIANT` this value owns and has not cleared;
        // `VariantClear` leaves it as `VT_EMPTY`, and `Drop` runs once.
        //
        // The result is discarded: `VariantClear` fails only for a variant that
        // was never valid, and there is nothing a caller could do with the
        // knowledge (AGENTS.md section 15).
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

/// Ensures this process has a multi-threaded COM apartment, once.
///
/// The same call, for the same reason, as `clipped_audio`'s apartment module
/// and `clipped_capture`'s: windows-rs caches activation factories in
/// process-wide statics, so uninitialising the last apartment in the process
/// leaves those dangling. The apartment is therefore taken once and never given
/// back, and each crate that needs one takes its own reference rather than one
/// layer-1 crate depending on another (README.md layering).
///
/// Multi-threaded specifically, because the notification enumerator is pumped
/// by blocking on it. In a single-threaded apartment that call would have to be
/// serviced by a message loop this crate does not have.
fn ensure_apartment() -> Result<(), SourceError> {
    /// The result of the one and only attempt. The cookie is dropped
    /// deliberately: it is useful only for `CoDecrementMTAUsage`, which is the
    /// call this function exists in order not to make.
    static APARTMENT: OnceLock<Result<(), HRESULT>> = OnceLock::new();

    APARTMENT
        .get_or_init(|| {
            // SAFETY: `CoIncrementMTAUsage` takes no arguments and no pointers.
            // Its only obligation is that a matching `CoDecrementMTAUsage` may
            // be called with the cookie it returns, which this crate never
            // does.
            unsafe { CoIncrementMTAUsage() }
                .map(|_cookie| ())
                .map_err(|error| error.code())
        })
        .map_err(|code| {
            SourceError::new(
                "CoIncrementMTAUsage",
                windows::core::Error::from_hresult(code),
            )
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn each_query_names_its_class_its_interval_and_its_target() {
        let creation = Change::Started.query(1);
        assert_eq!(
            creation,
            "SELECT * FROM __InstanceCreationEvent WITHIN 1 \
             WHERE TargetInstance ISA 'Win32_Process'"
        );

        let deletion = Change::Exited.query(4);
        assert!(deletion.contains("__InstanceDeletionEvent"), "{deletion}");
        assert!(deletion.contains("WITHIN 4"), "{deletion}");
        assert!(deletion.contains("Win32_Process"), "{deletion}");
    }

    #[test]
    fn the_query_interval_comes_from_the_configuration() {
        let config = WatchConfig {
            notification_interval: Duration::from_secs(3),
            ..WatchConfig::default()
        };

        assert!(Change::Started
            .query(config.notification_interval_seconds())
            .contains("WITHIN 3"));
    }

    #[test]
    fn a_subscription_can_be_established_on_this_machine() {
        // Not a mock: this asks the real WMI service for a real notification
        // query and releases it. It is the only assertion available that the
        // COM plumbing above — apartment, connection, proxy blanket, query — is
        // correct, and it fails on a machine where the primary source is
        // unavailable, which is exactly what it should report.
        let subscription = subscribe(Change::Started, WatchConfig::default())
            .expect("WMI process notifications are available to an ordinary user");

        drop(subscription);
    }
}
