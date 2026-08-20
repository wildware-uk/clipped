# What Clipped is known to record, and on what

**Status: a gathering, not a campaign of new testing.** Almost nothing on this
page is a fresh measurement. It is the readings that were already taken — in the
course of other work, mostly in the week of 2026-08-13 — collected into one
place with the test or the pull request that produced each one beside it, and
with the cells nobody has been able to fill saying **unknown** and what would
settle them.

This is milestone 13's matrix
([issue #96](https://github.com/wildware-uk/clipped/issues/96)), and SPEC.md
section 42 is the list it has to answer.

## What this page is for, and what would make it worthless

A table of green ticks. Its value is in the rows that are not ticks: the things
that do not work, the things nobody has hardware to test, and the things that
were written down as fact and turned out false the first time somebody measured
them. This repository has done that last one at least three times —
[ADR 0011](adr/0011-what-the-webview-plays.md)'s seek behaviour,
[issue #392](https://github.com/wildware-uk/clipped/issues/392)'s playback
claim, and the AVC-expiry framing in
[ADR 0008](adr/0008-codec-patent-position.md) — and each correction is a row
here, because a matrix that hid that pattern would be worse than none.

So: **where a cell is measured it names the test or the pull request that
measured it, and where it is not it says unknown.** If a claim below cannot be
traced to something a reader can re-run or re-read, it should not be here.

## Why this is four tables rather than one grid

Capture, encoders, audio and playback do not share an axis. Capture is a backend
against a target; an encoder is a vendor runtime against a codec; audio is a
source against a track; playback is a decoder against a container. Forcing them
into one grid would need a cell that means four different things depending on
which row you are in, which is how a matrix stops being read. Four tables with
different columns, and one index of failures at the end, says more.

## Where the detail lives

**This page holds the row and the citation. The subsystem documents hold the
reasoning**, and they are the source of truth:

| Subsystem                                       | Document                                                                            |
| ----------------------------------------------- | ----------------------------------------------------------------------------------- |
| Capture backends, fallback, frame lifecycle     | [capture-pipeline.md](capture-pipeline.md)                                          |
| What an encoder is asked, and what it answers   | [encoder-capabilities.md](encoder-capabilities.md), [encoder-pipeline.md](encoder-pipeline.md) |
| Tracks, scoping, what lands where               | [audio-routing.md](audio-routing.md)                                                |
| A/V offset and the hour-long drift run          | [av-sync.md](av-sync.md)                                                            |
| Every command named below, and what it needs    | [testing.md](testing.md)                                                            |

A summary that restated the reasoning would be a second copy of it, and second
copies drift — which is the failure mode this repository keeps finding. So the
rows below are deliberately short and the links are load-bearing.

## The machine every "measured" row was measured on

A matrix without its hardware is a rumour. Unless a row says otherwise, this is
the machine:

|                             |                                                                                                                                                                                                                     |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Operating system            | Windows 11 Pro build 26200                                                                                                                                                                                          |
| Processor                   | AMD Ryzen 9 9950X3D, 16 cores / 32 logical processors                                                                                                                                                                |
| Adapter capture lands on    | NVIDIA GeForce RTX 4090, device `0x2684`, driver 32.0.16.1074, 23.6 GiB dedicated                                                                                                                                    |
| Second adapter              | AMD Radeon(TM) Graphics (integrated), device `0x13C0`, driver 32.0.21043.5001, 2.0 GiB dedicated                                                                                                                     |
| Third adapter               | Microsoft Basic Render Driver (software rasteriser), driver 10.0.26100.8972                                                                                                                                         |
| Intel adapter               | **None.** That is the whole of why the Quick Sync column is empty ([#160](https://github.com/wildware-uk/clipped/issues/160))                                                                                        |
| Displays                    | Two 2560x1440. **Neither is rotated**, which is why the rotation rows are unmeasured ([#138](https://github.com/wildware-uk/clipped/issues/138)). The frame-accounting run in [testing.md](testing.md) was at 144 Hz |
| Default render endpoint     | Razer BlackShark V2 Pro 2.4 GHz wireless headset, 48 kHz mix format ([audio-routing.md](audio-routing.md))                                                                                                           |
| Virtual audio device        | Steam Streaming Microphone, render end to capture end, measured cable gain 31.6x ([PR #645](https://github.com/wildware-uk/clipped/pull/645))                                                                        |
| FFmpeg                      | n8.1.2-34-g9b6c8969e0-20260809, LGPL version 3 or later                                                                                                                                                             |

The adapter, encoder and FFmpeg rows are not typed from memory: they are what
`clipped-recorder capabilities` printed on this machine while this page was
written. See
[Filling in a column](#filling-in-a-column-from-your-own-machine).

## How to read a cell

|             |                                                                                                                                                                                                              |
| ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Yes**     | Something ran and was checked. The cell names the test or the pull request.                                                                                                                                  |
| **No**      | Something ran and did not work, or the code refuses the case deliberately. The cell names the issue.                                                                                                         |
| **Unknown** | Nobody has run it. The cell says what would settle it. This is not a soft no — `Claim::Unknown` exists in `crates/encoder/src/claim.rs` for exactly this reason, kept apart from a measured no in the type.   |

---

## 1. Capture backends

Two backends are registered: **Windows Graphics Capture** and **Desktop
Duplication** (`crates/capture/src/registry.rs`). A third method, **Game
Capture**, is named in `CaptureMethod::PREFERENCE_ORDER` and is deliberately
**not** registered — it would mean injecting a DLL into a game, which AGENTS.md
section 34 refuses. It is therefore unavailable on every machine and has no
measured row anywhere below.

`Automatic` prefers Game Capture, then Windows Graphics Capture, then Desktop
Duplication, and takes the first that both addresses the target and reports
itself available (`crates/capture/src/selection.rs`). Mid-recording it can
change its mind: `crates/capture/src/fallback.rs`.

### What each backend does with a target

| Target or condition                              | Windows Graphics Capture                                                                                                          | Desktop Duplication                                                                        | Traced to                                                                                                                                              |
| ------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| A borderless window                              | **Yes** — 181 of 181 source frames, 0 dropped, 0 duplicated, 0 out of order                                                       | Unknown as a whole-run frame accounting; no equivalent test exists                          | `tests/capture/wgc_video_pattern.rs::a_borderless_window_is_captured_frame_for_frame`                                                                  |
| A recording's frames, against the ones the source drew | **Yes** — 228 of 228 pictures decoded, counters 13 to 240, 0 missing, 0 duplicated, 0 out of order, 0 undecodable, through NVENC AV1. The capture row above is about frames *arriving*; this is about what reached the file | Unknown; no equivalent test drives it | `tests/capture/recorded_frames.rs::the_frames_in_a_recording_are_the_frames_the_source_drew_in_order`, [#183](https://github.com/wildware-uk/clipped/issues/183) |
| Recording over and over, for a quarter of an hour | **Yes, and nothing accumulates** — 149 recordings, 51,189 frames encoded, committed memory +14.4 MB against a 32 MB cap and handles +36 against 96. The handle count rises to +38 by cycle 30 and then does not move; over the last 49 cycles it *fell* by two | Unknown; no equivalent test drives it | `tests/capture/soak.rs::recording_over_and_over_does_not_leave_the_process_holding_more`, [#105](https://github.com/wildware-uk/clipped/issues/105) |
| A window with a title bar                        | **Yes** — the chrome around the pattern, the client area found at (1, 31) of a 1282x752 frame                                     | Yes, by crop; covered by `clipped-capture`'s own tests rather than end to end               | `tests/capture/wgc_video_pattern.rs::a_window_with_a_border_is_captured_with_its_chrome_around_the_pattern`                                            |
| An application holding a display exclusively     | **Yes, at about nine frames in ten** — 266 to 279 of 300 across eight granted runs. The shortfall is unexplained, [#192](https://github.com/wildware-uk/clipped/issues/192) | Unknown                                                                                    | `tests/capture/wgc_fullscreen_dx11.rs::a_fullscreen_application_is_captured_and_gives_its_display_back`, [capture-pipeline.md](capture-pipeline.md)     |
| A whole display                                  | Yes                                                                                                                               | **Yes** — a marker window on `\\.\DISPLAY1` is in `\\.\DISPLAY1`'s capture and absent from `\\.\DISPLAY2`'s | [capture-pipeline.md](capture-pipeline.md); `cargo test -p clipped-capture -- --ignored`                                                                |
| A client area with an odd dimension              | **Yes, one row short.** A 986x593 window records at 986x592                                                                       | Yes, one row short, by the same rule                                                        | `tests/capture/odd_client_area.rs::a_window_with_an_odd_client_area_is_recorded_one_row_short_of_it_rather_than_not_at_all`, [ADR 0013](adr/0013-capture-rounds-an-odd-dimension-away.md) |
| A target one pixel wide or tall                  | **No** — refused by name, because 4:2:0 has no such picture                                                                       | **No**, the same                                                                            | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| **A rotated display**                            | **Yes** — the compositor composes the rotation for the caller                                                                     | **No** — `CaptureError::UnsupportedTarget`, refused rather than recorded sideways            | [#138](https://github.com/wildware-uk/clipped/issues/138), `crates/capture/src/windows/desktop_duplication.rs`                                          |
| A minimised window                               | Waited out — `Acquisition::TargetMinimised`, no frames until it comes back                                                        | Waited out, the same                                                                        | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| An occluded window                               | **Yes** — the window's own content, whatever is in front of it                                                                    | **No** — whatever is drawn over it is in the recording                                      | `BackendCapabilities::is_occlusion_independent`, [capture-pipeline.md](capture-pipeline.md)                                                             |
| A protected window (`WDA_MONITOR`, `WDA_EXCLUDEFROMCAPTURE`) | Declined at availability                                                                                              | Declined at availability                                                                    | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| The mouse cursor                                 | Optional — `IsCursorCaptureEnabled`, from Windows 10 build 19041                                                                  | **Never** — DXGI reports the pointer separately and this backend does not composite it       | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| The capture border                               | Removable on **Windows 11 build 22000** and later, probed by `ApiInformation` rather than by build number; below that it stays and is logged | Not applicable                                                                   | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| A display that has powered off                   | Degrades to about 4 Hz — 40 frames delivered against 80 timeouts, where awake it was 597 against 0. **That reading has not been reproduced since**, and the page it is on says so | **Stops entirely** — 0 frames and 12 timeouts on either display, even while a window was repainting; reproduced | [#461](https://github.com/wildware-uk/clipped/issues/461), [ADR 0015](adr/0015-capture-holds-the-display-awake.md)                                      |
| A window straddling two displays                 | Not a problem — the window is the target                                                                                          | Captured from the display covering most of it; the overhang is black                        | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| A second capture of one display in one process   | Yes                                                                                                                               | **No** — DXGI gives a process one duplication per output; the second fails `E_INVALIDARG`   | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| A machine with no display output — headless, a VM, a remote session | Unknown; not measured                                                                                          | Declined at `initialise`, naming the case                                                   | [capture-pipeline.md](capture-pipeline.md)                                                                                                             |
| HDR                                              | **No** — the frame pool is `B8G8R8A8UIntNormalized`                                                                               | **No** — `IDXGIOutput1::DuplicateOutput` is always `B8G8R8A8`                                | [#99](https://github.com/wildware-uk/clipped/issues/99)                                                                                                |
| A still frame out of a capture — a screenshot    | **Yes**, for a borderless window, a bordered one, a fullscreen subject, every image format this build writes, and out of a running recording without interrupting it | Unknown; the screenshot tests drive the preferred backend                     | `tests/capture/screenshot.rs::a_screenshot_of_a_borderless_window_is_the_pattern_the_application_drew`, `tests/capture/screenshot_fullscreen.rs::a_screenshot_of_a_fullscreen_subject_is_the_pattern_it_drew`, `tests/capture/screenshot_during_recording.rs::screenshots_taken_out_of_a_running_recording_do_not_interrupt_it` |
| **A hosted CI runner**                           | **No, and never has been** — `CreateCaptureItemForWindow` answers `0x80070057` for any window, for want of a compositor            | **No** — a runner paints no window whose pixels can be found                                | [testing.md](testing.md), `.github/workflows/ci.yml`                                                                                                   |

That last row is the most important one in the table and is the easiest to skim
past. **Neither backend has ever captured a window in continuous integration and
neither ever will**, because a hosted runner has no compositor. Every "yes"
above came from somebody typing a command on the machine described in
[The machine](#the-machine-every-measured-row-was-measured-on). Nothing protects
any of them from a regression between one person's runs.

### A capture that breaks in the middle of a recording

No test application can cause one: a driver reset, a window that revokes capture
part way through, or a session that silently starts handing over frames with
nothing in them. Those are covered instead from inside `crates/session`, against
a scripted backend factory with real Direct3D devices, the real black-frame
sampler reading real pixels back off the GPU, and a real Matroska file that is
then decoded (`cargo test -p clipped-session --lib recording::tests`).

**What no row here can say is whether a real broken capture on real hardware
looks the way that fixture does.** A Windows Graphics Capture session that has
stopped working is *observed* to hand over frames of zeroes rather than to
report anything, which is what the detector is built around — and nothing in
this repository can make one do it on request. That is an open unknown, not a
covered case ([#97](https://github.com/wildware-uk/clipped/issues/97),
[#285](https://github.com/wildware-uk/clipped/issues/285)).

### The twelve scenarios SPEC.md section 42 names

This is the honest half of the matrix. Four of the twelve are measured, three
partly, four not at all, and one is refused by construction.

| Scenario             | State                | What was measured, or what would settle it                                                                                                                                                                                                                                                                                                    |
| -------------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fullscreen exclusive | **Measured**         | `tests/capture/wgc_fullscreen_dx11.rs`, Windows Graphics Capture only, about nine frames in ten ([#192](https://github.com/wildware-uk/clipped/issues/192) for the shortfall). Whether Windows *grants* the display is its focus policy rather than capture: on this machine the only condition that moved the answer was a live process that had synthesised an input event |
| Borderless           | **Measured**         | `tests/capture/wgc_video_pattern.rs::a_borderless_window_is_captured_frame_for_frame`, 181 of 181                                                                                                                                                                                                                                              |
| Windowed             | **Measured**         | `tests/capture/wgc_video_pattern.rs::a_window_with_a_border_is_captured_with_its_chrome_around_the_pattern`                                                                                                                                                                                                                                    |
| DirectX 11           | **Measured**         | Both test applications render with it: `test-apps/video-pattern` and `test-apps/fullscreen-dx11`                                                                                                                                                                                                                                              |
| DirectX 12           | **Unknown**          | No test application renders with it. A `video-pattern` sibling presenting the same decodable pattern through a DX12 swapchain would settle it and would reuse `pattern::decode` unchanged                                                                                                                                                      |
| Vulkan               | **Unknown**          | The same, through a Vulkan swapchain                                                                                                                                                                                                                                                                                                          |
| OpenGL               | **Unknown**          | The same, through a WGL context                                                                                                                                                                                                                                                                                                               |
| Multiple monitors    | **Partly measured**  | Desktop Duplication addresses the right display, and a window straddling two is captured from the one covering most of it. What is unmeasured is a *recording* that follows a window between displays. Owned by [#98](https://github.com/wildware-uk/clipped/issues/98)                                                                         |
| Ultrawide            | **Unknown**          | No display here is ultrawide. Both test applications take `--width` and `--height`, so a 3440x1440 or 5120x1440 borderless run needs the panel and nothing else                                                                                                                                                                                |
| HDR                  | **No**               | Refused by construction in both backends, and `encoding::surface_format` refuses a 10-bit surface by name. [#99](https://github.com/wildware-uk/clipped/issues/99) for capture, [#146](https://github.com/wildware-uk/clipped/issues/146) for the signalling that would make a 10-bit stream mean anything                                      |
| Resolution switching | **Partly measured**  | A **window** resize is measured end to end and ends the file, with the successor and the seam measured on the session's own timeline ([#184](https://github.com/wildware-uk/clipped/issues/184), [ADR 0012](adr/0012-a-session-follows-a-resize-with-a-new-file.md)). A **display mode** change from 2560x1440 to 1280x720 and back produced two access losses, two `SizeChanged` reports and 260 then 586 frames either side, with no error reaching the caller. What is unmeasured is a mode change during a *recording* |
| Alt-tab              | **Partly measured**  | Minimise, restore, occlusion and closure are each observed through `cargo run -p clipped-capture --example wgc_probe -- --mode lifecycle`, and restoring a minimised 1280x720 window is known to produce one bogus 160x28 `ContentSize` frame, told apart by asking `GetClientRect`. What is unmeasured is alt-tabbing out of an **exclusive fullscreen** subject, which loses the display rather than the window |

Reading down that column is the point of this section. **Nothing in this
repository has ever captured a DirectX 12, Vulkan or OpenGL application**, so
the per-game capture compatibility field SPEC.md section 42 wants fed has
nothing at all to say about the graphics API a game uses.

---

## 2. Encoders

Four families exist in `EncoderKind::ALL`. Three are implemented and one is not,
and `EncoderKind::is_implemented` says which in code rather than in prose, so
the sentence a user reads cannot outlive the backends.

| Encoder                | On this machine                                                                                | H.264                              | HEVC                                                             | AV1                                                                                                                                                              | Has it ever encoded a frame?                                                                       |
| ---------------------- | ---------------------------------------------------------------------------------------------- | ---------------------------------- | ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| **NVIDIA NVENC**       | Available, on the RTX 4090 — the adapter capture lands on                                      | Measured yes, 4096x4096            | Measured yes, 8192x8192                                          | Measured yes, 8192x8192                                                                                                                                          | **Yes.** Every end-to-end recording on this machine                                                 |
| **AMD AMF**            | *Present, and usable only if asked for by name* — it is on the adapter frames are **not** captured on | Measured yes, 4096x4096      | Measured yes, 8192x4352, and 300 fps at 1080p **measured** rather than inferred | **`unknown`** — this backend creates no AV1 component ([#165](https://github.com/wildware-uk/clipped/issues/165)), so AMF is never asked, and nothing said is not a no | **Yes, across adapters** — see below                                                                |
| **Intel Quick Sync**   | Unavailable: no adapter from this vendor is present                                            | Unknown                            | Unknown                                                          | Unknown                                                                                                                                                          | **No. Never.** [#160](https://github.com/wildware-uk/clipped/issues/160)                            |
| **Software (CPU)**     | Available; needs no adapter or driver                                                          | Inferred yes (libopenh264)         | `unknown`                                                        | `unknown`                                                                                                                                                        | **Yes**, and it is the encoder `crates/session/src/recording.rs`'s fallback tests are pinned to     |

**The AMF AV1 cell is the one worth stopping at.** It reads `unknown`, not
`no`. Windows registers `AMDh264Encoder` and `AMDh265Encoder` and no AV1
transform, so nothing was ever asked and nothing ever answered. Collapsing that
into a measured no is how a table starts lying, and `Claim` keeps the three
answers apart in the type rather than in a comment beside the value.

**Quality presets resolve differently per vendor for exactly that reason**, from
`clipped-recorder capabilities` on this machine
([#62](https://github.com/wildware-uk/clipped/issues/62),
[PR #653](https://github.com/wildware-uk/clipped/pull/653)):

| Preset        | NVENC | AMF  | Software |
| ------------- | ----- | ---- | -------- |
| `performance` | h264  | h264 | h264     |
| `balanced`    | av1   | hevc | h264     |
| `high`        | av1   | hevc | h264     |
| `ultra`       | av1   | hevc | h264     |

`ultra` asks NVENC for AV1 and AMF for HEVC because
`clipped_encoder::measured_codecs` filters to `is_measured_true()`: an `unknown`
never qualifies, so a preset can never ask an encoder for a codec nothing said
it has. A table keyed on the GPU model would have got that wrong.

### Two vendors in one machine

Capture creates its Direct3D device on the **default** adapter, and a vendor
encode runtime refuses another vendor's device. The failure is symmetrical, and
was measured with the vendor guards removed so the runtimes answered for
themselves ([PR #620](https://github.com/wildware-uk/clipped/pull/620)):

|           | On an NVIDIA device                                     | On an AMD device                                                       |
| --------- | ------------------------------------------------------- | ----------------------------------------------------------------------- |
| **AMF**   | `AMFContext::InitDX11 failed with AMF_INVALID_ARG (4)`  | Opened                                                                  |
| **NVENC** | Opened                                                  | `nvEncOpenEncodeSessionEx failed with NV_ENC_ERR_NO_ENCODE_DEVICE (1)`  |

So this is the general shape of "encoder and capture on different adapters",
not an AMF defect. A Direct3D 11 shared handle does not cross adapters either —
`OpenSharedResource1` answers `0x80070057` — so the way across is a copy through
system memory, which is what `crates/encoder/src/windows/bridge.rs` does when a
recording names an encoder and refuses a substitute.

**What that is worth, measured.** `record --encoder amf` reported
`536 frames of 1280x720 HEVC in 17.94s (AMD AMF, Windows Graphics Capture, 29.8 fps sustained; 0 frames dropped)`,
and `ffprobe -count_frames` decoded 536 — the recorder's count and the decoder's
count being two independent accounts of the same file. The same at 1080p (537)
and 4K (537). The carried cost is 2.97 ms a frame at 720p, 5.82 ms at 1080p and
7.84 ms at 4K ([encoder-pipeline.md](encoder-pipeline.md)).

**That figure is a recording somebody made, not a test that re-runs.** The three
`#[ignore]`d tests in `crates/encoder/src/windows/bridge.rs` guard the mechanism
with six frames of six solid colours in H.264, decoded and checked colour by
colour — not with 536 frames of HEVC. Reverting the fix fails them with the two
vendor errors above. Both are evidence; they are not the same evidence, and
citing the tests as the source of "536" would be wrong.

### Framerate ceilings, and one that is deliberately absent

NVENC's `NV_ENC_CAPS_MB_PER_SEC_MAX` reports 983,040 macroblocks a second — 121
frames a second at 1080p — and the same silicon measurably reached **1,034
frames a second at 720p** through this project's own backend. So the driver's
figure is not a ceiling and is not published as one; the codec level's bound is
published instead, marked `(i)` for inferred. AMF's `MaxThroughput` *is*
published, because it survives the same check: it reports 2.8 million
macroblocks a second for H.264 on the integrated Radeon here and the measurement
reaches 2.1 million, under the ceiling where a ceiling belongs
([encoder-capabilities.md](encoder-capabilities.md)).

---

## 3. Audio

| Question                                                             | Answer                                                                                                                                                                                                                                                                                                        | Traced to                                                                                                                                                     |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Does Windows really partition a machine's audio by process tree?      | **Yes.** Each track's own tone against the loudest tone that does not belong there: **1,884x** for the game, **1,887x** for the complement and **250x** for the microphone, against a documented rejection threshold of **8**                                                                                   | `tests/audio/track_isolation.rs::each_track_holds_the_tone_of_the_source_it_belongs_to_and_not_the_other_ones`, [PR #645](https://github.com/wildware-uk/clipped/pull/645) |
| Is the microphone leg measured?                                       | **Yes, through a virtual audio device** and never a real microphone. The device is found by keeping only root-enumerated endpoints and then proving the pair with a tone, and the run calibrates the cable's gain before it measures anything                                                                   | [#34](https://github.com/wildware-uk/clipped/issues/34), [PR #645](https://github.com/wildware-uk/clipped/pull/645)                                            |
| On a machine with no virtual audio device?                            | It **skips, loudly**, printing what it looked at and what installing one would give. `CLIPPED_REQUIRE_AUDIO` turns that skip into a failure                                                                                                                                                                    | `tests/audio/track_isolation.rs`                                                                                                                              |
| What does a process-scoped tap cost when its stream set changes?      | **1,504 frames — 31.33 ms — of exact digital zeros**, inside ordinary packets whose flags are `0`. Every application that starts *or stops* playing costs the other-system track that much, for the whole of every recording                                                                                    | [#626](https://github.com/wildware-uk/clipped/issues/626), `test-apps/process-tree-audio/tests/mid_recording_joiner.rs`                                        |
| Can Clipped avoid it?                                                 | **No.** Polling, a 10 ms buffer, a 1,000 ms buffer, `AUTOCONVERTPCM`, `SRC_DEFAULT_QUALITY` and a release build all produced the same 1,504 frames; `NOPERSIST`, `RATEADJUST` and `CROSSPROCESS` are refused outright with `AUDCLNT_E_INVALID_STREAM_FLAG`. Two taps activated separately against one tree are **sample-identical**, so there is no second copy to splice over the first | [PR #630](https://github.com/wildware-uk/clipped/pull/630)                                                                                                     |
| So what happens instead?                                              | It is **counted** — `CaptureStats::unflagged_dropouts` and `unflagged_dropout_frames`, the number of runs and the audio they held. The defect itself stays open                                                                                                                                                | [PR #634](https://github.com/wildware-uk/clipped/pull/634)                                                                                                     |
| Is a whole-endpoint tap affected?                                     | **No.** An ordinary loopback capture watched across the same join and leave produced no run of zeros longer than a millisecond. This is process-scoped taps specifically                                                                                                                                        | [PR #630](https://github.com/wildware-uk/clipped/pull/630)                                                                                                     |
| What happens where process scoping is unavailable?                    | One track called **`System Audio`** holding everything the machine played, rather than a failed recording — and never called `Game` or `Other System Audio`, neither of which would be true of it. A failure this build cannot classify still refuses the recording                                             | [#604](https://github.com/wildware-uk/clipped/issues/604), `tests/audio/system_audio_fallback.rs::a_machine_that_cannot_scope_audio_records_one_track_of_everything_it_played` |
| Which Windows build does scoping need?                                | Build **20348** or later, per the sentence the code gives a user. **The floor itself is unconfirmed on real hardware** — everything here has only run on 26200, where it works, which is why [prerequisites.md](prerequisites.md) states no minimum for it. Tests reach the fallback with `CLIPPED_FORCE_AUDIO_SCOPING_FAILURE` instead | [audio-routing.md](audio-routing.md)                                                                                                                          |
| A/V offset, and drift over an hour                                    | Measured on the endpoint named above, including a 60-minute run                                                                                                                                                                                                                                                | `tests/capture/av_sync.rs::av_offset_stays_within_tolerance_while_video_and_audio_are_captured_together`, `tests/capture/av_sync.rs::the_absolute_av_offset_of_a_synchronised_subject_is_within_tolerance`, [av-sync.md](av-sync.md) |
| A **real** microphone, a **real** game, and a person listening        | **Unknown, deliberately.** The automated tests prove routing; whether a headset in a room sounds right is a claim about the world. [testing.md](testing.md) carries the manual procedure                                                                                                                        | [testing.md](testing.md)                                                                                                                                      |

**One audio endpoint, one machine, one Windows build.** Every figure above is
Windows 11 Pro build 26200 with the endpoint named in
[The machine](#the-machine-every-measured-row-was-measured-on). A build or a
device on which the 31.33 ms hole does not happen would be worth knowing about,
and nobody has looked.

---

## 4. Playback, and what an editor will open

| Where                        | What it does                                                                                                                                                                                                                                                            | Traced to                                                                                                       |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| The Clipped window (WebView2) | **Plays a recording as it is** — Matroska, AV1 picture, `pcm_s16le` sound, with no transcode. Measured by loading each file into a `<video>` element and reading `webkitVideoDecodedByteCount` and `webkitAudioDecodedByteCount`, so the reading is bytes through the decoders rather than bytes fetched | [ADR 0011](adr/0011-what-the-webview-plays.md)                                                                  |
| Seeking in that window        | **Exact, not keyframe-quantised.** Assigning to `currentTime` decodes forward from the keyframe to the frame asked for; `fastSeek` is the keyframe-only one. Exact to about a microsecond, which at 60 fps is a five-hundredth of a frame                                  | [ADR 0011](adr/0011-what-the-webview-plays.md), correction dated 2026-08-19, [PR #654](https://github.com/wildware-uk/clipped/pull/654) |
| Track selection in that window | The element always plays the **first** declared sound track: `audioTracks` is unimplemented in Chromium, and the Matroska default-track flag is ignored                                                                                                                   | [ADR 0011](adr/0011-what-the-webview-plays.md)                                                                  |
| Adobe Premiere Pro 2025       | **No, by its own importer's registration.** `ImporterFFMPEG.prm` registers MJPEG, H.264 and Matroska and nothing else — zero AV1 decoder markers, zero `pcm_` markers. So no combination Clipped produces by default is one Premiere advertises                            | [#602](https://github.com/wildware-uk/clipped/issues/602)                                                       |
| DaVinci Resolve               | **Unknown.** Not installed on this machine. Its Matroska and AV1 support differ from Premiere's and have to be measured rather than inferred from it                                                                                                                      | [#602](https://github.com/wildware-uk/clipped/issues/602)                                                       |

Two cautions on the Premiere row, because a confident wrong answer there is
worse than none. It is a **static probe of plugin binaries, not an import**:
absent strings are strong evidence and present strings are weak evidence, and
the decisive test is importing a real recording and reading what the project
panel says. And **nothing in this repository measures either row** — no test
runs a browser and no test runs an editor. The WebView measurement is a script
attached to its issue that takes a minute; the importer probe is a one-time
reading recorded on [#602](https://github.com/wildware-uk/clipped/issues/602).

### Three claims that were written down and turned out false

Kept here because the pattern matters more than any one of them does.

| The claim                                                        | Where it was asserted                                                           | What measuring found                                                                                                                                                                                                                              |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A WebView seek is keyframe-quantised                             | [ADR 0011](adr/0011-what-the-webview-plays.md), in its own original text          | Wrong. `currentTime` is frame-exact. Corrected in place, and dated, by [PR #654](https://github.com/wildware-uk/clipped/pull/654)                                                                                                                  |
| No browser decodes PCM, so playback needs an audio encoder       | [#392](https://github.com/wildware-uk/clipped/issues/392)                        | Wrong. Chromium's bundled FFmpeg demuxer plays Matroska with PCM through `src=`; `canPlayType` says otherwise and is the API that had been checked. Nothing in Clipped needs an audio encoder to play or to share a recording                       |
| AVC patent expiry is the thing to watch                          | [ADR 0008](adr/0008-codec-patent-position.md), an earlier revision                | Misleading. AVC is third on the efficiency order and is reached only by a machine with neither AV1 nor HEVC; what the record is mostly about is HEVC, a 2013 standard with decades to run. Demoted, with the reason and the date, in the ADR itself |

Each of the three was written down by somebody reasonable, from documentation
rather than from a run. That is the failure this page exists to make visible,
which is why the **unknown** cells above name what would settle them instead of
guessing.

---

## Every failure on this page, and where it is tracked

Acceptance criterion two of
[#96](https://github.com/wildware-uk/clipped/issues/96) is that each failure has
a linked issue or a documented mitigation. This is that list.

| Failure                                                          | Issue                                                                                                              | Mitigation, if any                                                                                                                                    |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Quick Sync has never encoded a frame                             | [#160](https://github.com/wildware-uk/clipped/issues/160)                                                            | `is_implemented` returns `false`, so it is never chosen and never offered; a machine whose best encoder is Quick Sync encodes on the CPU instead       |
| Desktop Duplication refuses a rotated display                    | [#138](https://github.com/wildware-uk/clipped/issues/138)                                                            | Windows Graphics Capture composes the rotation and is preferred anyway, so the refusal only bites when the fallback is the only option                 |
| A process-scoped tap loses 31.33 ms on every stream-set change   | [#626](https://github.com/wildware-uk/clipped/issues/626)                                                            | None is possible on the client side of WASAPI. It is counted rather than hidden                                                                       |
| Premiere does not open AV1 in Matroska                           | [#602](https://github.com/wildware-uk/clipped/issues/602)                                                            | None yet. A container setting is [#307](https://github.com/wildware-uk/clipped/issues/307)                                                            |
| DaVinci Resolve is entirely unmeasured                           | [#602](https://github.com/wildware-uk/clipped/issues/602)                                                            | None. Nobody working on this has the install                                                                                                          |
| Neither backend captures HDR                                     | [#99](https://github.com/wildware-uk/clipped/issues/99), [#146](https://github.com/wildware-uk/clipped/issues/146)   | Refused by name rather than recorded wrongly                                                                                                          |
| Exclusive fullscreen loses about one frame in ten                | [#192](https://github.com/wildware-uk/clipped/issues/192)                                                            | None. The cause is not known and the readback cost has been ruled out                                                                                 |
| Desktop Duplication stops on a powered-off display               | [#461](https://github.com/wildware-uk/clipped/issues/461)                                                            | The display is held awake for the length of a recording, [ADR 0015](adr/0015-capture-holds-the-display-awake.md)                                       |
| Multi-monitor, ultrawide and display changes are unmeasured      | [#98](https://github.com/wildware-uk/clipped/issues/98)                                                              | None. Deliberately not done unattended, because it means changing somebody's display settings                                                         |
| No DirectX 12, Vulkan or OpenGL subject exists                   | [#96](https://github.com/wildware-uk/clipped/issues/96)                                                              | None. A `video-pattern` sibling per graphics API is the work                                                                                          |
| Neither backend has captured a window in CI, ever                | [#96](https://github.com/wildware-uk/clipped/issues/96)                                                              | None available: a hosted runner has no compositor. Every capture reading here is somebody's manual run                                                 |
| A real broken capture cannot be provoked on demand               | [#97](https://github.com/wildware-uk/clipped/issues/97), [#285](https://github.com/wildware-uk/clipped/issues/285)   | Covered against a scripted backend in `crates/session/src/recording.rs`, which is not the same claim                                                   |
| The Windows floor for process-scoped audio is unconfirmed        | [#604](https://github.com/wildware-uk/clipped/issues/604)                                                            | The fallback is exercised by forcing it, and no minimum version is published until somebody has a machine below the floor                              |

---

## Filling in a column from your own machine

The point of a matrix is that somebody with different hardware fills in a column
rather than starting again. Three of the four tables can be re-run.

**1. The encoder table answers itself.** No hand-editing at all:

```text
cargo run -p clipped-recorder --release -- capabilities --refresh
```

That prints the adapters, each encoder's availability with the evidence for it
line by line, the per-codec table with `(i)` marking anything inferred rather
than measured, what `Automatic` would choose, and what each quality preset
resolves to on each encoder. Without `--refresh` it opens no encoder session, so
the size and framerate cells stay inferred. **It is deliberately not committed
as a generated file**: a generated table sitting beside prose becomes a second
source of truth and drifts from it. Run it, read it, and put it on the issue —
do not paste it into this page.

**2. The capture table needs the hardware tests.** All of them are `#[ignore]`d,
because they need a GPU, a desktop session, a compositor and, in one case, a
whole display — and because a test that decides for itself it could not run
reads as a pass:

```text
cargo build -p clipped-video-pattern
$env:CLIPPED_REQUIRE_CAPTURE = "1"
cargo test -p clipped-video-pattern    --test wgc_video_pattern          -- --ignored --nocapture --test-threads=1
cargo test -p clipped-fullscreen-dx11  --test wgc_fullscreen_dx11        -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test odd_client_area            -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test screenshot                 -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test screenshot_fullscreen      -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test recorded_frames             -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test soak                       -- --ignored --nocapture
cargo test -p clipped-video-pattern    --test screenshot_during_recording -- --ignored --nocapture
cargo test -p clipped-capture -- --ignored --nocapture --test-threads=1
```

**3. The audio table needs an output endpoint, and makes a noise.** The
microphone leg additionally needs a virtual audio device — one whose speaker end
reappears on its microphone end — and skips loudly without one. No real
microphone is ever opened:

```text
$env:CLIPPED_REQUIRE_AUDIO = "1"
cargo test -p clipped-video-pattern      --test track_isolation       -- --ignored --nocapture
cargo test -p clipped-video-pattern      --test system_audio_fallback -- --ignored --nocapture
cargo test -p clipped-video-pattern      --test av_sync               -- --ignored --nocapture --test-threads=1
cargo test -p clipped-process-tree-audio --test mid_recording_joiner  -- --ignored --nocapture
```

**4. The playback table cannot be re-run from here.** No test in this repository
runs a browser or an editor. [ADR 0011](adr/0011-what-the-webview-plays.md)
carries the WebView method and
[#602](https://github.com/wildware-uk/clipped/issues/602) carries the importer
probe; both are a person's procedure, and both say so.

`--nocapture` is not optional on any of these. The frame accounting, the track
levels and the hole lengths are printed rather than merely asserted, and those
numbers are what a row is made of — a green tick is not.
[testing.md](testing.md) is the full account of what each command needs, which
environment variable turns a skip into a failure, and how to clear up after a
run without stopping somebody else's.

### Reporting what you measured

Put the printed output on the issue for the row it changes, with the machine
described the way
[The machine](#the-machine-every-measured-row-was-measured-on) describes this
one: operating system build, adapters and drivers, displays, audio endpoint. A
reading without its hardware cannot be compared with the one already here, and
AGENTS.md section 19 asks for the same list.

---

## What keeps this page honest

Two guards, and neither of them is a promise to remember.

**`cited_tests_exist.rs` already reads this file.** It scans every `.md` in the
repository for anything shaped like a path to a test file — a suite directory at
the root, or a crate's own `tests` directory — optionally followed by a double
colon and a test name, and fails if the file is not there or if the file does
not contain that function. So every test named above is checked to
exist, resolved from the repository root, on every `cargo test --workspace` —
including in CI, which cannot run a single one of the tests themselves.

**`compatibility_matrix_is_complete.rs` derives its list from the source.** It
reads the capture method labels out of `impl fmt::Display for CaptureMethod`,
the encoder labels out of `impl fmt::Display for EncoderKind`, and the system
test files out of `tests/capture/` and `tests/audio/` — keeping only those that
contain a `#[test]`, so a helper like `readback.rs` is not mistaken for one —
and fails if any of them has no row here. A new capture backend, a new encoder
family or a new system test therefore cannot land without a row on this page.
That is the shape `process_table_reads.rs`, `disk_space_reads.rs`,
`foreground_rules.rs` and `settings_reach_the_running_recorder.rs` already use,
and it is the only mechanism that has kept a document in this repository alive.

What neither guard can do is notice that a row has gone **stale** — that
something measured on 2026-08-19 quietly stopped being true. Nothing can. What
this page does instead is make every row cheap to re-run, and say plainly which
rows nobody has run at all.
