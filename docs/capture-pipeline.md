# Capture pipeline

**Status: the interface exists, and both Windows backends do.**
`crates/capture` defines the capture backend trait, the frame and timestamp
vocabulary, the policy that picks a backend and reports which one it picked,
and — since [issue #12](https://github.com/wildware-uk/clipped/issues/12) and
[issue #13](https://github.com/wildware-uk/clipped/issues/13) — the Windows
Graphics Capture and Desktop Duplication backends that implement all of it. A
Windows build can produce GPU frames from a window or a display today, by either
method, and `clipped-encoder` can both say what this machine could encode with
([encoder-capabilities.md](encoder-capabilities.md)) and encode a frame it is
handed, on NVENC or on the CPU ([encoder-pipeline.md](encoder-pipeline.md)).

What joins them up is `clipped-session`, which since
[issue #126](https://github.com/wildware-uk/clipped/issues/126) takes a frame
from a backend, gives it to an encoder and writes the packets into a Matroska
file: `recorder record` records. The loop is
`crates/session/src/recording.rs` and it obeys the ownership and threading rules
this document sets out.

Since [issue #97](https://github.com/wildware-uk/clipped/issues/97) the crate
also knows what to do when the backend under a recording stops working:
[Automatic capture fallback](#automatic-capture-fallback) restarts or replaces
it, notices a capture that has silently gone black, and reports which method is
actually in use. That part is built in `crates/capture` and is not yet called by
a session — the section says exactly where the boundary is.

So this document describes an interface, the rules a backend has to obey, and
the two backends that obey them. Where it describes behaviour that does not
exist yet it says so, because a document that quietly describes intentions as
facts is worse than a short one (AGENTS.md section 7). It answers the questions
AGENTS.md section 47 asks of a subsystem, and the sections still marked as
unwritten are listed at the end.

## What it does

Turns a chosen window or display into a stream of GPU frames, each carrying the
timestamp its source produced, and tells the rest of the system which capture
method is being used so that the product can show:

```text
Capture method: Automatic
Current method: Windows Graphics Capture
```

## Why it exists in this shape

Three things drive the design.

**A user must not have to understand capture APIs.** SPEC.md section 8 asks for
one setting — Automatic — and a line saying what Automatic chose. That makes
"which backend?" a decision the recorder has to make well and be able to explain
afterwards, which is why selection is a pure, logged, testable function rather
than a chain of `if` statements inside a constructor.

**A frame is a borrowed GPU texture, and borrowed GPU textures are where
recorders leak, tear and corrupt.** The interface therefore spends its
complexity budget on ownership, and encodes the rule in the type system instead
of in a comment.

**Timestamps decide whether the recording is watchable.** A frame's time has to
come from the source, so the interface has no way to express anything else.

## The interface

`crates/capture` is split into a part that describes a backend and a part that
is one.

| Type | What it is |
| --- | --- |
| `CaptureMethod` | The technique: `GameCapture`, `WindowsGraphicsCapture`, `DesktopDuplication`. Names a *technique*, not an implementation. |
| `BackendDeclaration` | What a backend says about itself: its method, its `BackendCapabilities`, and whether it is `Availability::Available` for a given target. Costs nothing to ask and touches no hardware. |
| `CaptureBackendFactory` | A `BackendDeclaration` that can also `create` an uninitialised backend. |
| `CaptureBackend` | A live capture: `initialise`, `acquire`, `resize`, `shut_down`. |
| `CapturedFrame` | One frame, borrowed from the backend, carrying a `FrameTexture`, a `FrameFormat` and a `CaptureTimestamp`. |
| `select` | The policy. Declarations plus target properties in, a `Selection` out. |

### Lifecycle

```text
CaptureBackendFactory::create
         ↓
    initialise(target, config) -> FrameFormat
         ↓
    acquire(timeout) ─┬─> Acquisition::Frame(frame)   … use it, drop it, loop
                      ├─> Acquisition::Timeout        … nothing changed, loop
                      └─> Acquisition::SizeChanged(s) … resize(s), then loop
         ↓
    shut_down()
```

`initialise` returns the frame format rather than leaving the caller to discover
it from the first frame, so that the encoder can be configured before capture
starts. A session that waits for a frame to size its encoder loses the first
fraction of a second of the recording — the fraction the user pressed the key
for.

`Acquisition::Timeout` is ordinary, not exceptional. A capture source produces a
frame when its content changes, so an idle game, a paused video or a menu can go
seconds without one. Whether to synthesise a repeat frame to keep an encoder's
rate steady is the encoder's decision; a backend never invents a frame.

`Acquisition::SizeChanged` is how a resize, a resolution change or an alt-tab
into a different mode reaches the caller. The backend discards the frame that
revealed the change and goes idle: it keeps reporting `SizeChanged` until
`resize` is called, because carrying on would feed frames of a new shape to an
encoder configured for the old one.

## Ownership: who owns GPU textures, and for how long

This is the part contributors get wrong, so it is stated as rules.

1. **The backend owns every native resource for its whole life.** The graphics
   device, the frame pool or duplication, and every texture belong to the
   backend from `initialise` until `shut_down` or `Drop`. Nothing else ever owns
   a frame's memory.
2. **A caller borrows exactly one frame at a time.** `acquire` takes `&mut self`
   and returns a `CapturedFrame<'_>` borrowed from it. Holding two, or holding
   one across the next acquisition, does not compile. This is not merely tidy:
   Desktop Duplication permits one outstanding frame per duplication and
   `AcquireNextFrame` fails until `ReleaseFrame` has been called, and Windows
   Graphics Capture recycles frames back into its pool.
3. **The borrow ends before the next acquisition, and that is when the backend
   releases.** An implementation releases the previous frame back to the
   platform API at the start of `acquire`, and again in `shut_down`. Because the
   borrow checker guarantees no frame is outstanding at that point, no
   reference counting and no callback is needed.
4. **The raw handle goes nowhere.** `FrameTexture::as_raw` is valid only while
   the frame is alive. Submit it to the encoder while you hold the frame, or
   copy the pixels into a resource you own. Putting the pointer into a queue for
   another thread to encode later is the most attractive mistake in this
   pipeline and it produces corrupted frames rather than a crash, days away from
   the code that caused it. There is deliberately no API for taking ownership of
   a frame; if a future design needs one, it is a new method with an explicit
   copy, not a relaxation of this rule.
5. **`Drop` must release everything `shut_down` does.** A panic on the capture
   thread must not leak a duplication — on Windows that would stop *other*
   applications from capturing the display until the process exits. The recorder
   is expected to run for days (AGENTS.md sections 58 and 59).
6. **`shut_down` cannot fail.** There is nothing a caller could do about a
   failure to release a resource, and a `Result` would collect `let _ =` at
   every call site. A backend that hits an error while tearing down logs it and
   carries on releasing the rest.

## Threading

Nothing in the capture interface is shared mutable state, and the split is
deliberate.

| Thing | Bounds | Lives on |
| --- | --- | --- |
| `BackendDeclaration`, `CaptureBackendFactory` | `Send + Sync` | Anywhere. A registry shared by whichever thread starts a session. |
| `CaptureBackend` | `Send`, not `Sync` | Built anywhere, moved to the capture thread, used only there. |
| `CapturedFrame`, `FrameTexture` | neither | The capture thread, for less than one acquisition. |

One backend belongs to one capture thread, which is where every method on
`CaptureBackend` is called. Frames cannot leave it — they hold a raw texture
handle, which makes them thread-bound automatically, and that is the intended
design rather than an accident of representation: the encoder is fed from the
capture thread, GPU to GPU, and only *encoded packets* cross to the muxer
thread. Queueing raw frames for another thread would mean a queue of VRAM whose
depth nobody bounded, which is exactly how a recorder ends up using a gigabyte
of video memory during a game.

What the capture thread must not do follows from AGENTS.md section 20: no
waiting on the UI, the database, thumbnails, the network or a plugin. It
acquires a frame, submits it, drops it and acquires the next one. The `timeout`
argument to `acquire` exists so the loop stays responsive to a stop request; it
is not a frame-rate control.

Selection runs before any of this and can happen on any thread, because reading
a declaration touches nothing.

The *capture thread itself* belongs to `clipped-session`, and is the thread that
calls `clipped_session::record`: `crates/session/src/recording.rs` acquires,
submits to the encoder, drains the packets into a bounded queue and polls the
stop signal, all on that one thread. Nothing but bytes crosses to the thread
that owns the file, which is what keeps the capture thread out of a write.
`crates/capture/examples/wgc_probe.rs` runs the same loop on its main thread
without an encoder, which is the smallest version of it.

## Timestamps

`CaptureTimestamp` has no `now()`, and `SourceClock` has no wall-clock variant.
A backend can only build a timestamp by naming the source clock and passing a
value the frame arrived with — `Direct3D11CaptureFrame::SystemRelativeTime` for
Windows Graphics Capture, `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` for Desktop
Duplication, both performance-counter readings.

The reason is that the moment a frame reaches this process is the moment a
compositor, a driver, a thread scheduler and an encoder queue have all finished
with it. That delay varies frame to frame and grows under load, so timestamps
taken at receipt record the recorder's jitter rather than the game's frame
pacing. The video then drifts against audio captured with its own device-clock
positions, and the drift is worst exactly when the machine is busiest, which is
during a game.

Two timestamps can only be subtracted when they name the same clock;
`duration_since` returns `None` otherwise, and also when the source reports time
going backwards, because that is a fault to report rather than a negative
duration to average away.

`CaptureClock` is what turns those readings into a recording: it names the one
clock the whole recording is timed against and the moment it started, and
converts every source's timestamps — video frames directly, audio positions
through `media_time_on` — into a `MediaTime`, which is signed nanoseconds from
the start of the file. [av-sync.md](av-sync.md) is the model in full: which clock
is authoritative and why, where a conversion is allowed to happen, what is done
about a dropped frame, an audio gap or a clock that steps, and how far the audio
device's own clock was measured to drift against the reference over a long run.

## Choosing a backend

`select(candidates, target, setting)` is a pure function. With
`CaptureMethodSetting::Automatic` it sorts the candidates it was handed by
preference — Game Capture, then Windows Graphics Capture, then Desktop
Duplication (SPEC.md section 8) — and takes the first that passes two tests:

1. its declared `BackendCapabilities` can address this kind of target at all,
   and
2. it answers `Availability::Available` when asked about this particular target.

A method with no registered candidate is skipped: it never appears, because
there is nothing to ask. Sorting the candidates rather than walking a list of
methods is deliberate. `CaptureMethod::preference_rank` is an exhaustive match,
so a new method has to be given a place in the order before the crate compiles,
and there is no second list of methods for it to be missing from — a registered
candidate that automatic selection cannot see is not a thing this code can
express. `CaptureMethod::PREFERENCE_ORDER` remains as the published order, for
documentation and for a settings screen, and a method's rank has to name that
method's own slot in it — checked while the crate compiles — so the published
order is complete and in the order selection actually uses.

With `CaptureMethodSetting::Forced(method)` the named candidate faces the same
two tests and there is no fall back: a setting the user typed is obeyed or
reported, never quietly swapped for something else. Automatic is the setting
for people who want fallback.

The result is a `Selection` carrying the setting, the chosen method, and the
list of candidates that were examined with the reason each was passed over. The
first two are the two lines the product shows; the third is for the session log
and the diagnostics screen, and it is the answer to "why is it using *that*
one?", which is otherwise unanswerable from a bug report.

Because selection depends on nothing but declarations and observed target
properties, it is fully unit tested today with fake declarations, on a machine
with no display — preference order, a preferred backend declaring itself
unavailable, a backend that cannot address the target kind, a forced method that
is missing or unusable, and nothing being available at all. Those fakes are
legitimate because the thing under test is the policy, not capture; the tests
say so at the top of the module. The *registry* those declarations come from in
a real recording is `registered_backends`, and it is checked separately: that no
two backends claim one method, that every one of them is findable by its method,
and that a Windows build has Windows Graphics Capture and does not have a
candidate for a method nothing implements.

### Game Capture

`CaptureMethod::GameCapture` is in the enumeration and at the top of the
preference order because SPEC.md section 8 puts it there. **Nothing in this
repository implements it, it is not scheduled, and it may never be built here.**
The usual technique is DLL injection into the game process, which AGENTS.md
section 34 rules out: a recorder that injects into an online game risks the
user's account, and no amount of capture quality is worth that.

It is represented honestly — a method with a name and no candidate. Selection
therefore never reaches it, and the diagnostics report does not mention it,
because "this build has no Game Capture backend" is a fact about the build
rather than about the target the user is trying to record.

It is also the one method with no `clipped_logging::CaptureBackend` counterpart,
which is a decision rather than an omission. That enumeration is the closed
vocabulary the `capture_backend` log field commits to (docs/logging.md); a value
no code can emit would be the log format promising a backend that does not
exist. `CaptureMethod::GameCapture.log_value()` is still `game_capture`, because
selection reports the method it was forced to and refused, and the word has to
be the one a future backend would use. A build that ever implements Game Capture
adds the logging variant in the same change.

## Windows Graphics Capture

The preferred implemented backend. It lives in `crates/capture/src/windows/`,
behind `#[cfg(windows)]`, which is where all the Windows code in the crate is;
`registered_backends` in `crates/capture/src/registry.rs` is the single place
that says a build has it. On any other platform that list is empty and `select`
reports "this build registered no capture backends at all" rather than
pretending.

### How it works

`Windows.Graphics.Capture` asks the desktop compositor for a *capture item*: one
window, or one display. The compositor already holds that content on the GPU, so
frames arrive as Direct3D 11 textures on the device the backend created, and the
backend never reads a pixel — a `Direct3D11CaptureFrame`'s surface is unwrapped
to an `ID3D11Texture2D` through `IDirect3DDxgiInterfaceAccess` and that pointer
is what `FrameTexture` carries. There is no `Map`, no staging texture and no
system-memory copy in the backend at all.

Because the compositor is asked for the *item's* content rather than for what is
on screen where the item is, a window captured this way is unaffected by
anything drawn over it. That is what
`BackendCapabilities::is_occlusion_independent` declares, and it is why SPEC.md
section 8 prefers this method to Desktop Duplication.

### Threading and the frame pool

The frame pool is created with `CreateFreeThreaded`, so `FrameArrived` is raised
on a thread-pool thread and the capture thread never needs a message loop — a
capture thread that had to pump messages to receive frames would stall whenever
something else posted to it (AGENTS.md section 20). The handler does no
allocation, no logging and no COM call: it takes a lock held for one increment
and wakes the capture thread, which then calls `TryGetNextFrame`. The count it
keeps says how many frames were collected, never how many the source produced —
see "Dropped frames" below, which is the one place that distinction matters and
the one place it is easy to get wrong.

The pool holds **three** buffers. The caller holds one frame while it submits the
texture to the encoder, so a pool of one would leave the compositor nothing to
compose into; two leaves one spare and loses a frame whenever an encode overruns
a single frame interval; three leaves two, which absorbs ordinary jitter. More
would buy latency and video memory rather than frames.

WinRT activation needs a COM apartment, and `windows/apartment.rs` makes sure
the *process* has a multi-threaded one — `CoIncrementMTAUsage`, once, never
released. A thread that has not initialised COM for itself is then treated as
belonging to it, which is what lets a capture thread activate WinRT types
without a message loop or a dispatcher queue.

The absence of a matching release is the one place this subsystem does not have
deterministic cleanup, and it is deliberate. The obvious design —
`RoInitialize` on the capture thread and `RoUninitialize` from a guard's `Drop`
— was written first and crashes: windows-rs caches activation factories in
process-wide statics and keeps the raw pointers for the life of the program, so
when the last thread in the apartment uninitialises, the apartment goes and
every cached pointer is left dangling. It surfaced as an intermittent
`STATUS_ACCESS_VIOLATION` in CI, in a run where one test thread's guard dropped
while another was activating a WinRT type. A recorder that stopped a recording
while its audio or encoder threads were mid-activation would be the same race
with a user attached to it. So the apartment is treated as process
infrastructure, like a loaded DLL: one reference for the life of the process, no
matter how many recordings start and stop.

### Timestamps

`Direct3D11CaptureFrame::SystemRelativeTime` is a WinRT `TimeSpan`, which always
counts 100-nanosecond units, so the conversion is
`CaptureTimestamp::from_performance_counter(ticks, 10_000_000)` and the clock is
declared as `SourceClock::PerformanceCounter`. Nothing in the backend reads a
clock.

### Dropped frames

Windows Graphics Capture has no "frames missed" field, so
`CapturedFrame::frames_missed` is *derived* — and it cannot be derived from
`FrameArrived`, which is the obvious idea and a wrong one.

`FrameArrived` does not fire once per composed frame. It fires once per frame
the compositor puts into a **free** pool buffer, and when the pool has no free
buffer the compositor does not compose and raises no event for what it skipped.
So arrivals track deliveries however far behind the caller falls, and any
arithmetic of the form "arrivals minus deliveries minus the pool depth" is
identically zero. Measured on Windows 11 build 26200, against the probe's 60 fps
test window with the caller stalling 200 ms per frame: **52 arrivals, 50
deliveries, over ten seconds in which the source presented about 600 frames.**
The five hundred and fifty frames nobody saw produced no event of any kind.

What does survive is the source's own clock. `Running` differences consecutive
delivered frames' `SystemRelativeTime` and divides the gap by the shortest
interval the capture has ever seen between two frames, which is its estimate of
how often the source produces one: a gap of `n` whole intervals means `n - 1`
source frames went by without one reaching the caller. Because the reference is
the shortest interval *seen*, it is never shorter than the source's real
interval, so the division cannot over-count — the figure is a lower bound, which
is the right direction for a number a user reads as "your machine could not keep
up". The same configuration, run again with this derivation in place, reports
**515 frames missed against 50 delivered**.

A gap only counts when the frame was already waiting in the pool the first time
the acquisition looked. That is what separates a caller too slow to collect what
the source produced from a source that produced nothing — a paused game, a
static menu, an idle desktop — which the caller sat and waited through. The
chain is broken for the same reason on a timeout, a discarded frame and a
resize. Measured against the scripted lifecycle run, whose window is minimised
for four seconds: the 4,044 ms interval that spans the minimise is reported as
**0 frames missed**, because nothing was dropped — the window was not composing.

Two things it does not do, stated rather than glossed:

- It cannot separate a source that slows down at the same moment the caller
  does. Nothing in the API distinguishes them.
- A capture whose caller never once kept up has no short interval to compare
  against, and reports nothing at all.

### Resize, minimise, occlusion and closure

| What happens | What the backend does |
| --- | --- |
| The window is resized | A frame arrives whose `ContentSize` differs from the pool's. It is discarded, `Acquisition::SizeChanged` is reported, and the backend goes idle until `resize` calls `Direct3D11CaptureFramePool::Recreate` — which keeps the session, the item and both event registrations, so frames composed during the change are not all lost. |
| The window is minimised | It stops composing, so acquisitions report `Acquisition::Timeout` — and go on reporting it until the window comes back, rather than deciding the window has gone. A frame that arrives with a zero dimension — which is what a minimised client area reports — is discarded rather than turned into a `FrameSize`, because `FrameSize` refuses to represent it and an encoder configured for it would fail on its first frame. The silence is not counted as dropped frames. `a_minimised_window_is_waited_out_rather_than_reported_as_a_size_or_a_loss` minimises a real window mid-capture and asserts all three. |
| Something is drawn over the window | Nothing. The compositor is asked for the item's own content. |
| The window closes | Reported as `CaptureError::TargetLost`. |

That last row is worth its own paragraph, because the obvious implementation of
it does not work. `GraphicsCaptureItem::Closed` is subscribed to, but it is
delivered through the creating thread's dispatcher queue, and a capture thread
deliberately has neither a dispatcher queue nor a message loop. Measured on
Windows 11 build 26200, destroying the captured window produces no `Closed`
callback at all: capture simply goes quiet, and a caller sits in
`Acquisition::Timeout` for ever waiting for a window that no longer exists,
never finalising its recording. So a *window* target is also checked with
`IsWindow` on the one path where the answer matters — an acquisition about to
report a timeout, at most a handful of calls a second, never one per frame. A
*display* target has no equivalent check; disconnection is left to the `Closed`
event and to [issue #98](https://github.com/wildware-uk/clipped/issues/98),
which owns display changes.

`IsWindow` is not a perfect answer and is not presented as one. Microsoft's
documentation advises against calling it on a window the calling thread did not
create, because handles are recycled: if the captured window is destroyed and
its `HWND` reissued to another window during the recording, `IsWindow` reports
true and closure is never noticed. That is accepted rather than solved. With
`Closed` not arriving at all, the choice is between a check that is wrong in a
rare race and no check at all, and the race reinstates the old behaviour rather
than inventing a new failure.

### The capture border, and which Windows build removes it

Windows draws a yellow border around a captured window unless the application
opts out through `GraphicsCaptureSession.IsBorderRequired`, which arrived in
**Windows 11 build 22000**. `docs/prerequisites.md` supports Windows 10 21H2
(build 19044) and later, so a supported machine can legitimately be without it.
The backend probes for the property with `ApiInformation::IsPropertyPresent`
rather than comparing build numbers, sets it where it exists, and where it does
not, logs that the recording will have a border and carries on. Refusing to
record over a cosmetic difference would be the wrong trade (AGENTS.md section
16). `IsCursorCaptureEnabled`, which honours `CaptureConfig::capture_cursor`,
arrived in Windows 10 build 19041 and is therefore present on every supported
build; it is probed the same way regardless.

### Ownership

`Running` owns the Direct3D device, the capture item, the frame pool, the
session, both event registrations and the frame currently lent to the caller.
The backend holds an `Option<Running>`, `shut_down` is `self.running = None`,
and `Drop` calls `shut_down` — so an unwind on the capture thread releases
exactly what a clean stop would. `Running::drop` releases the lent frame first
(an outstanding frame is a buffer the compositor cannot reclaim), then closes
the session, unsubscribes and closes the pool, and unsubscribes the item;
everything still holding a reference is released by its own `Drop` immediately
afterwards.

The COM apartment is the one thing `Running` does *not* own, for the reason
given under "Threading and the frame pool" above.

### How to run it

`crates/capture/examples/wgc_probe.rs` renders its own Direct3D 11 test window at
a fixed rate, captures it through this backend, and reports frame pacing, dropped
frames and resource usage. It is the answer to "how do I see this working?" and
to the acceptance criteria on issue #12, and it needs no game:

```text
cargo run --release -p clipped-capture --example wgc_probe -- --mode windowed --seconds 60
cargo run --release -p clipped-capture --example wgc_probe -- --mode borderless --seconds 60
cargo run --release -p clipped-capture --example wgc_probe -- --mode fullscreen --seconds 60
cargo run --release -p clipped-capture --example wgc_probe -- --mode monitor --seconds 60
cargo run --release -p clipped-capture --example wgc_probe -- --mode lifecycle --seconds 25
cargo run --release -p clipped-capture --example wgc_probe -- --mode windowed --seconds 10 --stall-ms 200
```

`--mode lifecycle` resizes, minimises, restores and finally closes the test
window on a fixed schedule, so the four behaviours in the table above can be
observed rather than assumed. `--stall-ms` holds each frame before releasing it,
which is what a machine that cannot encode fast enough does to the frame pool,
and is how the dropped-frame count is shown to move rather than assumed to.

**Do not minimise the probe window during a run.** Windows Graphics Capture
stops delivering frames for a minimised window — occlusion is fine, minimise is
not — so any pacing or dropped-frame figure covering a minimised stretch is
measuring Alt-Tab rather than the backend. The probe watches its own window and
refuses to let that pass quietly: a run in which the window was minimised or
hidden prints a `CONTAMINATED RUN - DISCARD IT` banner and exits non-zero. A
contaminated run is repeated, not reported. `--mode lifecycle` minimises on
purpose and is exempt.

It is an example rather than a test because it
needs a desktop, a GPU and minutes of wall-clock time, none of which the
pull-request CI job has, and because its output is a measurement somebody reads.
The automated version belongs in `tests/capture/` once the shared test
applications exist
([issue #23](https://github.com/wildware-uk/clipped/issues/23)).

### Exclusive fullscreen

Two questions live here, and they have different answers. Keeping them apart is
the whole of this section, because an earlier revision of this page ran them
together and got the second one wrong.

1. **Will Windows let a test-started application take the display?** That is
   Windows' focus policy. It is not about capture and this repository does not
   control it.
2. **Once an application does hold a display, does this backend capture it?**
   That is what
   [issue #12](https://github.com/wildware-uk/clipped/issues/12) asks.

#### (2) Yes — at about nine frames in ten

`tests/capture/wgc_fullscreen_dx11.rs` starts `test-apps/fullscreen-dx11`, reads
from its `ready` line whether Windows granted the display, captures the window
and decodes the frame counter out of every frame that arrives. It asserts the
result and, when the display was refused, says so and fails rather than passing
(see `CLIPPED_REQUIRE_CAPTURE` in `tests/capture/README.md`).

Windows 11 Pro build 26200, RTX 4090, `\\.\DISPLAY1` (non-primary), 2560x1440,
subject presenting at 60 fps, five seconds of capture — so 300 frames were there
to be had. Same binary, same readback path, same display; the runs are minutes
apart and the grants and refusals are interleaved among them:

| Windows granted the display | Delivered of 300 | Decoded | Timeouts | Undecodable |
| --- | ---: | ---: | ---: | ---: |
| yes | 279 | 279 | 0 | 0 |
| yes | 274 | 274 | 0 | 0 |
| yes | 274 | 274 | 0 | 0 |
| yes | 272 | 272 | 0 | 0 |
| yes | 272 | 272 | 0 | 0 |
| yes | 270 | 270 | 0 | 0 |
| yes | 268 | 268 | 0 | 0 |
| yes | 266 | 266 | 0 | 0 |
| yes | 229 | 229 | 4 | 0 |
| no (borderless) | 301 | 301 | 0 | 0 |
| no (borderless) | 300 | 300 | 0 | 0 |
| no (borderless) | 300 | 300 | 0 | 0 |
| no (borderless) | 298 | 298 | 0 | 0 |
| no (borderless) | 290 | 290 | 0 | 0 |
| no (borderless) | 273 | 273 | 0 | 0 |

That is every run taken, not a selection. Every frame that arrived decoded as
the pattern in every one of them, the subject survived every run, and the
display was the shape it started as afterwards. So the answer to (2) is yes, and
the test asserts it rather than reporting it. (The 301 is not a typo: a capture
started a frame before the source's five seconds began, so one more counter
arrived than the arithmetic allows for.)

The low rows — 229 granted, 290 and 273 refused — are the runs taken on sessions
that had been idle for eight to ten minutes, and in the 229 the *subject*
presented 278 frames rather than its usual 320-odd. A machine nobody has touched
winds its whole display pipeline down, so both halves of the table lose ten to
twenty points there. That is why the test now prints the session's idle time
beside its frame count, why neither number should be quoted without it, and why
both of the test's frame floors sit below the worst measurement for their case
rather than near the typical one.

**The gap between the two halves of that table is real and is not the readback.**
Compare like with like — the session state moves both cases, so take the runs on
an active session: granted delivers 266 to 279, refused delivers 298 to 301. An
earlier revision of this page said the shortfall was the test copying every
14 MiB frame into system memory and decoding it. It is not: the refused runs do
exactly the same copying and decoding, at the same size and rate, on the same
machine minutes apart, and lose one frame in a hundred where the granted runs
lose ten. Nor is it the caller falling
behind — the backend's own derived frames-missed figure
([Dropped frames](#dropped-frames)) is **0** in all five runs taken since the
test started reporting it, the granted 268 and 266 as well as the refused 301,
290 and 273. That says the compositor never composed the missing frames rather
than that this test was too slow to collect them. Whatever costs them is
specific to capturing a window that owns its display. That is a finding about
the case issue #12 asks about, so it is
[issue #192](https://github.com/wildware-uk/clipped/issues/192) rather than a
sentence explaining it away — and it is why the test's floor for a granted run
is 70% of the source's frames rather than the 80% the refused case holds.

#### (1) What decides whether the display is granted

`SetFullscreenState` needs the foreground, and Windows will not give the
foreground to a process the user has not interacted with. On this machine
exactly one thing changes the answer:

> **A process that has synthesised an input event must still be running.**

Not recent input — a *live process* that produced some. Everything else that
looked like the cause was varied and did not move the answer. Measured, same
binary, one afternoon:

| Condition at the moment of the run | Displays | `exclusive` | Delivered of 300 |
| --- | --- | --- | ---: |
| Nothing had synthesised input; session idle 45 minutes | off | **no** | — |
| Injecting process alive; its event 30 s earlier | on | yes | 272 |
| Injecting process alive; its event 302 s earlier | on | yes | 274 |
| Injecting process alive; its event 600 s earlier | on | yes | 229 |
| Injecting process alive; its event 952 s earlier | on | yes | 270 |
| Injecting process alive; its event 1250 s earlier | on | yes | 274 |
| Injecting process had exited 36 s before the run | on | **no** | 298 |
| Nothing had synthesised input | on | **no** | 300 |
| Injecting process alive; event moments earlier | on | yes | 279 |
| Injecting process alive; no fresh event for that run | on | yes | 272 |
| Injecting process exited 5 s before the run | on | **no** | 300 |
| Nothing had synthesised input; session idle 584 s | on | **no** | 290 |
| Nothing had synthesised input; session idle 487 s | on | **no** | 273 |
| Injecting process alive; its event 6 s earlier | on | yes | 266 |
| Same live injector, 13 s in, no fresh event | on | yes | 268 |

The split is exact: every row with a live injecting process was granted and
every row without one was refused, and nothing else in the table predicts the
answer. An input event five seconds old is refused if the process that made it
has gone; one twenty minutes old is granted if that process is still there. The
five rows counting up to 1250 s are a single
`SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED)` sweep that held
the displays on well past the 15-minute idle timeout, so a session idle for
twenty minutes refused nothing. The last two rows are the pair quoted in
[pull request #182](https://github.com/wildware-uk/clipped/pull/182) as this
page's evidence, taken with the procedure in `tests/capture/README.md`.

Three things this rules out, each of which has been believed on this project at
some point:

- **The launch path.** A process created through `Win32_Process::Create`, whose
  parent is `WmiPrvSE` and which is no relation of the injecting process, was
  granted the display while that process was alive. `cargo test` is granted or
  refused on the same rule as everything else.
- **Powered-off displays.** Five of the `no` rows were taken with both displays
  awake and the compositor at full rate — 273 to 300 of 300 frames, no timeouts
  in any of them. A powered-off display is a real and separate problem (below),
  but it is not what refuses the transition.
- **Idle time.** Grants were measured at every idle time from 30 seconds to 21
  minutes.

The mechanism behind the rule is not established here, only its behaviour. In
every `no` row the subject reported that `SetForegroundWindow` had been refused
as well, so whatever Windows is tracking, it is tracking it in the
foreground-lock machinery rather than anywhere in DXGI. Two things that do
*not* work, so that nobody spends the afternoon
again: `AttachThreadInput` plus `AllowSetForegroundWindow(ASFW_ANY)` returns
`ERROR_ACCESS_DENIED (5)`, and
`SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT)` returns
`ERROR_INVALID_PARAMETER (87)` from a background process.

The consequence for anybody reading a number off this page: **the exclusive rows
of the first table cannot be reproduced by running the test on its own.**
`tests/capture/README.md` has the procedure that produces them, and the test
fails rather than passing when the run did not get there.

### A powered-off display is a separate trap

Not the cause of the refusals above, but still worth knowing before trusting any
number on this page. When Windows has turned the displays off on the idle
timeout, the desktop compositor drops to about 4 Hz, so Windows Graphics Capture
delivers about one frame in fifteen for *any* target — it composes what the
desktop composes, and a desktop nobody is looking at is barely composed at all.

Measured on the development machine with both displays powered off after the
15-minute idle timeout — from an earlier session, and **not reproduced since**;
see the caveat below the second block — `wgc_probe --mode windowed --seconds 10`
at a target of 60 fps:

```text
frames delivered     : 40
acquisition timeouts : 80
measured rate        : 3.97 fps (from frame timestamps)
interval median      : 251.567 ms
late frames          : 39 of 39 intervals longer than 25.00 ms (1.5x target)
```

The same command with the displays awake:

```text
frames delivered     : 597
acquisition timeouts : 0
measured rate        : 59.40 fps (from frame timestamps)
interval median      : 16.667 ms
late frames          : 2 of 596 intervals longer than 25.00 ms
```

In that state a subject that *is* granted the display loses it again one
presented frame later
([issue #178](https://github.com/wildware-uk/clipped/issues/178)), and none of
it is a capture defect. `SetThreadExecutionState(ES_DISPLAY_REQUIRED)` held for
the length of a run keeps the displays on and keeps the numbers meaningful; it
does not power a display back on once Windows has turned it off, and neither
does `WM_SYSCOMMAND`/`SC_MONITORPOWER`. Only an input event does that.

**A caveat on those two blocks, because they are the only numbers on this page
that have not been taken twice.** A later attempt to re-enter the powered-off
state failed: with the session idle for 1,024 seconds — well past the machine's
900-second display timeout — `wgc_fullscreen_dx11.rs` still delivered 301 of a
possible 300 frames with zero timeouts, so the compositor was at full rate and
the display had plainly not gone off. The likeliest reason is that these tests
put a fullscreen application on that display every few minutes, and Windows
resets the display's own idle timer for reasons that have nothing to do with
input. So **a long `GetLastInputInfo` idle time is not on its own evidence that
the displays are off**, which is what the earlier draft of
`tests/capture/README.md` said; the reliable tell is the frame count itself,
because the 4 Hz state is an order of magnitude, not a few percent.

### What is not covered

- **HDR.** The pool is created as `B8G8R8A8UIntNormalized` and the backend always
  reports `PixelFormat::Bgra8Unorm`.
  [Issue #99](https://github.com/wildware-uk/clipped/issues/99) owns HDR;
  `PixelFormat` already has the variants it will need.
- **Multiple displays and display changes** belong to
  [issue #98](https://github.com/wildware-uk/clipped/issues/98).

### Runtime fallback

Falling back to a *different backend* after this one fails mid-recording is
["Automatic capture fallback"](#automatic-capture-fallback) below. (Desktop
Duplication losing access to its display is a different thing, and that one is
handled inside the backend: it rebuilds its own duplication rather than asking
anybody to choose another backend. See further below.)

## Desktop Duplication

The fallback, and the second implemented backend
([issue #13](https://github.com/wildware-uk/clipped/issues/13)). It lives beside
the other one in `crates/capture/src/windows/desktop_duplication.rs` and is
registered in the same list, so `select` reaches it whenever Windows Graphics
Capture declines a target or is missing from the system, and a user can pin it
with `CaptureMethodSetting::Forced(CaptureMethod::DesktopDuplication)`:

```text
Capture method: Desktop Duplication
Current method: Desktop Duplication
```

### How it works

DXGI's `IDXGIOutputDuplication` hands over a duplicate of one *display output* as
a Direct3D 11 texture. It predates Windows Graphics Capture, needs no compositor
cooperation, and is what remains when `GraphicsCaptureSession::IsSupported` says
no.

The device it duplicates with is created on **the adapter that owns the output**,
found by walking `IDXGIFactory1::EnumAdapters1` and `IDXGIAdapter::EnumOutputs`
and matching `DXGI_OUTPUT_DESC::Monitor` against the target's `HMONITOR`. This is
not a detail: `DuplicateOutput` refuses a device created on any other adapter,
and on a machine with a discrete and an integrated GPU — most laptops, and this
project's development machine — the *default* adapter is frequently the wrong
one. That is also why this backend does not use the `CaptureDevice` in
`device.rs`, which is deliberately the default adapter plus a WinRT view that
DXGI has no use for.

A **monitor** target is zero copy. The caller is handed DXGI's own desktop image,
and the frame stays outstanding until the next acquisition, exactly as the
ownership rules above require.

A **window** target is that image cropped. Every frame, the window's client area
is read with `GetClientRect` and `ClientToScreen`, converted into the output's
coordinates, and `CopySubresourceRegion`'d into a texture this backend owns. One
GPU-to-GPU copy per frame is the price of reaching a window through an API that
only knows about screens; there is still no `Map`, no staging texture and no
system-memory round trip. The client area rather than the whole window, which is
the same choice `clipped_windows::WindowGeometry` makes and for the same reason:
the frame, the title bar and the drop shadow are not what a game renders.

Because the crop is recomputed per frame, a window that is dragged is followed.
A window that is dragged to *another display* is followed too: the backend
notices that `MonitorFromWindow` now answers differently and rebuilds the
duplication against the new output, which takes a few milliseconds and happens
once per crossing.

The copy is clamped to the destination texture as well as to the output, because
the window and the frame can legitimately disagree about the size. The frame is
whatever size the caller last acted on; the client rectangle is read fresh for
every acquisition, and `AcquireNextFrame` blocks for up to 100 ms in between, so
a window being drag-resized is routinely read at a different size from the
texture it is about to be copied into. Direct3D defines a
`CopySubresourceRegion` that writes outside the destination resource as
*undefined behaviour*, so `place_window_in_output` takes the destination's size
and guarantees the copy fits inside it; a window that has shrunk copies less than
the whole frame and the remainder is cleared, exactly as for a straddling window.
The caller is told about the new size by the next acquisition either way.

### A window that straddles two displays

It is captured from the display showing most of it — Windows' own answer, via
`MonitorFromWindow`, which is what every other API agrees with — and the part
hanging over the edge is **black**. The frame stays the size of the window's
client area throughout, so dragging a window across a boundary does not
reconfigure the encoder every few pixels; what changes is how much of the frame
has pixels behind it. `place_window_in_output` clamps the copy to the output and
returns where in the frame it lands, and the uncovered part is cleared before
each copy — only while the window straddles, so an ordinary capture pays nothing
for it. Without that clear the strip would keep showing the part of the window
that *used* to be there, scrolling as the window moves, which is worse than
black because it looks deliberate.

As the majority crosses the boundary, the duplication switches to the other
output and the black strip moves to the other side of the frame. Windows Graphics
Capture has no equivalent problem, which is one more reason it is preferred.

### Timestamps

`DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` is a performance-counter reading, so
the conversion is `CaptureTimestamp::from_performance_counter(ticks, frequency)`
with the frequency read once from `QueryPerformanceFrequency`, and the clock is
declared as `SourceClock::PerformanceCounter` — the same clock, and therefore
comparable with, what the Windows Graphics Capture backend reports. Nothing here
reads a clock.

A frame whose `LastPresentTime` is zero, or whose `AccumulatedFrames` is zero, is
released without being delivered: DXGI wakes an acquisition for a pointer move as
well as for a desktop update, and those two fields are how it says which happened.
Delivering a pointer-only wake-up would be delivering the previous frame again,
with no timestamp of its own to carry.

### Dropped frames

`AccumulatedFrames` is a **real count** from the source, not an estimate:
"the number of frames the operating system accumulated in the desktop image since
the calling application processed the last desktop frame". So
`CapturedFrame::frames_missed` is `AccumulatedFrames - 1` — one means the caller
kept up — and none of the timestamp arithmetic the Windows Graphics Capture
backend needs applies here. That difference is the reason
`CapturedFrame::with_frames_missed` exists rather than the interface computing
anything itself.

For a window target the figure counts updates to the whole *display*, not to the
window, because that is what the duplication is of. It is still the number of
source frames that did not reach the caller; it is just measured against a larger
source.

### Access lost, and why the recording does not end

`DXGI_ERROR_ACCESS_LOST` is what a mode change, a full-screen transition, a
driver reset or a session switch does to a duplication, and the only correct
response is to release everything and build a new one. That happens inside
`acquire`: the session — device, duplication, destination texture — is dropped,
a new output is found, a new duplication is made, and the acquisition carries on.
The caller sees no error. If the display came back a different size, it sees
`Acquisition::SizeChanged` and resizes, which is the ordinary path it already
has.

Three refinements, each of which exists because the obvious version is wrong:

- **`DXGI_ERROR_DEVICE_REMOVED`, `DXGI_ERROR_DEVICE_RESET` and
  `DXGI_ERROR_SESSION_DISCONNECTED` are treated the same way.** They are the same
  event seen from further away, and rebuilding the whole session — including the
  Direct3D device — is what covers a driver reset as well as a mode change.
- **A `ReleaseFrame` that fails means the duplication is finished.** This is not
  theory: measured on Windows 11 build 26200, changing `\\.\DISPLAY1` from
  2560x1440 to 1280x720 mid-capture makes `ReleaseFrame` fail, after which every
  `AcquireNextFrame` on that duplication answers `DXGI_ERROR_INVALID_CALL`
  (`0x887A0001`) rather than `DXGI_ERROR_ACCESS_LOST` — for ever, because the
  frame DXGI is waiting for can never be given back. An earlier version of this
  backend ignored the failed release, and the recording ended at the mode change
  with an unclassified backend error. The failure is now remembered and treated
  as what it is.
- **A display that is missing from the enumeration is not immediately a lost
  target.** It is absent for a moment in the middle of the very topology change
  that caused the access loss. It has to stay absent for five seconds before the
  recording is told `CaptureError::TargetLost`. Rebuilding is retried every 100 ms
  in the meantime, and the failure is logged the first time and then every five
  seconds rather than ten times a second.
- **A window target's display is asked for again, not remembered.** Every rebuild
  re-reads `MonitorFromWindow`, so a display being *removed* — switching a
  DisplayPort monitor off is the everyday version — does not end a window
  recording: Windows moves the window to a surviving display, the rebuild finds
  it there, and the recording carries on. Only a monitor recording, whose target
  really has gone, reaches the five-second grace and `TargetLost`. For a window
  Windows can locate, the remembered display *name* is deliberately not used as a
  fallback either: it names the display the window has just been moved off, and
  matching it would record the wrong screen. A minimised window is on no display
  at all, so it keeps the remembered one until it is restored.

A display that is *attached* but cannot be duplicated is retried for as long as
it stays that way, with no time limit. That is deliberate: the tempting
alternative — give up after a while — ends a recording because a UAC prompt was
on screen for six seconds, which is precisely what `DXGI_ERROR_ACCESS_DENIED`
means here and precisely what a game recorder has to survive. Acquisitions report
`Acquisition::Timeout` throughout, and the log says why every five seconds. The
one case that cannot be retried away is a display rotated mid-recording, which is
refused every time until it is rotated back
([issue #138](https://github.com/wildware-uk/clipped/issues/138)).

### What it will not do

| | |
| --- | --- |
| The cursor | Never appears. Desktop Duplication does not draw the pointer into the desktop image; it reports the position and shape separately for an application to composite. So `CaptureConfig::capture_cursor` cannot be honoured in either direction, `BackendCapabilities::is_cursor_optional` is false, and asking for a cursor logs that there will not be one. |
| Occlusion | Anything drawn over the target is in the recording, because this is a duplicate of the screen. `is_occlusion_independent` is false, and this is the main reason SPEC.md section 8 ranks the method below Windows Graphics Capture. |
| A rotated display | Refused, with `CaptureError::UnsupportedTarget`. DXGI hands over a rotated display's image *unrotated*, so a portrait display would record sideways, and a window cropped out of it would be cropped from the wrong pixels entirely. [Issue #138](https://github.com/wildware-uk/clipped/issues/138) owns rotation. |
| A minimised window | Waited out, like the other backend: acquisitions report `Acquisition::Timeout` until it comes back, rather than cropping the rectangle at (-32000, -32000) where Windows parks it. |
| A protected window | Declined at `availability`, like the other backend. `WDA_MONITOR` renders the window black and `WDA_EXCLUDEFROMCAPTURE` leaves whatever is behind it in the frame; neither is the recording anybody asked for. |
| A machine with no display output | Declined at `initialise` with `UnsupportedTarget`, naming the case: a remote session, a headless server or a virtual machine with no display. A basic display driver that has outputs but cannot duplicate them is declined the same way, naming that instead. |
| Two captures of one display in one process | Not possible. DXGI gives a process **one duplication per output**; a second `DuplicateOutput` for a display this process is already duplicating fails with `E_INVALIDARG` (`0x80070057`), which the backend classifies and reports as an `UnsupportedTarget` naming the limit rather than as an unexplained backend failure. One target per session is already an assumption of this pipeline, but it is a hard limit here rather than a design choice, and it is why the tests that duplicate a display take a mutex. |

The crop also assumes the process is **per-monitor DPI aware** — window
positions and the duplicated image have to be in the same units. A recorder calls
`clipped_windows::enable_per_monitor_dpi_awareness` once at start-up; if it has
not, the backend notices that the output's desktop rectangle and its duplicated
image are different widths and says so in the log rather than cropping the wrong
part of the screen in silence.

### Ownership and threading

`Running` owns the capture; `Session` owns the part access loss throws away — the
Direct3D device, the duplication, the destination texture and the outstanding
frame. Releasing a `Session` gives the outstanding frame back before dropping the
duplication, which is what lets other applications duplicate that output again;
leaking one would be a fault a user could only clear by ending the process.
`shut_down` is `self.running = None` and `Drop` calls `shut_down`, so an unwind
releases exactly what a clean stop would.

There are no callbacks and no second thread. `AcquireNextFrame` blocks with its
own timeout, so unlike the Windows Graphics Capture backend there is no event
handler and no condition variable. The wait is sliced at 100 ms so that a window
target's "has it closed, been minimised, or moved to the other display?" checks
happen about ten times a second however long the caller's timeout is. It sleeps
on exactly two paths, both of which have no frame to wait for: between failed
rebuild attempts, where the alternative is
spinning on `DuplicateOutput` for the length of a display transition, and while a
window target is minimised, where there is nothing on the display to crop. Both
sleep 100 ms at a time and neither ever sleeps past the caller's deadline.

### How to test it

Unlike the other backend, this one's real behaviour is exercised by tests rather
than by a probe example, because it can be: the tests paint a window a colour
nothing produces by accident, capture it, and read the pixel back out of the
captured texture through a one-pixel staging copy. A frame of the right size
proves nothing about *which* display it came from; a pixel does.

```text
cargo test -p clipped-capture desktop_duplication
```

Two things are opt-in:

- `CLIPPED_REQUIRE_CAPTURE=1` turns "this machine could not run the test" from a
  skip into a failure, as it does for the other backend. CI sets it.
- `CLIPPED_ALLOW_DISPLAY_CHANGES=1` enables
  `access_lost_is_recovered_from_without_ending_the_recording`, which changes a
  display's mode for a few seconds to provoke a real `DXGI_ERROR_ACCESS_LOST`.
  It is off by default because an unattended `cargo test` should not change the
  resolution of the display somebody is working on. It uses `CDS_FULLSCREEN`, so
  Windows restores the mode when the process exits even if the test never gets to
  its own restore. Every test that creates a window puts it on a non-primary
  display where there is one, topmost and never activated.

Measured on Windows 11 build 26200 with an RTX 4090 and two 2560x1440 displays:
the marker window on `\\.\DISPLAY1` is found in `\\.\DISPLAY1`'s capture and
absent from `\\.\DISPLAY2`'s; a mode change from 2560x1440 to 1280x720 and back
produced two access losses, two `SizeChanged` reports, and 260 then 586 frames
either side of them, with no error reaching the caller; and a caller that let the
display update six times between acquisitions was told it had missed 8, 5 and 8
updates.

### What is not covered

- **Dirty rectangles.** `GetFrameDirtyRects` could tell a window capture that
  nothing inside the window changed, saving a copy. It is not used: the copy is
  cheap and the encoder wants frames at a steady rate anyway.
- **HDR**, as for the other backend
  ([issue #99](https://github.com/wildware-uk/clipped/issues/99)). The
  duplication is created through `IDXGIOutput1::DuplicateOutput`, which is always
  `B8G8R8A8`; `IDXGIOutput5::DuplicateOutput1` is what will take a format list.
- **Rotation** ([issue #138](https://github.com/wildware-uk/clipped/issues/138)),
  refused rather than recorded wrongly, as above.
- **Rebuilding changes the Direct3D device**, so a future encoder that has bound
  itself to the capture device has to notice. Nothing consumes frames yet, and
  the seam for it is the same `SizeChanged`/reconfigure path; it is written down
  here rather than discovered later.

`CaptureError`'s variants are split by what a caller can do about them —
`TargetLost` means the recording is over, `Interrupted` means reinitialise this
same backend, `UnsupportedTarget` means try another — which is the
classification that decision will read.

## Automatic capture fallback

**Status: built in `crates/capture`, and not yet wired into a session.**
`CaptureFallback` ([issue #97](https://github.com/wildware-uk/clipped/issues/97))
is what keeps a recording going when the backend under it stops working. The
recording loop in `crates/session/src/recording.rs` still creates a backend
directly and ends the recording on the first error; adopting the fallback there
is [issue #285](https://github.com/wildware-uk/clipped/issues/285), because
`crates/session` is owned by other work in this milestone. Everything below
describes what the capture crate does today, and says where the boundary is.

### The shape, and why it is not a wrapper

`CaptureFallback` is a policy object. The caller goes on owning the
`CaptureBackend` and goes on calling `acquire` on it; when that fails, it hands
the backend over **by value** and gets a running replacement back:

```rust
let (mut fallback, mut backend, format) =
    CaptureFallback::start(candidates, &target, &config, setting)?.into_parts();

match backend.acquire(timeout) {
    Ok(Acquisition::Frame(frame)) => { /* fallback.inspect(&frame); encode it */ }
    Ok(Acquisition::Timeout) => fallback.note_silence(timeout),
    Err(error) => backend = fallback.recover(backend, error)?.into_parts().0,
}
```

Wrapping the acquisition instead would mean handing out a frame borrowed from
the same value that has to replace the backend the frame came from, which the
borrow checker will not have and which would put a second layer of indirection
in the hottest loop in the recorder. Passing the failed backend by value is also
enforcement rather than manners: the caller cannot go on using a backend it has
given up, and the fallback shuts it down *before* asking the platform for
another — necessary, because DXGI gives a process one duplication per display,
and a replacement would be refused while the corpse of the old one still held
it.

### What counts as a failure

| What happened | What the fallback does |
| --- | --- |
| `CaptureError::TargetLost` | Nothing. The recording is over; no backend records a window that has closed. Reported as `FallbackError::Unrecoverable`, keeping the original error so a caller can still tell it apart. |
| `CaptureError::NotInitialised`, `AlreadyInitialised` | The same. These are programming errors, and another backend would meet the same caller. |
| `CaptureError::UnsupportedTarget` | Falls back. This backend has said it cannot capture this target; another may. |
| `CaptureError::Interrupted` | Restarts the *same* backend, which is what that variant means — a driver reset or a mode change leaves the target where it was. Twice at most, then the method is retired and the next candidate takes over. |
| `CaptureError::Backend` (unclassified) | The same as `Interrupted`: a hiccup in the preferred backend is likelier than a broken one, and the cost of being wrong is one restart before the fall back happens anyway. |
| Black frames | Falls back, without a restart first: the backend is running, and running is the problem. See below. |
| No frames at all | **Nothing but a log line.** See below. |
| Initialisation failure, before the first frame | Falls past that candidate to the next, so a machine where Windows Graphics Capture is present but broken records through Desktop Duplication rather than not recording. |

A method that has failed is not asked again during that recording, so the chain
is walked at most once per candidate and a recording cannot spend its length
cycling between two broken backends.

### The rule that constrains everything: the frame size cannot change

Matroska fixes a track's dimensions in the header
([ADR 0001](adr/0001-mkv-archival-container.md)) and the encoder is configured
for one resolution, so **a replacement that produces a different `FrameFormat`
than the recording committed to is not used.** It is shut down again, the
mismatch is recorded with both sizes in it, and the next candidate is tried;
when none matches, the recording ends where it is, with a report that says
exactly that:

```text
Windows Graphics Capture cannot capture a window: the window has opted out of
being captured; no other capture backend could take over; Desktop Duplication:
it would produce 1280x720 BGRA8 unorm frames, and this recording's video track
is fixed at 1920x1080 BGRA8 unorm
```

This is the honest answer rather than a limitation nobody mentioned: continuing
would write frames of one size into a track that declares another, and a player
would show the difference as a stretched or torn picture in a file that looks
finished. It is also the same answer the pipeline already gives to a window
resized mid-recording, which is
[issue #184](https://github.com/wildware-uk/clipped/issues/184). When #184
decides how a session follows a size change — by scaling in the capture path, or
by starting a second file — the rule here relaxes in that same change, and the
seam it relaxes at is `CaptureFallback::resize`, which is how a caller that
followed a resize tells the fallback what size the recording now is.

In practice the mismatch is rare: both Windows backends produce the target's
client area in `B8G8R8A8`, so a replacement normally produces exactly what the
failed one did. It is the transition cases — a game that changed resolution in
the same moment its capture broke — that end here.

A second consequence of a backend change is recorded rather than solved: **the
replacement's frames come from a different Direct3D device.** An encoder opened
against the old device (`crates/session/src/windows/device.rs`) cannot bind them
and has to be reopened. Nothing in `crates/capture` can do that, and it is part
of what issue #285 has to do when it adopts this.

### Black frames

A capture that has silently stopped working does not return an error: it keeps
returning frames, and every pixel in them is zero. That is the failure the issue
calls "never silently produce a black recording", and it is the only capture
failure that cannot be seen from the API's return values — so it is the only one
worth reading pixels for.

- **The rule is exactly zero, not a threshold.** A pixel counts as *lit* when any
  of its red, green or blue channels is non-zero. A sample is black only when
  none of the pixels it read is lit. A dark scene is dark, not empty: a
  night-time game frame, a dim menu or an unlit corridor has dithering, noise and
  a heads-up display in it, so its pixels are 3, 8 or 20 rather than 0, and a
  threshold of "below 16 is black" would call it a broken capture. Alpha is
  ignored, because a compositor leaves whatever it likes there.
- **Sixteen pixels, twice a second.** `D3d11FrameSampler` copies a 4x4 grid of
  single pixels into a 16x1 staging texture with `CopySubresourceRegion` and maps
  it once — 64 bytes, and the expense is the map rather than the bytes, because
  it waits for those copies. `BlackFrameWatch` therefore rations sampling to one
  frame every 500 ms, so 58 frames in 60 are never touched at 60 fps. The grid is
  inset by half a cell so that no sample lands on the frame's very edge, which is
  legitimately black in plenty of working captures.
- **Ten seconds, because duration is the only thing that separates the two
  cases.** A source that is *deliberately* black — a loading screen, a fade, a
  paused game — produces exactly the same pixels as a broken capture. Nothing in
  any capture API distinguishes them, so the watch waits: ten continuous seconds
  of black is far beyond a fade and longer than most loading screens. When it is
  wrong, the cost is bounded and visible — the recording carries on, one method
  change is logged, and the frames it was recording were black anyway.
- **A frame that cannot be sampled is no evidence.** An unsupported pixel format
  (HDR, [issue #99](https://github.com/wildware-uk/clipped/issues/99)) or a
  Direct3D call that declines produces no sample rather than a black one, so a
  readback failure can never end a recording.

The sampler is tested against real Direct3D textures with no window and no
capture involved: a texture filled with opaque black samples as black, one
filled with a blue channel of 4 does not, and one that is black apart from a
painted corner does not — which is the assertion that the grid reaches a
heads-up display rather than only the middle of a dark screen.

### Silence is reported, not acted on

A capture producing *no* frames is indistinguishable from a source producing
none, and the commonest reason for the second is a minimised window — which both
backends deliberately wait out (see the tables above). Falling back on silence
would swap a user's preferred backend for a worse one every time they alt-tabbed,
and the replacement would be just as silent. So `note_silence` accumulates it,
logs it every thirty seconds, and `silent_for` can be shown on a diagnostics
screen; nothing else happens. This is a deliberate narrowing of the issue's
"no frames" bullet, and the alternative would need a backend able to say "the
source is idle" rather than "no frame arrived", which neither Windows API
offers.

### What the user and the diagnostics see

`CaptureStatus` is the whole of it, and the two lines SPEC.md section 8 asks for
are unchanged by any of this — a user never learns that a backend was swapped
underneath them for their recording to survive:

```text
Capture method: Automatic
Current method: Desktop Duplication
```

`status().changes()` is the third thing, for the diagnostics screen and the
session log: every restart and replacement, in order, each carrying the method
before, the method after, what triggered it, and the failure in the words the
failure used. "Desktop Duplication", in a recording that started on Windows
Graphics Capture, is otherwise a fact with no explanation attached.

Every attempt and every outcome is also logged as it happens, with
`capture_backend`, `previous_capture_backend` and `trigger` fields, at `warn`
for a change and `error` for a recording that ends because nothing could take
over.

### What is not built

- **Remembering per game what worked.** The issue asks for it and it is not here:
  the value to remember is `status().current_method()`, and the place to keep it
  is per-game configuration, which is `clipped-config`'s
  ([issue #108](https://github.com/wildware-uk/clipped/issues/108)) rather than
  this crate's. [Issue #286](https://github.com/wildware-uk/clipped/issues/286)
  covers storing it and preferring it at the next launch of that game.
- **The session using any of this**
  ([issue #285](https://github.com/wildware-uk/clipped/issues/285)), including
  reopening the encoder against the replacement's graphics device.
- **The desktop UI showing the current method.** The status is there to be shown;
  no screen shows it yet
  ([issue #101](https://github.com/wildware-uk/clipped/issues/101) owns
  diagnostics).

## How platform-neutral this is

The interface is platform-neutral in shape: no trait method, signature or data
structure here is Windows-specific, and there is no Windows code in the crate at
all — it goes in `crates/windows` or a `windows/` submodule, and issue #12
creates the first one.

The vocabulary is another matter, and it is worth being exact rather than
reassuring. Three enumerations name a platform outright:
`CaptureMethod::WindowsGraphicsCapture` and `CaptureMethod::DesktopDuplication`
name Windows capture APIs, `TextureKind::D3d11Texture2D` names a Direct3D 11
interface, and `SourceClock::PerformanceCounter` names a Windows clock. A
capture interface cannot be wordless about what a texture is or which clock
stamped a frame — the encoder has to know what it has been handed — so the
unavoidable platform words are concentrated in three small closed enumerations
where a reader can see all of them at once, rather than spread through the
traits as casts and conditional compilation (AGENTS.md section 5).

The cost is that a backend for another platform is not only an implementation of
`CaptureBackend`. It needs variants in those enumerations, and they are closed to
other crates, so it is written here or the enumerations are opened first.
`SourceClock::Monotonic` already exists for that day; `TextureKind` has a single
variant and would need a second. Whether this shape is still right when a second
platform actually exists is a question for then.

## Adding a backend

1. **Put it in a platform module.** Windows code goes in `crates/windows` or in
   a `windows/` submodule of `crates/capture`, never spread through the
   platform-neutral modules (AGENTS.md section 5).
   `crates/capture/src/windows/` is the worked example. The layering test in
   `tests/integration/tests/workspace_layering.rs` enforces the crate-level half
   of this.
2. **Add a `CaptureMethod` variant** if it is a new technique. The crate will
   not compile until the variant has a `preference_rank`, and the rank will not
   compile unless it names that method's own slot in `PREFERENCE_ORDER`, so the
   published order and the order selection uses cannot part company. Add the
   matching `log_value` and a `clipped_logging::CaptureBackend` variant for it
   in the same change: `log_values_match_the_logging_field_vocabulary` compares
   the two enumerations, so the vocabularies cannot part company either. Game
   Capture is the one method with no logging counterpart, deliberately — see
   "Game Capture" above.
3. **Implement `BackendDeclaration`.** Declare capabilities as constants.
   `availability` must be cheap and must not allocate GPU resources: every
   candidate ahead of the winner is asked while a user waits for a recording to
   start.
4. **Implement `CaptureBackendFactory` and `CaptureBackend`.** Obey the six
   ownership rules above, take timestamps from the frame, and release in `Drop`
   as well as in `shut_down`.
5. **Register it.** Add it to `REGISTERED` in `crates/capture/src/registry.rs`,
   which is the one place that says what a build contains. Nothing else needs to
   change: `select` sorts whatever it is handed.
6. **Write the `SAFETY` comment.** `FrameTexture::new` is `unsafe` precisely so
   that a backend author has to state why the texture outlives the frame
   (AGENTS.md section 58).
7. **Test it for real.** Selection logic is unit tested; a backend cannot be.
   There are two patterns to follow, and which one fits depends on what the
   backend can be asked. `crates/capture/examples/wgc_probe.rs` is a controlled
   test window, a real capture of it, and measured pacing, dropped frames and
   resource usage — a measurement somebody reads. The Desktop Duplication tests
   in `crates/capture/src/windows/desktop_duplication.rs` are assertions a
   machine reads: a window painted a colour nothing produces by accident, and the
   captured pixel read back to prove which display, and which part of it, the
   frame came from. Prefer the second where the behaviour has a right answer.
   The shared test applications
   ([issue #23](https://github.com/wildware-uk/clipped/issues/23)) and the media
   validation harness
   ([issue #24](https://github.com/wildware-uk/clipped/issues/24)) will replace
   the bespoke half of it.

## The one frame that leaves the GPU: screenshots

Everything above keeps a frame on the GPU from the compositor to the encoder,
and the "Assumptions" section below says so outright. A screenshot is the one
thing that genuinely cannot: a PNG is bytes in system memory, and there is no
route from a Direct3D texture to a file that does not pass through them
(SPEC.md section 26,
[issue #67](https://github.com/wildware-uk/clipped/issues/67)).

So the exception is made as narrow as it can be. `StillFrame` is one frame's
pixels, owned and `Send`; `windows::D3d11StillCopier` is what produces one, on
request, never per frame. Nothing on the recording path calls it, and the type
that comes out is the only thing in this crate that outlives an acquisition.

**The copy is in two halves, and that is the whole design.** `CopyResource` into
a staging texture is queued for the GPU and returns; `Map` is what waits for it,
and a wait on the capture thread is a frame nobody recorded. So `begin` issues
the copy and flushes on the frame the key was pressed on, and `poll` maps with
`D3D11_MAP_FLAG_DO_NOT_WAIT` on a later one — answering "not yet" until the GPU
has caught up rather than blocking. Measured on an RTX 4090 with
`cargo run --release -p clipped-capture --example still_cost`:

| Frame | Bytes | `begin` | `poll` |
| --- | --- | --- | --- |
| 1920x1080 | 8.1 MB | 0.054 ms median | 1.05 ms median |
| 2560x1440 | 14.4 MB | 0.071 ms median | 1.95 ms median |
| 3840x2160 | 32.4 MB | 0.070 ms median | 4.29 ms median |

The frame the key was pressed on pays the first column; a frame or two later
pays the second, with a whole frame's budget in hand rather than whatever was
left of it. Encoding and writing happen on the thread that asked for the
screenshot and never here.

[screenshots.md](screenshots.md) is the rest of it: the rendezvous with the
thread that asked, the format and colour decisions, where the files go, and what
the path with no recording behind it costs.

## Assumptions

- A capture source stamps every frame with a monotonic reading from a clock the
  audio side can also read. Both Windows capture APIs do; a platform where that
  is untrue would need more than a new backend.
- One target per session. Capturing two windows at once is not in the interface
  and is not planned.
- Frames stay on the GPU **on the recording path**. There is no path for
  system-memory frames, because a CPU round trip at 1080p60 would exceed the
  whole performance budget in SPEC.md section 38 on its own. The one exception
  is a screenshot, above: one frame, on request, and measured.

## Still to be written

These belong to this document and are not in it, either because the code they
would describe does not exist or because it landed elsewhere and the prose here
has not caught up:

- Target selection and enumeration: how a window or monitor is chosen
  ([issue #10](https://github.com/wildware-uk/clipped/issues/10)), and what
  happens when it moves between displays or disappears
  ([issue #98](https://github.com/wildware-uk/clipped/issues/98)).
- Encoder selection across NVENC, AMF, Quick Sync and the software fallback
  ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
- Back-pressure: what happens when the encoder cannot keep up, and which frames
  are dropped when some must be. The answer the session gives today is in
  `crates/session/src/muxing.rs` — the loop stops submitting frames while the
  writer is behind and counts every frame it skipped — and belongs here in
  prose.
- HDR ([issue #99](https://github.com/wildware-uk/clipped/issues/99)) and
  multi-monitor and ultrawide behaviour
  ([issue #98](https://github.com/wildware-uk/clipped/issues/98)).

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md) for the
container the pipeline writes into.
