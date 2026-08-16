# 0010. The window plays the archival recording itself, and the recorder chooses which sound track it carries

- Status: Accepted
- Date: 2026-08-16
- Issue: [#304](https://github.com/wildware-uk/clipped/issues/304)

## Context

Clipped writes Matroska with uncompressed 16-bit PCM sound
([ADR 0001](0001-mkv-archival-container.md), [muxing.md](../muxing.md)), and the
desktop window is a WebView2, which is Chromium. Until this record, four facts
were held to stand between the two, each said to be enough on its own
([#304](https://github.com/wildware-uk/clipped/issues/304), and the table this
replaces in [desktop-ui.md](../desktop-ui.md)):

1. the window cannot load a file from the disk — no asset protocol, no
   file-system permission, and a content-security policy with no `media-src`;
2. **a recording is Matroska, and WebView2's Matroska support is the WebM
   subset** — Opus or Vorbis sound, VP8, VP9 or AV1 picture;
3. **the sound is PCM, and no browser decodes PCM** — so playback needed an
   audio encoder Clipped does not have
   ([#392](https://github.com/wildware-uk/clipped/issues/392));
4. `HTMLMediaElement.audioTracks` is not implemented in Chromium, so a media
   element handed a multi-track file plays whichever track its demuxer reaches
   first and offers no way off it.

Facts 2 and 3 were **never measured**. They were read off the WebM specification
and off what Chromium documents, and they are the two that decided the shape of
the answer: a live remux to fragmented MP4, an AAC encode of the chosen track,
byte ranges over a URI scheme, and a new audio-encoding subsystem in front of all
of it.

## The measurement

Run on 2026-08-16 against **Microsoft Edge 151.0.4129**, headless, which is the
same Chromium build as the WebView2 runtime installed on the same machine
(`C:\Program Files (x86)\Microsoft\EdgeWebView\Application\151.0.4129.86`) —
WebView2 *is* Edge, without the browser's own interface, and the media stack is
the same one. It was not driven inside the Clipped window itself, which is the
one gap in this measurement and is worth closing the next time somebody has that
window open.

Each file was loaded into a `<video>` from an HTTP server answering byte ranges,
played, and then asked what it had **decoded**: `webkitVideoDecodedByteCount` and
`webkitAudioDecodedByteCount`, which count bytes through the decoders rather
than bytes fetched. A container that demuxes but cannot decode reports zero.

| File | Picture | Sound | Played | Audio decoded |
| --- | --- | --- | --- | --- |
| A real Clipped recording, 35 s | AV1 1280×720 | `pcm_s16le`, 2 tracks | yes | 192,000 B |
| A real Clipped recording, 5.6 s | AV1 2560×1392 | `pcm_s16le`, 2 tracks | yes | 197,828 B |
| Matroska | H.264 | `pcm_s16le` | yes | 196,608 B |
| Matroska | AV1 | `pcm_s16le` | yes | 196,608 B |
| MP4 | H.264 | `pcm_s16le` (`ipcm`) | yes | 196,608 B |
| MP4, fragmented | H.264 | `pcm_s16le` (`ipcm`) | yes | 196,608 B |
| WebM | AV1 | Opus | yes | 17,805 B |
| MP4 | H.264 | AAC | yes | 16,615 B |

192,000 bytes is exactly one second of 48 kHz stereo 16-bit PCM, which is what a
second of decoded sound from those files should be.

**So facts 2 and 3 are wrong.** The engine the window is drawn by plays a
Clipped recording as it stands — Matroska, AV1, uncompressed PCM — and decodes
its sound. It is worth knowing *why* the reasoning failed: `canPlayType` and
`MediaSource.isTypeSupported` do answer as facts 2 and 3 predicted
(`video/mp4; codecs="ipcm"` is `""` and `false`), because those consult a codec
allow-list. The `src=` path does not: it goes to Chromium's bundled FFmpeg
demuxer, which knows Matroska and PCM. The published answer and the actual
behaviour differ, and only one of them was checked.

Two further measurements, because they decide the rest of the design:

- **`audioTracks` is not implemented** — `'audioTracks' in video` is `false` in
  the same build. Fact 4 stands.
- **The default-track flag is ignored.** Given a Matroska file whose *second*
  sound track carries Matroska's default flag, the element played the **first**
  one. So "the compatibility mix carries the flag" is not enough on its own; what
  a media element plays is the first sound track the container declares.

Fact 1 also stands, and is unchanged by any of this.

### Seeking

Measured on the same engine, over a server answering byte ranges exactly as the
`clip` scheme does: a **10 minute 37 second** recording — a real Clipped
recording looped by a stream copy, 188 MB, AV1 1280×720 with two PCM tracks in
Matroska.

| What | Measured |
| --- | --- |
| Metadata (`loadedmetadata`) | 16 ms |
| Seek to 25% (2:39) | 18 ms |
| Seek to 50% (5:19) | 6 ms |
| Seek to 90% (9:34) | 27 ms |
| Back to 10% (1:04) | 8 ms |
| Seek to 99% (10:31) | 13 ms |

Every seek landed on the time it was given and none took longer than 30 ms. The
element asks for the range it needs and reads it; nothing rewinds the file, and
nothing waits for the whole of it. What a seek is *not* is frame-accurate: the
picture that comes up is the nearest keyframe at or before the position, which
is the practical limit SPEC.md section 42's "where practical" leaves room for.

## Decision

**The window plays the recording itself. The recorder decides what is served,
and re-muxes only when the track somebody chose is not the one a media element
would reach.**

Concretely, and in three parts:

- **`open_playback`** ([ipc.md](../ipc.md)) names a recording and a sound track.
  The recorder opens the recording, lists its sound tracks, and answers with a
  file to play. When the chosen track is the first sound track of the recording
  — which for a Clipped recording is the compatibility mix, because that is what
  leads the file (`clipped_muxer::AudioSource`) — the file it answers with is
  **the recording**, and nothing is written or copied.
- **Choosing any other track** is `clipped_muxer::remux_to_mp4_carrying`, which
  copies the picture and that one sound track into an MP4 in
  `%LOCALAPPDATA%\Clipped\playback`. Still a stream copy: nothing is decoded and
  nothing is encoded, because MP4 carries PCM as `ipcm` in the pinned FFmpeg 8
  build and the measurement above says a WebView2 plays it.
- **The Tauri host registers a `clip` URI scheme** which serves, with byte
  ranges, only the files `open_playback` has answered with in this session. The
  window is handed `http://clip.localhost/3` and never a path.

**No audio encoder is involved, at any point.** The archival recording is
untouched: it is opened for reading, and the only thing ever written is a copy in
a cache directory of Clipped's own (AGENTS.md sections 56 and 57).

## Alternatives

### Fragmented MP4 with the chosen track encoded to AAC, streamed over the protocol

The design #304 was written around, and the one this record replaces. The
recorder would remux the source into fragmented MP4 as it was watched, encode
the chosen sound track to AAC, and answer byte ranges over the control protocol;
the Tauri host would relay those ranges into a URI scheme.

It would have worked, and it is what the facts above would have required if they
had been true. It costs: an audio encoder that does not exist
([#392](https://github.com/wildware-uk/clipped/issues/392)), a lossy re-encode of
sound Clipped deliberately keeps lossless, a live remux for **every** recording
anybody watches — including the common one, where nothing needs to change at all
— and byte-range plumbing over a one-request-at-a-time control connection.

Measured against the alternative chosen: watching a recording on its default
track now costs one `Mp4Plan::inspect` — the file is opened, its streams are
described, and it is closed. Nothing is written.

### Point a `<video>` at the recording through Tauri's asset protocol

The cheapest thing to write, and it is what the measurement says would play. It
was rejected on **privilege**, not on playback: the asset protocol serves any
path inside a scope, a recording lives wherever the recorder's output directory
points — a setting — so the only scope that would work is every path on the
machine. #304's last criterion is that the window gains the smallest privilege
that works, and `playbackReach.test.ts` is what holds it to that. A scheme that
serves only what the recorder has already opened is smaller.

It also cannot answer the track selector, for the reason fact 4 gives.

### Transcode to WebM on the way out

Rejected before the measurement and still rejected: WebM cannot carry H.264 or
HEVC, so the picture would have to be re-encoded, and a transcode of gameplay
footage beside a running game is the one cost worth avoiding. AV1-in-WebM would
have avoided that for AV1 recordings only.

### A native video surface behind the webview

No transport, no keyboard handling, no shared layout, and Tauri offers nothing
for it. It remains the answer if the media element proves inadequate for
frame-accurate seeking (SPEC.md section 42), and it is a much larger change.

## Consequences

**What becomes easy.** Playing a recording costs nothing: no copy, no encode, no
temporary file, and the picture is the recording's own bytes rather than a
generation of loss. Export ([#92](https://github.com/wildware-uk/clipped/issues/92))
and playback now share one remux with one selection parameter rather than two
paths.

**What this un-blocks.** [#392](https://github.com/wildware-uk/clipped/issues/392)
— "an audio encoder for the player path" — is not a blocker for #304 and, by its
own last comment, is not one for export either. Nothing in Clipped needs an audio
encoder to play or to share a recording. If that issue survives, it is for
*size*: PCM is about 5.5 MB per minute per stereo track, which matters for
uploading and not for playing.

**What becomes hard.** Choosing a sound track other than the leading one costs a
pass over the whole recording and a second copy of it on disk — roughly the size
of the recording, minus the tracks left out. For a ten-minute AV1 recording with
three sound tracks that is a few hundred megabytes and a few seconds. Entries are
kept in `%LOCALAPPDATA%\Clipped\playback` and swept after a day
(`apps/recorder/src/playback.rs`). If people switch tracks often, this wants
revisiting — a live remux over ranges is what it would become, which is the first
alternative above with the encode taken out.

**What has to be watched.** This decision rests on a measurement of one Chromium
build. A future WebView2 that drops the bundled FFmpeg demuxer's Matroska or PCM
support would break playback for every existing recording, and it would do so
silently — the element would simply fail to load.
`crates/muxer/tests/mp4_remux.rs` and `apps/recorder/tests/ipc_protocol.rs`
cannot see that, because neither runs a browser. What would see it is the
measurement above, repeated; it is scripted in the issue and takes a minute.

**What is unchanged.** The archival format: recordings stay Matroska with
lossless PCM, and this record does not touch ADR 0001. The window's permissions:
`capabilities/default.json` gains nothing, because a URI scheme this process
registers is not a permission the interface holds.

**What is still owed.** Frame-accurate seeking and keyboard shortcuts of
Clipped's own (SPEC.md section 42) — what is drawn is the element's transport,
which seeks to a keyframe; and a poster frame
([#57](https://github.com/wildware-uk/clipped/issues/57)) and a waveform
([#66](https://github.com/wildware-uk/clipped/issues/66)), which are files beside
the recording and need a route into the window of their own.
