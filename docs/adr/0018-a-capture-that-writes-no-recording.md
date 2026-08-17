# 0018. A capture that writes no recording has no `recordings` entry, and its clips point at nothing

- Status: Accepted
- Date: 2026-08-17
- Issue: [#423](https://github.com/wildware-uk/clipped/issues/423)

## Context

SPEC.md section 4 lists **Manual / Replay Buffer** as a capture *mode* beside
Full Session: "always maintain a rolling buffer while the game is active",
keeping only what the user asks for. Until this decision Clipped had the buffer
and not the mode. `clipped-recorder replay` records *and* buffers — one encoder,
two consumers of its packets — because `clipped-session` had no recording
without a file: `RecordingSettings::output` was a `PathBuf`, the muxing thread
owned an `MkvWriter`, and the disk guard, the report and the session record all
assumed one.

**What the mode is worth is not what the issue said it was worth.** Issue #423
argues it from memory — "a five-hour session costs the memory of its window
rather than a hundred gigabytes of disk" — and that was true when it was
written and is not true now. Since
[issue #36](https://github.com/wildware-uk/clipped/issues/36) a buffer spills to
disk whenever `SpillArea::default_root()` answers, which on Windows it does, and
`docs/replay-buffer.md` measures a thirty-minute window at **0.94 MB resident
and 208 MB on disk**. The buffer's cost is bounded either way, and it is bounded
by the *window* rather than by how long somebody plays.

The saving is real and it is the *recording*. A five-hour 1080p60 sitting at the
18.7 Mbit/s a recording is given is about **42 GB of video** that grows for as
long as the game runs, against a rolling 208 MB that does not. That is the
argument, and it is a disk argument rather than a memory one.

Three things then had to be decided, and only the first is obvious.

1. **How a capture with no file is expressed**, without becoming a second
   capture loop.
2. **What its session record says.** A sitting with no recording and some clips
   is a shape neither `docs/sessions.md` nor `clipped-storage` modelled.
   `clips.source_recording_id` is nullable for exactly this — `0004_clips_without_a_file.sql`
   says "a replay saved while nothing was being recorded has no source
   recording" — but the sidecar's `source_recording` was an index into a list
   that would be empty.
3. **What the disk guard watches**, since it watches the file being written and
   there is none.

## Decision

**One capture loop, one difference: the destination.**
`RecordingSettings::buffered(target, directory)` names a directory instead of a
file, `RecordingSettings::output` answers `Option<&Path>`, and
`crates/session/src/recording.rs` opens no `MkvWriter`, starts no
`MuxingThread` and arms no `SpaceGuard` when the answer is `None`. Everything
above that line — the backend selection, the first-frame device, the encoder,
the audio endpoints, the track layout, the replay buffer, the frame gate, the
silence reporting — is the same code running in the same order.

**A sitting that wrote no recording has no `recordings` entry.** Its clips carry
no `source_recording`, and `SessionClip::source_index` is therefore an
`Option<u32>`. `ManualSession::start_buffered` opens such a session;
`ManualSession::start` is unchanged.

**The disk guard is not armed, and the room check still is.** There is no file
growing on a drive for a guard to watch, and the buffer's own spill area is
bounded by its window and stops spilling rather than filling a disk
(`docs/replay-buffer.md`). What remains is `check_there_is_room`, run once
before the capture starts against the directory the clips will go in, because a
save that could never land is worth refusing while somebody is still looking at
their terminal (AGENTS.md section 45).

**`clipped-recorder replay --no-recording` is the mode's front end**, and the
only one. `serve` starts no buffered capture: `start_recording` names an output,
and the protocol has no shape for a sitting with no file.

## Alternatives

### A `recordings` entry with the `output` key absent

Keep the entry — index, `starts_at_nanos`, `started_at`, `ended_at`, `outcome`
and, importantly, `settings` — and omit only the path. It loses nothing: the
per-recording `settings` block is the only record of what a sitting was captured
at, and an empty `recordings` list drops it.

Rejected because of what the reader would have to become.
`clipped-storage`'s `recordings.path` is `TEXT NOT NULL UNIQUE` — a recording
row *is* a file — so `clipped-library` would need a new rule for an entry with
no path, in a crate four layers below the writer, with reconciliation semantics
(`missing_since`, `size_bytes`, the trash) to reason about for a row that can
never have a file. An empty list needs no rule at all: `ingest::write_recordings`
loops over zero items and `write_clips` resolves an absent `source_recording` to
NULL, which is what the column was made nullable for. `clipped-library`'s
`a_sitting_that_recorded_nothing_has_no_duration_rather_than_a_duration_of_zero`
already asserts a sitting with no recordings is listed.

The settings loss is real and it is the price. It is recorded in the log line
`ManualSession` writes when it opens a session, and giving it a home in the file
is worth doing when something needs to read it — not before, because a second
place to write settings is a second place for them to disagree (AGENTS.md
section 55).

### A `recordings` entry naming the file it would have written

Simplest of all: write the path `--output` names and let the outcome say no file
was produced, exactly as a `no-window` recording already does.

Rejected because the library does not read it that way. `ingest::write_recording`
writes the row whatever the presence, and `presence::judge` marks a file that is
not there `missing_since` — so every buffered sitting would put a recording in
somebody's library that is permanently missing and draws as a tile that cannot
be played. That is precisely the failure
[issue #383](https://github.com/wildware-uk/clipped/issues/383) removed from the
recording path, and reintroducing it through a different door would be worse for
having been chosen.

### A second capture function beside `record`

`record_buffered`, with its own loop: no writer, no muxing thread, no space
state, no summary. It reads cleanly and touches nothing that exists.

Rejected because it is a second implementation of the thing the workspace is
built around (AGENTS.md section 55). The recording loop is where the frame gate,
the epoch, the audio start, the resize handling, the minimise handling, the
screenshot service, the silence reporting and ADR 0017's `note_source_silence`
all live, and a copy of it would drift from the original in exactly the ways
nothing tests — a buffered sitting that stopped following a resize, or one whose
buffer was never told its source had gone quiet. The `Option` in `PacketSinks`
costs one branch per packet and makes the difference visible in one place.

### Express the mode in `RecordingOutputs` rather than in `RecordingSettings`

`RecordingOutputs` already carries everything a recording writes into besides
its own file, so "no file" looks like it belongs beside them.

Rejected because `RecordingOutputs` is documented as the things that **cannot
fail a recording**, and whether there is a recording at all is not one of them.
Where the video goes is a setting: it is what the caller asked for, it is
validated before anything opens, and it is what `--output` names.

### Keep `RecordingSettings::output` a `PathBuf` and add a flag beside it

`writes_a_recording: bool`, with the path still there for the directory and the
naming.

Rejected because it leaves a trap in a public API. Every existing caller of
`output()` — the report, the protocol's `recording_started`, the session record,
the failure message — would keep compiling and would keep naming a file that was
never going to exist. `Option` makes the compiler ask each of them what they
want to do about it, which is how each of them came to be looked at.

### Give the mode its own subcommand

`clipped-recorder buffer`, beside `record` and `replay`.

Rejected because it would carry every `record` argument a third time and would
have to answer, separately, what its hotkey is and where its clips go. The mode
is `replay` with the recording left out, and `--no-recording` says that in the
place somebody is already looking.

## Consequences

**A `replay` sitting can now produce a session with no recording in it, and the
desktop has never seen one.** `apps/desktop`'s session list reads
`session.recordings` with `map`, `length`, `reduce` and `filter` and tolerates an
empty array, and no screen draws `session.clips` yet — so such a sitting appears
as a session with nothing under it. That is honest and it is not useful, and the
screen that draws a sitting's clips is what makes the mode worth using from the
window rather than from a terminal.

**`RecordingReport::output` is an `Option` for every caller**, including the
ones that will never see `None`. `serve` expects a path when it starts a
recording and says why in as many words; a future protocol command that starts a
buffered capture has to give `recording_started` and `recording_stopped` a shape
for a sitting with no file rather than sending an empty string.

**`packets_written` is counted a step earlier when there is no muxer.** For a
recording it is the muxer's own account of the file; for a buffered capture it is
what `drain` moved. The two are the same number for the same capture and the word
"written" is doing less work in the second case, which the getter says.

**Nothing about this reaches `watch`.** An automatic session records, and a game
launching still produces a file. Whether a *user* can choose Manual/Replay as
their capture mode for automatic sessions is a settings question
(`replay_window_seconds` has no companion for it) and is not decided here.

**The mode's cost is now a documented figure rather than an assumption.**
`docs/replay-buffer.md`'s spilling measurements are what
`clipped-recorder replay --help` and `docs/recorder-cli.md` quote, so the
next person to reason about "what does keeping thirty minutes cost" reads
0.94 MB and 208 MB rather than the 3.9 GiB the in-memory table shows.

**ADR 0017's silence report has one more caller and it is the one that ADR named.**
"A future recording loop, or a Manual/Replay-mode capture with no continuous file
(#423), that never calls `note_source_silence` reintroduces the during-the-stall
half of this defect." It calls it, because it is the same loop, and
`a_capture_with_no_recording_still_tells_its_replay_buffer_when_its_source_goes_quiet`
is what holds that to be true rather than the fact that the code is shared today.
