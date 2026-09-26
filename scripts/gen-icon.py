#!/usr/bin/env python3
"""Generate Mind Map app icon (PNG + ICNS). Requires Pillow."""
from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets" / "icon"
SIZE = 1024


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(4))


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    bg_top = (250, 250, 248, 255)
    bg_bot = (236, 236, 232, 255)
    ink = (26, 26, 26, 255)
    ink_soft = (106, 106, 100, 255)
    line = (154, 154, 146, 255)
    accent = (37, 99, 235, 255)
    white = (255, 255, 255, 255)

    radius = int(SIZE * 0.223)
    margin = int(SIZE * 0.06)
    box = [margin, margin, SIZE - margin, SIZE - margin]

    shadow = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        [margin + 8, margin + 14, SIZE - margin + 4, SIZE - margin + 18],
        radius=radius,
        fill=(0, 0, 0, 55),
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(28))
    img = Image.alpha_composite(img, shadow)

    base = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    bd = ImageDraw.Draw(base)
    for y in range(SIZE):
        bd.line([(0, y), (SIZE, y)], fill=lerp(bg_top, bg_bot, y / (SIZE - 1)))
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius=radius, fill=255)
    rounded = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    rounded.paste(base, mask=mask)
    img = Image.alpha_composite(img, rounded)

    rim = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    ImageDraw.Draw(rim).rounded_rectangle(
        box, radius=radius, outline=(255, 255, 255, 90), width=3
    )
    img = Image.alpha_composite(img, rim)
    draw = ImageDraw.Draw(img)

    cx, cy = SIZE // 2, SIZE // 2 + 12
    nodes = {
        "root": (cx - 70, cy),
        "ideas": (cx + 130, cy),
        "goals": (cx - 70, cy - 160),
        "notes": (cx - 70, cy + 160),
        "ship": (cx + 290, cy),
    }
    for a, b in [
        ("root", "ideas"),
        ("root", "goals"),
        ("root", "notes"),
        ("ideas", "ship"),
    ]:
        draw.line([nodes[a], nodes[b]], fill=line, width=10)

    def chip(center, w, h, fill):
        x, y = center
        draw.rounded_rectangle(
            [x - w // 2, y - h // 2, x + w // 2, y + h // 2], radius=h // 2, fill=fill
        )

    def marks(center, n=3, color=white, gap=16):
        x, y = center
        start = x - ((n - 1) * gap) // 2
        for i in range(n):
            px = start + i * gap
            draw.ellipse([px - 5, y - 5, px + 5, y + 5], fill=color)

    chip(nodes["ideas"], 120, 36, ink)
    chip(nodes["goals"], 100, 36, ink)
    chip(nodes["notes"], 100, 36, ink)
    chip(nodes["ship"], 90, 34, ink)
    chip(nodes["root"], 140, 40, accent)
    marks(nodes["root"], 3, white)
    marks(nodes["ideas"], 3, (245, 245, 243, 255))
    marks(nodes["goals"], 2, (245, 245, 243, 255))
    marks(nodes["notes"], 2, (245, 245, 243, 255))
    marks(nodes["ship"], 2, (245, 245, 243, 255))
    rx, ry = nodes["root"]
    draw.rounded_rectangle([rx - 50, ry + 28, rx + 50, ry + 36], radius=4, fill=accent)

    mx = (nodes["root"][0] + nodes["ideas"][0]) // 2
    my = nodes["root"][1] - 2
    draw.ellipse(
        [mx - 14, my - 14, mx + 14, my + 14],
        fill=(247, 247, 245, 255),
        outline=line,
        width=3,
    )
    draw.rectangle([mx - 6, my - 2, mx + 6, my + 2], fill=ink_soft)
    draw.rectangle([mx - 2, my - 6, mx + 2, my + 6], fill=ink_soft)

    img.save(OUT / "AppIcon-1024.png")

    iconset = OUT / "AppIcon.iconset"
    if iconset.exists():
        shutil.rmtree(iconset)
    iconset.mkdir()
    for base_s, is2x in [
        (16, False),
        (16, True),
        (32, False),
        (32, True),
        (128, False),
        (128, True),
        (256, False),
        (256, True),
        (512, False),
        (512, True),
    ]:
        px = base_s * (2 if is2x else 1)
        name = f"icon_{base_s}x{base_s}" + ("@2x.png" if is2x else ".png")
        img.resize((px, px), Image.Resampling.LANCZOS).save(iconset / name)

    icns = OUT / "AppIcon.icns"
    subprocess.check_call(["iconutil", "-c", "icns", str(iconset), "-o", str(icns)])
    print(f"wrote {icns} ({icns.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
