# Muxing

`clipped-muxer` turns encoded packets into a file. This page is what it
guarantees, how timestamps are handled, and what happens when a recording is
interrupted — which is the reason the container is what it is.

The decision behind all of it is [ADR 0001](adr/0001-mkv-archival-container.md):
recordings are written into **Matroska**, incrementally, because a recording is
irreplaceable and the container is chosen for what happens when things go wrong.
[ADR 0004](adr/0004-ffmpeg-dependency-strategy.md) covers the FFmpeg dependency
that implements it, and [ffmpeg.md](ffmpeg.md) how to build against it.

## What exists today

- `MkvWriter` — creates an MKV, writes packets as they arrive, finishes the
  file.
- One video track and any number of named audio tracks, with language tags and
  a default-track flag.
- H.264, HEVC and AV1 video; PCM, AAC and Opus audio.

What does not exist: remuxing to MP4
([issue #92](https://github.com/wildware-uk/clipped/issues/92)) and the replay
buffer's segment writing. Nothing in the recorder is wired to the muxer yet —
capture, encoding and the session that joins them are separate M1 issues — so
the writer is exercised by its own tests rather than by `clipped-recorder`.

## Writing a recording

```rust
use std::path::Path;
use clipped_muxer::{
    AudioCodec, AudioTrack, EncodedPacket, FrameRate, MkvWriter, PacketTimestamp,
    RecordingLayout, TrackId, VideoCodec, VideoTrack,
};

let layout = RecordingLayout::new(
    VideoTrack::new(VideoCodec::H264, 2560, 1440)
        .with_codec_private(sequence_header)      // SPS and PPS from the encoder
        .with_frame_rate(FrameRate::per_second(60).unwrap()),
)
.with_audio_track(
    AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2)
        .with_name("Compatibility Mix")
        .as_default(),
)
.with_audio_track(AudioTrack::new(AudioCodec::PcmS16Le, 48_000, 2).with_name("Game"));

let mut writer = MkvWriter::create(Path::new("recording.mkv"), &layout)?;
writer.write_packet(
    &EncodedPacket::new(TrackId::Video, PacketTimestamp::from_nanos(t), &frame)
        .with_keyframe(true)
        .with_duration(frame_interval),
)?;
let summary = writer.finish()?;
```

Three things about the shape of that.

**The track layout is fixed when the file is created.** Matroska writes its
track entries into the header, so a track cannot appear halfway through a file.
A routing change mid-session is therefore a new recording, not a new track, and
that decision belongs to `clipped-session`.

**Audio is a list, not three fields.** How many audio tracks a recording has is
decided at run time from the user's routing (SPEC.md sections 11 and 44).
Multi-track routing itself is
[issue #28](https://github.com/wildware-uk/clipped/issues/28); nothing here
assumes the list has one entry, and the tests write three.

**Every track carries its name and language.** That is most of why the
container is Matroska: an editor opening the file sees `Microphone` rather than
`Audio 3`, with no sidecar. The first audio track is normally the compatibility
mix (SPEC.md section 13), and `as_default()` marks it as the one a player should
select on its own.

**Codec headers are required, and come from the encoder.** Matroska's track
entry carries a mandatory `CodecPrivate` for H.264, HEVC, AV1, AAC and Opus —
the parameter sets or the sequence header — so `create` refuses a track without
one rather than letting FFmpeg fail later with `Invalid data found when
processing input`. Uncompressed PCM needs none. The bytes are whatever the
encoder produced: Annex B parameter sets from a Windows hardware encoder are
converted to the `avcC`/`hvcC` record Matroska stores, and so are the packets,
by FFmpeg's Matroska muxer.

## Timestamps

This is where muxers go wrong, so it is written down.

**The unit callers work in.** Nanoseconds, signed, on one monotonic clock shared
by every track in a recording (`PacketTimestamp`). On Windows that clock is the
performance counter, which is what `clipped-capture` stamps frames with and what
Windows audio capture reports positions against, so audio and video are
comparable without a conversion nobody can check.

**The unit the file uses.** FFmpeg's Matroska muxer fixes every stream's time
base at 1/1000 while writing the header — a millisecond. The writer reads that
value back after `avformat_write_header` rather than assuming it, and rescales
into it, rounding to nearest and away from zero on a tie, which is what
`av_rescale_q` does. At 60 fps the error is at most half a millisecond, and it
does not accumulate: every timestamp is converted from the source clock, never
from its predecessor.

**The zero point.** The first packet written establishes the file's origin, and
every timestamp is stored relative to it. That is what lets a caller pass raw
performance-counter readings — nanoseconds since the machine booted — without
the recording starting several years in. A caller that has already rebased its
timestamps onto the recording's own epoch — `clipped_capture::MediaTime`, which
is what [av-sync.md](av-sync.md) says every source is converted to — passes those
instead, and the writer's origin then coincides with the recording's.

Which clock those nanoseconds count on, how each source is put on it, and what
is done when two sources disagree, is [av-sync.md](av-sync.md). The muxer takes
that as given: all it can do with two timestamps is subtract them.

**Monotonicity is enforced on decode timestamps, per track.** A container
requires that a track's decode timestamps increase; it does not require that its
presentation timestamps do, because a codec with B-frames produces packets in
decode order whose presentation order differs. So `EncodedPacket` carries both,
reordered presentation timestamps are left alone, and the writer corrects:

| What arrived | What is written | Why |
| --- | --- | --- |
| A decode timestamp at or before the previous one on that track | One tick past its predecessor | FFmpeg refuses the packet outright — `av_interleaved_write_frame` returns `EINVAL` — so writing it unchanged would end the recording. Dropping it would lose picture. |
| A timestamp before the start of the file | Clamped to the start | Happens when tracks begin at slightly different moments and the later-starting one is written first. It is a last resort rather than a policy: audio that genuinely precedes the recording is meant to have been trimmed at the epoch before it ever reaches a writer ([av-sync.md](av-sync.md)), because clamping stacks every such packet on the first instant of the file. A summary reporting many of these means that trim was not applied upstream. |
| A presentation timestamp before its own decode timestamp | Raised to match | Only reachable after one of the corrections above; no decoder can present a frame it has not decoded. |

Nothing is ever dropped and no correction is silent. Each kind is counted in
`RecordingSummary`, and the first of each kind is logged at `warn` — once, not
per packet, because the pathological case is thousands in a row and a recorder
that spends a session writing a log line per frame has become the problem it was
reporting. A summary reporting corrections in the thousands means something
upstream is reordering packets or a clock has stepped; that is a bug to find,
not a number to tolerate.

Above about 1000 frames per second two frames round onto the same millisecond
and the second is forced forward by a tick. No capture path in Clipped produces
frames that fast.

## Interruption: what a killed recording costs

The claim ADR 0001 rests on is that a recording killed mid-write remains a
playable recording of everything up to the kill. That is a thing to test, not to
assume, so it is tested: `crates/muxer/tests/abrupt_termination.rs` starts a real
recorder process, lets it write three seconds in real time, ends it with
`TerminateProcess` — no destructors, no flush, no notification — and takes the
survivor apart with `ffprobe`.

**What survives.** The header, with every track's codec, name and language, and
every Matroska cluster that was closed before the kill. A cluster does not reach
the disk until it closes, so the survivor ends on a cluster boundary rather than
in the middle of one. Measured on the development machine: the recorder reported
writing 3.020 seconds and was killed, with keyframes five seconds apart, and the
file left behind holds two closed clusters — 61 video packets reaching 2.000
seconds and 201 audio packets reaching 2.020 — with every one of the 61 frames
decoding.

**What is lost.** Whatever was in the cluster still being accumulated, and the
trailer. A file without a trailer has no segment length, no duration and no cue
index; it opens anyway, because the segment reads as unknown-length, which is
the same construct a live stream is written with. `ffprobe` reports the
truncation on standard error while reading everything in front of it. It is not
seekable by index, and a library reading its duration has to scan it.

That claim is tested against **FFmpeg's demuxer and no other**: the pinned
build's `ffprobe` is the only tool any test here runs. VLC, mpv and MPC-HC all
demux Matroska through their own readers and none of them has been tried, so
this document does not say what they do with a trailer-less file. Anything that
reads media through libavformat — which is most things, including `ffmpeg`
itself and the editor Clipped will ship — reads it.

**Two settings do that work**, and both are chosen for this rather than
inherited:

- `cluster_time_limit=1000` bounds how much media a cluster may accumulate.
- `AVFMT_FLAG_FLUSH_PACKETS` makes libavformat flush its own I/O buffer to the
  operating system after every packet, so a cluster that has been emitted is one
  that has reached the file rather than one sitting in a buffer the killed
  process takes with it.

### Why the cluster time limit, and what FFmpeg does without it

FFmpeg's Matroska muxer closes a cluster at the first of three things: a
keyframe on the video track, `cluster_size_limit` bytes, or `cluster_time_limit`
milliseconds of media. Both limits read `-1` in the option table and are
replaced with 5 MB and 5000 ms as the header is written. So the default window
is **not** the keyframe interval alone, and a recording left to the defaults
does not lose an unbounded amount — it loses at most five seconds.

Measured on the pinned build by counting Cluster elements in a 30-second file at
30 fps with keyframes deliberately 10 seconds apart:

| `cluster_time_limit` | Clusters | Starting at (ms) |
| --- | --- | --- |
| FFmpeg's default | 6 | 0, 5033, 10000, 15033, 20000, 25033 |
| `1000` | 30 | 0, 1033, 2067, 3100, … one a second |

The default file closes a cluster every five seconds, and at the two keyframes
(10 s and 20 s) as well. Raise the bitrate until five seconds no longer fits in
5 MB and the size limit takes over instead: the same 30 seconds at 40 Mbit/s and
720p is 9 clusters roughly four seconds apart, about 5 MB each.

So the trade `cluster_time_limit=1000` actually buys is **five seconds of loss
down to one**, and a loss that no longer moves when somebody changes the
encoder's keyframe interval. The cost is a cluster header — a few dozen bytes —
every second rather than every five, which against a recording running at tens
of megabits does not register.

Below five seconds the keyframe interval is what decides the default, which is
why the difference is stark for a recorder killed early. With keyframes five
seconds apart and the process killed 3.02 seconds in, the same run gives:

| `cluster_time_limit` | What survived |
| --- | --- |
| FFmpeg's default | 823 bytes. `ffprobe` reports `End of file` and finds no streams at all: no cluster of any kind had closed. |
| `1000` | 61 video packets to 2.000 seconds and 201 audio packets to 2.020, every frame decoding. |

Reproduce it with:

```text
cargo build -p clipped-muxer --examples
target\debug\examples\synthetic_recording.exe --output kill.mkv --seconds 600 --pace --keyframe-seconds 5
# kill it after a few seconds, then:
third-party\ffmpeg\current\bin\ffprobe.exe -v error -count_packets -count_frames ^
    -show_entries stream=index,codec_type,nb_read_packets,nb_read_frames -of csv kill.mkv
```

and the cluster counts with the pinned `ffmpeg` directly, which needs none of
this workspace:

```powershell
$ffmpeg = "third-party\ffmpeg\current\bin\ffmpeg.exe"
& $ffmpeg -v error -f lavfi -i testsrc2=size=320x240:rate=30 -t 30 -c:v libopenh264 -g 300 `
    -y default.mkv
& $ffmpeg -v error -f lavfi -i testsrc2=size=320x240:rate=30 -t 30 -c:v libopenh264 -g 300 `
    -cluster_time_limit 1000 -y limited.mkv

# Matroska's Cluster element id is 1F 43 B6 75, and nothing in the FFmpeg tools
# prints element structure, so count the ids:
foreach ($file in "default.mkv", "limited.mkv") {
    # Resolved because .NET does not share PowerShell's working directory.
    $bytes = [IO.File]::ReadAllBytes((Resolve-Path $file))
    $count = 0
    for ($i = 0; $i -lt $bytes.Length - 3; $i++) {
        if ($bytes[$i] -eq 0x1F -and $bytes[$i + 1] -eq 0x43 -and
            $bytes[$i + 2] -eq 0xB6 -and $bytes[$i + 3] -eq 0x75) { $count++ }
    }
    Write-Output "$file : $count clusters"
}
```

That scan counts the identifier wherever it appears rather than parsing the
segment, so it could in principle over-count by matching four bytes inside a
block. It does not on these files: it gives 6, 30 and 9, the same numbers a
proper walk of the segment's elements gives, which is also where the cluster
start timestamps in the table came from.

`crates/muxer/tests/abrupt_termination.rs` asserts the loss stays under 1.1
seconds: a second of cluster, plus the packet past the limit that closes it,
plus the one the recorder may have written but not yet printed. Sampling eight
kill points spread across a cluster, the worst loss measured on the development
machine was 1.02 seconds. That constant is the promise; loosening it is a change
to what Clipped guarantees, not a way to quieten a test.

**What is not covered.** Nothing here defends against the operating system
losing writes it acknowledged — a power cut can still take the last cluster,
because the muxer does not call `fsync` per cluster and would pay for it in a
game if it did. And a disk that fills mid-session ends the recording with a
`MuxError::Ffmpeg` carrying `ENOSPC`; the file up to that point is intact and is
finished normally.

## Resource ownership

One `AVFormatContext`, one `AVIOContext` and one `AVPacket` per writer, each
owned by a Rust value whose `Drop` releases it (AGENTS.md section 58). The
writer is `Send` and not `Sync`: a session may create it on one thread and run it
on the muxing thread, and `&mut self` on every operation enforces libavformat's
"one context, one thread at a time".

Dropping a writer without calling `finish` still writes the trailer, because a
session that fails or panics still drops its writer and a finalised recording is
strictly better for the person who made it than one without an index. `finish`
exists as well because only it can return the summary and report a failure.

Two behaviours protect what is already on disk:

- **An existing file is never opened.** `create` refuses with
  `MuxError::OutputExists`. Every mode that would let it write also truncates,
  and truncating is how a recorder destroys footage nobody can get back
  (AGENTS.md section 56).
- **A packet larger than a container block, or with no bytes at all, is
  refused** rather than silently truncated into FFmpeg's 32-bit size field.

## Validating output

Media is validated by inspecting it, never by the absence of an error
(AGENTS.md section 22). The inspecting is
[`clipped-media-validation`](../tests/media), the workspace's media harness,
which every crate that writes a file uses and which is itself tested against
truncated files, files with a track missing and files whose timestamps go
backwards ([docs/testing.md](testing.md#validating-produced-media)). This crate
had its own `ffprobe` wrapper until issue #24; it does not any more, and a
second one should not appear here.

The harness runs `ffprobe` **from the pinned FFmpeg build** in
`third-party/ffmpeg/current/bin` before falling back to `PATH`: the pinned build
is fetched by `scripts/fetch-ffmpeg.ps1` on every machine that builds this
workspace, including CI, so these tests run everywhere the crate compiles
instead of passing, failing or skipping depending on what somebody happened to
install.

| Test | What it proves |
| --- | --- |
| `tests/mkv_writing.rs` | Track names, languages, default flags, channel counts and codec metadata land in the file; packets go to the track they were addressed to; out-of-order and unrebased timestamps come out monotonic and from zero; an existing file is never overwritten. |
| `tests/synthetic_recording.rs` | A whole recording — real H.264 and two PCM tracks — opens, decodes frame by frame, has a plausible duration, carries the right codec metadata, and has tracks that start and end together. |
| `tests/abrupt_termination.rs` | A killed recorder leaves a playable file, reaching to within the bound above. |
| `tests/ffmpeg_linkage.rs` | The FFmpeg actually loaded is the pinned, LGPL-only build and contains what the pipeline needs. |

The packets come from `crates/muxer/examples/synthetic_recording.rs`, which
encodes a moving test pattern with the pinned build's own software encoder
(`libopenh264`) and generates tones as PCM. It is an example rather than a test
helper because two of those tests need it as a *process*: a Rust panic unwinds
and runs destructors, and destructors are exactly what a killed recorder does not
get.

```text
cargo test -p clipped-muxer
cargo run -p clipped-muxer --example synthetic_recording -- --output demo.mkv --seconds 5
```

## What has not been exercised

Stated rather than left to be discovered (AGENTS.md section 54):

- **Only H.264 video and PCM audio have been written end to end.** HEVC, AV1,
  AAC and Opus are mapped and declared, and the container is told about them
  correctly, but no encoder in this workspace produces them yet, so no file has
  been made in them.
- **No packet has come from a hardware encoder.** NVENC is
  [issue #15](https://github.com/wildware-uk/clipped/issues/15) and is not on
  this branch. The Annex B handling is what a Windows hardware encoder produces
  and is exercised by the software encoder, which produces the same form.
- **No B-frames.** Reordered presentation timestamps are handled and unit
  tested, but `libopenh264` does not reorder, so no file with B-frames has been
  written.
- **Sessions are minutes, not hours.** AGENTS.md section 59 asks for long-run
  testing; the longest recording written so far is a few seconds.
- **No colour signalling is written.** A 10-bit recording would not say what its
  colours mean, and a player would guess. Left out deliberately rather than
  forgotten — nothing here produces 10-bit output yet, so there is nothing to
  test it against — and tracked as
  [issue #146](https://github.com/wildware-uk/clipped/issues/146).
