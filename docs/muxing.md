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
- `AudioSource` — the track model: what each audio track is called, what order
  the tracks are in, and which one a player selects on its own. See
  [The audio tracks](#the-audio-tracks).
- `AudioTrackWriter` — the path from a capture's interleaved `f32` samples to
  packets on one of those tracks.
- H.264, HEVC and AV1 video; PCM, AAC and Opus audio.
- `remux_to_mp4` — copies a finished recording into MP4 without decoding it, and
  `Mp4Plan::inspect`, which says what that copy would cost before it is made.

The recorder writes video through this crate; **audio is not wired to a
recording session yet**, which is
[issue #180](https://github.com/wildware-uk/clipped/issues/180). What is here is
the container side of it: the tracks, their order and their metadata, and the
conversion from captured samples to packets, exercised by this crate's own tests
against real files. Nothing calls `remux_to_mp4` yet either: the setting that
offers it (SPEC.md section 15, "Allow automatic remux") and the retention policy
for the MKV it was made from belong to the session and library work, and are not
in this crate.

## Writing a recording

```rust
use std::path::Path;
use clipped_muxer::{
    AudioSource, AudioTrack, AudioTrackWriter, EncodedPacket, FrameRate, MkvWriter,
    PacketTimestamp, RecordingLayout, TrackId, VideoCodec, VideoTrack,
};

let layout = RecordingLayout::new(
    VideoTrack::new(VideoCodec::H264, 2560, 1440)
        .with_codec_private(sequence_header)      // SPS and PPS from the encoder
        .with_frame_rate(FrameRate::per_second(60).unwrap()),
)
.with_audio_track(AudioTrack::for_source(AudioSource::CompatibilityMix, 48_000, 2))
.with_audio_track(AudioTrack::for_source(AudioSource::Game, 48_000, 2))
.with_audio_track(AudioTrack::for_source(AudioSource::Microphone, 48_000, 1));

let mut writer = MkvWriter::create(Path::new("recording.mkv"), &layout)?;
writer.write_packet(
    &EncodedPacket::new(TrackId::Video, PacketTimestamp::from_nanos(t), &frame)
        .with_keyframe(true)
        .with_duration(frame_interval),
)?;

// Audio arrives as samples rather than as coded packets; see below.
let microphone = layout.audio_track_for(&AudioSource::Microphone).unwrap();
let mut track = AudioTrackWriter::new(microphone, &layout.audio_tracks()[3])?;
track.write_samples(&mut writer, PacketTimestamp::from_nanos(t), &samples)?;

let summary = writer.finish()?;
```

Three things about the shape of that.

**The track layout is fixed when the file is created.** Matroska writes its
track entries into the header, so a track cannot appear halfway through a file.
A routing change mid-session is therefore a new recording, not a new track, and
that decision belongs to `clipped-session`.

**Audio is a list, not three fields.** How many audio tracks a recording has is
decided at run time from the user's routing (SPEC.md sections 11 and 44).
Nothing here assumes the list has one entry.

**Every track carries its name and language.** That is most of why the
container is Matroska: an editor opening the file sees `Microphone` rather than
`Audio 3`, with no sidecar.

## The audio tracks

Multi-track audio is the product (SPEC.md section 11), and the rule the design
answers to is AGENTS.md section 21: **sources the user expects to stay separate
are never silently combined**. This section is what that means in a file.

### What the tracks are called, and what order they come in

`AudioSource` is the closed set of things a track can be, and it — not the
caller — supplies the name written into the container, so that two recordings
made on two machines with two sets of settings call the microphone track the same
thing. `AudioTrack::for_source` describes a track from one, and
`RecordingLayout::with_audio_track` puts it in the position the model gives it,
whatever order the tracks are declared in:

| Order | Source | Name in the file | Default track |
| --- | --- | --- | --- |
| 1 | `CompatibilityMix` | `Compatibility Mix` | yes |
| 2 | `Game` | `Game` | no |
| 3 | `OtherSystemAudio` | `Other System Audio` | no |
| 4 | `Microphone` | `Microphone` | no |
| 5 | `VoiceChat` | `Voice Chat` | no |
| 6+ | `Application { name }` | the application's name, as the user knows it | no |

Several application tracks keep the order they were configured in, because
nothing else distinguishes them and reordering them because somebody edited a
name would move tracks in a file.

The order is fixed here rather than left to the caller because the file is what
has to be predictable. A container fixes its track list in the header, an editor
project refers to tracks by index, and a recording whose microphone is track 3 on
one machine and track 2 on another is a recording nobody can write an instruction
about. A caller addresses its packets with
`RecordingLayout::audio_track_for(&source)` rather than by counting the order it
declared things in, which is the arithmetic that puts the microphone's audio on
the game's track.

**The compatibility mix leads and carries Matroska's `FlagDefault`** (SPEC.md
section 13). Some players pick one audio track from a multi-track file
arbitrarily, so the track a naive player lands on has to be the one that sounds
right. What goes *into* that mix is not this crate's decision — it is
[issue #29](https://github.com/wildware-uk/clipped/issues/29) — and neither is
which sources a recording has, which is routing configuration
([issue #33](https://github.com/wildware-uk/clipped/issues/33)). The muxer is
handed a set of tracks and told what to put in each.

**A language is not guessed.** Every track is written with Matroska's `und`
unless the caller states otherwise. Game audio has no language; a microphone's is
a fact about the person speaking, and inferring one from the operating system's
locale would put a wrong tag in a file nobody can rewrite. `with_language` is
there for a caller that knows.

**A blank name is refused.** `MkvWriter::create` rejects an audio track whose
name is empty or whitespace — reachable through an application track configured
with an empty name — rather than writing an empty `Name` element into a recording
that cannot be corrected afterwards.

### What the audio is stored as

**Uncompressed 16-bit PCM** (`RECORDING_AUDIO_CODEC`), at whatever rate and
channel count the capture produced. Three reasons, in order of weight:

1. **Nothing in this workspace encodes audio.** There is no Opus or AAC encoder
   in Clipped, and the pinned LGPL FFmpeg build is not used as one — this crate
   writes packets, it does not make them (`docs/ffmpeg.md`). Declaring a codec
   the recorder cannot produce would be a setting that does nothing.
2. **The recording is the archival copy** (ADR 0001). It is what an editor opens
   and what every clip is cut from, and putting a lossy encoder in front of it
   costs quality that no later step can recover.
3. **It is small beside the video.** One 48 kHz stereo track is 1,536,000 bit/s —
   1.54 Mbit/s, 11.5 MB a minute — which `ffprobe` reports directly. Four tracks
   are 6.1 Mbit/s; against a 1440p60 recording at 40 Mbit/s that is about 13% of
   the file.

The cost is stated plainly: a four-track recording spends about 46 MB a minute on
sound, and a long session on a small drive notices. Compressing the tracks that
are not the archival ones — Opus at 128 kbit/s is a twelfth of the size — is a
real option once something in Clipped can encode audio, and the container and
this crate already carry Opus and AAC for the day it can. That is not this
ticket, and it is not implied by anything here.

Because the storage format is a *decision* rather than a parameter,
`AudioTrackWriter` refuses a track declared in any other codec: uncompressed
samples cannot be dressed as Opus, and a file whose header says Opus and whose
blocks are PCM opens, looks healthy, and decodes to noise.

### From captured samples to packets

`clipped-audio` produces interleaved `f32` in `[-1.0, 1.0]` with a timestamp on
the recording's clock (`docs/audio-routing.md`); a container wants packets of
bytes. `AudioTrackWriter` is that conversion, in one place rather than in every
caller that has audio to write:

| | What it does | Why |
| --- | --- | --- |
| Packet length | at most 20 ms; a shorter buffer is written as it stands | 20 ms is what Windows loopback delivers. Holding a short buffer back to fill a packet would leave sound waiting in this process, and what waits in memory is what a killed recorder loses. |
| Timestamps | each packet is timed from its own frame offset within the buffer | Rounding once per packet rather than adding to the packet before it, so a rate whose packet length is not a whole number of nanoseconds cannot accumulate an error over a session. |
| Sample scale | `clamp(-1.0, 1.0) * 32767`, rounded | Full scale is representable in both directions, and a sample that wrapped would turn the loudest instant of a recording into a full-scale click of the opposite sign. |
| Channel count | taken from the declared track, and a buffer that is not a whole number of frames is refused | The alternative swaps the channels of every frame after a short one, which is a stereo image that wanders and surround channels in the wrong speakers. |

### When a source produces nothing

The track is **declared and empty**. Matroska fixes the track list in the header,
so the track is in the file whatever the source does afterwards; nothing is
invented to fill it.

That is a decision rather than an omission. A capture is the only thing that
knows the difference between "the device produced silence" and "there was no
device", and `clipped-audio` already synthesises silence to keep a track the
length of its recording (`docs/audio-routing.md`). A muxer that manufactured more
would be writing audio nobody captured.

What it does instead is **say so**: `RecordingSummary::audio_tracks_without_packets`
counts the tracks that took nothing, `MkvWriter::packets_written` answers for one
track, and `finish` logs one `warn` naming them. An empty track is the visible
symptom of a microphone Windows had muted, a device that never opened, or routing
pointed at an application that was not running — and the session above this crate
is the layer that can turn that into something a user can act on (AGENTS.md
section 45).

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

## Remuxing to MP4

MKV is the archival container, and ADR 0001 accepted the consequence: several
upload targets, chat clients and editors reject it. So a shareable copy is
produced by **remuxing** — copying the coded packets into an MP4 — rather than by
re-encoding.

```rust
use clipped_muxer::{remux_to_mp4, Mp4Plan};

// What would this cost? Answered without writing anything.
let plan = Mp4Plan::inspect(Path::new("recording.mkv"))?;
for loss in plan.losses() {
    eprintln!("warning: {loss}");
}

let summary = remux_to_mp4(Path::new("recording.mkv"), Path::new("recording.mp4"))?;
```

or from a shell, which is what the numbers below were taken with:

```text
cargo run -p clipped-muxer --example remux_recording -- \
    --source recording.mkv --destination recording.mp4
cargo run -p clipped-muxer --example remux_recording -- --source recording.mkv --inspect
```

### No decoder and no encoder run

The bytes that come out of the source are the bytes that go into the
destination; only the boxes around them change. That is the whole claim, so it
is the thing the tests check directly rather than inferring from a duration or a
frame count: `crates/muxer/tests/mp4_remux.rs` hashes the payload of every packet
of both files with `ffprobe -show_data_hash` and compares them stream by stream,
in order.

Measured on the development machine — a 30-second 1280×720 recording at 60 fps
with three PCM audio tracks, 19.1 MB, produced by `examples/synthetic_recording`,
with everything built in the **debug** profile:

| | Wall time | Result |
| --- | --- | --- |
| `remux_recording` | 0.05 s (0.031 s inside `remux_to_mp4`) | 1800 frames, 30.000 s, 19,071,344 bytes |
| `ffmpeg -c:v libopenh264 -b:v 8000k` | 1.85 s | 1800 frames, 30.000 s, 19,751,402 bytes |

Thirty-six times faster on this machine, and the comparison is generous to the
re-encode: it is the same picture size and rate, in the pinned build's *software*
encoder, which is the only H.264 encoder an LGPL build has. The remux's output is
4 KB larger than the source, which is the difference between Matroska's boxes and
MP4's; the re-encode's is 700 KB larger and its pixels are not the source's.
Repeat it with `scripts`-free commands: build the examples, run the two above,
and compare with `ffprobe -count_frames`.

### What MP4 cannot carry

Matroska accepts nearly anything. MP4 stores only what has a registered mapping,
so every stream is put to the **linked FFmpeg build's own MP4 muxer**
(`avformat_query_codec`) rather than to a list written down in Clipped, which
would go stale: FFmpeg 8 added uncompressed audio to MP4 as `ipcm`, and a list
written a year ago would still be refusing it.

| Stream | MP4 can carry it | What happens |
| --- | --- | --- |
| Picture or sound | yes | copied |
| Picture or sound | no | **the remux is refused**, before the MP4 is created |
| Subtitle, attachment, data | yes | copied |
| Subtitle, attachment, data | no | left out, and named in the plan and the log |

Refusing rather than dropping is the half that matters. An MP4 missing one of a
recording's five audio tracks is indistinguishable from one that only ever had
four, and the person who discovers it is the one who uploaded it. A missing
attachment is not the recording, so that is a warning rather than a refusal.

`Mp4Plan::inspect` answers all of this **without writing anything**, so a user
interface can say what will be lost before somebody waits for the copy —
`Mp4Plan::losses` is that list in words, and it includes the refusing tracks as
well as the merely-dropped ones, because somebody deciding wants one list.

Chapters are not carried. `Mp4Plan::chapters` counts them so that a caller can
say so; nothing Clipped records has any.

### What survives, and the two things that move

Every carried track keeps its coded packets, its timestamps, its language, its
default-track flag, its frame rate, and everything
`avcodec_parameters_copy` copies — pixel format, profile and level, colour
signalling, channel layout. Two things are stored differently on the other side,
and both are worth knowing before somebody concludes the remux lost them:

- **The track name.** Matroska keeps it in the track entry's `Name` element,
  which `ffprobe` reports as the `title` tag. MP4 keeps it in a `udta`/`name` box
  on the track, which `ffprobe` reports as the `name` tag. Same string, different
  place; a tool that only looks for `title` will not find it.
- **An unknown language.** Matroska omits the element; MP4's media header cannot,
  and writes `und`. Same statement.

One thing is *filled in* rather than copied. Matroska stores a channel **count**
and no arrangement, so its demuxer reports an unspecified channel order for every
uncompressed track — and MP4's `chnl` box has to name an arrangement. Left alone,
FFmpeg's MP4 muxer rejects the track while writing the trailer, after the whole
file has been written, with `unsupported channel layout 2 channels`. So a track
whose order is unspecified is given the conventional layout for its channel count
— the same one `MkvWriter` writes when it creates such a track, and the one every
player assumes. A track that *did* state its arrangement is left alone, because
overwriting a stated layout is how a surround mix gets its channels relabelled.

### Timestamps, and where a naive remux drifts

The source's timestamps are rescaled into the destination's units and otherwise
left exactly as they are. In particular:

| The source has | What is written | Why |
| --- | --- | --- |
| Tracks that do not start together | the same offset between them | Rebasing each track onto its own first packet is the classic remux bug: it pulls the later track forward and the copy plays out of sync with the recording. |
| A first timestamp before zero | the same negative timestamp | MP4 represents it with an **edit list**, which is what FFmpeg's muxer writes. Opus carries its pre-skip this way; clamping it to zero would shift the sound. |
| A decode timestamp before its own presentation timestamp | both, unchanged | A stream with B-frames leaves the encoder in decode order. MP4 stores the gap as a **composition offset**; flattening it reorders the picture. |
| A decode timestamp that does not advance | one tick past its predecessor, counted | FFmpeg's muxer refuses the packet outright, which would end the copy part-way and leave a truncated MP4. Nothing Clipped records produces this — `MkvWriter` already enforces it — so a non-zero `RemuxSummary::timestamps_forced_monotonic` means the source's own decode order was broken. |

The rescaling is `av_rescale_q_rnd` with FFmpeg's own rounding, so a remuxed
timestamp is the one `ffmpeg -c copy` would have written. The correction policy
in the last row is `crate::timeline`'s, shared with the recording writer, so
there is one answer to "what do we do about a timestamp that does not advance"
rather than two.

`crates/muxer/tests/mp4_remux.rs` builds a source with two B-frames per group
using the pinned build's MPEG-4 encoder — `libopenh264` does not reorder, so a
recording written here cannot exercise this — and asserts that **both** timestamps
of **every** packet come out within 2 ms of the source's, in the same order.

### The recording is never touched

`avformat_open_input` opens for reading and nothing here opens the source any
other way, so a remux leaves the recording byte for byte as it found it — on the
failing paths as well as the succeeding one (AGENTS.md section 56). The tests
read the source before and after and compare the bytes, for a successful copy, a
copy refused for an uncarryable track, and a copy refused because the destination
was already taken.

The destination gets the same protection the recording writer gives:
`MuxError::OutputExists` rather than truncating whatever was there, and a partial
MP4 removed again if the copy fails part-way, so that retrying the same name does
not run into a stub left by the last attempt.

### `faststart`

The MP4 is written with `-movflags +faststart`: the index is moved to the front
of the finished file, which costs one extra pass over the output and is the
difference between a file that plays while it is still downloading and one that
has to be fetched whole first. Sharing is the entire reason this container exists
here, so the pass is worth paying for.

That second pass re-opens the output by URL, which is why
`OutputContext::open_output` sets `AVFormatContext::url` — without it the muxer
fails at the trailer, after the whole file has been written, with
`Unable to re-open output file for shifting data`.

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
| `tests/multi_track_audio.rs` | Each source is *audible* on its own track and on no other, by Goertzel filter over the decoded samples; the compatibility mix carries all of them; the tracks are named, ordered and flagged as the model says; a source that produced nothing leaves a declared, empty track that the summary and the log name; a blank track name and a buffer that is not a whole number of frames are refused; and the video's timestamps are identical in a one-track recording and a five-track one. |
| `tests/synthetic_recording.rs` | A whole recording — real H.264 and two PCM tracks — opens, decodes frame by frame, has a plausible duration, carries the right codec metadata, and has tracks that start and end together. |
| `tests/abrupt_termination.rs` | A killed recorder leaves a playable file, reaching to within the bound above. |
| `tests/mp4_remux.rs` | An MP4 made from a recording holds the same tracks, the same names, languages and default flags, decodes the same pictures, keeps the offset between tracks that do not start together and the composition offsets of a stream that reorders — and holds the source's coded bytes, packet for packet. A track MP4 cannot carry is refused before anything is created, an existing MP4 is never overwritten, and the recording is unchanged afterwards on every path. |
| `tests/ffmpeg_linkage.rs` | The FFmpeg actually loaded is the pinned, LGPL-only build and contains what the pipeline needs. |

The packets come from `crates/muxer/examples/synthetic_recording.rs`, which
encodes a moving test pattern with the pinned build's own software encoder
(`libopenh264`) and generates tones through `AudioTrackWriter` — the same path a
session takes. Its audio tracks are the model's, in the model's order, and each
carries a frequency of its own: 440 Hz for the game, 880 Hz for other system
audio and 1320 Hz for the microphone, which are AGENTS.md section 26's own
tones, with the compatibility mix carrying all of them at once. Identical tracks
would hide a writer that sent the same packets to every stream.

It is an example rather than a test helper because two of those tests need it as
a *process*: a Rust panic unwinds and runs destructors, and destructors are
exactly what a killed recorder does not get.

Three of its options exist only so that a test can fail. `--audio-offset-ms`
starts the audio later than the video, because a remux that rebased each track
onto its own first packet passes every other assertion. `--default-audio-track`
marks a track other than the first as the one to play, because FFmpeg's MP4 muxer
enables the first track of each kind whether or not it was told to — so a
recording whose default track *is* the first one produces an identical MP4 with
the flag copied and with the copy deleted. `--audio-language` states a language
the track model deliberately does not invent, because Matroska omits the element
for an unknown language and MP4 writes `und`, so a recording that stated nothing
would prove nothing about whether the tag survived a remux.

Sources that Clipped's own writer cannot produce — a stream with B-frames, a
codec MP4 refuses — are built in the test with the pinned build's `ffmpeg`, the
same program `clipped-media-validation` inspects with. Nothing in the recorder
shells out to FFmpeg ([ffmpeg.md](ffmpeg.md)).

```text
cargo test -p clipped-muxer
cargo run -p clipped-muxer --example synthetic_recording -- --output demo.mkv --seconds 5
cargo run -p clipped-muxer --example remux_recording -- --source demo.mkv --destination demo.mp4
```

## What has not been exercised

Stated rather than left to be discovered (AGENTS.md section 54):

- **Only H.264 video and PCM audio have been written end to end.** HEVC, AV1,
  AAC and Opus are mapped and declared, and the container is told about them
  correctly, but no encoder in this workspace produces them yet, so no file has
  been made in them.
- **No recording here has been opened in an editor.** The claim that a
  multi-track file arrives in an NLE with separately selectable, correctly named
  tracks is tested against `ffprobe` — names, order, default flags, and what is
  audible on each track — and against nothing else. No editor is installed on the
  machine this was written on, and issue #28's second criterion asks for exactly
  that check, so it remains unmet and unclaimed.
- **The audio a recording carries is not a capture's yet.** The samples that have
  been through `AudioTrackWriter` are generated tones, not WASAPI's; joining
  `clipped-audio` to a session is
  [issue #180](https://github.com/wildware-uk/clipped/issues/180).
- **No packet has come from a hardware encoder.** NVENC is
  [issue #15](https://github.com/wildware-uk/clipped/issues/15) and is not on
  this branch. The Annex B handling is what a Windows hardware encoder produces
  and is exercised by the software encoder, which produces the same form.
- **No B-frames have been *recorded*.** Reordered presentation timestamps are
  handled and unit tested, and `tests/mp4_remux.rs` remuxes a real reordered
  stream end to end, but that stream is MPEG-4 built by `ffmpeg` for the test:
  `libopenh264` does not reorder, so no *recording* with B-frames has been
  written by `MkvWriter`.
- **No MP4 has been played by anything but FFmpeg.** The remuxed files are
  decoded frame by frame with the pinned build's `ffprobe` and nothing else. That
  is the same limit `MkvWriter`'s interruption claim has, and for the same reason
  — but it matters more here, because the whole point of the MP4 is that other
  software accepts it. Whether a given upload target accepts these files is a
  thing to check by uploading one, and it has not been checked.
- **No remux has been driven by the application.** Nothing calls
  `remux_to_mp4` yet; the setting, the retention policy for the MKV, and the
  progress a long copy should report belong to the session and library work.
- **Sessions are minutes, not hours.** AGENTS.md section 59 asks for long-run
  testing; the longest recording written so far is a few seconds.
- **No colour signalling is written.** A 10-bit recording would not say what its
  colours mean, and a player would guess. Left out deliberately rather than
  forgotten — nothing here produces 10-bit output yet, so there is nothing to
  test it against — and tracked as
  [issue #146](https://github.com/wildware-uk/clipped/issues/146).
