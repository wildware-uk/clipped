# Application icons

`source.png` is the master: Clipped's brand mark — white waveform bars in the
accent disc, on the neutral-900 rounded square — at 2048 × 2048 with an alpha
channel. Everything beside it is generated from it, and is regenerated rather
than edited:

```powershell
npm run tauri --workspace @clipped/desktop -- icon src-tauri/icons/source.png
```

The path is relative to `apps/desktop`, not to the repository root.

That command also writes macOS, iOS, Android and Microsoft Store variants.
Clipped is a Windows application (SPEC.md section 3), so only the four files
named by `bundle.icon` in `tauri.conf.json` are kept; delete the rest after
regenerating.

## This is the application's mark, not the tray's

The notification-area icon is **not** this image, and should not be changed to
it. It is four drawn marks — one per state, in `src/tray_icon.rs` — because a
tray icon is sixteen pixels wide, carries no label, and has to say whether a
recording is running. Each state is a different *shape* so that it survives
being printed in black and white, which is what AGENTS.md section 46 asks and
what a single brand mark in four colours would fail.

The application icon identifies the application. The tray marks identify the
state. They are different jobs and the same picture cannot do both.
