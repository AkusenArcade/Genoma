# Changelog

Every change an interface could notice, newest first. Versions follow
[semantic versioning](https://semver.org); before 1.0 a minor version may
break interfaces, and each such change is marked **Breaking** with what to
change. Internal work (performance, refactors) is listed only when it
changes what a shell sees.

## Unreleased

### Changed

- **Breaking:** `genoma pam` needs the PAM service as its first argument,
  `genoma pam <service> [directory]`. It no longer falls back to Bioma's
  `bioma-lock`.
- The notification server reports the program it runs as as its vendor
  (`exec -a myshell genoma …` gives "Myshell"), and "Genoma" when started
  under its own name. It was always "Bioma".
- Property descriptions in `genoma types` no longer refer to Bioma.

### Added

- `fill_angle` and `fill_shape` on capsules: the fill gradient (two colours
  or `fill_stops`) turned to any angle, CSS degrees, or drawn radial or
  conic. `fill_angle` animates and takes bindings. Without either, fills
  are drawn exactly as before. See `examples/gradients`.
- `@genoma/kit`, a component kit with a neutral look: `button`,
  `toggle`, `slider`, `progress`, `menu`, `dropdown`, `tooltip`, `list`,
  `tabs` and `dialog` (a surface of its own, the rest dimmed), restyled
  live with `kit.theme.set`. See `examples/kit`.
- A `g.popup` written as a child of a node becomes a surface of its own
  anchored to that node, so a menu is written inside its button. Before,
  every popup had to be returned beside the surfaces with an `anchor`,
  which still works.
- `g.timing_default { … }`, which defines only the timing names not
  defined yet: what a library offers and a shell may retime.
- `g.list`, a keyed list for thousands of items: only the rows in view
  are mounted, every one `row_height` tall. It scrolls by itself (wheel
  steps glided to, touchpad followed and flung), brings a row into view
  with `reveal`, and reports its position to `on_scroll`. See
  `examples/list`, a run prompt over `/usr/bin`.
- `on_wheel` gets a third argument, `steps`: true when the movement came
  from a wheel's steps (a high-resolution wheel's fractions included),
  false for a touchpad's distance.
- `@genoma/compositor`: workspaces, windows and the focused window under
  niri, Hyprland or Umbriel, one model for all three, with
  `focus_workspace`, `focus_window` and `close_window`. Requiring it fails
  where none of them runs. `@genoma/niri` is unchanged.
- `examples/bar`: a classic top bar written against Genoma alone. It runs
  under niri, Hyprland and Umbriel.
- `g.field`, a line of text to type into: caret and selection by key and
  by pointer (double and triple click), word moves (Ctrl+arrows,
  Ctrl+BackSpace), undo and redo (Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y) with typing
  undone a word at a time, Ctrl+A/C/X/V through the clipboard, horizontal
  scrolling under the caret, a placeholder, `secret` (dots, never copied),
  and input methods through `text-input-v3` (composition underlined at the
  caret). Props `placeholder`, `placeholder_color`, `selection_color`,
  `caret_color`, `focused`, `secret`; handlers `on_change(text)` and
  `on_submit(text)`; keys it leaves (Escape, Tab, Up, Down) reach `on_key`.
  `examples/prompt`.
- `keyboard` on `g.surface`: `"none"` (the default: only while a node
  catches), `"on_demand"` (when pressed) or `"exclusive"`.
- `g.popup`, a surface that opens beside a node of another one
  (`xdg_popup`): `anchor` (a node's `id`, or a function giving one), `side`,
  `align`, `gap`, `grab`, `present`, `on_dismiss`. The compositor flips or
  slides it to keep it on screen, and moves it when its anchor moves or its
  content changes size. Opened by a press or a key, it takes the keyboard and
  closes on a press elsewhere. `examples/bar` opens the date from the clock.
- Nodes take an `id` (a string), which is what a popup's anchor names.
- `@genoma/clipboard`, the clipboard without `wl-copy`/`wl-paste`, through
  `ext-data-control-v1`: `types` and `text` of every copy (text up to
  1 MiB), `changed` moving on each copy, the same text twice included;
  `clipboard:set_text(text)` and `clipboard:set(types, path)` to own the
  clipboard, kept after the program that copied before is gone;
  `clipboard:save(type, path)` to write the current copy to a file (a
  history of images); `clipboard:clear()`. `available` is false on a
  compositor without the protocol. The primary selection is not followed.
- This changelog, and a versioning policy in the README.

### Fixed

- A `full` surface whose `present` was false took the keyboard when a
  node in it caught. Asleep, a surface takes no keys. It goes on drawing,
  as before, so what is closing finishes leaving.
- A `full` surface naming no output was built for a size it did not have
  (its content function got no size) and never built again, so what was
  centred in it could be off screen. It is built again once the
  compositor gives its size.

## 0.1.2 — 2026-11-07

### Fixed

- A tilted subtree's content (rects, images, shaders, particles) turns with
  it. Clip and fade are measured before projection, and shader nodes and
  particle emitters take the subtree's transform.

## 0.1.1 — 2026-11-04

### Changed

- A surface that keeps moving redraws only what changed.
- What barely moves requests no frames.

## 0.1.0 — 2026-11-04

First release: everything Bioma's shell, lock screen and greeter need.

- `on_drag` is measured from where the pressed node was at the press, not
  from where it was last laid out (changed 2026-11-03, before the tag). A
  node that moves with its own drag places itself at press position plus the
  value given.
