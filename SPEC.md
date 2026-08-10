# Open-Source Automatic Game Recorder — Agent Build Specification

## 1. Product Vision

Build a completely open-source Windows game recording application combining the best parts of:

- NVIDIA ShadowPlay / NVIDIA App
- Outplayed
- Medal
- SteelSeries Moments
- OBS Replay Buffer
- Segra

The application should behave like software that the user **installs once and mostly forgets about**.

When the user launches a game:

1. Detect the game automatically.
2. Start the configured capture mode.
3. Record with hardware acceleration.
4. Capture audio into independent editable tracks.
5. Detect/bookmark interesting events where possible.
6. Stop automatically when the game closes.
7. Organise the resulting footage by game/session.
8. Make clips immediately searchable, editable and exportable.

The software must work entirely locally without requiring an account or cloud service.

Open-source should not mean "OBS with a different UI."

The application should feel like a polished consumer product.

---

# 2. Core Design Principles

## Automatic

The user should not need to:

- launch the recorder
- create scenes
- configure sources
- manually select game windows
- manually route Windows audio
- start/stop recordings

after initial setup.

## Lightweight

Gameplay performance takes priority over everything except recording correctness.

Background UI, metadata processing and analysis must never interfere with the game.

## Local-first

Everything works without:

- registration
- cloud services
- telemetry
- subscription
- internet connectivity

Cloud sharing may eventually exist as an optional extension.

## Non-destructive

Original recordings must remain untouched unless explicitly deleted.

Editing should normally create:

- edit metadata
- clips referencing source footage
- exported renders

rather than modifying source recordings.

## Extensible

Game detection, highlight detection and event integrations must use a plugin/provider architecture.

---

# 3. Primary Platform

Initial target:

**Windows 11 / modern Windows 10**

Architecture should leave room for Linux later but Windows quality takes precedence.

---

# 4. Recommended Technology

## Capture engine

Use **Rust** for the main native recording engine.

Reasons:

- native Windows API access
- strong concurrency model
- low runtime overhead
- memory safety
- excellent FFI
- suitable for a permanently running system application

Low-level C/C++ libraries may be integrated where necessary.

Do not implement capture-critical functionality on the JVM or in JavaScript.

## UI

Recommended:

**Tauri + React/TypeScript**

Rust backend communicates with a lightweight web UI.

Alternative UI frameworks can be considered if they materially improve GPU/video integration.

The recording process must not depend on the UI process remaining alive.

## Media layer

Use FFmpeg libraries where appropriate for:

- muxing
- demuxing
- thumbnails
- transcoding
- remuxing
- waveform generation
- exports

Do not shell out to `ffmpeg.exe` for the real-time capture pipeline unless unavoidable.

## Database

SQLite.

Use it for:

- games
- sessions
- recordings
- clips
- bookmarks
- events
- favourites
- tags
- recording settings
- storage metadata

Video files themselves remain normal files.

---

# 5. System Architecture

Split the application into independent modules.

```text
Desktop UI
    │
    ▼
Application Service
    │
    ├── Game Detector
    ├── Session Manager
    ├── Capture Coordinator
    ├── Replay Buffer
    ├── Audio Router
    ├── Encoder Manager
    ├── Event/Highlight Engine
    ├── Media Library
    ├── Storage Manager
    ├── Export Engine
    └── Plugin Manager

Native Capture Engine
    ├── Game Capture
    ├── Screen Capture
    ├── Audio Capture
    ├── Hardware Encoder
    └── Media Muxer
```

The recording engine should preferably run as an independent process.

If the UI crashes, recording must continue.

---

# 6. Game Detection

Automatically detect when games launch.

Detection sources should include:

- running processes
- Steam
- Epic Games
- Xbox / Microsoft Store
- Battle.net
- EA
- Ubisoft
- Riot
- manually registered executables

Maintain a local game database mapping:

```text
game_id
name
executables
launcher
icon
capture compatibility
known child processes
default settings
highlight providers
```

Users must be able to:

- add an unknown executable
- rename detected games
- exclude applications
- disable capture per game
- configure per-game overrides

SteelSeries Moments currently performs launcher/game detection and automatically starts capture when recognised games launch; this behaviour should be considered baseline functionality.

---

# 7. Capture Modes

Support four major modes.

## Full Session

Start when game starts.

Stop when game exits.

Everything is captured.

Comparable to Outplayed's Full Session mode.

## Match Recording

Record only actual matches where an integration can determine:

```text
MATCH_START
MATCH_END
```

Menus/lobbies are excluded or stored separately.

## Highlights Only

Maintain a replay buffer.

When an event occurs:

```text
KILL
DEATH
ASSIST
WIN
GOAL
BOSS_KILL
CUSTOM_EVENT
```

save a configurable period surrounding it.

Example:

```text
15 seconds before
+
10 seconds after
```

Outplayed and SteelSeries already provide event-triggered clipping for supported games, so this should be considered a competitive requirement rather than a novelty.

## Manual / Replay Buffer

Always maintain a rolling buffer while the game is active.

Default hotkey:

```text
Ctrl + F10
```

Example:

```text
Save previous 60 seconds.
```

User-selectable periods:

- 15 sec
- 30 sec
- 1 min
- 2 min
- 5 min
- custom

---

# 8. Video Capture

Capture methods should be selected dynamically.

Preferred hierarchy:

```text
Game Capture
↓
Windows Graphics Capture
↓
Desktop Duplication / Screen Capture
```

If one mechanism fails, automatically fall back where safe.

The user should see:

```text
Capture method: Automatic
Current method: Game Capture
```

rather than needing to understand capture APIs.

---

# 9. Hardware Encoding

Automatically detect GPU capabilities.

Support:

## NVIDIA

NVENC:

- H.264
- HEVC
- AV1 where supported

NVIDIA officially exposes hardware encoding through its Video Codec SDK.

## AMD

AMF:

- H.264
- HEVC
- AV1 where supported

## Intel

Quick Sync:

- H.264
- HEVC
- AV1 where supported

## Software fallback

x264 or equivalent.

Hardware encoding should be preferred automatically.

---

# 10. Recording Quality

Presets:

```text
Performance
Balanced
High
Ultra
Custom
```

Custom settings:

- resolution
- framerate
- bitrate/quality
- codec
- encoder
- HDR
- audio bitrate
- colour format
- colour space
- recording container

Support at least:

- 720p
- 1080p
- 1440p
- 4K

FPS:

- 30
- 60
- 90
- 120
- 144

where hardware allows.

Segra currently advertises capture up to 4K/144 FPS and H.264, HEVC and AV1 across NVENC, AMF and QSV; matching this eventually makes sense.

---

# 11. CRITICAL FEATURE — True Multi-Track Audio

This is a core architectural requirement.

Do not simply record:

```text
Desktop Audio
Microphone
```

Instead automatically create independent sources.

Example recording:

```text
Video Stream 0
Audio Track 1 — Game
Audio Track 2 — Other System Audio
Audio Track 3 — Microphone
Audio Track 4 — Voice Chat [optional]
Audio Track 5+ — custom application sources
```

## Game Audio

Capture only audio emitted by the detected game's process tree.

Windows supports application loopback capture scoped to a process and its child processes through `ActivateAudioInterfaceAsync`.

## Other System Audio

Capture:

```text
All system audio
MINUS
Game process tree
```

Examples:

- Spotify
- browser
- notification sounds
- media players

Windows' process-loopback API supports the inverse mode required to capture everything except a target process tree.

## Microphone

Capture the selected input device independently.

## Voice Chat

Allow optional app-specific tracks.

Examples:

```text
Discord
TeamSpeak
Steam Voice
Browser
```

Eventually allow arbitrary routing:

```text
Discord → Track 4
Spotify → Track 5
Chrome → excluded
```

Medal now supports arbitrary application audio tracks, proving this UX is viable.

---

# 12. Audio Track UX

The user should see something like:

```text
Recording Audio

Game                    ✓  100%
Microphone              ✓   85%
Other PC Sounds         ✓   50%
Discord                  ✓  100%
Spotify                  ✕
```

Advanced view:

```text
Application                  Track
------------------------------------------------
Cyberpunk2077.exe             Game
Discord.exe                   Voice Chat
Spotify.exe                   Music
chrome.exe                    System
Microphone                    Microphone
```

Audio configuration must survive restarts.

---

# 13. Audio Compatibility Track

Some players incorrectly choose one audio track from multi-track files. Medal itself documents this issue with several Windows applications.

Therefore optionally include:

```text
Track 1 — Mixed Playback
Track 2 — Game
Track 3 — System
Track 4 — Microphone
Track 5 — Discord
...
```

Track 1 contains the normal user-configured mix.

Opening the file casually therefore sounds correct.

Editors can access every isolated source.

This should be the default.

---

# 14. Audio Processing

Per-source processing:

- gain
- mute
- noise suppression
- noise gate
- compressor
- limiter
- optional automatic microphone gain

The raw/pre-filter microphone track should optionally also be preserved.

Example:

```text
Track 4 — Microphone Processed
Track 5 — Microphone Raw
```

---

# 15. Containers

Primary archival format:

**MKV**

Benefits include resilience against incomplete recordings and flexible stream support.

Allow automatic remux:

```text
MKV → MP4
```

without re-encoding.

Provide:

```text
Recording format:
● MKV — Recommended
○ MP4
```

---

# 16. Replay Buffer Architecture

Do not store the replay buffer as raw frames.

Continuously hardware encode packets into rolling media segments.

Example:

```text
segment-0001
segment-0002
segment-0003
...
```

Keep only the configured time window.

When clip is requested:

1. identify relevant segments
2. retain segments
3. construct clip
4. continue capture uninterrupted

This prevents replay-buffer RAM consumption scaling dramatically with duration/resolution.

Support long buffers such as:

```text
30 seconds
1 minute
5 minutes
10 minutes
30 minutes
```

with disk-backed buffering where appropriate.

---

# 17. Recording Library

Home screen:

```text
Recent Sessions
Recently Clipped
Favourites
Games
```

Games view:

```text
Counter-Strike 2
217 sessions
48 clips
16 favourites
83 GB
```

Session:

```text
Counter-Strike 2
10 August 2026
21:04–22:37

Match 1
Match 2
Match 3
Misc
```

Outplayed already separates sessions/matches and bookmarks game events; this is good baseline behaviour.

---

# 18. Timeline

Every recording has a timeline.

Display:

```text
────────●────⚔────☠────────★──────
        kill death      bookmark
```

Markers may originate from:

- game integrations
- manual bookmarks
- clipping
- microphone activity
- screenshots
- custom plugins

Click marker → jump directly to event.

---

# 19. Clip Editor

Keep the editor deliberately lightweight.

This is not Premiere.

Essential tools:

- trim start/end
- split
- delete section
- crop
- aspect ratio
- rotate
- volume
- individual audio-track volume
- mute track
- fade audio
- combine clips
- timeline markers
- simple text
- speed
- export

Audio should visually appear as individual editable tracks.

```text
VIDEO        █████████████████

GAME         ▃▅▆▃▇▆▄▅▄▄▅▇▅▄

MIC          ▁▁▃▇▅▁▁▃▆▁▁▁▅▁

DISCORD      ▂▂▁▅▁▁▇▂▁▅▃▁▁▁
```

Segra already includes a clip editor with timeline/audio waveform support.

---

# 20. Instant Clip Creation

Right click timeline:

```text
Create Clip
```

Drag range.

Save as a virtual clip without initially copying/re-encoding video.

Store:

```text
source_recording_id
start_timestamp
end_timestamp
title
game
tags
```

Actual media only needs rendering when exported/shared.

---

# 21. Game Event System

Define universal events:

```text
GameStarted
GameEnded

MatchStarted
MatchEnded

Kill
Death
Assist

RoundStarted
RoundEnded

Win
Loss

Score
Goal

Achievement

Custom
```

Game integrations translate native events into this model.

---

# 22. Highlight Provider API

Game support must not live inside the recorder core.

Define plugins such as:

```text
HighlightProvider
    supports(process)
    attach(session)
    events()
    detach()
```

Possible strategies:

- official game APIs
- local telemetry APIs
- game logs
- websocket feeds
- Game State Integration
- replay/demo files
- process-safe event interfaces
- OCR only as last resort

Do **not** use memory injection/cheat-like techniques that could trigger anti-cheat systems.

---

# 23. Example Integrations

Initial milestone integrations:

### Counter-Strike 2

Use Game State Integration.

Detect:

- kills
- deaths
- assists
- round starts
- round ends
- match start/end
- wins

### League of Legends

Use supported local client/game APIs where permitted.

### Dota 2

Use supported telemetry/game-state mechanisms.

The integration framework matters more than supporting hundreds of games initially.

---

# 24. Generic Highlight Detection

Unsupported games should eventually have optional generic detectors.

Possibilities:

### Audio detection

Detect sudden sound-energy changes.

### Scene detection

Identify major scene transitions.

### Computer vision

Periodic low-cost analysis looking for:

- victory screens
- defeat screens
- kill indicators
- score changes

### AI

Optional local model can analyse thumbnails/frames after recording.

This processing must happen:

- asynchronously
- at low priority
- preferably after gameplay

Never sacrifice game FPS for highlight analysis.

---

# 25. Manual Bookmarks

Global hotkey:

```text
Ctrl + F9
```

Creates a bookmark without saving a standalone clip.

Bookmark metadata:

```text
timestamp
label
colour
duration
```

Optional voice bookmark:

```text
"bookmark that"
```

could eventually be supported.

---

# 26. Screenshots

Global screenshot hotkey.

Screenshots belong to the current game/session.

Support:

- PNG
- JPEG
- lossless WebP where practical
- HDR-aware capture eventually

---

# 27. Storage Management

Storage is a major product feature.

Configuration:

```text
Maximum storage:
250 GB

When full:
Delete oldest non-favourite recordings
```

Options:

```text
Maximum GB
Minimum free disk space
Maximum recording age
```

Never automatically delete:

- favourites
- locked recordings
- actively edited recordings

Outplayed already provides automatic storage management and protects favourites from deletion.

---

# 28. Trash / Recovery

Deleting footage first moves it into application trash.

Configurable retention:

```text
3 days
7 days
30 days
Immediately
```

Allow restore.

---

# 29. Favourites

Any:

- recording
- session
- clip
- screenshot

can be favourited.

Favourites are automatically storage-protected.

---

# 30. Search

Search locally by:

- game
- session
- date
- clip title
- event type
- tags
- favourite
- duration

Example:

```text
game:cs2 kill favourite
```

---

# 31. Per-Game Configuration

Each game can override:

- enabled
- capture mode
- resolution
- FPS
- codec
- bitrate/quality
- replay duration
- audio configuration
- microphone
- event types
- auto-clipping
- storage behaviour
- HDR

Segra already supports per-game quality/recording/HDR/volume overrides, so this belongs in the baseline feature set.

---

# 32. Performance HUD

Optional minimal overlay:

```text
● REC 00:43:16
```

or:

```text
Replay Buffer Active
```

Overlay must never appear in the recording unless explicitly configured.

---

# 33. System Tray

Tray menu:

```text
Recording Cyberpunk 2077
------------------------
Save Replay
Add Bookmark
Start/Stop Recording
Open Library
Settings
Exit
```

Closing the main window should normally minimise to tray.

---

# 34. Global Hotkeys

Configurable hotkeys for:

- save replay
- bookmark
- screenshot
- start/stop recording
- mute microphone
- toggle microphone
- open overlay

Detect conflicts where possible.

---

# 35. Recording Failure Protection

Recording software must fail gracefully.

Handle:

- game crash
- encoder failure
- GPU driver reset
- recording process crash
- low disk space
- drive unplugged
- audio device removed
- microphone changed
- display changed
- resolution changed
- HDR changed
- sleep/resume

Prefer recoverable segmented recordings over giant fragile files.

---

# 36. Diagnostics

Build excellent diagnostics from day one.

Log:

```text
game detection
capture backend
resolution changes
encoder
dropped frames
encoder latency
audio drift
audio devices
recording paths
muxer status
disk latency
plugin events
```

Provide:

```text
Diagnostics → Export Support Bundle
```

Bundle must not contain recorded media.

---

# 37. Performance Metrics

Internally track:

```text
captured FPS
encoded FPS
dropped frames
capture latency
encode latency
CPU utilisation
GPU utilisation
encode-engine utilisation
disk throughput
audio drift
buffer utilisation
```

Expose an optional diagnostics overlay.

---

# 38. Resource Targets

At 1080p60 hardware encoding:

Target recorder CPU usage:

```text
< 3%
```

Idle application:

```text
< 150 MB RAM
```

Actual capture overhead should be benchmarked against:

- NVIDIA App
- OBS
- Medal
- Segra

Do not claim targets are achieved until benchmarked.

---

# 39. Privacy

Default:

```text
No telemetry
No account
No cloud upload
No automatic data transmission
```

If crash reporting is added:

```text
Opt-in only.
```

Plugin network access should be obvious/documented.

---

# 40. Open-Source Structure

Suggested repository:

```text
/apps
    /desktop

/crates
    /capture
    /audio
    /encoder
    /muxer
    /game-detection
    /session
    /events
    /storage
    /library
    /plugins
    /windows

/packages
    /ui
    /shared

/plugins
    /cs2
    /dota2
    /league

/tests
    /capture
    /audio
    /integration
    /performance
```

---

# 41. Agent Development Rules

Agents implementing this project MUST:

1. Never mark a ticket complete merely because code exists.
2. Build and run the affected application.
3. Add automated tests where feasible.
4. Validate the behaviour end-to-end.
5. Record evidence of successful validation in the ticket.
6. Never silently replace a required feature with a mocked implementation.
7. Never add placeholder TODO implementations and call the task complete.
8. Keep capture-engine code isolated from UI code.
9. Keep platform-specific APIs behind interfaces.
10. Measure performance-sensitive changes.
11. Avoid unnecessary dependencies in the recording process.
12. Preserve backwards compatibility for recording metadata wherever practical.
13. Document architectural decisions.

Definition of Done:

```text
IMPLEMENTED
+
COMPILES
+
TESTED
+
MANUALLY VERIFIED WHERE REQUIRED
+
NO REGRESSION
```

---

# 42. Development Milestones

## Milestone 1 — Recording Engine

Deliver a CLI:

```text
recorder.exe record --window <window>
```

It must:

- capture a window/game
- encode through hardware
- capture system audio
- capture microphone
- output valid MKV
- maintain A/V synchronisation

No GUI required.

**Playable result:** record a game and watch the resulting video.

---

## Milestone 2 — True Audio Separation

Add:

```text
Game
System excluding game
Microphone
Mixed compatibility track
```

Use Windows process-specific audio capture.

Validation must inspect the output container and prove separate tracks exist.

Test:

1. Start game.
2. Play game sound.
3. Play Spotify/browser sound.
4. Speak into microphone.
5. Record.
6. Open in an editor.
7. Individually mute each source.

This milestone is not complete until each can be isolated independently.

---

## Milestone 3 — Replay Buffer

Implement rolling encoded buffer.

Deliver:

```text
recorder replay --duration 60
```

Hotkey saves previous 60 seconds.

Must preserve all audio tracks.

**Playable result:** score/do something interesting, press hotkey afterwards, receive clip.

---

## Milestone 4 — Automatic Game Detection

Detect game launch/exit.

Automatically:

```text
Game starts → replay/recording begins
Game exits → recording finalises
```

Add manual executable registration.

**Playable result:** launch Steam game without touching recorder and receive session recording.

---

## Milestone 5 — Desktop Application

Implement:

- tray app
- recording status
- recent recordings
- settings
- games page
- clip playback

The native engine remains separate.

---

## Milestone 6 — Recording Library

Add SQLite metadata.

Support:

- games
- sessions
- recordings
- thumbnails
- favourites
- deletion
- search

**Playable result:** recordings automatically appear organised by game.

---

## Milestone 7 — Per-Game Settings

Add:

- capture mode
- quality
- FPS
- encoder
- replay length
- audio settings

**Playable result:** CS2 records 1440p60 while another game records 1080p60 automatically.

---

## Milestone 8 — Timeline & Bookmarks

Implement:

- timeline
- bookmark hotkey
- event markers
- thumbnails
- waveform

**Playable result:** bookmark something during gameplay and jump straight to it afterwards.

---

## Milestone 9 — Highlight Plugin API

Create stable plugin contract.

Implement CS2 integration as reference plugin.

Automatically detect at least:

```text
kill
death
round
match
```

**Playable result:** kills visibly appear on recording timeline.

---

## Milestone 10 — Automatic Highlights

Configure:

```text
Kill:
15 sec before
10 sec after

Death:
10 sec before
5 sec after
```

Automatically create virtual clips.

**Playable result:** finish a CS2 match and immediately see generated highlights.

---

## Milestone 11 — Clip Editor

Implement:

- trim
- split
- delete
- audio tracks
- track volume
- text
- export

Edits remain non-destructive.

---

## Milestone 12 — Storage Manager

Implement:

- disk quota
- minimum remaining space
- oldest-first deletion
- favourite protection
- trash
- restore

---

## Milestone 13 — Capture Compatibility

Test:

- fullscreen exclusive
- borderless
- windowed
- Vulkan
- DirectX 11
- DirectX 12
- OpenGL
- multiple monitors
- ultrawide
- HDR
- resolution switching
- alt-tab

Build automatic capture fallback.

---

## Milestone 14 — Performance Hardening

Benchmark:

```text
No recorder
NVIDIA App
OBS
This recorder
```

Measure:

- average FPS
- 1% low FPS
- CPU
- RAM
- GPU
- encoder utilisation

Optimise bottlenecks.

---

# 43. Future Features

After the core product is mature:

- Linux
- webcam track
- facecam overlay
- Stream Deck support
- controller hotkeys
- local AI highlight detection
- speech transcription
- semantic search
- automatic clip titles
- clip compilations
- YouTube export
- Discord sharing
- Twitch integration
- remote/mobile library
- LAN sharing
- plugin marketplace
- OBS plugin interoperability
- import existing recordings
- Twitch/YouTube stream recording
- instant GIF export
- vertical TikTok/Shorts exports

---

# 44. Potential Killer Features

Features worth designing around from the beginning:

### Unlimited audio isolation

Rather than hardcoding three tracks, treat audio as arbitrary routing:

```text
Game       → Game
Discord    → Voice
Spotify    → Music
Chrome     → Browser
Mic        → Microphone
Everything else → System
```

### Retrospective editing

Because full sessions contain every independent audio source, a user can discover hours later that:

> Discord was too loud.

and simply lower Discord in the edit.

### Event-aware recordings

Record full sessions while storing events as metadata.

The application can later produce automatically:

```text
All kills
Funny microphone moments
Wins
Deaths
Last match
Best plays
```

without duplicating the original footage.

### Zero-configuration game audio isolation

This should be a headline product feature:

> **Your game, microphone, Discord and PC audio. Automatically separated.**

No virtual cables.

No OBS scenes.

No manual routing.

---

# 45. MVP Definition

The MVP is **not** a UI mock-up.

It is complete when a fresh user can:

1. Install the app.
2. Launch it once.
3. Select microphone and recording directory.
4. Close the window.
5. Launch a game.
6. Play for ten minutes.
7. Press a replay hotkey at least once.
8. Close the game.
9. Open the recorder.
10. See the game automatically recognised.
11. See the completed session.
12. See the replay.
13. Open the video in DaVinci Resolve/Premiere.
14. Independently edit:
    - game audio
    - non-game system audio
    - microphone
15. Repeat the process without manually configuring capture sources.

If any of those steps require OBS, virtual audio cables, manual source selection or restarting the recorder, the MVP is not finished.

---

# 46. Product Goal

The final product should answer:

> "Why would I use this instead of ShadowPlay, Medal, Outplayed or OBS?"

with:

**Because it is open-source, lightweight, automatic, local-first, records everything, organises everything, and gives you proper editable source audio instead of baking your entire PC into one recording.**