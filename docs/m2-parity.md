# M2 — parity with Bioma's membrane

The M2 gate compares one membrane in Genoma with the same membrane in Bioma
**as Bioma is now** (PRD §11.1). This file is the checklist of what "the
same" means, taken from Bioma's current QML (2026-10-28) and a capture of the
live desktop, never from the early mockups. Each line names its source in
Bioma. A box is ticked only when a side-by-side capture shows the item
matching.

Paths are relative to `~/Documents/Development/Bioma`.

## Palette

The live palette is four roles read from
`~/.config/bioma/generated/palette.json` (matugen) or a manual palette file.
On 2026-10-28 they were background `#0c1515`, text `#dae5e3`, primary
`#ffb77b` and secondary `#a4cdde`. `core/Theme.qml` derives every other role,
and Genoma's theme module must derive them the same way:

- [ ] `isDark` = luminance(background) < 0.5, with luminance =
      0.2126 r + 0.7152 g + 0.0722 b.
- [ ] `lift(c, a)`: HSL lightness + a when dark, − a when light.
      `elevated` = lift(bg, 0.045), `cell` = lift(bg, 0.075).
- [ ] `textMuted` = text at 0.56 alpha.
- [ ] Structural family, `hsla(hue, sat, L)`:
      - hue = bg hue if bg saturation > 0.05, else the primary's hue;
      - sat = max(min(bg sat + 0.04, 0.40), min(primary sat × 0.33, 0.28));
      - lightness when dark: `line` 0.32, `node` 0.43, `rim` 0.55;
      - lightness when light: `line` 0.72, `node` 0.62, `rim` 0.50.
- [ ] Gradients at 165°:
      - top = (hue, sat + 0.14, L + 0.16);
      - bottom = (hue, sat − 0.15, L − 0.15).
- [ ] State colours, fixed:
      - dark: calm `#6EE7A6`, active `#F2C14E`, alert `#FF6B5D`;
      - light: `#2E9C6A`, `#B37414`, `#C43A2E`;
      - thresholds 0.30 and 0.80.
- [ ] Genoma reads `palette.json` at load: `g.json(path)`, restricted to
      the user's config directory.

## Surfaces

- [ ] **Cell fill** (`structure/Cell.qml:590`): `cell` at `cell.opacity`,
      0.6 in his override (0.72 by default).
- [ ] **Rim** (`components/Rim.qml`): `rim` at the top to `line` at
      mid-height. Width `Metrics.rim()`: 1 logical unit rounded, never under
      2 physical pixels.
- [ ] **Tissue band** (`structure/Tissue.qml:541`):
      - `elevated` at `tissue.opacity` 0.5, drawn only outside the cells
        (cut-out pipeline, done);
      - padding `tissue.padding` 2, gap 2 × padding = 4;
      - radius from `Metrics.radiusFor(thickness)`: a pill at
        `appearance.radius` 100.
- [ ] **Blur** (`structure/Membrane.qml:646`): the cells' shapes only, never
      the band, through `ext-background-effect`, with rounded corners.
      `cell.blur` true, strength 5.
- [ ] **Membrane edge**: `appearance.edge` 12, the margin from the screen
      edge.

## Typography

- [ ] Expressive = Spectral, technical = Orbitron (`core/Typography.qml`).
- [ ] `fontTitle` = round(14.5 × factor) = 15 at normal scale
      (`core/Metrics.qml:160`). Read `fontLabel` and the weights
      (`weightValue`, `weightLabel`) from the same file before using them.
- [ ] Tabular figures where Bioma asks for `Typography.tabular`. Orbitron
      has no `tnum`; check how Bioma gets equal widths there.

## Icons

- [ ] SVG from `assets/icons/<name>.svg`, with `currentColor` replaced by
      the role's colour (or the 165° gradient). Rasterised at the size it is
      shown in physical pixels, never scaled (`components/Icon.qml`).
- [ ] Genoma needs: an `image`/`icon` node, `resvg` rasterisation cached per
      (name, colour, pixels), and a textured-quad pipeline on the shared
      device.

## Cells

- [ ] **Workspaces** (`cells/workspaces/Workspaces.qml`):
      - padding 16/16, spacing 11, mark 16;
      - the mark is `components/WorkspaceBars.qml`: two rounded bars
        16 × 7.5 of a 24 grid, pitch 9.5, radius 3, stroke 1.4 units
        crisped, in the primary, and the bars shift one step on a change;
      - the name in Spectral `fontTitle`, elided; an unnamed workspace shows
        its number in Orbitron at `fontLabel`.
- [ ] **Window title** (`cells/window_title/WindowTitle.qml`):
      - spacing 10, the icon 30 in a disc of lift(bg, −0.015) with a
        `Metrics.rim()` border in text at 0.5;
      - the application icon, or `app-fallback` in `textMuted`;
      - the loader glyph while the window is waiting;
      - the title in Spectral `fontTitle`, elided;
      - present while any window has focus, on any output (`Niri.focusedWindow`).
        Corrected 2026-10-28 from "on this output", which was an assumption.
- [ ] **Vitals** (`cells/vitals/Vitals.qml`, `components/Vital.qml`):
      - a 64-unit grid, ring radius 24, stroke max(crisp(1.5 u), 1.5 u);
      - CPU: a beating dot, interval 60000 / (45 + rate × 95) ms, swelling
        to 1.45 over 9%, settling over 15%, resting 76%;
      - RAM: a liquid that sloshes, with drops; read the slosh and the drops
        in `Vital.qml` before drawing them;
      - GPU: a satellite, period 6000 − rate × 4800 ms, its rate the GPU
        clock, not the load (§9.2 of Bioma's PRD).
      Genoma's version so far is an approximation to replace with this.
- [ ] **Clock** (`cells/clock/Clock.qml`):
      - HH:MM in Spectral tabular at `timeSize`, weight `weightValue`;
      - spacing 8;
      - a `components/Dial.qml` of 26: the hour's fraction as an arc, with a
        hand in orbit.
      Genoma needs an arc (start and sweep) on the ring primitive.

## Measurement

- [ ] The isolated Bioma gets the live `palette.json` too, copied into its
      HOME, or it draws in the default palette.
- [ ] Bioma isolated with `membranes: []`, then with the one membrane; and
      Genoma. Each run: 30 s to settle, 60 s measured.
- [ ] Measured in each run: shell CPU, shell PSS, **niri CPU**, frames a
      second.
- [ ] A side-by-side capture of the two membranes, cropped and checked
      element by element against this list, before any number is reported.
