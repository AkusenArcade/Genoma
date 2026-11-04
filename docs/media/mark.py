# Writes the Genoma mark, icon and logo (docs/media/README.md): python3 mark.py <dir>
import math, sys
OUT = sys.argv[1]
A, P, Y0, Y1, C = 5.4, 12.53, 2.6, 21.4, 12.0
GAP = 1.0  # half the gap left in the strand that passes behind, along y

def x_of(strand, y):
    theta = 2 * math.pi * (y - C) / P + math.pi / 2
    return C + (A if strand == 0 else -A) * math.sin(theta)

# Crossings inside the helix, and which strand is behind there.
crossings = [(C - P / 4, 1), (C + P / 4, 0)]

def strand_paths(strand):
    cuts = [y for y, back in crossings if back == strand]
    pieces, start = [], Y0
    for y in cuts:
        pieces.append((start, y - GAP)); start = y + GAP
    pieces.append((start, Y1))
    out = []
    for a, b in pieces:
        n = max(8, int((b - a) / 0.12))
        pts = [(x_of(strand, a + (b - a) * i / n), a + (b - a) * i / n) for i in range(n + 1)]
        out.append("M" + " L".join(f"{x:.3f} {y:.3f}" for x, y in pts))
    return out

def mark(stroke, node_fill, sw=1.5):
    rungs = []
    for y in (C - P / 2, C + P / 2):
        rungs.append(f'<path d="M{x_of(1,y):.3f} {y:.3f} H{x_of(0,y):.3f}"/>')
    paths = [f'<path d="{d}"/>' for s in (0, 1) for d in strand_paths(s)]
    return (f'<g fill="none" stroke="{stroke}" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round">'
            + "".join(rungs) + f'<path d="M2.6 12 H21.4"/>' + "".join(paths) + '</g>'
            + f'<circle cx="2.6" cy="12" r="1.5" fill="{node_fill}"/><circle cx="21.4" cy="12" r="1.5" fill="{node_fill}"/>')

# 24-grid glyph, as the icon set: currentColor.
open(f"{OUT}/genoma-mark.svg", "w").write(
    '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">'
    + mark("currentColor", "currentColor") + '</svg>\n')

GRAD = ('<defs><linearGradient id="p" gradientUnits="userSpaceOnUse" x1="9.5" y1="2.6" x2="14.5" y2="21.4">'
        '<stop offset="0" stop-color="#dbf4a7"/><stop offset="1" stop-color="#94bc41"/></linearGradient></defs>')
BG = "#12160f"

def placed(scale, tx, ty):
    return f'<g transform="translate({tx} {ty}) scale({scale})">' + mark("url(#p)", "url(#p)") + '</g>'

# The application icon: the mark alone, on the dark ground.
s = 1024 / 24 * 0.78
open(f"{OUT}/genoma-icon.svg", "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{GRAD}'
    f'<rect width="1024" height="1024" fill="{BG}"/>' + placed(s, 512 - 12 * s, 512 - 12 * s) + '</svg>\n')

# The logo, as Bioma's: mark, the name in Orbitron, a line in Spectral.
s = 21.5
open(f"{OUT}/logo.svg", "w").write(
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">{GRAD}'
    f'<rect width="1024" height="1024" fill="{BG}"/>' + placed(s, 512 - 12 * s, 400 - 12 * s)
    + f'<text x="512" y="752" text-anchor="middle" font-family="Orbitron" font-weight="400" font-size="104" letter-spacing="6" fill="#ffffff">GENOMA</text>'
    + f'<text x="512" y="842" text-anchor="middle" font-family="Spectral" font-size="66" fill="#c9e89a">A Living Toolkit</text>'
    + '</svg>\n')
