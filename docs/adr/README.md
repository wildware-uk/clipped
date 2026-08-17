# Architecture decision records

An ADR records a decision that constrains later work, together with the
alternatives that were genuinely considered and the consequences that were
accepted. The point is not to document that a decision happened — git already
does that — but to make reopening it a deliberate act rather than an accident.

## When to write one

Write an ADR when a decision:

- is expensive to reverse once code depends on it;
- rules out an approach a reasonable contributor would otherwise take;
- forces cost onto another part of the system, now or later;
- or affects a compatibility surface: containers, configuration, database
  schema, plugin API, recording metadata, command-line arguments.

Do not write one for a choice that is cheap to change, local to one function,
or has no serious alternative (AGENTS.md section 48). An ADR for a trivial
decision dilutes the ones that matter.

Trigger to watch for: a pull request review argues about an approach, agrees on
one, and the argument is not written down anywhere. That is an ADR.

## How to write one

Copy [0000-template.md](0000-template.md) to `NNNN-short-title.md`, taking the
next free number. Numbers are never reused, and a file is never renumbered once
merged, because links to it exist.

Four sections, all required:

- **Context** — the forces in play. What problem, what constraints, what has to
  be true. No solution here.
- **Decision** — what was chosen, stated plainly.
- **Alternatives** — what else was seriously considered and why it lost. This
  section carries most of the value of the record, so give each alternative its
  real case before rejecting it. An alternatives section written as a formality
  is worse than no ADR, because it makes a decision look examined when it was
  not.
- **Consequences** — what this costs, including the work it creates. Bad
  consequences are the most useful ones to write down; an ADR with only
  benefits has not been thought about.

Length follows from the decision. Two or three pages is normal for a decision
that closes real doors — the records already here run to about that, and nearly
all of it is the alternatives and the consequences, which is where the value is.
Be brief in Context and Decision; do not compress Alternatives or Consequences
to hit a length. A short record is a good sign only when the decision was
genuinely simple.

## Status and supersession

An accepted ADR is not edited to change its decision. If the decision changes,
write a new ADR that supersedes it, and add a line to the old one pointing at
the replacement. The old record stays: knowing that MKV was chosen for
particular reasons and later abandoned for particular reasons is more useful
than either fact alone.

Correcting a typo or clarifying wording in place is fine.

## Records

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-mkv-archival-container.md) | MKV is the archival recording container | Accepted |
| [0002](0002-separate-recorder-process.md) | The recorder runs as an independent process from the desktop UI | Accepted |
| [0003](0003-process-specific-audio-capture.md) | Process-specific audio capture is the basis for track separation | Accepted |
| [0004](0004-ffmpeg-dependency-strategy.md) | FFmpeg is a pinned LGPL build, linked dynamically through a sys binding | Accepted |
| [0005](0005-named-pipe-control-protocol.md) | A named pipe carries the control protocol between the UI and the recorder | Accepted |
| [0006](0006-recorder-lifetime-and-supervision.md) | The desktop application starts a detached recorder and supervises it, and neither stops the other | Accepted |
| [0007](0007-visual-c-runtime-linkage.md) | The recorder links the Visual C++ runtime statically and the universal CRT dynamically | Accepted |
| [0008](0008-codec-patent-position.md) | AV1 is the codec Clipped commits to, and the AVC and HEVC exposure is named rather than assumed away | Proposed |
| [0009](0009-the-recorder-registers-global-hotkeys.md) | The recorder registers the global hotkeys, and a press becomes a protocol command | Accepted |
| [0010](0010-user-labelled-event-kinds.md) | A user-labelled event kind carries its own text, and a host subsystem gets its own source | Accepted |
| [0011](0011-what-the-webview-plays.md) | The window plays the archival recording itself, and the recorder chooses which sound track it carries | Accepted |
| [0012](0012-a-session-follows-a-resize-with-a-new-file.md) | A session follows a mid-recording size change with a new file, and Clipped does not scale | Accepted |
| [0013](0013-capture-rounds-an-odd-dimension-away.md) | Capture rounds an odd dimension away, and crops the frame to match | Accepted |
| [0014](0014-a-milestone-is-not-a-version.md) | A milestone is not a version, and the first release is v1.0.0 | Accepted |
| [0015](0015-derived-pictures-cross-the-control-protocol.md) | A recording's thumbnail and its waveform cross the control protocol, and the window gains no file-system reach | Accepted |
