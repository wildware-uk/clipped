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

## The tray wears this mark, drawn rather than loaded, on a ground of its own

The notification-area icon is the same mark. It is **not** this file: it is
drawn, in `src/tray_icon.rs`, and three things stop it simply being this PNG.

It is sixteen pixels wide. The five waveform bars here become five one-pixel
columns separated by gaps narrower than a pixel, which resolves into a smear, so
the tray draws three. The mark survives the loss, because what identifies it is
the accent disc and the tall centre bar rather than the number of bars.

It also has to say whether a recording is running, and a tray icon carries no
label. So the state rides on top of the mark as a badge in the bottom-right
corner — a filled disc for recording, a ring for connecting, a slash for
unavailable, and no badge at all when idle. It is the badge's _shape_ that
carries the state, not its colour, so the state survives being printed in black
and white; that is what AGENTS.md section 46 asks for, and
`every_mark_is_a_different_shape_and_not_only_a_different_colour` measures it
rather than trusting this paragraph.

And its ground is **neutral-900** rather than the white one here, which is the
difference that looks like a mistake and is not. An application icon is drawn on
surfaces the application does not choose but can predict — a taskbar, a Start
menu, Explorer — and white reads on all of them. A tray icon is drawn on a strip
Windows paints dark by default, light on some machines and some versions, and
never tells the application which. A white ground vanishes into a light taskbar;
a dark one carries its own contrast on either, which is the "sticker" treatment
`tray_icon.rs` documents and
`every_mark_reads_on_a_light_ground_and_on_a_dark_one` measures.

So the application icon and the tray icon are the same mark doing two jobs.
Changing this image does not change the tray: `src/tray_icon.rs` has to be
changed to match, and the ground under it should stay dark while one of the two
surfaces is unknown at drawing time.
