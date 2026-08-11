# Application icons

`source.png` is the master: the design system's brand mark — the accent square
on the neutral-900 title strip — at 1024 × 1024. Everything beside it is
generated from it, and is regenerated rather than edited:

```powershell
npm run tauri --workspace @clipped/desktop -- icon src-tauri/icons/source.png
```

That command also writes macOS, iOS, Android and Microsoft Store variants.
Clipped is a Windows application (SPEC.md section 3), so only the four files
named by `bundle.icon` in `tauri.conf.json` are kept; delete the rest after
regenerating.
