# Game detection

Clipped records games without being told to, which means it has to know what a
game is. `clipped-game-detection` is where that knowledge lives.

**Status: the game catalogue exists ([#42]), the process watcher exists ([#41]),
Steam detection exists ([#43]) and a user can correct all of it ([#45]).** The
watcher reports what started and what stopped; the catalogue can say which game
a process is; Steam can say which of its applications an executable belongs to,
which is what the catalogue's strongest matching rung needs; and a user can
register a game nothing recognises, rename one, or exclude one that should never
be recorded. Nothing in *this* crate joins the watcher to the others,
deliberately — the layer that asks one about the other, and starts a recording,
is the session manager in `clipped-session` ([#46],
[sessions.md](sessions.md)), because deciding what to do about a game is not
detection's business. The other launchers are [#44], and the screen that drives
the registration API is [#63]/[#107]. This document grows a section per part as
they land, and marks the rest as intent (AGENTS.md section 7).

[#63]: https://github.com/wildware-uk/clipped/issues/63
[#107]: https://github.com/wildware-uk/clipped/issues/107

[#41]: https://github.com/wildware-uk/clipped/issues/41
[#42]: https://github.com/wildware-uk/clipped/issues/42
[#43]: https://github.com/wildware-uk/clipped/issues/43
[#44]: https://github.com/wildware-uk/clipped/issues/44
[#45]: https://github.com/wildware-uk/clipped/issues/45
[#46]: https://github.com/wildware-uk/clipped/issues/46

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
| Edited by | a pull request | the user by hand, or the registration API ([#45]) |
| On update | replaced wholesale | never touched |
| If the schema changes | rewritten by us | migrated, with a backup |

An overlay entry whose `game_id` matches a shipped one **replaces it entirely**
— it is not merged field by field. Merging would mean a user could add to a
shipped entry but never correct one, and would leave "which of the two `name`
fields won?" as a question about somebody's own file. An overlay entry with an
identifier of its own is simply another entry.

For correcting Clipped's own entries rather than replacing them, the overlay has
a second kind of block: see [Registering, renaming and excluding](#registering-renaming-and-excluding).

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

`[[decision]]` blocks ([#45]) were added to version 1 rather than making a
version 2, and that is worth justifying because it is the sort of decision that
is wrong later. Clipped has never been released — there is no build in anybody's
hands that would meet a file it cannot read — and a version 2 whose only change
was "there may now be blocks you have never seen" would migrate every existing
overlay: a backup, a rewrite, and the user's comments moved into a `.bak` file,
for a conversion that changes nothing. **After the first release the answer is
the other one**: a build in use is a build that can meet a newer file, and being
told "this file is from a newer Clipped" is worth a rewrite that says so. Until
then, a build old enough not to know `[[decision]]` refuses the file as an
unknown key and leaves it exactly as it is, which is the behaviour that matters.

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

### Registering, renaming and excluding

Detection is sometimes wrong, and everything above is what it is wrong *with*.
So a user can register an executable Clipped does not know, rename a game, and
exclude one that should never be recorded ([#45], SPEC.md section 6). All three
are the same file — the overlay above — through
`clipped_game_detection::catalogue::Overlay`, which is the API the settings
screen ([#63], [#107]) drives. There is deliberately no second store: a game the
user added is a catalogue entry, matched by the same rules and loaded by the same
loader as one Clipped ships (AGENTS.md section 55).

#### Two kinds of block

```toml
schema_version = 1

# A game nobody has contributed upstream: an entry of my own.
[[game]]
game_id = "obscure-game"
name = "Obscure Game"

[[game.executables]]
name = "obscure-game.exe"

# A game Clipped ships, and what I decided about it.
[[decision]]
game_id = "counter-strike-2"
name = "CS2"

[[decision]]
game_id = "some-launcher"
excluded = true
```

A `[[decision]]` names a `game_id` and says only what the user changed. It is
applied on top of whichever entry has that identifier — shipped or their own —
after the overlay's entries have been applied.

| | Written as | Why not the other way |
| --- | --- | --- |
| Describing a game yourself | `[[game]]` | — |
| Renaming a game Clipped ships | `[[decision]]` | A replacement entry would also freeze that game's executables, launcher identifier and icon at whatever this build shipped. The next release adds the executable a publisher renamed, and the one person it never reaches is the user who typed a shorter name. |
| Excluding a game | `[[decision]]` | **An exclusion is not a deletion.** The entry stays and the decision sits over it, so an update that changes the entry — or re-adds one this build does not have — cannot resurrect a game somebody excluded. |

Two consequences worth stating:

- **An excluded entry is still in the catalogue.** It is listed, it is found by
  `game_id`, and a session recorded before the exclusion still has a game to be
  filed under. What it never does is match a process, which is what stops a
  recording — there is no second switch anywhere else to keep in step.
- **A decision about a game no catalogue has is kept**, and reported by
  `Catalogue::pending_decisions`. Dropping it is the resurrection above wearing
  a different hat: a user who excludes a game and then runs a build whose seed
  data does not list it would find the exclusion quietly gone the next time it
  did.

#### The operations

| Operation | What it does |
| --- | --- |
| `Overlay::load` | The shipped catalogue with this overlay applied — the list a screen shows, each entry carrying its source and what was decided about it. |
| `Overlay::register` | Adds a `[[game]]`. The identifier is derived from the name (`My Game` → `my-game`) and made unique against everything already catalogued, so registering a game whose name collides with a shipped one adds a game rather than replacing it. |
| `Overlay::rename` / `clear_rename` | Changes the user's own entry where they have one, and writes a decision where the entry is Clipped's. `Entry::renamed_from` is the name underneath, so "reset" is an offer a screen can make. |
| `Overlay::exclude` / `include` | Writes or removes `excluded = true`. Including a game again removes the block rather than leaving `excluded = false` behind. |
| `Overlay::forget` | Removes the user's entry for a game and any decision about it: undo everything I said about this one. |

Every operation **states what the file should say** rather than toggling
anything. Excluding an excluded game is not an error, clearing a rename that is
not there is not an error, and asking for what the file already says writes
nothing at all — so a screen driving a checkbox has no state of its own to keep
in step.

An executable is registered by **file name**, not by path, so a registration
still matches after the game moves to another drive; a path qualifier is
available for the case that needs one — two games shipping the same executable
name — at the cost of that. A game that is uninstalled simply stops matching:
its entry stays, listed, until the user forgets it.

#### What an edit does, in order

1. **Reads the file through the loader that will read it at start-up.** A file
   this build could not load is not written to, and the edit fails saying so.
   Refusing to *read* a newer build's file preserves nothing on its own: the
   user opens the settings screen on the machine that is a version behind, sees
   a list without their entries, changes one thing, and the *save* is what
   destroys the file. The settings store learned this the same way ([#108],
   AGENTS.md section 56).
2. **Edits the document rather than a rendering of it.** The file is changed
   with `toml_edit`, so comments, ordering and layout survive an edit made from
   a screen. This file is one users are told to hand-edit and comment; a screen
   that silently deleted what they wrote in it would be destroying their data.
3. **Reads the result back before writing it**, through the same parser and the
   same validation the loader uses. A change that would not load — a rename to
   an empty name, an executable with a directory in it — fails with the message
   the loader would have given, and nothing is written.
4. **Writes through a temporary file and a rename**, so an interrupted write
   leaves the previous file rather than half of a new one.

The window between step 1 and step 4 is the one the settings store documents:
cross-process locking is [#194] and nothing smaller closes it.

[#108]: https://github.com/wildware-uk/clipped/issues/108
[#194]: https://github.com/wildware-uk/clipped/issues/194

#### Why a process matched what it did

A wrong detection is only fixable if a user can see what matched and why, so
`Catalogue::explain_process` returns the same answer `match_process` does —
computed once, by the same code — together with every entry that took an
interest and the verdict it reached:

| Verdict | What it means |
| --- | --- |
| `Claimed(rung)` | The entry matched at that rung. |
| `Excluded(rung)` | It would have, and the user excluded the game. |
| `PathElsewhere { fragment }` | The executable name matched and the game is running from somewhere the entry does not name. This is what a moved game looks like. |
| `PathUnknown { fragment }` | The name matched a path-qualified rule and no path was reported. The ordinary cause is a protected process an unelevated Clipped cannot open. |

Entries that took no interest are not listed: nine hundred entries saying "that
is not my executable" is not a diagnosis.

#### What is deliberately not here

**Per-game settings.** SPEC.md section 6 also asks for "disable capture per
game" and per-game overrides. An exclusion covers *never record this*, which is
the same observable behaviour; a setting that leaves a game recognised while
turning its recording off belongs to the configuration API ([#108],
[configuration.md](configuration.md)), which already has a per-game section and
already owns the settings file. Building a second one here would be the accident
AGENTS.md section 55 exists to prevent. `default_settings` on an entry is still
carried and interpreted by nothing.

#### Testing it

| What | Where | Needs |
| --- | --- | --- |
| That a rename survives an update that changes the shipped entry, that an exclusion is not a deletion, and that a decision with no game is kept | `src/catalogue/mod.rs` | nothing |
| That an excluded entry never matches, never breaks a tie, and says so in a report; and every verdict a report can carry | `src/catalogue/matching.rs` | nothing |
| Every edit: the file that must not be overwritten, the change that must not be written, the comments that must survive, the identifier that must not collide | `src/catalogue/overlay/edits.rs` | a temporary directory |
| The whole flow through the public API — unrecognised, registered, recognised, excluded, explained | `tests/registration.rs` | a temporary directory |

No test touches `%LOCALAPPDATA%`: every one of them names its own directory
(AGENTS.md section 25).

## Launcher detection

A launcher knows something no amount of looking at a process can tell you: which
of *its* games this is. That is the catalogue's strongest matching rung
(`LauncherIdentity`), and until [#43] nothing produced it. Launcher detection is
provider-based — one module per shop, so support for a new one is an addition
rather than a change to shared logic (SPEC.md section 6). Steam is the first;
Epic, Xbox, Battle.net, EA, Ubisoft, Riot and GOG are [#44] and are deliberately
not stubbed, because a provider that always answers "no" is a control that
silently does nothing.

There is no `trait LauncherProvider` yet either. One implementation is not
enough to know the shape of the abstraction, and [#44]'s launchers keep their
metadata in three different kinds of place.

### Steam

```text
HKCU\Software\Valve\Steam  SteamPath        ─── absent? then Steam is not installed
           │                                    and that is not an error
           ▼
<steam>\steamapps\libraryfolders.vdf        ─── every library, not just this one
           │
           ▼
<library>\steamapps\appmanifest_<appid>.acf ─── app id, name, install directory
           │
           ▼
Steam::candidate_for(name, path) ──▶ ProcessCandidate ──▶ Catalogue::match_process
```

Everything is read from files Steam wrote on this machine. There are no network
calls, by the ticket and by SPEC.md section 6, which also means detection works
offline, works while Steam is closed, and cannot be slowed down by Valve having
a bad afternoon.

#### Two libraries is the normal case

A machine with games on more than one drive is ordinary. The machine this was
developed against keeps three applications under `C:\Program Files (x86)\Steam`
and eighty-five under `B:\SteamLibrary`, so a detector that read only the
default library would miss almost everything on it. The library index is
therefore the first thing read and the default library is one entry in it rather
than a special case — and because Steam lists its own directory in that file,
libraries are de-duplicated by normalised path, or every application in the
default library would be reported twice.

The index is read from `steamapps\libraryfolders.vdf`, falling back to
`config\libraryfolders.vdf`, which is where it used to live; current clients
write both, and the two files were byte-identical on the machine above.

The list of applications comes from the manifests on disk rather than from the
`apps` table inside the index, because the index's copy is a cache of what the
directory already says.

#### Where Steam says it is

| Key | Value | Written by |
| --- | --- | --- |
| `HKEY_CURRENT_USER\Software\Valve\Steam` | `SteamPath` | the client, for this user |
| `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Valve\Steam` | `InstallPath` | the installer, for the machine |

The per-user value first, because the client keeps it current; the machine-wide
one after it, because a user account that has never launched Steam has no
per-user key while the installation is plainly there. `WOW6432Node` is not a
guess: Steam is a 32-bit application, so its installer writes under the
redirected key, and Clipped is a 64-bit process that has to name the redirected
key to see it.

**A hard-coded `C:\Program Files (x86)\Steam` is deliberately not a fallback.**
No registry entry means no Steam, and guessing would find the leftovers of an
uninstall or quietly report a real installation elsewhere as "not installed".

#### Missing Steam is an answer, not an error

`Steam::discover()` returns `Ok(None)` when there is no registry entry, and when
there is one naming a directory that has gone — uninstalling Steam leaves the
entry behind. A machine with no Steam on it is a machine Clipped runs perfectly
well on, so the caller detects nothing rather than telling the user something is
wrong.

#### One bad file does not blind the detector

| What is unreadable | What happens |
| --- | --- |
| The library index | **Fatal.** Without it there is no coherent view of anything, and guessing at a directory layout would be inventing data. |
| One application manifest | Collected into `Steam::problems()`, logged at `warn`, and the rest are read. |
| A whole library | The same. The ordinary cause is a drive that is not plugged in. |
| A manifest whose `installdir` leaves its library | The same. See below: it is refused rather than believed. |

The split is a trade rather than an inconsistency. Steam rewrites manifests
while games install, so a half-written one is an ordinary state of the disk, and
refusing to detect *any* game because of one would be the wrong answer for a
recorder. Nothing is dropped silently either way: every problem names its file,
and `problems()` is there so a diagnostics screen can say why one game is never
detected instead of leaving somebody to wonder.

**The messages redact their paths.** A problem's `Display` is what
`Steam::problems()` logs at `warn`, and a Windows path starts with the account
name and goes on to name the folders somebody chose. Every message therefore
names its file through `RedactedPath` — final component plus a digest of the
whole path — so a log file a user sends to a stranger says *which* manifest
without describing their disk (AGENTS.md section 13, `docs/logging.md`). The
whole path stays on the error and is reachable through `SteamError::path()`, for
the one caller that legitimately shows it: a diagnostics screen, on the user's
own machine.

#### A manifest is a file Clipped did not write

`installdir` is the value that matters, because `Steam::app_for_path` claims
every executable beneath an installation directory and the catalogue believes a
launcher identity above every other rung. Joined onto the library unchecked,
`"installdir" "C:\\Windows\\System32"` — or the same thing spelled
`..\..\..\..\Windows\System32` — would make Clipped record Notepad as whatever
game the manifest named, and record it *more* confidently than a game it
recognised properly.

So the value has to be a relative path of ordinary names: no drive or share, no
leading separator, no `.` or `..`, nothing empty. More than one component is
still allowed, because a nested `installdir` escapes nothing; all eighty-eight
manifests on the machine this was developed against name a single directory.
A value that is refused is a reported problem naming the manifest, not an
application and not a silent drop.

#### Icons

`SteamApp::icon()` is a **file on this machine**, and normally the real
application icon: a 32x32 JPEG in the application's own directory under
`appcache\librarycache`.

```text
appcache\librarycache\730\
    8dbc71957312bbd3baea65848b545be9eae2a355.jpg    32x32     the icon
    library_600x900.jpg                             300x450   the capsule
    library_hero.jpg                                1920x620
    logo.png
```

The icon is named for the SHA-1 `appinfo.vdf` records for it, so it cannot be
recognised by spelling — and Steam hashes the names of some large artwork too.
It is recognised by *being that shape* instead: the two numbers in a JPEG's
frame header are a documented format and about forty lines of reader, which is
not the guess at an undocumented binary layout that reading `appinfo.vdf` would
be. The hash is needed to name the file, not to find it, because the directory
is already the application's.

An earlier draft of this module reported the portrait capsule and said an icon
was not reachable without `appinfo.vdf`. That was wrong: of the 660 cached
applications on the machine it was written against, **none** has the
`<appid>_icon.jpg` file that draft looked for first and 511 have the hashed icon.
Reading the same 660 directories also settled how to pick it — taking the
*smallest* hashed JPEG would have called a 2048x2048 image an icon for four of
them.

When Steam has cached no icon, the artwork with names that say what they are is
reported instead — the portrait capsule, the header, the logo — which is visibly
not an icon and is better than nothing. `None` means Steam has cached nothing at
all, which is ordinary for a game installed but never shown in the library.
Nothing is fetched, nothing is decoded and nothing is invented.

#### KeyValues, and why the parser is ours

Both kinds of file are in Valve's KeyValues text format: a key followed either
by a value or by a braced table of more keys. `keyvalues-parser` and
`steamlocate` both exist; the rule (AGENTS.md section 10) is to ask whether the
functionality is small enough to implement safely, and here it is a tokeniser
and a loop — three token kinds, no schema — tested against text Valve's own
client wrote. Against that, a dependency brings a public API this crate would be
bound to and, for `steamlocate`, a whole opinion about what an installed game is
that the catalogue has already formed.

Four things the reader does that are worth knowing:

- **`\\` is one backslash.** Steam writes these files with escaping on, which is
  why every path in one is spelled `C:\\Program Files (x86)\\Steam`. Without
  this every library path is wrong.
- **An escape the format does not define keeps both characters**, where Valve's
  own reader would drop the backslash. A hand-edited `C:\Program Files` becoming
  `C:Program Files` is a path that is silently wrong; better visibly odd.
- **Keys are matched without regard to case**, because the format is
  case-insensitive and Steam has changed the capitalisation of these keys
  between client versions. A case-sensitive reader is a detector that stops
  working after an update, for no reason visible in a diff.
- **Parsing is iterative over an explicit stack, and nesting is capped.** A file
  with ten thousand opening braces in it is an error naming the line, not a
  blown stack — which on Windows is not an error at all but the end of the
  process.

Platform conditionals (`"key" "value" [$WIN32]`), `#base`, `#include` and binary
KeyValues are all outside what this reads. They do not appear in anything Steam
writes for itself, and a conditional is reported as a syntax error naming the
line rather than half-understood.

#### Matching an executable to an application

An executable belongs to an application when its path is **inside** the
application's installation directory, compared as whole directory names by the
same code the catalogue uses for `path_contains` — so `common\Portal` is not
found in `common\Portal 2`, and the installation directory itself is not a
program in it. Where two applications' directories nest, the innermost answers,
so a tool installed inside another game's directory is not reported as that
game.

`Steam::candidate_for` returns the `ProcessCandidate` the catalogue consumes,
with the launcher identity attached when Steam claims the path and left off when
it does not. Leaving it off is what makes the catalogue fall back to its path
and name rungs, so a game Steam has never heard of matches exactly as well as it
did before.

#### Testing it

| What | Where | Needs |
| --- | --- | --- |
| The KeyValues reader: escapes, comments, bare tokens, repeated keys, every way a file can be malformed | `src/launcher/keyvalues.rs` | nothing |
| That a missing registry value is not an error, and that a real one comes back whole | `src/launcher/steam/registry.rs` | Windows |
| That neither shape of "Steam is not installed" is an error, and every way an `installdir` can leave its library | `src/launcher/steam/mod.rs` | Windows for the first |
| That no problem's message carries a directory above the file it names, and that the whole path survives on the error | `src/launcher/steam/error.rs` | nothing |
| Reading a JPEG's dimensions: metadata segments, progressive frames, truncation, a file that is not a JPEG, and a hostile one | `src/launcher/steam/icon.rs` | nothing |
| Two libraries, the game in the second one, its name and icon, the catalogue answering by launcher identity, and every failure path | `tests/steam.rs` | nothing |

The fixtures under `tests/fixtures/steam/` are **files Steam wrote**, copied off
a real client with two libraries and scrubbed of the account identifier; their
README says exactly what was copied and what was changed. That is the point of
them: a KeyValues fixture written by hand agrees with the parser that reads it by
construction, and would prove nothing about Valve's tabs, Valve's escaping, or
the four nested tables at the bottom of every manifest. The only edit any test
makes is to substitute the two absolute library paths for temporary directories,
because a fixture cannot name `B:\SteamLibrary` on a machine that has no B
drive — and the substitution is asserted, so a fixture edited until those paths
no longer appear fails the test rather than quietly leaving it with no libraries
to find.

The one thing not taken from a real client is the artwork. A Steam icon is
somebody's copyrighted picture, and nothing in this crate decodes an image
anyway, so the tests build a JPEG *header* declaring the dimensions they want.
That is exactly the part the reader reads.

What no test does is read the developer's own Steam (AGENTS.md section 25).
That check is a probe, run by hand:

```powershell
cargo run -p clipped-game-detection --example steam_probe
cargo run -p clipped-game-detection --example steam_probe -- cs2.exe "B:\SteamLibrary\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\cs2.exe"
```

#### Assumptions and limits

- **A manifest is not a promise that a game is installed.** Steam writes one
  when a download *starts*, so an installation directory may not exist yet.
  Nothing here filters on `StateFlags`, because reading a flag nobody has
  verified the meaning of would be guessing about somebody's library.
- **Applications are not games.** Steam manages redistributables, tools,
  soundtracks and playtests with the same file. `Steam::apps()` reports what
  Steam says is installed and leaves deciding what is worth recording to the
  catalogue.
- **Paths are compared as text.** Two spellings of one directory — a substituted
  drive, a junction, a short 8.3 name — are two directories to this code, as
  they are to the catalogue's `path_contains`.
- **Nothing watches for changes.** An installation is read once; a caller that
  wants to notice a game installed since start-up reads it again, which is a few
  dozen small files.

## The process watcher

### What it does

```text
Windows ──▶ process started / exited ──▶ debounce ──▶ one launch, or one exit
```

The watcher reports two things, and deliberately not a third:

- **A launch**: every process the debounce collected into it, oldest first, with
  the executable path, the process identifier and the parent identifier of each.
- **An exit**: a process it had previously reported is gone, carrying what it
  was reported with.

It does not report *games*. It has no idea what a game is, and it does not
consult the catalogue above: the layer that puts the two together, and decides
that a launch is worth recording, is [#46]. Keeping them apart is what lets the
debounce be tested against process trees written down in a test, and the
matching rules against paths written down in a test, neither needing the other.

### Why it exists in this shape

#### The watcher does not poll

SPEC.md section 6 asks for automatic detection, which means something has to be
watching all the time, on a machine that is also running a game. A loop that
enumerates every process every few hundred milliseconds is precisely what
AGENTS.md section 18 forbids, so the primary source is a subscription: this
process blocks inside `IEnumWbemClassObject::Next` and Windows wakes it.

Windows offers three ways to be told, and only one of them is available to an
application running as an ordinary user:

| Mechanism | Latency | Standing cost | Requires |
| --- | --- | --- | --- |
| WMI `__InstanceCreationEvent` / `__InstanceDeletionEvent` `WITHIN n` over `Win32_Process` | up to `n` seconds | the WMI service compares the process table with itself once per `n` seconds, per subscription | nothing |
| `Win32_ProcessStartTrace` / `Win32_ProcessStopTrace` (WMI over ETW) | milliseconds | none until something happens | administrator |
| An ETW kernel session | milliseconds | none until something happens | administrator, and a session other tools compete for |

Clipped runs unelevated, because a recorder that demands administrator rights is
a recorder people run once. That rules the second and third rows out as the
primary source — a mechanism that works only for some users is not a detection
strategy — and leaves the first, which works for everybody at the cost of a
bounded delay and of work done in a service rather than here. What that cost
actually is, measured, is [below](#what-it-costs).

#### The fallback, and what triggers it

WMI is a service, and services stop. Its repository can be corrupted, group
policy can refuse the connection, and `winmgmt` can be restarted underneath a
working subscription. Every one of those ends with no events arriving, which for
a recorder means silently never recording again, so the failure is handled
rather than hoped against (AGENTS.md section 16):

```text
                 ┌─ established ──▶ WMI notifications ─── lost ──┐
start ──▶ try WMI┤                                               ├──▶ snapshot polling
                 └─ refused, or no answer within 10 seconds ─────┘         │
                                                                       lost │
                                                                            ▼
                                                             WatchEvent::Stopped
```

The fallback is `CreateToolhelp32Snapshot` on a timer, diffed against the
previous pass. It polls, which is the thing the design is trying to avoid, and
it is still worth having: the alternative when WMI is unavailable is no
detection at all. It is the only mechanism left that needs neither WMI nor
elevation.

Two things make the change visible rather than silent. `WatchEvent::SourceChanged`
is delivered to the caller with the reason, and `ProcessWatcher::declined_source`
reports why the preferred source was not used, so a diagnostics screen can say
"this machine is on the fallback, because …" instead of leaving somebody to
wonder why detection feels slow. If the fallback is lost too, the watcher
reports `WatchEvent::Stopped` and goes quiet — it never pretends to be watching.

The end of the stream is a distinct answer rather than an absence. `next_event`
returns `Next::Idle` when nothing happened in the timeout, which is the normal
state of a machine nobody is playing anything on, and `Next::Finished` once
every source is gone, which is not normal and means the user has to be told that
automatic detection has stopped. Spelling both as "no event" would leave a
consumer looping forever on the one that never changes, so they are different
values, and `Next` is deliberately not `#[non_exhaustive]`: a wildcard arm is
exactly the mistake the type exists to prevent. A finished watcher still waits
out its timeout, so that a loop which ignores the answer idles rather than
occupying a processor for the rest of the session.

#### A game launch is not one process

Steam starts a launcher, the launcher starts the game, an anti-cheat wrapper may
sit between them, and some titles re-execute themselves once. Reporting each of
those separately would have the session layer start and abandon three recordings
in two seconds, so starts are collected into a *launch* by following the parent
chain — which is why every event carries a parent — and reported once the chain
goes quiet.

The rules, in full:

1. A process that starts joins the launch its parent belongs to, if that launch
   is still being collected; otherwise it opens a new one.
2. A launch stays open until nothing has joined it for `launch_quiet_period`, up
   to `max_launch_window` from its first member. The cap is what stops a
   launcher that starts a helper every second from deferring its own launch
   indefinitely.
3. A process that exits before its launch is reported is removed from it, and
   neither its start nor its exit is ever reported. **This is what makes a
   launcher that hands over to the game and disappears invisible.** A launch that
   loses every member is dropped entirely.
4. A process that exits after its launch was reported is reported gone, with the
   executable and parent it was reported with. Nothing looks it up at that
   point, because there is nothing left to look up.
5. Exits are gathered for `exit_settle_period` and reported children first, so a
   parent and a child dying together arrive in an order that means something.
6. A process identifier that starts again while the watcher still believes it is
   alive means Windows reused it; the old one is reported gone first.

`LaunchGroup::newest` is the best single guess at "the thing that was actually
launched", and it is a guess with a proof behind it rather than a heuristic: a
process cannot start before its parent, so the last member to start has no
descendants inside the group.

### What it costs

All figures measured on:

| | |
| --- | --- |
| Machine | AMD Ryzen 9 9950X3D, 16 cores / 32 logical processors, Windows 11 Pro 26200 |
| Processes running | 370–390 |
| Build | `--release` |
| Window | 180 seconds per idle measurement; 20 runs for latency and for start-up |
| Method | cumulative `\Process V2(…)\% Processor Time` raw counters for the `Winmgmt` service host and every `WmiPrvSE.exe`, sampled before and after; `GetProcessTimes` for the watcher's own process (`examples/process_watch_probe.rs`) |

The machine was in ordinary use during these runs rather than quiescent — the
watcher saw 79 real process events in the 180-second window at the default
settings — so the control row is what the numbers are read against.

#### Starting the watcher

Paid once, and it is the most expensive thing the watcher ever does in its own
process: the baseline opens every process on the machine to ask what it is
running, because a game that was already running when Clipped opened still needs
a name to report when it exits.

| | min | median | max |
| --- | --- | --- | --- |
| `ProcessWatcher::start`, 20 runs | 167.6 ms | 240.6 ms | 305.5 ms |

370 processes were running. Windows gave up an executable path for 222 of them
and refused the other 148, which is the ordinary outcome rather than a fault: an
unelevated application cannot open a protected or higher-integrity process at
all. The figure includes establishing both WMI subscriptions as well as the
snapshot, so it is an upper bound on the baseline rather than the baseline
alone.

A quarter of a second, once, while the application is starting anyway. Measured
because the code claimed it was cheap, and a claim in a comment is not a
measurement (AGENTS.md section 7).

#### Idle cost

"Idle" here means the watcher is running and no game is being launched. It is
not a quiet machine: Windows starts and stops background processes constantly,
and that is the load the WMI comparison is doing work about.

| Configuration | WMI side, % of one core | Attributable to Clipped | Watcher process, % of one core |
| --- | --- | --- | --- |
| No watcher (control, two runs) | 2.47, 2.37 | — | — |
| `WITHIN 1` | 25.73 | +23.3 | 0.069 |
| `WITHIN 2` | 13.78 | +11.4 | 0.017 |
| **`WITHIN 4` (the default)** | **7.51** | **+5.1** | **below the 15.6 ms clock granularity** |

A second run of the same method, taken independently during review of
[#231](https://github.com/wildware-uk/clipped/pull/231), gave 2.08% for the
control and 14.43% at `WITHIN 2`: +12.4 rather than +11.4. Read that row as
*twelve per cent of one core, give or take one*. The shape either side of it —
roughly inverse in the interval — is what the sweep is for, and both runs agree
about that.

**The default is `WITHIN 4`.** SPEC.md section 38 budgets 3% of the machine for
the recorder; on a four-core machine `WITHIN 2` is about 2.9% of it, spent
before anything is being recorded, and `WITHIN 4` is about 1.3%. The two extra
seconds of detection latency are invisible against the ten to sixty seconds a
game takes to reach anything worth recording, so the cheaper interval is the
one that ships. [#230](https://github.com/wildware-uk/clipped/issues/230)
removes the trade-off rather than tuning it, by not standing a subscription for
exits at all — and questions whether a WMI subscription is the right primary
source when the snapshot poller this crate already ships as its fallback may
cost less than either.

**The interval drags the debounce with it.** `launch_quiet_period` must exceed
`notification_interval`, because a parent and its child can arrive in
consecutive notification batches and a quiet period shorter than one batch
cannot hold a launch open long enough to join them — it would report a launcher
as a game and record the wrong window. `the_quiet_period_outlasts_the_interval_it_watches`
enforces that, and it is what caught this when the interval was changed without
the debounce. So the quiet period moved from 2.5 s to 5 s alongside, and the
worst case from 4.5 s to **9 s**, not the 6.5 s the interval change alone would
suggest.

Nine seconds is still cheap against the ten to sixty a game takes to reach
anything worth recording, which is why this is the default anyway — but it is a
real cost and a bigger one than halving the CPU first appears to ask.

Detection latency was measured at `WITHIN 1` and `WITHIN 2`, **not** at
`WITHIN 4`, and not with the longer quiet period. The figures below are those
runs. The nine-second worst case is arithmetic from the two constants, not
something anybody has watched.

Read the two halves of the table separately, because they are not the same kind
of cost.

**The watcher's own process is close to free**: 31 ms of processor time in 180
seconds at the default settings, 0.017% of one core, or 0.0005% of a
32-processor machine. That is the point of not polling — the thread is asleep
except when something happens.

**The cost is in the WMI service, not here.** It lands in `Winmgmt` and
`WmiPrvSE.exe`, because the service compares the whole `Win32_Process` table with
itself once per interval, per subscription, and this watcher opens two. That is
work done on Clipped's behalf and it counts against Clipped, but no profile of
Clipped's own process will ever show it, which is exactly why it is measured with
performance counters here rather than assumed to be small.

**At the shipped default it is most of the recorder's entire idle CPU budget.**
SPEC.md section 38 allows the recorder 3% of the machine. Eleven to twelve per
cent of one core is 0.36% of this 32-processor machine, but about **2.9% of a
four-core machine** — spent while idle, before a game has been detected, before
anything is being recorded, and on top of whatever recording then costs. Two
seconds is already double the interval that would give the best latency, and it
is still not a comfortable number.

**[Issue #230](https://github.com/wildware-uk/clipped/issues/230) is what closes
the gap**, and it is a design change rather than a tuning one: exit detection
moves off the second subscription and onto waiting on process handles, which
removes half of this standing cost outright and takes exit latency from two
seconds to milliseconds at the same time. Until that lands, a machine with few
cores pays measurably for detection it may not be using. Whether the interim
default should be four seconds instead — 5.1% of one core, about 1.3% of a
four-core machine, for two more seconds of worst-case launch latency — is argued
on that issue rather than settled here, because it is a product decision about
latency and not a fact about the measurement.

Reporting this rather than a flattering summary is deliberate (AGENTS.md
section 19). "Negligible" would have been wrong by an order of magnitude.

#### Detection latency

Twenty runs at the default settings, with a `cmd.exe` started and ended by the
probe so that both moments are known exactly:

| | min | median | max |
| --- | --- | --- | --- |
| Process started → `WatchEvent::Launched` | 4.112 s | 4.142 s | 4.612 s |
| Process exited → `WatchEvent::Exited` | 2.134 s | 2.158 s | 2.282 s |

The launch figure includes the 2.5-second debounce; the source itself delivered
the creation event a median of 1.64 seconds after the process started. At a
one-second interval with a 1.5-second quiet period, the same twenty runs gave a
median launch latency of 2.330 s and exit latency of 1.004 s — one and a bit
seconds faster, for twice the standing cost.

Four seconds to notice a launch is comfortable for what this is for: a game
takes tens of seconds to reach anything worth recording, and the session layer
has only to be recording by then.

#### Taking the measurements again

```powershell
cargo run --release -p clipped-game-detection --example process_watch_probe -- latency 20
cargo run --release -p clipped-game-detection --example process_watch_probe -- idle 180
cargo run --release -p clipped-game-detection --example process_watch_probe -- start 20
# and, for the WMI side, the counters named in the table above
```

A third argument overrides the notification interval in seconds, which is how
the sweep was taken. The counter samples are taken either side of the `idle` run
and divided by its wall time; there is no sampling loop, so the measurement does
not pay for itself.

### Testing it

| What | Where | Needs |
| --- | --- | --- |
| The debounce rules — launcher and game, re-exec, two games at once, a process that comes and goes inside the window, exit ordering, identifier reuse | `src/process_watcher/debounce.rs` | nothing; constructed process trees and an explicit clock |
| The process table, the executable name, the stop latch | `src/process_watcher/windows/` | Windows |
| That WMI answers at all, that the fallback poller really reports a process starting and exiting, and that it honours the one-second floor rather than the interval it was handed | `src/process_watcher/windows/{wmi,mod}.rs` | Windows, and a working WMI service for the first |
| What a watcher that has lost every source answers, and that it waits rather than spins | `src/process_watcher/watcher.rs` | Windows |
| The whole watcher against a real process | `tests/process_watcher.rs` | Windows |

The split matters. "What did Windows say?" can only be answered by Windows and
has almost no logic in it; "is that one launch or three?" is all logic and needs
no machine at all, so it is a state machine over `(event, now)` and is tested
against process trees written down in the test rather than against whatever
happened to be running (AGENTS.md section 25).

The subject for the tests that do need a real process is `cmd.exe` with its
standard input piped and nothing else attached: it is on every Windows machine,
it starts immediately, and it exits when its input closes — so the test knows
the exact moment the process began and the exact moment it ended.

### Assumptions and limits

- **A process that starts in the gap between the baseline snapshot and the
  subscription is invisible** for the life of that watcher. The gap is tens of
  milliseconds. The other ordering was considered and is worse: it produces a
  spurious exit for a process that is still running.
- **Processes already running when the watcher starts are not launches.** They
  are remembered, so their exits are reported, and they are available through
  `ProcessWatcher::already_running`.
- **Executable paths are not always available.** Protected and higher-integrity
  processes — the anti-cheat services that sit alongside games, most system ones
  — refuse to be opened, and WMI reports a null `ExecutablePath` for them. The
  file name is always present.
- **An executable path is a user path.** It carries the account name and the
  shape of somebody's games library, so it reaches a log line only through
  `RedactedPath` (docs/logging.md).
- **Shutdown waits for the notification interval.** A thread blocked inside a
  COM call cannot be interrupted, so dropping the watcher takes up to
  `notification_interval` while that call returns.
- **The notification interval has a floor of one second, on both paths.**
  `WatchConfig` is public and so are its fields, so a consumer can ask for fifty
  milliseconds; it will get one second. The WQL `WITHIN` clause and the fallback
  poller's sleep are both derived from the same clamped value, because a floor
  that held only for the subscription would leave the poller enumerating every
  process on the machine twenty times a second — the thing this design exists to
  avoid.
