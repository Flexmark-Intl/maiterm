#!/usr/bin/env python3
"""Chrome Web Store brand images into chrome-extension/store-assets/:

  store-icon-128.png          the app icon at the store's 96px artwork size, 16px transparent padding
  promo-small-440x280.png     icon + wordmark + "for Chrome"
  promo-marquee-1400x560.png  the same with the one-line pitch

    python3 chrome-extension/scripts/store-brand.py

Everything is drawn from maiTerm's own brand sources: the shipped app icon
(src-tauri/icons/icon-source-1024.png) and the wordmark scripts/make-brand.py renders, on the
icon's own dark ground with its warm glow low right. Needs Pillow + numpy (as make-brand.py does).
"""

import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / 'chrome-extension' / 'store-assets'
ICON = REPO / 'src-tauri' / 'icons' / 'icon-source-1024.png'
FONTS = REPO / 'scripts' / 'assets' / 'fonts'
SEMI = str(FONTS / 'Poppins-SemiBold.otf')

GROUND = (28, 25, 23)       # stone-900, the icon's ground family
GLOW = (120, 113, 108)      # stone-500, the icon's warm glow
STONE_300 = (214, 211, 209)
STONE_400 = (168, 162, 158)

PITCH = 'Let your AI agent debug and drive a browser tab you link to it.'


def art_bbox(im):
    return im.getchannel('A').point(lambda a: 255 if a > 8 else 0).getbbox()


def ground(w, h):
    """The icon's dark ground with a soft warm glow toward the lower right."""
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    d = np.sqrt(((x - w * 0.82) / (w * 0.75)) ** 2 + ((y - h * 1.05) / (h * 1.1)) ** 2)
    t = np.clip(1 - d, 0, 1) ** 2 * 0.55
    rgb = np.array(GROUND, np.float32) * (1 - t[..., None]) + np.array(GLOW, np.float32) * t[..., None]
    return Image.fromarray(rgb.astype(np.uint8), 'RGB')


def fit(im, height):
    w = round(im.width * height / im.height)
    return im.resize((w, height), Image.LANCZOS)


def store_icon(icon):
    """96px of artwork inside 128, as the store's icon guideline asks."""
    art = icon.crop(art_bbox(icon))
    art = art.resize((96, round(art.height * 96 / art.width)), Image.LANCZOS)
    out = Image.new('RGBA', (128, 128), (0, 0, 0, 0))
    out.alpha_composite(art, ((128 - art.width) // 2, (128 - art.height) // 2))
    return out


def tile(w, h, icon, wordmark, icon_h, word_h, sub, sub_size, pitch=None, pitch_size=0):
    img = ground(w, h).convert('RGBA')
    art = icon.crop(art_bbox(icon))
    art = fit(art, icon_h)
    word = fit(wordmark.crop(art_bbox(wordmark)), word_h)
    sub_font = ImageFont.truetype(SEMI, sub_size)
    pitch_font = ImageFont.truetype(SEMI, pitch_size) if pitch else None

    gap = round(icon_h * 0.22)
    text_w = max(word.width, sub_font.getbbox(sub)[2])
    block_h = word_h + round(sub_size * 0.55) + sub_size
    if pitch:
        block_h += round(pitch_size * 1.3) + pitch_size
        text_w = max(text_w, pitch_font.getbbox(pitch)[2])
    x0 = (w - (art.width + gap + text_w)) // 2
    img.alpha_composite(art, (x0, (h - art.height) // 2))

    tx = x0 + art.width + gap
    ty = (h - block_h) // 2
    img.alpha_composite(word, (tx, ty))
    d = ImageDraw.Draw(img)
    ty += word_h + round(sub_size * 0.55)
    d.text((tx + 2, ty), sub, font=sub_font, fill=STONE_400, anchor='la')
    if pitch:
        ty += sub_size + round(pitch_size * 1.3)
        d.text((tx + 2, ty), pitch, font=pitch_font, fill=STONE_300, anchor='la')
    return img.convert('RGB')


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    icon = Image.open(ICON).convert('RGBA')
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run([sys.executable, str(REPO / 'scripts' / 'make-brand.py'), tmp], check=True, capture_output=True)
        # logo-light: the wordmark for DARK grounds (light "term").
        wordmark = Image.open(Path(tmp) / 'logo-light.png').convert('RGBA')

    store_icon(icon).save(OUT / 'store-icon-128.png', optimize=True)
    tile(440, 280, icon, wordmark, icon_h=118, word_h=50, sub='for Chrome', sub_size=21).save(
        OUT / 'promo-small-440x280.png', optimize=True)
    tile(1400, 560, icon, wordmark, icon_h=250, word_h=104, sub='for Chrome', sub_size=38,
         pitch=PITCH, pitch_size=27).save(OUT / 'promo-marquee-1400x560.png', optimize=True)
    for f in ('store-icon-128.png', 'promo-small-440x280.png', 'promo-marquee-1400x560.png'):
        print(f'store-assets/{f}')


if __name__ == '__main__':
    main()
