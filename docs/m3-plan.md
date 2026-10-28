# M3 — interaction

PRD §11: pointer, keyboard, touchpad with momentum, input regions, compositor
blur, multi-output, and expansions growing from their origin. M3 is done when
**one Bioma cell with its expansion is fully usable by keyboard and pointer**.
The cell is **workspaces**: a click opens the list of the output's
workspaces, a click on a row jumps there and closes, the wheel steps through
them without opening anything, and the keyboard moves, activates and closes.

The reference is Bioma's current QML, as for M2 (see
`docs/m2-parity.md`). Paths are relative to `~/Documents/Development/Bioma`.
Compositor blur is already done (M2). AccessKit, the AT-SPI adapter, is the
last step (PRD §12, question 5).

## Steps, in dependency order

### 1. Pointer events reach the interface

*Done 2026-10-28.* Checked with a synthetic pointer on HDMI-A-1, the raw
Wayland events logged beside the handlers' output: one Enter, two
Press/Release pairs giving two `on_click(272)`, and wheel steps as ±1.
`on_hover` follows Enter and Leave. Momentum on the touchpad comes in step 5.

- [x] **Hit testing** on the laid-out tree. The topmost node under the
      point that has a handler takes the event.
- [x] **Handlers in Luau**: `on_click`, `on_press`, `on_release`,
      `on_wheel(dx, dy)`, and `hovered`, a signal a binding can read. They
      run inside the reactive graph like any binding.
- [x] **Input region** from the nodes that take input. Everything else
      passes the pointer through to the windows below. Rebuilt from layout,
      never maintained by hand (PRD §6.2).
- [x] Wheel and touchpad are told apart by the scroll source. Discrete
      wheel steps go to `on_wheel` as steps; touchpad scrolling comes with
      momentum (step 5).

### 2. niri actions

- [x] `niri:focus_workspace(id)` and the like: a request on a second socket,
      one JSON line out and one reply in. The event stream stays read-only.

### 3. Expansion grows from its origin

*Done 2026-10-28, with step 4.* A contact sheet of the synthetic click shows
the thread, the panel growing from the node at its tip, the content fading
in, and the closing run back. New in the toolkit for it: `delay` in the
timing set; transition names chosen by a function (so opening and closing
use different entries); `growth` and `reach`; `attach = "below" |
"below-center"`; group `opacity`; `catch` with `on_outside` and `on_key`.

From `structure/Cell.qml` (Expansion, `opening`, `closing`):
- [x] a vertical thread (`components/Thread.qml`), 1.3 px with a 2.5 node at
      its tip, grows from the cell's edge over `gap` = marginEdge +
      tissuePadding (14) in `Timing.grow` 100 ms on `easeOpenFlat`
      `[0.2, 0.9, 0.3, 1.0]`;
- [x] after `Timing.stagger` × 2 (32 ms), the panel grows from its anchor
      (origin start: left-aligned under the cell) over `Timing.grow` on the
      same curve;
- [x] content enters only once the panel is grown, by opacity. That needs a
      **group opacity** property, which multiplies every alpha in a
      subtree;
- [x] closing runs back: the panel over `Timing.close` 150 ms on
      `easeClose` `[0.5, 0.0, 0.9, 0.5]`, and the thread after a 32 ms
      pause;
- [x] **growth is width and height**, never scale.

### 4. The surface while open

- [x] On open, the membrane's surface grows to the output's height (one
      configure) and catches the pointer everywhere. A click outside the
      panel closes, as Bioma's full-screen catcher (`structure/InputSurface.qml`)
      does. On close, the surface shrinks back after the animation.
- [x] Keyboard interactivity goes **exclusive** while a cell is open (Bioma's
      open cell holds the keyboard) and back to none when it closes.

### 5. Keyboard and touchpad

*Touchpad done 2026-10-28.* A synthetic two-finger touchpad gave the
following: ten followed moves of about 28 units, then `on_fling(-2196)`
when the fingers lifted moving, and a small fling for a short stroke. The
keys are wired: an exclusive keyboard while open, with Escape, Up, Down and
Return in the example. **Akusen tries the keys by hand**, because synthetic
Escape or Return would reach the terminal if the grab failed.

- [ ] Keys through xkbcommon (SCTK seat keyboard): an `on_key` handler with
      the key's name, and the open cell holding the keyboard.
- [ ] Bioma's keys in an open list: arrows move the mark, Enter
      activates, Escape closes (`components/KeyMark.qml`, `core/Focus.qml`).
- [x] Touchpad momentum (`components/Glide.qml`): follow the fingers, and
      on lift flick on at the speed of the last `Timing.fling` 80 ms.

### 6. The workspace list (`cells/workspaces/WorkspaceList.qml`)

*Done 2026-10-28, two differences left.* With nine rows the well shows five
and scrolls: a row a wheel step, the fingers followed, a fling slowing to a
stop. The active row is kept inside the five, and so is the keyboard's mark.
New in the toolkit: `clip` (rect and image shaders discard outside it, text
uses glyphon's bounds; hit bounds are clipped too), `scroll_y`, and
`on_fling`. Two differences remain: the free place's ring is fainter rather
than dashed, and "empty" is not italic, because there are no font styles
yet.

- [x] 216 wide, rows of 44, five rows visible and then it scrolls, the
      active row always inside. The well clips the scroll, never the panel.
- [x] Each row:
      - a round button 36 at x 8, drawn as the Theme.line ring;
      - the number in Orbitron 14 tabular. The active one is in
        `background` on the primary; a vacant one is text at 0.3, and the
        others text at 0.72;
      - the name in Spectral 14 at x = button + 12. The active one is in
        text, the others at 0.5, and "empty" in italic at 0.28.
- [x] The well (`components/Well.qml`): lift(bg, −0.01) at 0.8, with a radius
      concentric with the panel's.
- [ ] Read the remaining details from the QML when building it: the
      active button's fill, the scroll mark.

### 7. Multi-output

*Done 2026-10-28; hotplug coded, not yet seen.* `g.surface { outputs = "all"
| { names }, function(output) … end }` gives one surface per output, each
with its own scene, scale, frames, regions and pointer state. Evaluation and
mounting are now two steps: the runtime fits the surfaces to the description,
then the script mounts one tree per surface under one scope, so a reload
replaces them together. Handlers are keyed by (tree, node). Checked with a
clock capsule on DP-1 and HDMI-A-1 at once, one frame a second each. An
output added or removed refits the surfaces through the same path. That
cannot be exercised here without unplugging a monitor.

- [x] One surface per output for a spanning interface (`outputs = "all"`),
      each with its own scale. An output added later gets its surface; a
      removed one loses it.

### 8. AccessKit (AT-SPI)

*Done 2026-10-28.* Each surface is an AT-SPI window through `accesskit_unix`,
holding the nodes with a role or a text. A node's name is its `name` or the
text it shows, and labels carry the text as their value. Read back over the
accessibility bus, with the session's `IsEnabled` set for the check and then
restored: "Genoma on HDMI-A-1", then Workspace → "Secondary", Vitals, Clock →
"14:34". The cost at rest is unchanged (1.92% of a core, 27 MB). The tree is
compared at most every 250 ms and rebuilt only when it changed. A screen
reader's actions (pressing a button) are logged, not yet performed.

- [x] Every node's role and name (already carried since M1) published
      through `accesskit_unix`, so Orca can read the cell and its list.

## Done when

The workspace cell on Genoma behaves as Bioma's, checked by Akusen side by
side:
- pointer: open, jump, close by a click outside;
- wheel steps;
- keyboard: arrows, Enter, Escape;
- the opening and closing cascade.

The membrane at rest still costs nothing extra: no frame and no wake-up
from the input machinery while nothing is touched.
