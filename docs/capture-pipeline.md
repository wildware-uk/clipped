# Capture pipeline

**Status: the interface exists, and both Windows backends do.**
`crates/capture` defines the capture backend trait, the frame and timestamp
vocabulary, the policy that picks a backend and reports which one it picked,
and — since [issue #12](https://github.com/wildware-uk/clipped/issues/12) and
[issue #13](https://github.com/wildware-uk/clipped/issues/13) — the Windows
Graphics Capture and Desktop Duplication backends that implement all of it. A
Windows build can produce GPU frames from a window or a display today, by either
method, and `clipped-encoder` can say what this machine could encode with
([encoder-capabilities.md](encoder-capabilities.md)) without being able to
encode anything yet.

What is still missing is everything downstream. `clipped-encoder`,
`clipped-muxer` and `clipped-session` are still documentation-only crates, so
nothing consumes a frame: `recorder record` still reports that the capture
engine is not implemented, because a pipeline needs more than its first stage.

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

The *capture thread itself* does not exist yet: `clipped-session` owns it and is
still a documentation-only crate. What exists is a backend that obeys the rules
above — see "Windows Graphics Capture" below — and the `Send`/`Sync` bounds that
will hold the session to them. `crates/capture/examples/wgc_probe.rs` runs the
loop on its main thread, which is the shape a session's capture thread will
take.

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
duration to average away. The audio/video synchronisation model that consumes
all this is [issue #22](https://github.com/wildware-uk/clipped/issues/22).

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

### What is not covered

- **Exclusive fullscreen has not been exercised.** The probe asks for it through
  `IDXGISwapChain::SetFullscreenState`; on the development machine DXGI refuses
  with `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` because Windows will not grant the
  foreground to a process the user did not interact with, and DXGI will not go
  exclusive for a background window. Borderless-fullscreen — a `WS_POPUP` window
  covering the whole display, which is what most modern games actually use — is
  exercised and works.
- **HDR.** The pool is created as `B8G8R8A8UIntNormalized` and the backend always
  reports `PixelFormat::Bgra8Unorm`.
  [Issue #99](https://github.com/wildware-uk/clipped/issues/99) owns HDR;
  `PixelFormat` already has the variants it will need.
- **Multiple displays and display changes** belong to
  [issue #98](https://github.com/wildware-uk/clipped/issues/98).

### Runtime fallback is not built

Falling back to a *different backend* after one fails mid-recording — black
frames, no frames — is
[issue #97](https://github.com/wildware-uk/clipped/issues/97) in M13, and none of
it exists. (Desktop Duplication losing access to its display is a different
thing, and that one is handled: it rebuilds its own duplication rather than
asking anybody to choose another backend. See below.) No seam had to be invented
for #97: because selection is a pure function of the candidate list, falling back
is calling `select` again with the failed method removed. What it has to add is
the part that is genuinely missing, which is deciding *when* a backend has failed
and remembering the answer per game.

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
| Two captures of one display in one process | Not possible. DXGI gives a process **one duplication per output**; a second `DuplicateOutput` for a display this process is already duplicating fails with `E_INVALIDARG` (`0x80070057`). One target per session is already an assumption of this pipeline, but it is a hard limit here rather than a design choice, and it is why the tests that duplicate a display take a mutex. |

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
happen about ten times a second however long the caller's timeout is. The one
place this backend sleeps is between failed rebuild attempts, where there is no
frame to wait for and the alternative is spinning on `DuplicateOutput` for the
length of a display transition.

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

## Assumptions

- A capture source stamps every frame with a monotonic reading from a clock the
  audio side can also read. Both Windows capture APIs do; a platform where that
  is untrue would need more than a new backend.
- One target per session. Capturing two windows at once is not in the interface
  and is not planned.
- Frames stay on the GPU. Nothing here has a path for system-memory frames,
  because a CPU round trip at 1080p60 would exceed the whole performance budget
  in SPEC.md section 38 on its own.

## Still to be written

These belong to this document and are not in it, because the code they would
describe does not exist:

- The path from a frame to a packet in the container, and the thread that owns
  each stage past capture — with `clipped-encoder`, `clipped-muxer` and
  `clipped-session` still empty, there is no path to describe.
- Target selection and enumeration: how a window or monitor is chosen
  ([issue #10](https://github.com/wildware-uk/clipped/issues/10)), and what
  happens when it moves between displays or disappears
  ([issue #98](https://github.com/wildware-uk/clipped/issues/98)).
- Encoder selection across NVENC, AMF, Quick Sync and the software fallback
  ([issue #14](https://github.com/wildware-uk/clipped/issues/14)).
- Back-pressure: what happens when the encoder cannot keep up, and which frames
  are dropped when some must be.
- How to run a capture from the command line. `recorder record` still reports
  that the capture engine is not implemented, because a capture backend on its
  own is not a recording; `crates/capture/examples/wgc_probe.rs` is how a
  capture is exercised today. The shared test applications and the media
  validation harness are issues #23 and #24.
- HDR ([issue #99](https://github.com/wildware-uk/clipped/issues/99)) and
  multi-monitor and ultrawide behaviour
  ([issue #98](https://github.com/wildware-uk/clipped/issues/98)).

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md) for the
container the pipeline writes into.
