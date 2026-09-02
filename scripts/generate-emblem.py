#!/usr/bin/env python3
"""maiTerm's emblem, drawn parametrically. Writes SVG to stdout.

The mark is a constellation: eight nodes around a dominant core, four ACTIVE
(large, on the cardinals) and four IDLE (small, on the diagonals). There is
deliberately no enclosing ring, no connecting web and no arcs -- the dots ARE
the mark.

Two constraints drove that, both learned the hard way:

  * macOS renders the app icon in six appearances, four of which (Mono,
    TintedLight, TintedDark, ClearDark) discard colour entirely. So the
    active/idle distinction cannot be carried by hue; it is carried by SIZE,
    which survives every rendition.

  * Finder list view and the menu bar render at 16-32px. Thin strokes, dashed
    rings and fine lattices vanish there -- an earlier line-art draft measured
    2.1% ink coverage and was invisible as a glass layer. Solid discs hold.

Rendered black-on-white; the icon pipeline inverts it into an alpha mask.
Icon Composer lights that alpha itself, which is why nothing here carries a
gradient, glow, bevel or shadow -- baked lighting would double up.
"""
import math
import sys

S = 1024                 # canvas
C = S / 2
R_ORBIT   = 325          # distance from centre to the node centres
R_ACTIVE  = 76           # cardinal node radius
R_IDLE    = 40           # diagonal node radius  (1.9x ratio: reads at 16px)
R_CORE    = 131          # centre disc


def emblem(fg: str = "#000", bg: str | None = "#fff") -> str:
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {S} {S}" '
           f'width="{S}" height="{S}">']
    if bg:
        out.append(f'<rect width="{S}" height="{S}" fill="{bg}"/>')
    out.append(f'<g fill="{fg}">')
    for k in range(8):
        a = math.radians(-90 + k * 45)
        x, y = C + R_ORBIT * math.cos(a), C + R_ORBIT * math.sin(a)
        r = R_ACTIVE if k % 2 == 0 else R_IDLE
        out.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}"/>')
    out.append(f'<circle cx="{C}" cy="{C}" r="{R_CORE}"/>')
    out += ['</g>', '</svg>']
    return "\n".join(out)


if __name__ == "__main__":
    fg = sys.argv[sys.argv.index("--fg") + 1] if "--fg" in sys.argv else "#000"
    bg = None if "--transparent" in sys.argv else "#fff"
    sys.stdout.write(emblem(fg, bg))
