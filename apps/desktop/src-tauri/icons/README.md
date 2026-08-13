# Application icons

`source.png` is the master: Clipped's brand mark — white waveform bars in the
accent disc, on a white rounded square — at 2048 × 2048 with an alpha channel,
the corners being the only transparent part of it. Everything beside it is
generated from it, and is regenerated rather than edited:

```powershell
npm run tauri --workspace @clipped/desktop -- icon src-tauri/icons/source.png
```

The path is relative to `apps/desktop`, not to the repository root.

That command also writes macOS, iOS, Android and Microsoft Store variants.
Clipped is a Windows application (SPEC.md section 3), so only the four files
named by `bundle.icon` in `tauri.conf.json` are kept; delete the rest after
regenerating.

## The tray's ground is dark, and that is deliberate

The mark in the notification area (`src/tray_icon.rs`) is drawn on the
**neutral-900** ground rather than this white one, and the two are allowed to
differ.

An application icon is drawn on surfaces the application does not choose but can
predict — a taskbar, a Start menu, Explorer — and white reads on all of them. A
tray icon is drawn on a strip Windows paints dark by default, light on some
machines and some versions, and never tells the application which. A white
ground vanishes into a light taskbar; a dark one carries its own contrast on
either, which is the "sticker" treatment `tray_icon.rs` documents and
`every_mark_reads_on_a_light_ground_and_on_a_dark_one` measures.

So the brand is the same mark in both places. Only the ground under it differs,
and it differs because one of the two surfaces is unknown at drawing time.
