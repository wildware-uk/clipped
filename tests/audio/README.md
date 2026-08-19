# tests/audio

System tests that record known tone generators and assert that audio sources
which are meant to stay separate really are separate (AGENTS.md section 21).

| File | What it is |
| --- | --- |
| `track_isolation.rs` | Records `test-apps/video-pattern` playing one tone, this test process playing another, and a **simulated microphone** playing a third into a virtual audio device, then measures every track of the resulting file by frequency: each of the three source tracks holds its own tone and not the others, and the compatibility mix holds all three ([issue #34](https://github.com/wildware-uk/clipped/issues/34)). It also measures where the tracks *end*, within a packet of the picture ([issue #320](https://github.com/wildware-uk/clipped/issues/320)) |
| `system_audio_fallback.rs` | The same window and the same two tones, recorded with process scoping **forced to fail**: the recording happens rather than failing, its one track holds *both* tones — everything the machine played — and that track is called `System Audio` and not `Game` or `Other System Audio`. In the same run, a failure this build cannot classify still refuses the recording ([issue #604](https://github.com/wildware-uk/clipped/issues/604)) |

The test belongs to the package that owns the application it starts — Cargo only
sets `CARGO_BIN_EXE_…` for a test in the binary's own package — so it is declared
as a `[[test]]` target in `test-apps/video-pattern/Cargo.toml` with its source
here, beside the other system tests.

## Running it

It needs a GPU, a desktop session, an encoder and an output endpoint, and it puts
a window on a display and plays two quiet tones for about four seconds — a third
goes into a virtual audio device and reaches no speaker. `track_isolation.rs`
also needs a virtual audio device for its microphone leg, and says what it looked
at when there is none. Like every test in `tests/capture/`, both are `#[ignore]`d
rather than left to decide for themselves that they could not run — a test that
skips reads as a pass, and
[tests/capture/README.md](../capture/README.md) has the reasoning, including why
the displays have to be awake before a run means anything.

```powershell
$env:CLIPPED_REQUIRE_AUDIO = "1"
cargo test -p clipped-video-pattern --test track_isolation -- --ignored --nocapture
cargo test -p clipped-video-pattern --test system_audio_fallback -- --ignored --nocapture
```

The second forces process-scoped capture to fail with
`CLIPPED_FORCE_AUDIO_SCOPING_FAILURE`, which `clipped_session::audio` reads. It
sets and clears the variable itself, and it is deliberately one `#[test]` making
two recordings rather than two tests: the variable is process-wide, and a
recording made while another test had it set would measure the wrong build and
pass.

`CLIPPED_REQUIRE_AUDIO` is not optional when the result is being recorded as
evidence. Without it, a machine with no output endpoint prints
`SKIPPED (audio): …` and passes, which is the right default for somebody who just
ran the suite and useless as a measurement. `CLIPPED_SKIP_AUDIO` asks for quiet
and skips; setting both fails, because there is no behaviour that satisfies them
both.

`--nocapture` is worth typing: the test prints what every track measured at both
frequencies, which is the evidence AGENTS.md section 53 asks to be recorded on
the issue.

## What is measured here, and what is measured elsewhere

Isolation is asserted in three places and they prove different things. Only this
directory's test involves Windows.

| Where | What it proves | Where it runs |
| --- | --- | --- |
| `crates/muxer/tests/multi_track_audio.rs` | The **writer** keeps five tracks apart, from synthesised samples | Anywhere |
| `crates/session/src/audio/tests.rs` | The **routing** declares a track per source and puts each source on its own, from scripted captures | Anywhere |
| `tests/audio/track_isolation.rs` | **Windows** really partitions the machine's audio: the include mode hands over one process tree, the exclude mode hands over everything else wherever it was rendered, and a capture endpoint hands over what is on it and nothing else | A machine with a GPU, a display, an output endpoint and a virtual audio device |
| `tests/audio/system_audio_fallback.rs` | What a machine that **cannot** partition it records instead, and that the track says so | The same, with the failure forced |

The rejection threshold is `clipped_media_validation::Tone`'s default and is the
same in all three: a track's own tone must measure at least **eight times** — about
18 dB — whatever a tone belonging to another source measures on it. On the machine
this was written on the real recording measures about 1,900 times for the game,
1,800 for the complement and 250 for the microphone, so the threshold is nowhere
near being scraped past.

## The microphone, and the device it needs

A simulated microphone needs a capture endpoint a test can feed, which means a
virtual audio device somebody installed — not something this repository can
assume of a contributor's machine (AGENTS.md section 25) — and opening the real
microphone of whoever is running the tests would record their room (section 14).
Both hold. What changed is that an installed device is a *presence* problem, and
`#[ignore]` plus `CLIPPED_REQUIRE_AUDIO` is what this repository has for those.

`test-apps/video-pattern/src/virtual_audio.rs` reads
`PKEY_Device_EnumeratorName` off every endpoint and keeps only the ones Windows
root-enumerated — a device installed by software rather than plugged into a bus —
so a headset, a webcam and a line-in jack are excluded before any audio client is
activated. `track_isolation.rs` then renders a tone into a candidate's output end
and listens for it on its input end, which is the only honest way to know two
endpoints are two ends of one cable and works whatever the device is called.
**There is no fallback to a real capture endpoint**: a machine whose only
microphone is somebody's own gets a skip naming what would make the test runnable,
and `CLIPPED_REQUIRE_AUDIO` turns that skip into a failure.
[docs/testing.md](../../docs/testing.md) has the detail and an example of the
skip.

## The manual procedure, for what no test can cover

A **real** game, a **real** microphone and a person listening — routing is what
the test above proves, and whether a headset in a room sounds right is a claim
about the world. The procedure is in
[docs/testing.md](../../docs/testing.md): record with a game, something else
playing, and a microphone connected; open the file in an editor that shows tracks
separately; solo each track in turn; then mute all of them and unmute the
compatibility mix. Record what you found on the milestone issue.
