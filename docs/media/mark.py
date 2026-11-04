# Writes the Genoma mark, icon and logo (docs/media/README.md): python3 mark.py <dir>
#
# Bioma's capsule (the same 10.8 x 18.8, radius 5.4, on the 24 grid) holding a
# double helix seen through it: two strands crossing once in the middle, the
# one behind cut either side of the crossing, cropped by the capsule so they
# curve back along its ends.
import math, sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "."
C, A, L, GAP = 12.0, 5.4, 15.0, 1.15
GRAD = ('<linearGradient id="p" gradientUnits="userSpaceOnUse" x1="9.5" y1="2.6" x2="14.5" y2="21.4">'
        '<stop offset="0" stop-color="#dbf4a7"/><stop offset="1" stop-color="#94bc41"/></linearGradient>')
CLIP = '<clipPath id="cap"><rect x="6.6" y="2.6" width="10.8" height="18.8" rx="5.4"/></clipPath>'
GROUND = "#12160f"


def x_of(strand, y):
    v = A * math.sin(math.pi * (y - C) / L)
    return C + (v if strand == 0 else -v)


def strand(s):
    spans = [(0.0, 24.0)] if s == 0 else [(0.0, C - GAP), (C + GAP, 24.0)]
    out = []
    for a, b in spans:
        n = int((b - a) / 0.1)
        out.append("M" + " L".join(f"{x_of(s, a + (b - a) * i / n):.3f} {a + (b - a) * i / n:.3f}" for i in range(n + 1)))
    return out


def mark(stroke):
    return (f'<g fill="none" stroke="{stroke}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">'
            '<rect x="6.6" y="2.6" width="10.8" height="18.8" rx="5.4"/>'
            '<g clip-path="url(#cap)">' + "".join(f'<path d="{d}"/>' for s in (0, 1) for d in strand(s)) + '</g></g>')


def placed(scale, cx, cy):
    return f'<g transform="translate({cx - 12 * scale} {cy - 12 * scale}) scale({scale})">{mark("url(#p)")}</g>'


open(f"{OUT}/genoma-mark.svg", "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><defs>{CLIP}</defs>'
    + mark("currentColor") + '</svg>\n')
open(f"{OUT}/genoma-icon.svg", "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><defs>{GRAD}{CLIP}</defs>'
    f'<rect width="1024" height="1024" fill="{GROUND}"/>' + placed(1024 / 24 * 0.82, 512, 512) + '</svg>\n')
open(f"{OUT}/logo.svg", "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><defs>{GRAD}{CLIP}</defs>'
    f'<rect width="1024" height="1024" fill="{GROUND}"/>' + placed(21.5, 512, 400)
    + '<text x="512" y="752" text-anchor="middle" font-family="Orbitron" font-size="104" letter-spacing="6" fill="#ffffff">GENOMA</text>'
    + '<text x="512" y="842" text-anchor="middle" font-family="Spectral" font-size="66" fill="#c9e89a">A Living Toolkit</text>'
    + '</svg>\n')
