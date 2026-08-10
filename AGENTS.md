# AGENTS.md

# Agent Development Guidelines

This repository is an open-source desktop game recording application.

It is intended to become a long-lived public project maintained by multiple contributors, not a disposable prototype.

All agents working in this repository must optimise for:

- correctness
- maintainability
- readability
- performance
- testability
- documentation
- contributor friendliness

Do not optimise purely for completing the current ticket as quickly as possible.

---

# Work Tracking

GitHub issues are the source of truth for all work on this project:

```text
https://github.com/wildware-uk/clipped/issues
```

`SPEC.md` describes the product. It is not a task list and it is not updated to reflect progress. Every piece of work is tracked as an issue, and the issue is where scope, acceptance criteria and verification evidence live.

## Before starting

1. Work from an issue. If no issue covers the work, raise one first.
2. Read its acceptance criteria; they are the definition of the ticket, not a suggestion.
3. Check the milestone. Issues are grouped into milestones `M0` to `M14`, which follow the milestone order in `SPEC.md` section 42. Do not implement later-milestone work to satisfy an earlier ticket.
4. Check the `area:` labels to find the modules involved, and the linked issues for dependencies.

## While working

Reference the issue number in branches, commits and pull requests, so history explains itself later.

Keep the ticket honest about scope:

- Work discovered mid-ticket that is not required by the acceptance criteria belongs in a new issue, not in the current change (see section 40).
- Substantial `TODO` comments must link to an issue (see section 36).
- If the scope turns out to be wrong, say so on the issue and adjust it deliberately rather than silently widening the change.

## Before closing

An issue is closed when its acceptance criteria are met, not when code exists.

Record verification evidence on the issue in the form described in section 53: what was built, what was tested, what was measured, and what was manually verified. State anything that remains incomplete rather than closing over it.

Design work for the desktop application is tracked against the design project referenced from the UI issues. Build UI from that design system rather than inventing parallel styling.

---

# 1. General Principles

## Build production code

Treat all code as if it will be:

- publicly reviewed
- maintained for years
- modified by contributors unfamiliar with the original implementation
- used on thousands of different machines
- debugged without access to the original author

Avoid shortcuts that create hidden maintenance costs.

---

## Prefer simple solutions

Use the simplest architecture that correctly solves the problem.

Avoid:

- unnecessary abstraction
- speculative frameworks
- excessive indirection
- premature generalisation
- unnecessary dependencies
- clever code that sacrifices readability

A straightforward implementation is generally preferable to an impressive one.

---

## Do not over-engineer

Do not introduce:

- factories with one implementation
- interfaces that serve no architectural purpose
- generic frameworks for hypothetical future requirements
- deeply nested abstraction layers
- unnecessary dependency injection

Create abstractions when they solve a real problem.

---

# 2. Code Quality

Code must be:

- readable
- idiomatic
- cohesive
- appropriately modular
- consistently formatted

Names should clearly communicate intent.

Prefer:

```text
capture_game_audio()
```

over:

```text
process_audio()
```

Prefer:

```text
ReplayBuffer
```

over:

```text
Manager
```

Avoid vague names such as:

```text
Manager
Helper
Util
Processor
Handler
Thing
Data
Misc
```

unless the name genuinely describes the responsibility.

---

# 3. Functions

Functions should generally do one coherent thing.

Avoid extremely large functions containing:

- validation
- capture setup
- encoding
- persistence
- UI communication
- error handling

all mixed together.

Extract meaningful operations where doing so improves comprehension.

Do not split trivial logic into dozens of tiny functions purely to satisfy arbitrary line-count rules.

---

# 4. Modules

Modules should represent clear domains.

Example:

```text
capture/
audio/
encoder/
muxer/
games/
sessions/
replay/
storage/
library/
plugins/
```

Avoid circular dependencies.

Lower-level modules should not depend on high-level UI/application concerns.

For example:

```text
capture
```

must not depend on:

```text
desktop-ui
```

---

# 5. Architecture Boundaries

Maintain clear separation between:

```text
Capture Engine
Application Logic
Persistence
Desktop UI
Plugins
Platform APIs
```

The recording engine must remain usable independently from the desktop UI.

The UI should communicate with the recorder through an explicit application/service boundary.

Platform-specific code should remain isolated.

Example:

```text
audio/
    mod.rs

    windows/
        process_loopback.rs

    linux/
        ...
```

rather than spreading Windows API calls throughout unrelated modules.

---

# 6. Comments

Do not comment obvious code.

Bad:

```text
// Increment count
count += 1
```

Comments should explain:

- why something exists
- unusual platform behaviour
- non-obvious performance decisions
- API limitations
- concurrency assumptions
- workarounds
- protocol behaviour
- ownership/lifetime constraints

Good:

```text
Windows may continue returning audio packets briefly after the
target process exits, so the capture stream is intentionally drained
before closing the muxer.
```

---

# 7. Documentation

Public APIs should be documented.

Important modules should explain:

- purpose
- responsibilities
- ownership
- assumptions
- threading model
- lifecycle

Complex subsystems should have supporting documentation under:

```text
docs/
```

Examples:

```text
docs/architecture.md
docs/capture-pipeline.md
docs/audio-routing.md
docs/replay-buffer.md
docs/plugin-api.md
```

Documentation must be updated when behaviour changes.

Do not knowingly leave documentation describing obsolete behaviour.

---

# 8. README

Keep the project README useful for new contributors.

It should eventually contain:

- what the project does
- supported platforms
- screenshots
- current project status
- installation
- building from source
- development setup
- project architecture
- contribution instructions
- licence
- links to technical documentation

Do not fill the README with marketing copy.

---

# 9. Open-Source Considerations

Assume every implementation may be read, forked and modified externally.

Avoid relying on:

- undocumented local setup
- proprietary internal tooling
- personal infrastructure
- hardcoded machine paths
- private credentials
- private APIs

A contributor should be able to clone the repository and understand how to build it.

---

# 10. Dependencies

Every dependency increases:

- security surface
- compile time
- binary size
- maintenance burden
- licence complexity

Before adding a dependency, ask:

1. Is it genuinely necessary?
2. Is it maintained?
3. Is its licence compatible?
4. Is the functionality small enough to implement safely ourselves?
5. Does an existing dependency already provide it?

Avoid duplicate libraries solving the same problem.

---

# 11. Dependency Licensing

Because this is an open-source project, dependency licences matter.

Before introducing a dependency:

- identify its licence
- verify compatibility with the project's licence
- avoid dependencies with unclear licensing
- document unusual licensing requirements

Never copy source code from another project without confirming licence compatibility and preserving required attribution.

---

# 12. Third-Party Code

When adapting code from another project:

- verify the licence
- preserve attribution where required
- document the source
- document significant modifications
- avoid copying more than necessary

Do not present third-party code as original project code.

---

# 13. Security

Never commit:

- passwords
- API keys
- authentication tokens
- signing certificates
- private keys
- personal information

Use environment variables or documented local configuration where necessary.

Do not log sensitive data unnecessarily.

Recording software can potentially interact with sensitive user content, so logs must avoid capturing:

- window contents
- microphone content
- private message contents
- file contents

unless explicitly necessary and expected.

---

# 14. Privacy

The application is local-first.

New features must not introduce network communication silently.

Any functionality involving:

- telemetry
- analytics
- crash uploads
- cloud storage
- online accounts
- remote APIs

must be explicit.

No hidden tracking.

No advertising SDKs.

No behavioural analytics by default.

---

# 15. Error Handling

Do not silently swallow errors.

Bad:

```text
let _ = save_recording();
```

unless failure is intentionally irrelevant and documented.

Errors should contain useful context.

Prefer errors such as:

```text
Failed to initialise NVENC encoder for 2560x1440 AV1 recording
```

rather than:

```text
Encoder failed
```

User-facing errors should remain understandable without exposing unnecessary implementation detail.

Diagnostic logs may contain deeper technical information.

---

# 16. Expected Failure

Recording software interacts heavily with external system state.

Assume things will fail.

Examples:

- game closes unexpectedly
- GPU driver resets
- encoder becomes unavailable
- audio device disappears
- output drive is disconnected
- disk becomes full
- resolution changes
- monitor disappears
- system sleeps
- Windows changes the default audio device

Design recovery behaviour explicitly.

Do not rely solely on happy paths.

---

# 17. Recording Safety

Protect user recordings above almost everything else.

Where possible:

- write incrementally
- use recoverable containers
- flush important metadata
- tolerate abrupt termination
- avoid keeping irreplaceable state only in memory

A UI failure should not destroy an active recording.

A metadata database failure should not corrupt video files.

---

# 18. Performance

This application runs alongside games.

Performance-sensitive code must be treated accordingly.

Avoid unnecessary:

- memory copying
- frame copying
- GPU/CPU synchronisation
- allocation in tight loops
- filesystem polling
- high-frequency database writes
- serialization
- locks on capture threads

Prefer:

- hardware encoding
- bounded queues
- asynchronous processing
- reusable buffers
- zero-copy paths where practical

Do not claim an optimisation improves performance without measuring it.

---

# 19. Performance Measurements

Performance-related changes should ideally record before/after measurements.

Useful metrics include:

```text
CPU usage
RAM usage
GPU usage
encoder usage
frame capture latency
encode latency
dropped frames
disk throughput
audio drift
```

Benchmarks should document:

- hardware
- game/workload
- resolution
- frame rate
- codec
- encoder
- test duration

---

# 20. Threads and Concurrency

Concurrency must be understandable.

Document important threading assumptions.

Avoid:

- uncontrolled thread creation
- hidden blocking operations
- holding locks during expensive operations
- sharing mutable state unnecessarily

Capture threads should avoid waiting on:

- UI
- database
- thumbnails
- network
- plugin processing

Non-critical work should happen asynchronously.

---

# 21. Audio Correctness

Audio routing is a core feature.

Never silently combine sources that are expected to remain isolated.

The primary track model includes:

```text
Compatibility Mix
Game Audio
Other System Audio
Microphone
Application-specific tracks
```

Changes to audio routing must test source isolation.

Audio functionality should also account for:

- clock drift
- sample-rate differences
- device disconnects
- device switching
- application process trees
- duplicate capture
- clipping
- channel layouts

---

# 22. Media Correctness

Generated media must be validated.

Do not assume successful encoder/muxer calls mean the recording is valid.

Tests should verify where appropriate:

- container opens
- video stream exists
- expected audio streams exist
- duration is plausible
- timestamps are monotonic
- A/V synchronisation is acceptable
- codec metadata is correct

Tools such as `ffprobe` may be used in development/testing to inspect generated media.

---

# 23. Tests

Add tests wherever they provide meaningful confidence.

Use:

### Unit tests

For isolated logic.

Examples:

- replay ranges
- storage cleanup rules
- configuration resolution
- game matching
- event transforms

### Integration tests

For subsystem interaction.

Examples:

- encoder + muxer
- session + database
- plugin + event system

### System tests

For real capture behaviour where feasible.

Examples:

- capture test window
- record generated audio
- verify resulting media streams

Do not write meaningless tests purely to increase coverage.

---

# 24. Bug Fixes

Whenever practical, a bug fix should include a regression test.

The preferred process is:

```text
Reproduce
↓
Write failing test
↓
Fix
↓
Confirm test passes
```

If automated reproduction is impractical, document the manual reproduction and verification procedure.

---

# 25. Test Determinism

Tests should not depend unnecessarily on:

- internet connectivity
- current time
- random machine state
- personal files
- installed games
- specific user devices

Use fixtures and controlled test applications where possible.

---

# 26. Test Utilities

Create dedicated test applications for capture testing.

For example:

```text
test-apps/
    video-pattern/
    audio-generator/
    process-tree-audio/
    fullscreen-dx11/
```

A controlled audio test could generate:

```text
Game channel:
440 Hz

System channel:
880 Hz

Microphone simulation:
1320 Hz
```

The resulting recording can then automatically verify that tracks remain isolated.

This is preferable to manually testing Spotify + Discord repeatedly.

---

# 27. UI Development

UI code should reflect actual application state.

Avoid fake placeholder data making unfinished features appear functional.

If a backend feature is unavailable, the UI should clearly represent that state.

Do not implement UI controls that silently do nothing.

---

# 28. UI Copy

Keep copy concise.

Prefer:

```text
Replay saved
```

over:

```text
Awesome! Your replay has been successfully saved and is ready to view!
```

Prefer:

```text
Microphone disconnected
```

over vague messages such as:

```text
Something went wrong with your audio.
```

---

# 29. Avoid AI-Generated Design Clichés

Do not generate generic "AI SaaS" interfaces.

Avoid:

- excessive cards
- meaningless dashboard statistics
- giant welcome banners
- marketing text inside the application
- unnecessary gradients
- excessive pills
- excessive rounded containers
- decorative charts
- emojis as primary UI icons
- vague buttons like "Explore" and "Enhance"
- fake scores and percentages
- unnecessary AI assistants

The software is a desktop utility.

Prioritise:

- information density
- hierarchy
- predictable controls
- useful state
- direct actions

---

# 30. Configuration

Settings should have:

- sensible defaults
- explicit types
- validation
- backwards-compatible migration where practical

Do not scatter configuration reads throughout the codebase.

Resolve configuration through clearly defined configuration APIs.

Per-game settings should inherit from global settings unless explicitly overridden.

Example:

```text
Global:
60 FPS

Counter-Strike 2:
120 FPS

Minecraft:
inherits 60 FPS
```

---

# 31. Database Changes

Database schema changes must use migrations.

Never rely on users deleting their database after an update.

Migrations should be:

- deterministic
- forwards-safe
- tested
- recoverable where practical

Do not store large media blobs inside SQLite.

Store file references and metadata.

---

# 32. File Formats

Recorded media should remain usable outside this application.

Avoid proprietary containers or unnecessary custom binary formats.

Metadata specific to the application should either:

- live in SQLite
- use documented sidecar formats
- use standard container metadata where appropriate

Users must retain access to their recordings even if they stop using the application.

---

# 33. Plugins

Game-specific integrations belong behind a plugin/provider boundary.

Plugins must not bypass core architecture.

A plugin should expose events through a stable abstraction such as:

```text
GameStarted
MatchStarted
Kill
Death
Win
Custom
```

rather than forcing the core application to understand every game's native protocol.

---

# 34. Plugin Safety

Do not implement game integrations using techniques likely to resemble cheats.

Avoid:

- DLL injection
- process memory modification
- code injection
- anti-cheat bypasses

Prefer:

- official APIs
- local telemetry
- game logs
- Game State Integration
- documented IPC
- supported replay files

User account safety takes priority over richer highlight detection.

---

# 35. Logging

Use structured logging.

Include useful context such as:

```text
session_id
game_id
capture_backend
encoder
audio_source
```

Avoid extremely noisy per-frame logs.

Logging should be configurable.

Debug-level diagnostics must not significantly affect recording performance when disabled.

---

# 36. TODOs

TODO comments must describe real outstanding work.

Bad:

```text
TODO: fix
```

Good:

```text
TODO(#184): WGC reports the old frame size for one frame after
a fullscreen resolution transition. Delay encoder resize until the
new size appears twice consecutively.
```

Where an issue tracker exists, link substantial TODOs to an issue.

Do not leave TODOs for work required to satisfy the current ticket.

---

# 37. Dead Code

Remove dead code.

Do not keep:

- abandoned implementations
- commented-out code
- unused experiments
- duplicate old modules

Version control already preserves history.

---

# 38. Feature Flags

Feature flags should only exist when useful.

Appropriate uses:

- experimental capture backend
- optional codec support
- platform-specific functionality

Do not use feature flags to permanently hide unfinished broken implementations.

---

# 39. Commit Scope

Keep changes focused.

Avoid combining unrelated:

```text
refactors
formatting
feature work
dependency upgrades
bug fixes
```

into one large change unless there is a genuine reason.

Small coherent changes are easier to review and revert.

---

# 40. Refactoring

Do not perform broad unrelated refactoring while implementing a small task.

If necessary refactoring is discovered:

1. make the minimum required change
2. document larger cleanup separately
3. avoid unexpectedly expanding the ticket

However, do not preserve obviously dangerous code purely to keep diffs small.

Use judgement.

---

# 41. Formatting

Use automated formatting.

Repository formatting tools are authoritative.

Do not manually fight the formatter.

CI should eventually verify formatting.

---

# 42. Linting

Treat meaningful compiler and linter warnings seriously.

Do not solve warnings using blanket suppression.

Bad:

```text
#[allow(warnings)]
```

Prefer fixing the cause.

Local suppressions are acceptable when justified and documented.

---

# 43. Compatibility

Do not unnecessarily break:

- configuration
- database schema
- plugin APIs
- stored recording metadata
- command-line arguments

Breaking changes must be intentional and documented.

---

# 44. Public APIs

Public interfaces should be deliberately designed.

Do not expose implementation details simply because it is easier.

Before making something public, ask:

> Do external modules actually need this?

Keep the API surface small.

---

# 45. Error Recovery UX

When something fails, give the user a useful action.

Example:

```text
Microphone unavailable

Shure MV7 was disconnected.

Use Default Microphone
Choose Device
```

rather than:

```text
Error 0x88890004
```

Technical codes can still appear in diagnostics.

---

# 46. Accessibility

Desktop UI should support:

- keyboard navigation
- visible focus
- screen reader labels
- scalable text
- sufficient contrast
- non-colour-only state indicators

Do not require precise mouse interaction for core workflows.

---

# 47. Documentation for Contributors

New subsystems should document enough context that a contributor can answer:

```text
What does this do?

Why does it exist?

Where does it sit in the architecture?

How do I run it?

How do I test it?

What assumptions does it make?
```

If those questions cannot be answered without reading the entire implementation, documentation is insufficient.

---

# 48. Architecture Decisions

Significant architectural choices should be recorded as ADRs.

Example:

```text
docs/adr/
    0001-use-mkv-for-recording.md
    0002-separate-recorder-process.md
    0003-process-specific-audio-capture.md
```

An ADR should briefly explain:

```text
Context
Decision
Alternatives
Consequences
```

Do not create ADRs for trivial implementation decisions.

---

# 49. Contributor Experience

Optimise for a clean development workflow.

Aim for commands such as:

```text
cargo build
cargo test
npm install
npm run dev
```

rather than undocumented multi-step environment setup.

Provide scripts where repeated setup is unavoidable.

Error messages from development scripts should explain missing dependencies.

---

# 50. Platform Requirements

Document platform requirements explicitly.

Examples:

```text
Minimum Windows version
Visual Studio Build Tools
Windows SDK version
GPU driver expectations
FFmpeg/lib dependencies
Node version
Rust version
```

Prefer pinned or bounded toolchain versions where reproducibility benefits.

---

# 51. CI

CI should eventually verify:

```text
Formatting
Linting
Compilation
Unit tests
Integration tests
Licence checks
Dependency vulnerability checks
```

Do not merge code that only works on the original developer's machine.

---

# 52. Definition of Done

A ticket is not complete because code was written.

A feature is complete when applicable requirements are satisfied:

```text
Implemented
↓
Builds
↓
Tests pass
↓
Behaviour verified
↓
Errors handled
↓
Documentation updated
↓
No known regression introduced
```

Agents must not claim completion if major requirements remain mocked, disabled or unverified.

---

# 53. Verification Evidence

When completing a task, report what was actually verified.

Example:

```text
Verification

- cargo test passes
- cargo clippy passes
- recorded 30-second test session
- ffprobe confirms:
  - 1 AV1 video stream
  - compatibility audio track
  - game audio track
  - microphone audio track
- manually verified tracks can be muted independently
```

Do not simply state:

```text
Everything works.
```

---

# 54. Do Not Fake Completion

Never:

- replace implementation with mock data
- hardcode expected output
- skip unsupported cases without mentioning them
- hide failures
- disable failing tests
- remove assertions to make tests pass
- mark functionality complete without exercising it

If a requirement cannot be completed, state exactly what remains.

---

# 55. Existing Code Comes First

Before implementing a feature:

1. inspect the relevant architecture
2. search for existing utilities
3. understand current patterns
4. reuse appropriate infrastructure

Do not create a second implementation of functionality that already exists.

---

# 56. Preserve User Data

Changes involving recordings, clips or databases must prioritise data preservation.

Do not:

- delete media during migrations
- overwrite source recordings during edits
- automatically destroy incompatible metadata

Prefer migrations, backups or recovery paths.

---

# 57. Non-Destructive Editing

Editing should normally reference original recordings.

Represent edits as metadata where practical:

```text
source
in
out
audio_levels
cuts
overlays
```

Do not re-encode or modify source recordings merely because the user created a clip.

---

# 58. Resource Ownership

Native resource ownership must be explicit.

Examples:

- GPU textures
- encoder sessions
- Windows handles
- audio clients
- file handles

Prefer deterministic cleanup.

Leaks in a process intended to run for many hours are serious bugs.

---

# 59. Long-Running Behaviour

Test important functionality over extended sessions.

Many bugs only appear after:

- hours of recording
- multiple game launches
- repeated replay saves
- audio device changes
- large media libraries

Design components assuming the recorder may remain active for days.

---

# 60. Final Rule

When deciding between:

```text
fast but fragile
```

and:

```text
slightly slower to implement but clear, testable and maintainable
```

prefer the second.

This project should remain understandable after hundreds of contributors and years of development.

Leave the codebase cleaner than you found it.