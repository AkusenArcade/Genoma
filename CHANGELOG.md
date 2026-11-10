# Changelog

Every change an interface could notice, newest first. Versions follow
[semantic versioning](https://semver.org); before 1.0 a minor version may
break interfaces, and each such change is marked **Breaking** with what to
change. Internal work (performance, refactors) is listed only when it
changes what a shell sees.

## Unreleased

### Added

- `genoma snapshot <dir>`: the interface run with no compositor, each
  surface drawn on the GPU into a picture (`<n>-<namespace>.png`) and every
  tree written as JSON (`tree.json`), on one screen of the size and name
  asked for, after `--wait` seconds and the `--ipc` calls given. A shell
  seen and compared in a script or in CI, nothing shown on the desktop.
  After the load, in the order written: `--ipc` calls, `--click <id>`
  presses (a press, a release and a click on that node) and `--sleep`.
  `--mock @genoma/vitals=busy.luau` gives a module of the test's own in
  place of a service. `genoma new` gives its clock an id to press.
  The compositor's blur is not drawn. Bioma's ten surfaces, with a cell
  opened by `--ipc`, in about four seconds.

## 0.1.7 — 2026-11-10

### Added

- `genoma new <dir>`: a shell to start from, a bar with a clock that opens
  a popup, written with the kit, with its types in `types/` and a
  `.luaurc` that points `@genoma` at them (luau-analyze passes it in
  strict mode), and a README of the commands. It writes nothing into a
  directory that holds something.
- `@genoma/async`: waiting written straight down. Inside `async.run(f)`,
  `async.process(command)` returns the exit status and the output and
  error lines, `async.sleep(seconds)` waits, `async.call` and `async.get`
  are `g.dbus.call` and `.get` with their reply returned, and
  `async.activation_token` the token; `async.all { … }` runs several side
  by side and returns what each did. A mistake in a task goes to the log
  with its line; waiting outside a task is an error at the caller's line.

## 0.1.6 — 2026-11-10

### Changed

- What is not seen keeps no surface drawing. A transition on a node in
  nothing visible (inside something absent, of no size, or clipped away)
  waits, its clock running, and lands where it should be once seen; a
  node that moves of itself (a pulse, an orbit, a wave, particles, a
  shader's clock) stops while unseen and goes on from where it was. A
  transition on a node growing from nothing still runs, its parent being
  seen. Seen in Bioma: a closed vitals cell's ripples kept its membrane at
  141 frames a second at rest; now 3, and the shell's CPU during a run of
  cells opening and closing went from 4.6 % to 3.1 % of a core, the
  animations drawn the same.

### Added

- `genoma ipc genoma tree` says, for each surface, what is moving and why
  (`moving`: transitions by node and property, nodes that move of
  themselves and how).

## 0.1.5 — 2026-11-10

### Added

- A profiling overlay, `genoma ipc genoma profile on|off` or
  `GENOMA_OVERLAY=1`: over every surface a line with its size, frames a
  second, the last frame's time on the main thread and on the GPU, the
  wait for a buffer when it counts, and how much was drawn again; the
  textures kept and their memory; and an outline round what the last
  frame drew again. A surface at rest draws nothing, and its line says
  what its last frame cost.
- `genoma check [dir]`: every `.wgsl` checked as a `g.shader` compiles it
  (with the prelude and the library) and every `.luau` by the Luau
  compiler, each mistake told with its file and its own line; exit status
  1 when there is one.
- `g.activation_token { app_id, on_token }`: a token from the compositor
  (xdg-activation), asked with the press or key just handled, for a
  program about to start: its window takes the focus where the compositor
  would otherwise refuse it, and a program already running (a browser
  asked for a new window) is raised. `apps:launch` takes it as a fourth
  argument and sets `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`;
  `g.process` passes it in `env`. nil where the compositor gives none.
  `examples/launcher` uses it.
- Drag and drop (`wl_data_device`), with other programs and within the
  shell. A node with `on_drop` takes what is dropped on it: the files of a
  `text/uri-list` as paths (and the URIs), or the text, and where it fell;
  `on_drop_hover` hears something come over it and leave, and the offer
  is accepted only over such a node. A node with `drag_files` (paths, one
  a line) or `drag_text`, pressed and moved six units, is dragged out as a
  copy, to a file manager, an editor or another node. See
  `examples/dragdrop`.

### Changed

- A `g.shader` whose WGSL does not compile is drawn as a red plate with
  its file and the mistake, at the line in that file, over the node; it
  draws again as soon as the file is saved right. Before, the node stayed
  empty and the mistake went to the log, its line counted in Genoma's
  prelude.

## 0.1.4 — 2026-11-10

### Added

- `g.window { title, app_id, width, height, min, max, present, on_close }`:
  a surface the compositor manages as a program's window (`xdg_toplevel`),
  tiled or floating, in its alt-tab, decorated by it where it decorates.
  Its content is laid out across the size it is given. The title may be a
  function. Asked to close, a window with `on_close` is only told and goes
  when `present` lets it go (a question first); one without closes itself.
  See `examples/window`.
- `move_window` on a node: in a window, pressing it moves the window and
  pressing it twice maximizes it or gives back its size.
- `kit.titlebar { title, on_close }`: a title bar for a window that moves
  it, with a close button.
- A `g.popup` opens from a window as it does from a layer surface (a
  `kit.dropdown` in a window), and closes before its window does.

- `combine_with` names several capsules, separated by spaces: up to five
  shapes drawn as one, each met as `combine` says (three drops and their
  necks). See `examples/shapes`.
- `combine_with` names a text or a path too: the capsule meets its outline
  as a distance field, made once from the outline and kept while it stays
  (a drop taking a label into itself, in `examples/shapes`). A scene that
  names none uses no texture for it.

### Changed

- Inside a `navigate` node, the keyboard's focus walks a whole `g.list`:
  a row it reaches is scrolled into view the least way, so the rows after
  it are mounted for the next step. Before, it reached only the rows
  mounted when it began, then went round to the start.

## 0.1.3 — 2026-11-09

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
- `clipboard.history`: the texts copied, newest first, each once, in
  memory only; a copy a password manager marks secret
  (`x-kde-passwordManagerHint`) is left out. `clipboard:forget(i)`,
  `:clear_history()`, `:keep(n)`; restoring one is `clipboard:set_text`.
- Examples for the rest of a desktop: `examples/dock` (pinned and running
  applications, their windows focused in turn, one tooltip following the
  pointer), `examples/osd` (volume, and `genoma ipc osd show`),
  `examples/notifications` (a notification centre) and `examples/lock`
  (ext-session-lock with PAM, its own PAM file beside it). The kit's
  tooltip says it cannot live in a row of `g.each`, and how a list does
  it instead.
- `@genoma/spectrum`: the sound leaving the machine (or the microphone)
  as 64 logarithmic bands a channel, from Bioma's sinestesia-bands, now
  `genoma spectrum`: a process of its own, started by the first
  `spectrum:hold()` and stopped by the last `:release()`. A shader takes
  the bands as its values; `examples/effects` draws them.
- `backdrop = true` on a `g.shader`: it reads what its surface drew under
  it through `g_backdrop(p)`, 24 logical units beyond its edges too,
  drawn again only when that changes. `genoma:glass` (and
  `effects.glass` in the kit) is a pane that bends it at its rim, the
  colours parting there, frosted, tinted, its top edge lit.
- `backdrop_blur`: frosted glass inside the shell. What the surface drew
  under a flat node shows through it blurred, inside its rounded shape,
  whatever the compositor offers; re-blurred only when what is under it
  changes.
- `blur_radius`: a node and its subtree drawn blurred, after depth, the
  spread in logical units; nothing done at 0, and a blur at rest kept
  rather than drawn again (the subtree's texture is keyed by what it
  draws). See `examples/blur`.
- `shader.pointer` in every `g.shader`: where the pointer is from the
  node's corner, whether it is over the surface and whether a button is
  held. A shader that reads it takes the pointer and is drawn again as it
  moves; the others are not. `examples/effects` has a spotlight.
- `@genoma/session`, logind: `session.can` (each action "yes", "no",
  "challenge" or "na"), `suspend`, `hibernate`, `hybrid_sleep`, `reboot`,
  `power_off`, `preparing_for_sleep` and `preparing_for_shutdown`,
  `lock_requests` (loginctl lock-session), `lock_session`, and inhibitors:
  `session:inhibit(what, why, mode)` held until `:release()`, and the
  list of who holds what. A "delay" inhibitor on "sleep" with
  `preparing_for_sleep` locks before the machine sleeps.
- `@genoma/apps`: the applications the menu offers, read in the user's
  language with their actions; `apps:search` ranks by name, words,
  description and letters in order, the most used rising among equals
  (kept in `$XDG_STATE_HOME/genoma/apps.json`); `apps:launch` expands
  `%f`/`%u`/`%i`/`%c`/`%k`, opens a terminal for those that want one and
  starts them out of the shell's session; `apps:find` names the
  application of a window's class or a process. See `examples/launcher`.
- Keyboard navigation: `navigate = true` on a node lets Tab, Shift+Tab
  and the arrows walk its buttons and sliders, a ring (`focus_color`)
  round the focused one, Return and space press it; a focused slider takes
  Left and Right (the kit's steps by its `step`). Keys the walk leaves go
  to the focused node and up. Opt-in: interfaces without it behave as
  before.
- An inspector: `genoma ipc genoma tree` gives a running interface's
  surfaces and nodes (kind, role, id, text, where drawn, the properties
  set) as JSON, `genoma ipc genoma highlight <surface> <node>` outlines a
  node in magenta, and `examples/inspector` is a panel built on both. The
  IPC target `genoma` is Genoma's own: an interface's `g.ipc("genoma", …)`
  is no longer reached.
- `label_width` on the kit's list: a long label ends in an ellipsis.
- `docs/reference.md`, the API reference, generated by `genoma reference`
  from the node schema and the API's types; a test keeps it current and
  another keeps the types naming everything the runtime registers.
- `genoma types` prints the whole API as Luau types (`g`, every service,
  every node's input handlers), and `genoma types <dir>` writes them as a
  directory to alias `@genoma` to, with the kit's own sources, so
  luau-lsp checks an interface's requires. The kit's components export
  the types of what they take.
- A shader library in every `g.shader`, prefixed `g_`: gradient, simplex,
  cellular and value noise, fbm, smooth min and max, distance fields of
  circles, boxes and segments, antialiased coverage, colour helpers.
  Shaders that define their own names are unaffected.
- Built-in effects, `src = "genoma:noise"`, `"genoma:ripple"`,
  `"genoma:dissolve"`, `"genoma:vignette"` and `"genoma:grain"`, and
  `@genoma/kit/effects` to use them with named parameters. See
  `examples/effects`.
- `g.path`: any outline, from SVG path data (`d`) or a polygon
  (`points`), filled, stroked, dashed, with `join`, `cap` and `even_odd`.
- `combine_with`, `combine` and `combine_soft` on capsules: drawn as one
  shape with another capsule by id, joined (softly: metaballs), with it
  cut out, or where both are. See `examples/shapes`.
- `@genoma/kit`, a component kit with a neutral look: `button`,
  `toggle`, `slider`, `progress`, `menu`, `dropdown`, `tooltip`, `list`,
  `tabs`, `field` (a text field, its rim lit while it has the keys,
  optionally clearable) and `dialog` (a surface of its own, the rest
  dimmed), restyled
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

- `examples/bar` took a battery as charging when its state was
  "fully-charged", which UPower's service never says: "full".
- A full, still capsule (no `level` below 1, no wave) cut off what it drew
  past its own box: the part of a bud's bridge beyond it, by a row of
  pixels, and now a shape combined with another.
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
