# tests/audio

System tests that record known tone generators and assert that audio sources
which are meant to stay separate really are separate (AGENTS.md section 21).

| File | What it is |
| --- | --- |
| `track_isolation.rs` | Records `test-apps/video-pattern` playing one tone and this test process playing another, then measures every track of the resulting file by frequency: the game's track holds the game's tone and not the neighbour's, the complement's track holds the neighbour's and not the game's, and the compatibility mix holds both ([issue #34](https://github.com/wildware-uk/clipped/issues/34)) |

The test belongs to the package that owns the application it starts — Cargo only
sets `CARGO_BIN_EXE_…` for a test in the binary's own package — so it is declared
as a `[[test]]` target in `test-apps/video-pattern/Cargo.toml` with its source
here, beside the other system tests.

## Running it

It needs a GPU, a desktop session, an encoder and an output endpoint, and it puts
a window on a display and plays two quiet tones for about four seconds. Like
every test in `tests/capture/`, it is `#[ignore]`d rather than left to decide for
itself that it could not run — a test that skips reads as a pass, and
[tests/capture/README.md](../capture/README.md) has the reasoning, including why
the displays have to be awake before a run means anything.

```powershell
$env:CLIPPED_REQUIRE_AUDIO = "1"
cargo test -p clipped-video-pattern --test track_isolation -- --ignored --nocapture
```

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
| `tests/audio/track_isolation.rs` | **Windows** really partitions the machine's audio: the include mode hands over one process tree and the exclude mode hands over everything else | A machine with a GPU, a display and an output endpoint |

The rejection threshold is `clipped_media_validation::Tone`'s default and is the
same in all three: a track's own tone must measure at least **eight times** — about
18 dB — whatever a tone belonging to another source measures on it. On the machine
this was written on the real recording measures about 1,900 times, so the
threshold is nowhere near being scraped past.

## The manual procedure, for what the automated test cannot cover

**The microphone.** A simulated microphone at a known frequency needs a capture
endpoint a test can feed, which means a virtual audio device somebody installed —
not something this repository can assume of a contributor's machine (AGENTS.md
section 25) — and opening the real microphone of whoever is running the tests
would record their room (section 14). So the recording made above has a game
track, a complement track and a compatibility mix, and no microphone track.

The microphone's isolation is checked by hand, and the procedure in
[docs/testing.md](../../docs/testing.md) covers it: record with a game, something
else playing, and a microphone connected; open the file in an editor that shows
tracks separately; solo each track in turn; then mute all of them and unmute the
compatibility mix. Record what you found on the milestone issue.
