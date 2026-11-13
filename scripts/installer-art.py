#!/usr/bin/env python3
"""Draws the installer artwork from the app icon and the bundled Inter font.

    python3 scripts/installer-art.py

Writes, in the brand colours of the icon:

- packaging/macos/dmg-background.png and dmg-background@2x.png: the window
  of the disk image (660 x 420 points). The icons are placed on it by
  packaging/macos/dmg-settings.py, so the two must agree on ICON_Y.
- packaging/windows/wizard-image-{100,150,200}.bmp: the left panel of the
  first and last setup pages (164 x 314 at 100 %).
- packaging/windows/wizard-small-{100,150,200}.bmp: the corner image of the
  other setup pages (55 x 55 at 100 %).

Needs Pillow and NumPy. The output is committed; run this again only when
the artwork changes.
"""

from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ICON = ROOT / "assets" / "icon-1024.png"
REGULAR = ROOT / "assets" / "fonts" / "Inter-Regular.ttf"
SEMIBOLD = ROOT / "assets" / "fonts" / "Inter-SemiBold.ttf"

# The icon's own gradient, top left to bottom right.
TOP = (0x17, 0x60, 0x6B)
BOTTOM = (0x1A, 0x1C, 0x3A)
ACCENT = (0x8F, 0xE3, 0xFF)
SUBTLE = (0xA9, 0xC9, 0xD6)
RULE = (0x4E, 0x8E, 0xA4)
CHEVRONS = [(0x2F, 0x7F, 0xD0), (0x4A, 0xA8, 0xEA), ACCENT]

# The disk image window, in points.
DMG_W, DMG_H = 660, 420
# Where the centres of the two icons go (dmg-settings.py places them).
ICON_Y = 235
APP_X, APPLICATIONS_X = 140, 520
PROTOCOLS = "SSH  ·  Telnet  ·  Serial  ·  SFTP  ·  SCP"
HINT = "DRAG PRZMTERM ONTO APPLICATIONS TO INSTALL"


def gradient(w, h):
    """The diagonal brand gradient, w x h pixels."""
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    t = (x / max(w - 1, 1) * 0.45 + y / max(h - 1, 1) * 0.55)[..., None]
    rgb = np.array(TOP, np.float32) * (1 - t) + np.array(BOTTOM, np.float32) * t
    return Image.fromarray(rgb.round().astype(np.uint8), "RGB")


def font(path, size):
    return ImageFont.truetype(str(path), size)


def spaced(draw, xy, text, fnt, fill, tracking, anchor_center=False):
    """Text with letter spacing (Pillow has none); xy is the left of the
    baseline, or its centre with anchor_center."""
    widths = [draw.textlength(c, font=fnt) for c in text]
    total = sum(widths) + tracking * (len(text) - 1)
    x, y = xy
    if anchor_center:
        x -= total / 2
    for c, w in zip(text, widths):
        draw.text((x, y), c, font=fnt, fill=fill, anchor="ls")
        x += w + tracking


def chevron(draw, cx, cy, size, width, fill):
    """A '>' centred on (cx, cy)."""
    half = size / 2
    points = [(cx - half / 2, cy - half), (cx + half / 2, cy), (cx - half / 2, cy + half)]
    draw.line(points, fill=fill, width=round(width), joint="curve")


def dmg_background(scale):
    s = scale
    img = gradient(DMG_W * s, DMG_H * s)
    draw = ImageDraw.Draw(img)
    left, right = 48 * s, (DMG_W - 48) * s

    draw.text((left, 76 * s), "PrzmTerm", font=font(SEMIBOLD, 40 * s), fill="white", anchor="ls")
    draw.line([(left, 92 * s), (right, 92 * s)], fill=RULE, width=max(1, s))
    draw.text((left, 118 * s), PROTOCOLS, font=font(REGULAR, 14 * s), fill=SUBTLE, anchor="ls")

    # The arrow between the icons: a prompt, '>>> _'.
    cx, cy = DMG_W / 2 * s, ICON_Y * s
    step = 24 * s
    for i, colour in enumerate(CHEVRONS):
        chevron(draw, cx - 40 * s + i * step, cy, 22 * s, 3.2 * s, colour)
    draw.rounded_rectangle(
        [cx + 30 * s, cy + 5 * s, cx + 50 * s, cy + 11 * s], radius=1.5 * s, fill="white"
    )

    spaced(
        draw,
        (DMG_W / 2 * s, 372 * s),
        HINT,
        font(SEMIBOLD, 11 * s),
        SUBTLE,
        0.8 * s,
        anchor_center=True,
    )
    return img


def icon(size):
    return Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)


def wizard_image(scale):
    """The left panel: the gradient, the icon and the name."""
    w, h = round(164 * scale), round(314 * scale)
    img = gradient(w, h)
    size = round(96 * scale)
    img.paste(i := icon(size), ((w - size) // 2, round(70 * scale)), i)
    draw = ImageDraw.Draw(img)
    draw.text((w / 2, 206 * scale), "PrzmTerm", font=font(SEMIBOLD, 22 * scale), fill="white", anchor="ms")
    draw.line(
        [(32 * scale, 220 * scale), (w - 32 * scale, 220 * scale)],
        fill=ACCENT,
        width=max(1, round(scale)),
    )
    draw.text((w / 2, 240 * scale), "SSH · Telnet · Serial", font=font(REGULAR, 11 * scale), fill=SUBTLE, anchor="ms")
    draw.text((w / 2, 256 * scale), "SFTP · SCP", font=font(REGULAR, 11 * scale), fill=SUBTLE, anchor="ms")
    return img


def wizard_small(scale):
    """The corner image: the icon on the page's white."""
    size = round(55 * scale)
    img = Image.new("RGB", (size, size), "white")
    inner = round(size * 0.9)
    off = (size - inner) // 2
    img.paste(i := icon(inner), (off, off), i)
    return img


def main():
    mac = ROOT / "packaging" / "macos"
    win = ROOT / "packaging" / "windows"
    mac.mkdir(parents=True, exist_ok=True)
    dmg_background(1).save(mac / "dmg-background.png", optimize=True)
    dmg_background(2).save(mac / "dmg-background@2x.png", optimize=True)
    for percent in (100, 150, 200):
        scale = percent / 100
        wizard_image(scale).save(win / f"wizard-image-{percent}.bmp")
        wizard_small(scale).save(win / f"wizard-small-{percent}.bmp")


if __name__ == "__main__":
    main()
