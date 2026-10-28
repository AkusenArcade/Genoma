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

- [ ] **Hit testing** on the laid-out tree. The topmost node under the
      point that has a handler takes the event.
- [ ] **Handlers in Luau**: `on_click`, `on_press`, `on_release`,
      `on_wheel(dx, dy)`, and `hovered`, a signal a binding can read. They
      run inside the reactive graph like any binding.
- [ ] **Input region** from the nodes that take input. Everything else
      passes the pointer through to the windows below. Rebuilt from layout,
      never maintained by hand (PRD §6.2).
- [ ] Wheel and touchpad are told apart by the scroll source. Discrete
      wheel steps go to `on_wheel` as steps; touchpad scrolling comes with
      momentum (step 5).

### 2. niri actions

- [ ] `niri:focus_workspace(id)` and the like: a request on a second socket,
      one JSON line out and one reply in. The event stream stays read-only.

### 3. Expansion grows from its origin

From `structure/Cell.qml` (Expansion, `opening`, `closing`):
- [ ] a vertical thread (`components/Thread.qml`), 1.3 px with a 2.5 node at
      its tip, grows from the cell's edge over `gap` = marginEdge +
      tissuePadding (14) in `Timing.grow` 100 ms on `easeOpenFlat`
      `[0.2, 0.9, 0.3, 1.0]`;
- [ ] after `Timing.stagger` × 2 (32 ms), the panel grows from its anchor
      (origin start: left-aligned under the cell) over `Timing.grow` on the
      same curve;
- [ ] content enters only once the panel is grown, by opacity. That needs a
      **group opacity** property, which multiplies every alpha in a
      subtree;
- [ ] closing runs back: the panel over `Timing.close` 150 ms on
      `easeClose` `[0.5, 0.0, 0.9, 0.5]`, and the thread after a 32 ms
      pause;
- [ ] **growth is width and height**, never scale.

### 4. The surface while open

- [ ] On open, the membrane's surface grows to the output's height (one
      configure) and catches the pointer everywhere. A click outside the
      panel closes, as Bioma's full-screen catcher (`structure/InputSurface.qml`)
      does. On close, the surface shrinks back after the animation.
- [ ] Keyboard interactivity goes on-demand while a cell is open and back
      to none when it closes.

### 5. Keyboard and touchpad

- [ ] Keys through xkbcommon (SCTK seat keyboard): an `on_key` handler with
      the key's name, and the open cell holding the keyboard.
- [ ] Bioma's keys in an open list: arrows move the mark, Enter
      activates, Escape closes (`components/KeyMark.qml`, `core/Focus.qml`).
- [ ] Touchpad momentum (`components/Glide.qml`): follow the fingers, and
      on lift flick on at the speed of the last `Timing.fling` 80 ms.

### 6. The workspace list (`cells/workspaces/WorkspaceList.qml`)

- [ ] 216 wide, rows of 44, five rows visible and then it scrolls, the
      active row always inside. The well clips the scroll, never the panel.
- [ ] Each row:
      - a round button 36 at x 8, drawn as the Theme.line ring;
      - the number in Orbitron 14 tabular. The active one is in
        `background` on the primary; a vacant one is text at 0.3, and the
        others text at 0.72;
      - the name in Spectral 14 at x = button + 12. The active one is in
        text, the others at 0.5, and "empty" in italic at 0.28.
- [ ] The well (`components/Well.qml`): lift(bg, −0.01) at 0.8, with a radius
      concentric with the panel's.
- [ ] Read the remaining details from the QML when building it: the
      active button's fill, the scroll mark.

### 7. Multi-output

- [ ] One surface per output for a spanning interface (`outputs = "all"`),
      each with its own scale. An output added later gets its surface; a
      removed one loses it.

### 8. AccessKit (AT-SPI)

- [ ] Every node's role and name (already carried since M1) published
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
