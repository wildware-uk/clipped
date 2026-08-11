# Game detection

Clipped records games without being told to, which means it has to know what a
game is. `clipped-game-detection` is where that knowledge lives.

**Status: the game catalogue exists (issue #42). Nothing watches processes
yet.** Watching for process start and exit is [#41], launcher-specific
detection is [#43] and [#44], and the user-facing registration screen is [#45].
This document grows a section per part as they land; everything below is about
the catalogue.

[#41]: https://github.com/wildware-uk/clipped/issues/41
[#43]: https://github.com/wildware-uk/clipped/issues/43
[#44]: https://github.com/wildware-uk/clipped/issues/44
[#45]: https://github.com/wildware-uk/clipped/issues/45

## The game catalogue

The catalogue answers one question: *given a running process, which game is
this?* It holds the record SPEC.md section 6 asks for — identity, name,
executables, launcher, icon, capture compatibility, known child processes,
default settings and highlight providers — and the rules for matching a process
against it.

It does not start recordings, watch processes, or talk to a launcher.

### Adding a game needs no code

Append a `[[game]]` block to
[`crates/game-detection/data/games.toml`](../crates/game-detection/data/games.toml)
and open a pull request. That is the whole procedure. Nothing has to be
registered, no Rust file changes, and the file's own header carries the field
reference so you need not read this document to add an entry.

```toml
[[game]]
game_id = "some-game"
name = "Some Game"

[[game.executables]]
name = "somegame.exe"
path_contains = "steamapps/common/Some Game"

[game.launcher]
kind = "steam"
app_id = "12345"

[game.capture]
compatibility = "unknown"
```

The file is compiled into the binary with `include_str!`, so `cargo build`
publishes the entry and there is no install step that can go wrong or data file
that can fall out of step with the build reading it.

### Why TOML

The seed file's audience is a contributor reading a pull request diff, so the
format was chosen for that and not for the parser:

- **Entries are stanzas.** Adding one appends contiguous lines; it does not
  modify the entry above it. In JSON, a trailing comma makes every insertion a
  two-line diff and makes the last entry a merge conflict for the next
  contributor.
- **It has comments.** Half of what an entry needs to say is *why* — why an
  executable is path-qualified, what a launcher's identifier refers to, what has
  and has not been verified about a game's capture. A `"_comment"` key is a
  worse version of a comment.
- **Its errors carry a line and a column**, which is most of the requirement
  that a bad entry fails loudly and says where.

The cost is a second parser in the dependency graph: `serde_json` stays where it
is for the encoder capability cache and the IPC protocol, which are
machine-written files where none of the three reasons applies.

### Two files, and which wins

| | Seed data | User overlay |
| --- | --- | --- |
| Where | compiled in from `crates/game-detection/data/games.toml` | `%LOCALAPPDATA%\Clipped\games.toml` |
| Whose | the project's | the user's |
| Edited by | a pull request | the user, or the registration UI ([#45]) |
| On update | replaced wholesale | never touched |
| If the schema changes | rewritten by us | migrated, with a backup |

An overlay entry whose `game_id` matches a shipped one **replaces it entirely**
— it is not merged field by field. Merging would mean a user could add to a
shipped entry but never correct one, and would leave "which of the two `name`
fields won?" as a question about somebody's own file. An overlay entry with an
identifier of its own is simply another entry.

### Matching, and the precedence order

A process is looked up by its executable's file name, and — where the caller has
them — its full image path and whatever its launcher says it is. Every entry
that matches at all matches at one of three strengths:

| Rung | Strength | What matched |
| --- | --- | --- |
| 1 (strongest) | `LauncherIdentity` | The launcher's own identifier for the game. The executable is not consulted. |
| 2 | `QualifiedPath` | The file name matched **and** the image path contains the entry's `path_contains` fragment, as whole directory names. |
| 3 | `ExecutableName` | The file name matched an entry that asked for nothing else. |

The order in full:

1. **The strongest rung wins outright.**
2. **At equal strength, the user's overlay entry beats the shipped one.**
3. **If entries still tie, the answer is ambiguous** and names every candidate.
   Nothing picks one.

Rule 2 sits *below* rule 1 deliberately, and it is the one part worth
explaining. If a user's bare `game.exe` entry outranked a shipped entry that had
path-qualified the same `game.exe`, one broad personal entry would quietly take
over every game sharing a common executable name. Specificity is evidence about
*this* process; a bare file name is a guess by whoever wrote it. A user who
wants to override one specific installation writes a path qualifier of their own
and wins on rung 2 by having said something equally specific.

Ambiguity is reported rather than resolved, because a recorder that guessed
would file somebody's session under the wrong game. Three publishers shipping
`launcher.exe` is a real situation, and the caller — the process watcher, or
eventually the user — is in a far better position to settle it.

Two details that follow from the rungs:

- **A qualified rule never falls back to matching on the name alone.** The
  qualifier exists precisely because the name is not enough, so a process whose
  path is unknown does not match a qualified entry at all.
- **`child_processes` is not a way in.** A game's anti-cheat service or crash
  handler is not the game, and matching on that list would make every helper
  process a reason to start recording. The list is carried for whoever watches
  processes ([#41]) to use.

Comparison follows Windows: executable names and path fragments are matched
case-insensitively, and a `path_contains` fragment may be written with either
kind of slash.

#### A path qualifier names directories, not characters

The fragment is compared segment by segment, and it has to line up with
directory boundaries at both ends. This is a correctness rule rather than
tidiness. `steamapps/common/Half-Life 2` is a *substring* of
`steamapps/common/Half-Life 2 Deathmatch`, which is a different Steam
application — 320 rather than 220 — that installs beside Half-Life 2 and runs
the same `hl2.exe`. Half-Life 2: Lost Coast and both episodes are the same
shape. Matched as characters, the shipped Half-Life 2 entry claims all of them:
a confident wrong answer at rung 2, with nothing reported as ambiguous, which is
precisely the outcome the ambiguity rule above exists to avoid.

The same applies at the front of a fragment, so `common/Portal` is not found
inside `.../uncommon/Portal/...`.

Leading, trailing and doubled separators in a fragment change nothing, because
what is compared is the list of directory names either side of them. A fragment
that names no directory at all — `"/"` — matches nothing rather than everything;
validation cannot tell it from a real qualifier, since it is not empty, and an
entry that claimed every process on the machine is the failure this rung exists
to prevent.

### Fields for subsystems that do not exist yet

`default_settings` and `highlight_providers` are held and validated by the
catalogue and interpreted by nothing. Per-game settings are M7 (SPEC.md section
31) and the highlight provider API is M9, and inventing behaviour for either
here would be building a later milestone's work. They are carried rather than
omitted so that adding them later does not invalidate every entry contributed in
the meantime.

`icon` is the same: a name the desktop application will resolve, carried and not
resolved.

`compatibility` is a record of what somebody actually verified, not a
prediction. Every shipped entry says `unknown`, because nobody has yet run a
capture against these games and written down what happened; a test asserts that,
so a future entry cannot quietly claim otherwise without the evidence arriving in
the same pull request.

### Nothing is skipped quietly

A malformed entry fails the load with a message naming the file, the entry — by
position and by `game_id` — and what to write instead:

```text
C:\Users\somebody\AppData\Local\Clipped\games.toml: entry 2 (`my-game`): executable 1 has
`name = "C:/Games/MyGame/my-game.exe"`, which contains a directory separator; `name` is the
file name alone and the directory belongs in `path_contains`
```

Unknown keys are refused rather than ignored, so a misspelled `path_contian`
fails instead of silently doing nothing. Nothing is ever dropped and the rest of
the file loaded: a game silently missing from the catalogue is a recording that
silently does not happen.

### Schema versioning and the migration path

Both files carry `schema_version`, which is the version of the *format* and not
of Clipped. It changes when the shape of an entry changes and at no other time,
so adding a game never touches it. It is version 1 today.

| What Clipped finds | What happens |
| --- | --- |
| The current version | Read directly. The file is not rewritten, so a user's comments and layout survive. |
| An **older** version | Converted in memory, validated, then — for the overlay only — backed up and rewritten. |
| A **newer** version | Refused, with a message saying so. **Nothing is written back.** |

Refusing a newer file matters: an older Clipped rewriting a file it does not
understand is exactly how a user loses the entries they added on the machine
that was up to date.

Migrating the overlay happens in a careful order, because that file is the
user's and cannot be replaced from anywhere (AGENTS.md sections 43 and 56):

1. Read it. Nothing is written yet.
2. Convert it in memory and validate the result. If either fails, **the file has
   not been touched at all**, and the error says so.
3. Copy the original to `games.toml.v<old>.bak`. If that fails, stop — still
   without having touched the original.
4. Write the converted document to a neighbouring temporary file and rename it
   over the original, so an interrupted write leaves the previous file rather
   than half of a new one.

A migration therefore always leaves two readable files: the converted one, and
the exact bytes the user had before. That backup is not a nicety — TOML comments
do not survive the conversion, because the document is rewritten from its parsed
form.

The list of conversions is **empty today, and correctly so**: version 1 is the
first version there has ever been, so no file older than the current schema
exists anywhere, and writing a migration for a version that never shipped would
be inventing history. The machinery that runs them is built and tested now —
against conversions the tests supply themselves, including a two-step chain, a
step that refuses, a conversion whose result does not validate, and a backup that
cannot be taken. When version 2 arrives, the change is one entry in a list and
one function.

### Why this is not in the database

The catalogue is reference data that ships with the application and is read at
start-up. M6's [#55] introduces the SQLite schema and migration framework and
does not exist yet; adding a second persistence mechanism ahead of it would be
the accident AGENTS.md section 55 exists to prevent. The user overlay is a file
for the same reason it is separate from the seed data: it is the user's, it
should be editable by hand, and it should be obvious where it is.

[#55]: https://github.com/wildware-uk/clipped/issues/55
