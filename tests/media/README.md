# tests/media

`clipped-media-validation`: the harness every crate that writes media checks its
output with, so that "the encoder returned `Ok`" is never mistaken for "the
recording is valid" (AGENTS.md section 22).

| File | What it is |
| --- | --- |
| `src/probe.rs` | Opening a file with `ffprobe` and describing what came back — streams, packets, durations, tags |
| `src/expect.rs` | The expectations a test declares, and the report it gets when they are not met |
| `src/audio.rs` | What is audible on a track: dominant frequency, and whether a tone that belongs to another source is present |
| `src/tools.rs` | Finding the pinned build's `ffprobe` and `ffmpeg`, and skipping cleanly when there is none |
| `tests/valid_media.rs` | The harness against media that is genuinely valid |
| `tests/invalid_media.rs` | The harness against media that is broken, which is the half that matters |

[docs/testing.md](../../docs/testing.md#validating-produced-media) is the
document to read: what the harness asserts, how a test uses it, what it cannot
see yet, and why it inspects files with FFmpeg's own programs rather than with
the libraries `crates/muxer` links.

```text
cargo test -p clipped-media-validation
CLIPPED_REQUIRE_MEDIA=1 cargo test -p clipped-media-validation   # a skip becomes a failure
```
