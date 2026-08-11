# Application icons

The Clipped brand mark as the design system draws it: a flat accent square
(`--color-accent`, `#ec3013`), zero corner radius, on a transparent ground with
an eighth of the canvas as margin. It is the same mark the title bar shows
beside the wordmark.

`icon.ico` carries the 16, 24, 32, 48, 64, 128 and 256 pixel sizes Windows
picks between for the taskbar, the window, Alt+Tab and Explorer. The PNGs are
what `tauri.conf.json` lists for the bundler.

These are generated rather than drawn, so re-generating them is a matter of
running the snippet below with [Pillow](https://python-pillow.org) installed —
there is no design source file to lose. Changing the mark is a design decision
that belongs in the design project first (see `docs/desktop-ui.md`).

```python
from PIL import Image, ImageDraw

ACCENT = (0xEC, 0x30, 0x13, 255)

def mark(size: int) -> Image.Image:
    image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    inset = round(size / 8)
    ImageDraw.Draw(image).rectangle([inset, inset, size - inset - 1, size - inset - 1], fill=ACCENT)
    return image

for name, size in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)]:
    mark(size).save(name)

mark(256).save("icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
```
