"""
maiTerm's brand art in the maiSoft house style (maiProjects/brand/make-icon.py, maiServer
DESIGN.md 2026-10-05): "mai" in Poppins Bold in the product colour, the product word in Poppins
SemiBold, lowercase, in white (ink on light grounds), on a dark ground tinted toward the colour.

maiTerm's colour is warm grey since 2026-10-19 (maiProjects took blue): stone-400 #a8a29e on dark
grounds, stone-500 #78716c on light. Every other stop derives from BRAND, so a re-colour is one edit.

Writes into the directory given as argv[1] (generate-app-icons.sh turns these into the shipped
files):
  art-dark.png, art-light.png   1024 full-bleed icon art: stacked "mai" over "term"
  glass-mai.png, glass-word.png 1024 transparent layers for the macOS 26 .icon
  glass-ground.txt              the .icon's background colour, extended-srgb
  logo-light.png, logo-dark.png the horizontal in-app wordmark (for dark / light themes), 320 high

Fonts are vendored in scripts/assets/fonts (OFL-1.1, OFL-Poppins.txt).
"""
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

N = 1024
FONTS = Path(__file__).parent / 'assets/fonts'
BOLD = str(FONTS / 'Poppins-Bold.otf')
SEMI = str(FONTS / 'Poppins-SemiBold.otf')
OUT = Path(sys.argv[1])

BRAND = (168, 162, 158)        # stone-400 #a8a29e
BRAND_LIGHT_GROUND = (120, 113, 108)  # stone-500 #78716c — "mai" on light grounds
STEM = 'term'
TRACK = -0.025                 # the house wordmark's tracking-tight, in em


def mix(a, b, t):
    return tuple(int(round(x * (1 - t) + y * t)) for x, y in zip(a, b))


def to_oklch(rgb):
    lin = [((c / 255 + 0.055) / 1.055) ** 2.4 if c / 255 > 0.04045 else c / 255 / 12.92 for c in rgb]
    r, g, b = lin
    l_ = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m_ = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s_ = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    L = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_
    A = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_
    B = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_
    return L, float(np.hypot(A, B)), float(np.degrees(np.arctan2(B, A)) % 360)


def from_oklch(L, C, h):
    A, B = C * np.cos(np.radians(h)), C * np.sin(np.radians(h))
    l_ = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3
    m_ = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3
    s_ = (L - 0.0894841775 * A - 1.2914855480 * B) ** 3
    lin = (4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
           -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
           -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_)
    out = [1.055 * max(c, 0) ** (1 / 2.4) - 0.055 if c > 0.0031308 else 12.92 * max(c, 0) for c in lin]
    return tuple(int(round(min(max(c, 0), 1) * 255)) for c in out)


_, _, H = to_oklch(BRAND)
# The theme tokens for this brand (@maisoft/theme theme.css): foreground on dark and on light.
INK_DARK = from_oklch(0.95, 0.008, H)
INK_LIGHT = from_oklch(0.22, 0.02, H)

P = {
    'brand_light': mix(BRAND, (255, 255, 255), 0.32),  # top of the "mai" gradient
    'ground_top': mix(BRAND, (12, 14, 16), 0.86),
    'ground_bottom': mix(BRAND, (4, 5, 6), 0.95),
    'glow': tuple(int(c * 0.58) for c in BRAND),       # the soft glow, low right
    'word': (244, 248, 250),
}


def silhouette(word, font):
    layer = Image.new('L', (N * 2, N), 0)
    ImageDraw.Draw(layer).text((0, 0), word, font=font, fill=255)
    return layer.crop(layer.getbbox())


# --- the stacked mark: "term" spans the same width as "mai", both on one left edge ---
mai = silhouette('mai', ImageFont.truetype(BOLD, 470))
word = silhouette(STEM, ImageFont.truetype(SEMI, 196))
target = int(N * 0.78)
mai = mai.resize((target, int(mai.height * target / mai.width)), Image.LANCZOS)
word = word.resize((target, int(word.height * target / word.width)), Image.LANCZOS)
gap = int(N * 0.045)
total = mai.height + gap + word.height
left = (N - target) // 2
top_y = (N - total) // 2 - int(N * 0.01)
word_y = top_y + mai.height + gap


def mai_fill(c1, c2):
    grad = np.linspace(0, 1, mai.height)[:, None, None]
    fill = (np.array(c1) * (1 - grad) + np.array(c2) * grad) * np.ones((1, mai.width, 1))
    return Image.fromarray(fill.astype('uint8'), 'RGB')


def art_dark():
    y, x = np.mgrid[0:N, 0:N] / N
    t = np.clip((x * 0.35 + y * 0.65), 0, 1)[..., None]
    ground = np.array(P['ground_top']) * (1 - t) + np.array(P['ground_bottom']) * t
    glow = np.exp(-(((x - 0.95) ** 2 + (y - 1.0) ** 2) / 0.12))[..., None]
    ground = ground + glow * np.array(P['glow']) * 0.55
    img = Image.fromarray(np.clip(ground, 0, 255).astype('uint8'), 'RGB')
    shadow = Image.new('L', (N, N), 0)
    shadow.paste(mai, (left, top_y))
    shadow = shadow.filter(ImageFilter.GaussianBlur(40))
    img = Image.composite(Image.new('RGB', (N, N), BRAND), img, shadow.point(lambda v: int(v * 0.22)))
    img.paste(mai_fill(P['brand_light'], BRAND), (left, top_y), mai)
    img.paste(Image.new('RGB', word.size, P['word']), (left, word_y), word)
    return img


def art_light():
    # The light launch screen's palette (maiProjects splash-2732.png): the theme's light ground,
    # "mai" at stone-500, the word in ink.
    img = Image.new('RGB', (N, N), from_oklch(0.972, 0.008, H))
    img.paste(Image.new('RGB', mai.size, BRAND_LIGHT_GROUND), (left, top_y), mai)
    img.paste(Image.new('RGB', word.size, INK_LIGHT), (left, word_y), word)
    return img


def glass_layer(mask, pos, rgb):
    layer = Image.new('RGBA', (N, N), (0, 0, 0, 0))
    layer.paste(rgb if isinstance(rgb, Image.Image) else Image.new('RGB', mask.size, rgb), pos, mask)
    return layer


def wordmark(mai_rgb, stem_rgb, height=320):
    """Horizontal "mai" + stem on one baseline, tracking-tight, trimmed and scaled to `height`."""
    size = 400
    bold, semi = ImageFont.truetype(BOLD, size), ImageFont.truetype(SEMI, size)
    canvas = Image.new('RGBA', (size * 6, size * 2), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    x, base = size // 4, int(size * 1.3)
    for text, font, rgb in (('mai', bold, mai_rgb), (STEM, semi, stem_rgb)):
        for i, ch in enumerate(text):
            # Each glyph at its kerned advance, pulled in by the tracking.
            gx = x + font.getlength(text[:i]) + i * TRACK * size
            d.text((gx, base), ch, font=font, fill=rgb + (255,), anchor='ls')
        x += font.getlength(text) + len(text) * TRACK * size
    canvas = canvas.crop(canvas.getbbox())
    return canvas.resize((round(canvas.width * height / canvas.height), height), Image.LANCZOS)


OUT.mkdir(parents=True, exist_ok=True)
art_dark().save(OUT / 'art-dark.png')
art_light().save(OUT / 'art-light.png')
glass_layer(mai, (left, top_y), mai_fill(P['brand_light'], BRAND)).save(OUT / 'glass-mai.png')
glass_layer(word, (left, word_y), P['word']).save(OUT / 'glass-word.png')
g = P['ground_top']
(OUT / 'glass-ground.txt').write_text('extended-srgb:' + ','.join(f'{c / 255:.5f}' for c in g) + ',1.00000')
# Dark themes: "mai" at stone-400 (the brand at L >= .7), the word in the dark foreground.
dark = wordmark(BRAND, INK_DARK)
dark.save(OUT / 'logo-light.png')
# Light themes: "mai" at stone-500, the word in the light foreground.
wordmark(BRAND_LIGHT_GROUND, INK_LIGHT).save(OUT / 'logo-dark.png')
print(f'wordmark {dark.width}x{dark.height}')
