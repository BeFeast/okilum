#!/usr/bin/env python3
"""Render the DMG window background (#993) at 1x and 2x from the brand mark and fonts.

    uv run --with pillow==12.0.0 python3 scripts/macos-dmg/background.py   (needs rsvg-convert)

Writes scripts/macos-dmg/background.png (660x400) and background@2x.png. The
window layout in dmg-settings.py places the app at (165, 200) and Applications
at (495, 200); the arrow sits between them.
"""
import io
from pathlib import Path
import subprocess
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
FONTS = ROOT / 'crates/okilum-shell/assets/brand/fonts'
OUT = Path(__file__).resolve().parent
W, H = 660, 400
SURFACE, INK, ACCENT, MUTED = '#F2F6FC', '#102344', '#38BDF8', '#4D6384'
SYMBOL = ROOT / 'crates/okilum-shell/assets/brand/symbol-primary.svg'


def symbol(size):
    """The approved mark, rasterised from its master; the drawing area is 8..56 of 64."""
    png = subprocess.run(['rsvg-convert', '--width', str(size * 64 // 48), '--height', str(size * 64 // 48), str(SYMBOL)],
                         check=True, capture_output=True).stdout
    full = Image.open(io.BytesIO(png)).convert('RGBA')
    pad = (full.width - size) // 2
    return full.crop((pad, pad, pad + size, pad + size))


def render(scale):
    image = Image.new('RGB', (W * scale, H * scale), SURFACE)
    draw = ImageDraw.Draw(image)
    s = lambda v: round(v * scale)
    # Mark and wordmark, centred at the top.
    title = ImageFont.truetype(str(FONTS / 'noto-sans-600.ttf'), s(22))
    mark, gap = 28, 10
    width = s(mark) + s(gap) + draw.textlength('Okilum', font=title)
    left = (W * scale - width) / 2
    top = s(40)
    glyph = symbol(s(mark))
    image.paste(glyph, (round(left), top), glyph)
    draw.text((left + s(mark) + s(gap), top + s(mark) / 2), 'Okilum', font=title, fill=INK, anchor='lm')
    # Arrow from the app slot to Applications.
    y = s(200)
    draw.line([(s(258), y), (s(392), y)], fill=ACCENT, width=s(6))
    draw.polygon([(s(404), y), (s(386), y - s(14)), (s(386), y + s(14))], fill=ACCENT)
    caption = ImageFont.truetype(str(FONTS / 'noto-sans-400.ttf'), s(14))
    draw.text((W * scale / 2, s(345)), 'Drag Okilum to Applications to install', font=caption, fill=MUTED, anchor='mm')
    return image


if __name__ == '__main__':
    render(1).save(OUT / 'background.png', optimize=True)
    render(2).save(OUT / 'background@2x.png', optimize=True)
