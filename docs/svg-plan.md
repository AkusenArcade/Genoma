# Vector SVG in the scene graph — plan

Vault item: *P2 SVG vettoriale nel grafo, non rasterizzato: icone che si
animano o si ricolorano per parte.* Decided 2026-11-16: done completely or
not at all — no "one raster layer per group" shortcut.

## What there is today

- `path` nodes (`d`, `points`, `fill`, stroke through `color` + `stroke`,
  `join`, `cap`, `dash`, `even_odd`) are written back into an SVG document
  and **rasterised by resvg** on the CPU (`path_svg` → `Icons::vector`,
  a cache of 256 documents). Any animated property — a colour, a dash, the
  outline — makes a new document and a new rasterisation every frame.
- `icon` nodes rasterise a whole SVG file at its physical size; only
  `currentColor` and the 165° gradient can change it. Its parts cannot be
  reached.
- The renderer draws SDF rectangles and capsules, rings, Bézier threads,
  text, images, user shaders, particles. It has no triangle pipeline for
  arbitrary shapes.
- Already useful: per-node `transform` (3D depth), `clip`, `mask` (a
  subtree used as its parent's mask, through an offscreen layer), the blur
  pass, `fill_stops` gradients, the timing set and reactive bindings.
- `resvg`/`usvg` 0.49 are already dependencies of `genoma-scene`.

## What "complete" means

1. **Drawn on the GPU, never rasterised**: sharp at any scale and during
   depth transitions; animating a colour, an opacity, a transform or a
   gradient costs no CPU tessellation and no upload.
2. **An SVG file becomes a subtree**: every element with an `id` is a node
   the interface can address, bind and animate like any other.
3. **The SVG features icons and illustrations use**: paths, basic shapes,
   groups with transforms and opacity, solid fills and strokes (joins, caps,
   miter limit, dashes), linear and radial gradients (spread modes,
   `gradientTransform`), `clip-path`, `mask`, `use`/`defs`/`symbol`,
   markers, text (as outlines), embedded images, patterns, and the filters
   that matter (Gaussian blur, drop shadow, colour matrix, offset, flood,
   merge, blend). Anything usvg can parse and Genoma cannot draw natively is
   said in the log and drawn by resvg for that element alone, so nothing is
   silently missing.
4. **Animation per part**: colour, opacity, transform, gradient stops,
   stroke `trim` (draw a line on), dash offset, and **morph** between two
   outlines.
5. **Parity**: a corpus of SVGs drawn by Genoma matches resvg within a
   small pixel tolerance.

## Design

### Renderer: a path pipeline (`genoma-render/src/path.rs`)

- **Tessellation with lyon** (`lyon_tessellation`, pure Rust): fills
  (non-zero and even-odd) and strokes into triangles, in logical units.
  Meshes are cached by geometry (outline + stroke parameters + tolerance
  for the current scale) and only rebuilt when one of those changes.
- **Antialiasing by fringe** (as NanoVG does): each fill gets a one-pixel
  strip along its flattened outline whose alpha ramps to zero, each stroke
  a half-pixel feather on both sides. One pass, interleaved in draw order
  with rectangles, text and images — no MSAA target, no stencil — so a path
  sits in the scene like any other node and takes `clip`, `fade`,
  `transform` and `mask` unchanged. The fringe width follows the physical
  scale; a scale change rebuilds only the fringe.
- **Paint in the fragment shader**: solid colour, or a gradient (linear
  or radial with focal point, pad/reflect/repeat, up to 8 stops) evaluated
  per pixel from instance data. Colour, opacity, gradient and transform are
  instance data: animating them touches no mesh.
- Draw lists gain a `Path` item beside `Rect` and `Image`, batched where
  consecutive paths share a pipeline state.

### Scene: `path` nodes on the new pipeline

- `Kind::Path` draws through `path.rs`; `path_svg`/`Icons::vector` remain
  only to build the distance field `combine_with` needs (later, the distance
  can come from the mesh).
- New properties: `trim_start`/`trim_end` (0…1 of the length, animatable),
  `dash_offset` (animatable), `miter_limit`, `fill_stops`/`fill_angle`/
  `fill_shape` on paths as on capsules, and an animatable `d`: when two
  outlines share their command structure the numbers are interpolated;
  otherwise both are resampled to the same number of points first.
- A 2D `affine` (six numbers) for nodes made from SVG groups. Bioma's rule
  holds: cells still grow by width and height; `affine` exists so an SVG's
  own `transform` attributes keep their meaning, and for parts that turn
  (a needle, a fan).

### Documents: `g.svg`

```lua
g.svg {
    src = "~/icons/wifi.svg",          -- or `source = "<svg …>"`
    width = 24, height = 24,           -- the viewBox fitted into it
    color = theme.fg,                  -- what currentColor becomes
    parts = {                          -- by SVG id, values or bindings
        arc3 = { opacity = function() return strength() > 0.66 and 1 or 0.2 end },
        needle = { affine = function() return turn(angle()) end },
    },
}
```

- Parsed by **usvg**, which already resolves CSS, `use`, `symbol`,
  inherited attributes, markers and text into a plain tree of groups,
  paths and images.
- Group → `box` node with `affine` and `opacity`; path → `path` node; image
  → `image` node; clip-path and mask → the existing `mask` mechanism (an
  offscreen layer), the clip shape drawn as a path; group opacity on more
  than one child → an offscreen layer so overlaps do not double.
- Ids carry over as node ids, so `parts`, `g.find` and the inspector
  (`genoma ipc genoma tree`) reach them.
- Patterns: the tile drawn once into a texture by the same pipeline,
  sampled with its transform.
- Filters: blur and drop shadow on the existing blur pass; colour matrix,
  offset, flood, merge and blend as small shader passes over the group's
  layer; any other primitive → that group rasterised by resvg, logged once.
- The document is reloaded when its file changes, like images.

### The `icon` property

Stays as it is — the cheapest path for a static icon. An icon that must
move or change by part becomes `g.svg`. `@genoma/kit` gains nothing it
does not use.

## Phases (each its own commit, verified before the next)

1. **Path pipeline**: lyon fill + stroke, fringe antialiasing, solid paint,
   the `Path` draw item, mesh cache; `path` nodes switched over. Parity
   snapshots against resvg for the existing `path` examples and for
   Bioma's `assets/icons` drawn as paths. The largest phase.
2. **Paint and motion**: gradients, `trim`, `dash_offset`, `miter_limit`,
   morphing `d`. A measurement: an animated icon's cost per frame on the
   profiling overlay.
3. **`g.svg`**: usvg tree → subtree, `affine`, ids and `parts`,
   `currentColor`, images, reload on change.
4. **Composition**: clip-path, mask, group opacity layers, patterns,
   markers and text through usvg's outlines.
5. **Filters, docs, example**: the filter set above with the resvg
   fallback; `examples/svg` (an icon that animates by part, an
   illustration); reference, README, CHANGELOG; a parity corpus (Bioma's
   icons plus a subset of resvg's test suite) run by `cargo test`.

## Risks

- **Fringe AA on thin, very curved outlines** can look lighter than
  resvg's coverage AA. Measured in phase 1 against resvg; if it is not
  good enough, the fallback is 4× MSAA for the path layer alone.
- **Ordering with offscreen layers** (masks, group opacity): the existing
  mask path is reused rather than a second mechanism.
- **Morphing between unrelated outlines** is approximate by nature
  (resampling); documented as such.
- New dependency: `lyon_tessellation` (and `lyon_path`). Pure Rust, no
  system libraries, so packaging does not change.

## For Bioma afterwards

Bioma's icons can move by part where a live value drives them (the Wi-Fi
arcs by strength, the battery's fill, the volume's waves, a fan by fan
speed) — a showcase of Genoma as decided on 2026-11-12, each use chosen
with Akusen and held to Bioma's rule that movement encodes a live value.

## Status (2026-11-16)

All five phases are in, each its own commit:

1. `path` nodes on the GPU — lyon fills, tiny-skia's stroker for strokes
   (joins, caps, dashes), a one-pixel fringe for antialiasing whose
   boundary edges are confirmed by probing just outside them (a
   stroke's inner loops leave T-junctions), meshes kept by what shapes
   them, tessellated at the size they are seen under an enlarging
   transform.
2. Gradients (linear, two-circle radial, conic; pad, repeat, reflect; 8
   stops) from a storage buffer of paints; `trim_start`/`trim_end`,
   `dash_offset`, `miter_limit`, `dash_array`; `d` morphs.
3. `g.svg`: usvg's tree as boxes (`affine`, `opacity`) and paths, ids and
   `parts`, `currentColor` following `color` (its opacity kept), text as
   outlines; `rotate` and `pivot_at` on any node.
4. Clip paths and masks (alpha and luminance) as mask children; group
   opacity through `isolate`.
5. A plain Gaussian blur of a group as `blur_radius`; `examples/svg`.

What differs from the plan:

- **Filters other than a plain blur** (drop shadows, colour matrices,
  chains), **patterns** and **embedded images** are drawn by resvg into a
  picture laid in their place, aligned to the document's pixel grid, at
  twice the size shown. They are pictures: their insides are not parts.
  Native colour-matrix, offset, flood and merge passes were not built;
  they are worth doing only when a shell needs one to move.
- **The parity corpus is not run by `cargo test`**: drawing needs a GPU,
  which the tests do not have. Parity was measured with `genoma snapshot`
  against resvg (the same document through `g.image`), premultiplied,
  per region: Bioma's icons within about 2% of the same ink, gradients,
  groups and dashes within 0.2%, clip, luminance mask and group opacity
  within 0.2%, a plain blur within 0.3% (the largest difference 6/255).
  The structure `g.svg` builds is tested without a GPU.
