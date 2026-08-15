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

## The tray wears this mark, but it draws it rather than loading it

The notification-area icon is the same mark. It is **not** this file: it is
drawn, in `src/tray_icon.rs`, and two things stop it simply being this PNG.

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

So the application icon and the tray icon are the same mark doing two jobs, and
the tray's copy is geometry in a source file. Changing this image does not change
the tray: `src/tray_icon.rs` has to be changed to match.
