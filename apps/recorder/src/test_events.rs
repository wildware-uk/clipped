//! A real subscriber on a real pipe, for the tests that have to prove an event
//! was **published**.
//!
//! Every other test in this crate builds its recorder over
//! `EventPublisher::new()` with nobody listening, which is right for a test
//! about what the recorder *decides*: the publisher is not the subject and a
//! subscriber would be scenery. It is exactly wrong for a test about whether an
//! event is sent at all.
//!
//! [Issue #241](https://github.com/wildware-uk/clipped/issues/241) has produced
//! three defects of one shape — a state nothing reported, a field nothing filled
//! in, and an event nothing sent — and each of them would have passed any test
//! that called the producer itself. So the tests for the last of them subscribe
//! the way the desktop application subscribes: a real
//! [`Server`](clipped_ipc::Server) over a real named pipe, a real
//! [`EventClient`](clipped_ipc::EventClient), and an assertion on the frame that
//! comes out of it. A pipe needs no desktop, no GPU and no encoder, which is why
//! this can run in CI rather than being one more `#[ignore]`d test
//! (`apps/recorder/tests/ipc_protocol.rs` says the same of itself).
//!
//! Reading is done on a thread of its own and delivered through a channel, so
//! that a build which publishes **nothing** fails on a timeout rather than
//! hanging the suite on a pipe that will never produce another byte.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipped_ipc::transport::{Listener, ListenerStopper};
use clipped_ipc::{
    CommandHandler, Endpoint, Event, EventClient, EventPublisher, EventStream, PeerIdentity, Server,
};

/// How long a test waits before deciding nothing is going to publish.
///
/// Generous, because it is only ever waited out by a failure: everything here
/// happens in one process over a pipe that is already connected, so the arrival
/// is immediate when it happens at all (AGENTS.md section 25).
const PATIENCE: Duration = Duration::from_secs(10);

/// A subscription to a recorder's `status` stream, over a pipe of its own.
///
/// Dropping it stops the server and ends the subscription, so a test that
/// panics does not leave a listener bound to a name the next one might use.
pub(crate) struct Subscribed {
    events: Receiver<Event>,
    publisher: EventPublisher,
    stopper: ListenerStopper,
    serving: Option<JoinHandle<()>>,
    reading: Option<JoinHandle<()>>,
}

impl Subscribed {
    /// Serves `handler` on a pipe named after `label` and subscribes to its
    /// `status` stream.
    ///
    /// `publisher` must be the one the handler publishes through — the same
    /// value, not a second one — because that is the whole point: what reaches
    /// this subscriber is what the recorder really sent.
    pub(crate) fn to<H: CommandHandler + 'static>(
        publisher: &EventPublisher,
        handler: &Arc<H>,
        label: &str,
    ) -> Self {
        let endpoint =
            Endpoint::named(&unique_name(label)).expect("the generated endpoint name is valid");
        let mut listener = Listener::bind(&endpoint).expect("a pipe of this test's own is free");
        let stopper = listener.stopper();

        let server = Server::new(
            Arc::clone(handler),
            publisher.clone(),
            PeerIdentity {
                name: "clipped-recorder".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        );
        let serving = thread::Builder::new()
            .name("test-ipc-server".to_owned())
            .spawn(move || {
                let _ = server.serve(&mut listener);
            })
            .expect("a thread can be started to serve on");

        let mut client = EventClient::subscribe(
            &endpoint,
            "clipped-tests",
            "0.0.0",
            vec![EventStream::Status],
            PATIENCE,
        )
        .expect("the status stream is delivered");

        let (sender, events) = mpsc::channel();
        let reading = thread::Builder::new()
            .name("test-ipc-events".to_owned())
            .spawn(move || {
                while let Ok(event) = client.next_event() {
                    if sender.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("a thread can be started to read events on");

        Self {
            events,
            publisher: publisher.clone(),
            stopper,
            serving: Some(serving),
            reading: Some(reading),
        }
    }

    /// The first event this subscriber is sent that `wanted` accepts.
    ///
    /// Events before it are skipped rather than failed on: a `status`
    /// subscription opens with the state the recorder is in, and a test about
    /// one event should not have to enumerate every status change that happened
    /// on the way to it.
    ///
    /// Panics — naming `described` — when nothing matching arrives, which is
    /// what a producer that was never written looks like from out here.
    pub(crate) fn wait_for(&self, described: &str, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(event) if wanted(&event) => return event,
                Ok(event) => seen.push(event),
                Err(RecvTimeoutError::Timeout) => {
                    panic!("nothing published {described} within {PATIENCE:?}; what arrived was {seen:#?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("the event connection ended before {described} arrived; what arrived was {seen:#?}")
                }
            }
        }
    }
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        // The subscription first, so the reading thread's `next_event` returns
        // rather than waiting on a pipe nobody will write to again, and then
        // the listener, which is what makes `serve` return.
        self.publisher.close();
        self.stopper.stop();
        if let Some(reading) = self.reading.take() {
            let _ = reading.join();
        }
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// An endpoint name no other test, and no recorder anybody is using, will have.
///
/// The pipe namespace is machine-wide, and these tests run beside a maintainer's
/// own recorder (AGENTS.md section 25).
fn unique_name(label: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "clipped-recorder-unit.{label}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}
