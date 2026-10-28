# M4 — the living layer

PRD §8 and §11: threads that grow along a path, budding and reabsorption,
particles with data behind them, depth transitions, and springs as an
animation source. **Each primitive at rest costs nothing, and in motion stays
within the M2 frame budget** (p99 under 2 ms; per frame, Genoma's 0.21 ms
against Bioma's 0.31).

Bioma's principles still decide what is drawn with them. Movement means
something: these are transitions that express a change of state or of
relation, and indicators driven by data. None of them is a loop that runs for
its own sake. Whether Bioma adopts a primitive, and the rule that relaxes for
it (`transform: scale` and depth, §8.4), is decided in Bioma's design
documents, not here.

## Steps, in dependency order

### 1. Springs as an animation source (§8.5)

- [x] A timing entry may be a spring instead of a curve:
      `settle = { spring = { stiffness = 300, damping = 26, mass = 1 } }`.
      The integration is per frame, and a spring settles when it is within a
      quarter physical pixel and still. It requests no frame after that.
- [x] **Interruptible:** a new target keeps the current velocity, so a
      capsule redirected mid-flight bends toward the new place instead of
      starting over.
- [x] Works for every animatable property, numbers and colours.

### 2. Threads along a path (§8.1)

*Built 2026-10-28, `examples/living`.*
- A `thread` node draws a quadratic Bézier in the rect shader as a distance
  field (Quilez's closed form, its roots clamped to the grown fraction, with
  a round cap at the tip). It is crisp at 1.3 and has a node of 5 at each
  end.
- Linking two capsules grows it.
- Moving the second on a spring takes the thread's end along on the same
  spring and its middle on a softer one, so it bends in flight and settles.
  The contact sheet shows the overshoot and the sag deepening and settling.
- At rest: 0 frames in 5 s after the motion.

- [x] A `thread` primitive: a quadratic Bézier from one end to the other with
      a width, drawn as a distance field (no tessellation) with antialiased
      edges and round or flat caps.
- [x] `growth` 0…1 draws the path from its start up to that fraction, with a
      node at each end as Bioma's Thread.qml has.
- [x] The control point can follow a spring, so a thread that joins two
      moving things bends and settles.

### 3. Budding and reabsorption (§8.2)

*Built 2026-10-28, `examples/living` ("bud").*
- A capsule with `bud` is drawn as one shape with the nearest capsule above
  it: the polynomial smooth minimum of the two fields, masked outside the
  parent so its glass is never covered twice. The rim follows the joined
  outline, and the quad grows to cover the parent's box, where the bridge
  is.
- The panel starts inside its cell, grown to nothing. It leaves on the
  timing set with its neck narrowing from 36 to 0, and is taken back the
  same way, faster.
- A shape grown to nothing draws nothing, bridge included.
- Zoomed captures show the panel coming out of the cell's edge as one shape.
- At rest: no frames.

- [x] A capsule may bud from its parent: the panel and its cell joined by a
      smooth union of their two distance fields, with a neck that narrows to
      a thread and parts. Closing runs back as reabsorption.
- [x] Only the panel and the bridge are drawn from the union; the parent's
      own interior is taken out, so glass never sits on glass.
- [x] The rim follows the joined outline.

### 4. Particles with data behind them (§8.3, §5.4)

*Built 2026-10-28, `examples/living` (the link's flow).* One correction to
the plan: positions are a function of age and seed on the GPU, but the
births are the CPU's, in a ring. A position from index and time alone
would make particles in flight jump when the rate changes, and freeze in
mid-air at rate 0. The CPU writes one float when a particle is born and
nothing else.
- Ballistic, or carried to a target along the thread's own Bézier.
- In the example the rate is the processor's load × 300 while linked.
- Measured with the flow running: 120 frames/s (the particles are fast),
  2.05% of a core, 30 MB, render p99 177 µs.
- After unlinking, once the last life is over: 0 frames in 5 s.

*Akusen, 2026-10-28:* kept in the toolkit. **How Bioma uses particles is
decided case by case,** only where they actually carry information, and
none is adopted until such a case is defined. **3D particles** come later,
with the depth work.

- [x] An `emitter` node with properties that bind like any other: rate,
      lifetime, speed, spread, size, colour, and gravity or attraction to a
      point.
- [x] Particles are **stateless on the GPU**: each one's position is a
      function of its index, its seed and the time, computed in the vertex
      shader. The CPU sends only the emitter's parameters, and only when they
      change. There is no compute pass and no particle buffer to update.
- [x] Frames are paced by the particles' speed, as the orbit is. At rate 0,
      once the last particle's lifetime is over, it costs nothing.

### 5. Depth (§5.5, §8.4)

*Built 2026-10-28, `examples/living` ("unfold").*
- `rotate_x`, `rotate_y`, `depth`, `perspective` (default 800) and
  `pivot_x`/`pivot_y` (default the centre) compose into one 4×4 matrix per
  subtree. Rects and images carry it to the GPU with w for the
  perspective, so the local coordinates interpolate perspective-correct
  and the distance field stays exact. Antialiasing comes from the local
  coordinates' screen derivative, so a foreshortened edge stays one pixel
  soft.
- The matrices travel in a storage buffer, each distinct one once, the
  instance carrying its index: as four more vertex attributes they took
  the rect past the sixteen a device guarantees.
- Blur under depth: the compositor's region is rectangles, so a tilted
  shape sends its projected outline (corners in segments), cut in rows one
  logical unit tall. Zoomed captures show the glass and the blur meeting
  at the rim with no halo.
- The panel unfolds from its top edge on a spring, overshooting past flat
  and settling, and folds back on a stiffer one.
- In motion: render median 114 µs, p99 178 µs, max 495 µs. At rest: 0
  frames in 5 s.

- [x] Each node may carry a 3D transform: rotation about x and y, translation
      in z, and a perspective. It applies to its subtree. Primitives evaluate
      their distance field in local space after the projection, so edges
      stay analytic and sharp.
- [x] **Text under depth** (PRD §12, question 2) is decided here by
      measurement: glyphs rasterised at their transformed size, or MSDF.
      *Decided 2026-10-28:* neither. A tilted text is rasterised once on the
      CPU at its flat physical size, with glyphon's subpixel placement, and
      drawn as an image through the matrix; at rest it is glyphon's again.
      The panel at 0.001° (image) against 0° (glyphon) differs in a few
      antialiasing values and no glyph position. Render p99 193 µs with the
      text, PSS 26 MB before and after. Image textures no surface has drawn
      for 1024 uploads are dropped, so a text that changes while tilted does
      not leave its rasters behind.

### 6. The half-pixel eye test (deferred from M2)

- [ ] With the living layer's slow movers on screen, Akusen compares a
      quarter-pixel step with a half-pixel one. The threshold becomes the
      pacing rule for every slow mover.

### 7. Carried from M3

- [ ] `g.each(list, row)`: dynamic children by key, so the workspace list
      has a row per workspace instead of ten static ones.
- [ ] Output hotplug, exercised when possible.

## Done when

Each primitive is shown in an example, checked by Akusen, and measured:
- at rest: no frame and no wake-up;
- in motion: frame time inside the M2 budget, frames paced by the motion.
