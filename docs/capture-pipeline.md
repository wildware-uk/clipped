# Capture pipeline

**Status: the interface exists; no backend does.** `crates/capture` defines the
capture backend trait, the frame and timestamp vocabulary, and the policy that
picks a backend and reports which one it picked. Nothing in this repository can
currently produce a frame: Windows Graphics Capture is
[issue #12](https://github.com/wildware-uk/clipped/issues/12) and Desktop
Duplication is [issue #13](https://github.com/wildware-uk/clipped/issues/13),
both in M1, and neither has landed. The muxer is likewise still a
documentation-only crate, and `clipped-encoder` can say what this machine could
encode with ([encoder-capabilities.md](encoder-capabilities.md)) without being
able to encode anything.

So this document describes an *interface* and the rules a backend has to obey.
Where it describes behaviour that does not exist yet it says so, because a
document that quietly describes intentions as facts is worse than a short one
(AGENTS.md section 7). It answers the questions AGENTS.md section 47 asks of a
subsystem, and the sections still marked as unwritten are listed at the end.

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

None of the threading above is implemented. `clipped-session` owns the capture
thread and it is still a documentation-only crate; what exists today is the
`Send`/`Sync` bounds that will hold it to this shape.

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
say so at the top of the module. Real capture is tested against real Windows
APIs in issues #12 and #13.

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

### Runtime fallback is not built

Falling back *after* a backend fails mid-recording — black frames, no frames,
access lost — is [issue #97](https://github.com/wildware-uk/clipped/issues/97)
in M13, and none of it exists. No seam had to be invented for it: because
selection is a pure function of the candidate list, falling back is calling
`select` again with the failed method removed. What #97 has to add is the part
that is genuinely missing, which is deciding *when* a backend has failed and
remembering the answer per game.

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
   platform-neutral modules (AGENTS.md section 5). No such module exists yet;
   issue #12 creates the first one. The layering test in
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
5. **Write the `SAFETY` comment.** `FrameTexture::new` is `unsafe` precisely so
   that a backend author has to state why the texture outlives the frame
   (AGENTS.md section 58).
6. **Test it for real.** Selection logic is unit tested; a backend is not. It
   needs a controlled test application
   ([issue #23](https://github.com/wildware-uk/clipped/issues/23)) and the media
   validation harness
   ([issue #24](https://github.com/wildware-uk/clipped/issues/24)), and the
   acceptance criteria on issues #12 and #13 ask for measured frame pacing and a
   documented check that a long capture leaks no GPU resources.

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
- How to run a capture from the command line, and how to test one without a
  game (issues #23 and #24).
- HDR ([issue #99](https://github.com/wildware-uk/clipped/issues/99)) and
  multi-monitor and ultrawide behaviour
  ([issue #98](https://github.com/wildware-uk/clipped/issues/98)).

Related decisions: [ADR 0001](adr/0001-mkv-archival-container.md) for the
container the pipeline writes into.
