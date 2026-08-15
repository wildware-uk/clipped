//! Where process events actually come from on Windows.
//!
//! Everything platform-specific in the watcher is under this module, behind
//! `#[cfg(windows)]`, so that the debounce and the vocabulary above it compile
//! and run their tests anywhere (AGENTS.md section 5).
//!
//! The choice between the two sources is made here, once, at start: the WMI
//! subscription if it can be established, and the snapshot poller if it cannot.
//! `wmi.rs` explains why that is the order.

mod snapshot;
mod stop;
mod wmi;

use std::sync::mpsc::{sync_channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub(crate) use snapshot::baseline;

use self::stop::Stop;
use super::config::WatchConfig;
use super::error::{SourceError, WatchError};
use super::source::{EventSource, SourceMessage};

/// How long the watcher waits for WMI to say whether it will serve.
///
/// A healthy connection takes tens of milliseconds. An unhealthy WMI service
/// does not fail — it *hangs*, sometimes for minutes, and a recorder that
/// blocked its own start on that would look broken to a user whose only
/// symptom is a service they have never heard of. The wait is therefore bounded
/// and the fallback takes over.
const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Holds back every test in this crate that opens a real WMI notification
/// query, so that only one of them has one at a time.
///
/// WMI allows an account only a few notification queries at once — ten, on the
/// machine this was measured on. A source takes *two* of them, and this crate
/// has four tests that take a source or a whole watcher, so a run with enough
/// threads asked for more than the machine had and the ones that lost were
/// reported as `0x8004106C`: a failure of the suite's own concurrency, wearing
/// the clothes of a broken machine
/// ([issue #466](https://github.com/wildware-uk/clipped/issues/466)).
///
/// Serialising them costs a few seconds of a suite that already waits on real
/// processes, and it is only half the fix: `cargo test --workspace` runs several
/// binaries at once and this lock does not reach across them, which is why the
/// tests that hold it also accept [`wmi::is_the_machines_quota`] and say so
/// rather than failing.
#[cfg(test)]
pub(super) fn one_subscription_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // A test that panics while holding it poisons it, and the next test's
    // failure would then be the first one's, one file away from where it
    // happened.
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A running event source and the channel it delivers on.
///
/// The channel belongs to the source rather than to the watcher, which is what
/// makes falling back safe: a subscription thread that was hung and wakes up
/// later finds its receiver gone, and cannot inject events into a watcher that
/// has moved on without it.
#[derive(Debug)]
pub(crate) struct Source {
    /// What the source is delivering on.
    pub(crate) events: Receiver<SourceMessage>,
    /// Which mechanism it is.
    pub(crate) kind: EventSource,
    /// Why the preferred source was not used, when it was not.
    pub(crate) declined: Option<SourceError>,
    stop: Arc<Stop>,
    threads: Vec<JoinHandle<()>>,
}

impl Source {
    /// Starts the best available source, falling back if the preferred one
    /// cannot be established.
    ///
    /// `known` is the set of process identifiers already accounted for, which
    /// the poller needs so that its first pass reports changes rather than the
    /// entire process table.
    ///
    /// # Errors
    ///
    /// [`WatchError::NoSource`] when neither can be started, which means no
    /// process events will ever arrive and the caller should say so rather than
    /// run blind.
    pub(crate) fn start(config: WatchConfig, known: &[u32]) -> Result<Self, WatchError> {
        let subscription = match Self::start_notifications(config) {
            Ok(source) => return Ok(source),
            Err(subscription) => subscription,
        };

        // Two different things to say. "Unavailable" is right for a service
        // that is stopped, a repository that is corrupt or a policy that
        // refuses — states somebody has to fix. A quota refusal is none of
        // those: WMI is working and its queries are all in use, so the same
        // start would succeed later, and telling a user their WMI is
        // unavailable would send them to fix something that is not broken.
        if wmi::is_the_machines_quota(&subscription) {
            tracing::warn!(
                error = %subscription,
                "WMI has no notification query left for this account; falling back to snapshot \
                 polling"
            );
        } else {
            tracing::warn!(
                error = %subscription,
                "WMI process notifications are unavailable; falling back to snapshot polling"
            );
        }

        match Self::start_polling(config, known) {
            Ok(mut source) => {
                source.declined = Some(subscription);
                Ok(source)
            }
            Err(fallback) => Err(WatchError::NoSource {
                subscription,
                fallback,
            }),
        }
    }

    /// Starts the two WMI subscriptions, or reports why it could not.
    fn start_notifications(config: WatchConfig) -> Result<Self, SourceError> {
        let (events, receiver) = std::sync::mpsc::channel();
        let (ready, acknowledged) = sync_channel(2);
        let stop = Arc::new(Stop::default());

        let mut threads = Vec::with_capacity(2);
        for change in [wmi::Change::Started, wmi::Change::Exited] {
            let events: Sender<SourceMessage> = events.clone();
            let ready = ready.clone();
            let stop = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name(format!("clipped-process-{}", change.as_str()))
                .spawn(move || wmi::run(change, config, &events, &ready, &stop))
                .map_err(|error| SourceError::new("std::thread::Builder::spawn", error))?;
            threads.push(thread);
        }
        drop(events);
        drop(ready);

        for _ in 0..threads.len() {
            match acknowledged.recv_timeout(SUBSCRIPTION_TIMEOUT) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(Self::abandon(stop, threads, error)),
                Err(error) => {
                    // Timed out, or every thread died without answering. Either
                    // way the threads are left to finish on their own: one of
                    // them is inside a COM call that cannot be interrupted, and
                    // joining it here is the hang this timeout exists to avoid.
                    // The latch is raised, so it stops as soon as the call
                    // returns, and its channel is dropped with this `Source`.
                    stop.raise();
                    return Err(SourceError::new("IWbemLocator::ConnectServer", error));
                }
            }
        }

        Ok(Self {
            events: receiver,
            kind: EventSource::WmiNotification,
            declined: None,
            stop,
            threads,
        })
    }

    /// Starts the fallback poller.
    ///
    /// Called directly when a working subscription is lost: re-subscribing to a
    /// service that has just stopped answering is not a recovery strategy.
    pub(crate) fn start_polling(config: WatchConfig, known: &[u32]) -> Result<Self, SourceError> {
        let (events, receiver) = std::sync::mpsc::channel();
        let stop = Arc::new(Stop::default());

        let known = known.to_vec();
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("clipped-process-poll".to_owned())
                .spawn(move || snapshot::poll(config, &known, &events, &stop))
                .map_err(|error| SourceError::new("std::thread::Builder::spawn", error))?
        };

        Ok(Self {
            events: receiver,
            kind: EventSource::SnapshotPolling,
            declined: None,
            stop,
            threads: vec![thread],
        })
    }

    /// Stops threads that have already answered, and reports `error`.
    fn abandon(stop: Arc<Stop>, threads: Vec<JoinHandle<()>>, error: SourceError) -> SourceError {
        stop.raise();
        for thread in threads {
            let _ = thread.join();
        }
        error
    }
}

impl Drop for Source {
    /// Stops the source and waits for its threads.
    ///
    /// The wait is bounded by the notification interval, because a thread
    /// blocked inside `IEnumWbemClassObject::Next` cannot be interrupted and
    /// only notices the latch when that call returns. Waiting is still the
    /// right thing to do: the alternative is a thread writing into a channel
    /// belonging to a watcher that no longer exists.
    fn drop(&mut self) {
        self.stop.raise();
        for thread in std::mem::take(&mut self.threads) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use super::super::source::SourceEvent;
    use super::*;

    /// How long a test waits for the poller to notice something.
    const PATIENCE: Duration = Duration::from_secs(30);

    /// Waits for an event the predicate accepts.
    fn wait_for(source: &Source, mut accepts: impl FnMut(&SourceEvent) -> bool) {
        let start = Instant::now();
        while start.elapsed() < PATIENCE {
            match source.events.recv_timeout(Duration::from_millis(250)) {
                Ok(SourceMessage::Event(event)) if accepts(&event) => return,
                Ok(SourceMessage::Event(_)) | Err(_) => {}
                Ok(SourceMessage::Lost(reason)) => panic!("the poller stopped: {reason}"),
            }
        }
        panic!("the poller reported nothing matching within {PATIENCE:?}");
    }

    #[test]
    fn the_preferred_source_is_the_subscription_where_wmi_answers() {
        let _held = one_subscription_at_a_time();

        let source = Source::start(WatchConfig::default(), &[])
            .expect("a source can be started on this machine");

        // A machine with no notification query left to give falls back, and
        // that is the fallback working rather than this failing: the
        // subscription reached WMI and WMI answered with a resource limit. The
        // lock above means it was not this suite that used them up, so accepting
        // it here accepts only genuine outside contention — another test binary
        // in a `--workspace` run, or a second copy of Clipped.
        if let Some(declined) = &source.declined {
            assert!(
                wmi::is_the_machines_quota(declined),
                "WMI was not used, and not because the machine was out of queries: {declined}"
            );
            assert_eq!(
                source.kind,
                EventSource::SnapshotPolling,
                "a declined subscription has to leave the poller running"
            );
            println!("skipped: this machine had no WMI notification query to spare ({declined})");
            return;
        }

        assert_eq!(source.kind, EventSource::WmiNotification);
    }

    #[test]
    fn the_fallback_reports_a_process_starting_and_exiting() {
        // The fallback is only reached on a machine where WMI is broken, which
        // is not a state to put a contributor's machine into. It is started
        // directly instead, so that the mechanism the documentation promises is
        // actually exercised rather than only described (AGENTS.md section 54).
        let known: Vec<u32> = snapshot::process_table()
            .expect("the process table can be read")
            .iter()
            .map(|row| row.pid)
            .collect();
        let source = Source::start_polling(WatchConfig::default(), &known)
            .expect("the fallback poller can be started");
        assert_eq!(source.kind, EventSource::SnapshotPolling);

        let mut subject = Command::new("cmd.exe")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd.exe is present on every Windows machine");
        let pid = subject.id();

        wait_for(&source, |event| {
            matches!(event, SourceEvent::Started(process)
                if process.pid == pid && process.image_name.to_lowercase() == "cmd.exe")
        });

        drop(subject.stdin.take());
        subject
            .wait()
            .expect("the subject exits when its input closes");

        wait_for(
            &source,
            |event| matches!(event, SourceEvent::Exited { pid: gone } if *gone == pid),
        );
    }

    #[test]
    fn the_fallback_poller_never_looks_more_often_than_once_a_second() {
        // `WatchConfig` is public and so are its fields, so the interval that
        // reaches the poller is whatever a consumer wrote — and this loop
        // enumerates every process on the machine each time round. Twenty times
        // a second it would be exactly the high-frequency polling the whole
        // design exists to avoid (AGENTS.md section 18), so the floor has to
        // hold on this path and not only on the WQL one.
        let config = WatchConfig {
            notification_interval: Duration::from_millis(20),
            ..WatchConfig::default()
        };
        let known: Vec<u32> = snapshot::process_table()
            .expect("the process table can be read")
            .iter()
            .map(|row| row.pid)
            .collect();
        let source =
            Source::start_polling(config, &known).expect("the fallback poller can be started");
        let started = Instant::now();

        let mut subject = Command::new("cmd.exe")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cmd.exe is present on every Windows machine");
        let pid = subject.id();

        // The first pass cannot happen before the floor, so nothing about this
        // process can be reported inside it. The bound is one-sided on purpose:
        // a slow machine makes the event later, never sooner, so this cannot
        // fail for being run on busy continuous integration hardware.
        let too_soon = Duration::from_millis(700);
        while started.elapsed() < too_soon {
            if let Ok(SourceMessage::Event(SourceEvent::Started(process))) =
                source.events.recv_timeout(Duration::from_millis(50))
            {
                assert_ne!(
                    process.pid,
                    pid,
                    "the poller reported a process {:?} after it started: \
                     it is polling at its configured 20ms, not the one-second floor",
                    started.elapsed()
                );
            }
        }

        // And it does arrive, so what is being measured is a poller that waited
        // rather than one that never ran at all.
        wait_for(
            &source,
            |event| matches!(event, SourceEvent::Started(process) if process.pid == pid),
        );

        drop(subject.stdin.take());
        subject
            .wait()
            .expect("the subject exits when its input closes");
    }
}
