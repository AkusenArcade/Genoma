<h1 align="center">
  <img src="docs/media/logo.png" width="240" alt="Genoma — A Living Toolkit">
</h1>

A Wayland shell toolkit in Rust. Interfaces are written in Luau and reloaded
while they run.

Genoma opens layer surfaces on the compositor, draws them on the GPU with
distance-field primitives, feeds them the system's state as reactive signals,
and reloads them when their files change. It was built for
[Bioma](https://github.com/AkusenArcade/Bioma), and for what Bioma could not
afford on Quickshell: shapes that bud and grow, particles, and depth
transitions. **At rest it costs nothing.** A surface where no value changes
requests no frame, runs no timer and evaluates no binding.

**Status:** 0.1, young. Bioma runs on it whole: its shell, its lock screen
and its greeter. Other shells can be written on it today (see
[`examples/bar`](examples/bar/shell.luau)), but the API is still shaped by
Bioma's needs and will change between minor versions. Changes are listed in
[`CHANGELOG.md`](CHANGELOG.md). The design is in
[`docs/genoma-prd.md`](docs/genoma-prd.md).

## Installing

- **Arch:** `genoma` from the AUR, which installs `/usr/bin/genoma`.
- **From source:** Rust stable, `clang` (for the PipeWire bindings), a
  Vulkan driver, and the `wayland`, `libxkbcommon`, `libpipewire` and `pam`
  libraries. Then:

  ```sh
  cargo build --release -p genoma
  ```

It runs on Wayland compositors with `wlr-layer-shell`. Blur behind surfaces
needs `ext-background-effect-v1`, as niri 26.04 has it. Workspaces and
windows come from the compositor's own IPC where Genoma knows it:
`@genoma/compositor` reads niri, Hyprland (0.57, configured in Lua or
hyprlang) and Umbriel, whichever it finds running. Any other compositor is
reached through the standard protocols, `ext-workspace-v1` and
`wlr-foreign-toplevel-management` (`compositor.name` is then `"wayland"`):
the same calls, plus minimizing, but no window is placed on a workspace.
`GENOMA_COMPOSITOR=wayland` asks for the protocols even where an IPC is
known.

## A first shell

```sh
GENOMA_OUTPUT=DP-1 genoma examples/bar
```

[`examples/bar`](examples/bar/shell.luau) is a classic top bar in about 200
lines: workspaces, the focused window's title, CPU, memory, volume (wheel to
change, click to mute), battery, and the time, which opens the date in a
popup. Edit `shell.luau` while it
runs and the bar reloads; values kept with `g.state` survive the reload. An
error keeps the last good tree on screen and is logged.

The other pieces of a desktop are examples too, each a page or two:
[`launcher`](examples/launcher/shell.luau) (applications found and
started), [`dock`](examples/dock/shell.luau) (pinned and running, on any
compositor), [`notifications`](examples/notifications/shell.luau)
(a notification centre, Genoma serving the bus name),
[`osd`](examples/osd/shell.luau) (volume, and `genoma ipc osd show`),
[`lock`](examples/lock/shell.luau) (ext-session-lock and PAM, its own PAM
file beside it), [`prompt`](examples/prompt/shell.luau) and
[`list`](examples/list/shell.luau). [`kit`](examples/kit/shell.luau),
[`shapes`](examples/shapes/shell.luau),
[`theme`](examples/theme/shell.luau), [`layout`](examples/layout/shell.luau), [`gestures`](examples/gestures/shell.luau), [`components`](examples/components/shell.luau), [`effects`](examples/effects/shell.luau), [`passes`](examples/passes/shell.luau), [`particles`](examples/particles/shell.luau), [`simulations`](examples/simulations/shell.luau), [`morph`](examples/morph/shell.luau), [`blur`](examples/blur/shell.luau),
[`charts`](examples/charts/shell.luau) and [`inspector`](examples/inspector/shell.luau) show the toolkit itself.

An interface is a directory with a `shell.luau` that returns one or more
surfaces:

```luau
local g = require("@genoma")
local clock = require("@genoma/clock")

g.timing {
    reflow = { duration = 0.2, curve = { 0.2, 0.0, 0.0, 1.0 } },
}

return g.surface {
    anchor = "top",
    margin = { top = 12 },
    g.capsule {
        height = 36,
        padding_x = 16,
        fill = "#16181ded",
        g.text {
            size = 14,
            color = "#d8dee9",
            -- A function is a binding: it re-runs when what it reads changes.
            text = function() return string.format("%02d:%02d", clock.hour, clock.minute) end,
        },
    },
}
```

### Concepts

- **Nodes:** `g.box` lays out, `g.capsule` is a box that draws (fill, rim,
  shadow or glow outside it, inner shadow, an outline apart from the rim,
  linear/radial/conic gradients, radius, blur), and there are also `g.text`, `g.icon`,
  `g.image`, `g.shader` and particle emitters. `g.each` is a keyed list.
  An image may be an animated GIF, WebP or PNG: it loops in its own frame
  times while `playing` (true by default), and the surface is drawn again
  only when the frame changes. `slice = 16` (or `{ top, right, bottom,
  left }`, in the picture's own pixels) draws it as a nine-slice: corners
  kept, edges and middle stretched, a frame of any size from one picture.
  An image takes `saturation`, `contrast`, `brightness` and `tint`, applied
  as it is drawn and animatable (a cover greyed while paused, a wallpaper
  dimmed and blurred behind a lock with `soften`).
  `g.list` is one that scrolls and mounts only the rows in view.
  `g.path` draws any outline: `d` (SVG path data) or `points` (a polygon),
  filled with `fill`, stroked with `color` and `stroke`, with `join`,
  `cap`, `dash` and `even_odd`. It is drawn at the exact pixel size, and
  takes its outline's size unless given one.
- **Layout:** rows and columns (flexbox: `direction`, `align`, `justify`,
  `gap`, `grow`). `flow = true` sends what does not fit on to the next
  line, `gap_lines` apart. An `overlay` sits over its siblings at its
  `attach`, or pinned to its parent's edges with `left`, `top`, `right`
  and `bottom` — stretched between two when it has no size there.
  `scroll = "y"` (`"x"`, `"both"`) makes any node scroll its children:
  wheel, touchpad and fling, `snap = "start"` or `"center"` to rest on a
  child, `on_scroll(x, y)`; a scroller inside another takes only its own
  axes and hands the rest on. See `examples/layout`.
- **Shaders:** `g.shader { src = "field.wgsl", values = … }` draws a
  rectangle whose every pixel is your `fn shade(p: vec2<f32>) ->
  vec4<f32>`, fed `shader.size`, `shader.scale`, `shader.time` (with
  `clock = true`), `shader.pointer` (x, y from the node's corner; z 1 over
  the surface; w 1 while a button is held: a shader that reads it takes the
  pointer and is drawn again as it moves) and `value(i)`, the numbers
  bound: a theme's palette goes there. Every shader also has
  Genoma's library, prefixed `g_`: noise (`g_noise`, `g_noise3`,
  `g_simplex`, `g_worley`, `g_fbm`, `g_fbm3`, `g_value_noise`,
  `g_hash`…), distance fields (`g_smin`, `g_smax`, `g_sd_circle`,
  `g_sd_box`, `g_sd_rounded_box`, `g_sd_segment`, `g_cover` for an
  antialiased edge) and colour (`g_rgba`, `g_premul`, `g_over`); see
  `crates/genoma-render/src/shaders/library.wgsl`. A shader with
  `backdrop = true` reads what its surface drew under it with
  `g_backdrop(p)`, a little beyond its edges too. Eleven effects come built
  in, `src = "genoma:noise"` (`ripple`, `dissolve`, `vignette`, `grain`,
  `glass`, a pane that bends what is under it at its rim, `chart`, and the
  passes `pixelate`, `chromatic`, `wave` and `glow`), and
  `require("@genoma/kit/effects")` gives them as nodes with named
  parameters. `GENOMA_SHADER=file.wgsl cargo test -p genoma-render
  shader_file` checks a shader without drawing it. See `examples/effects`.
- **Particles:** `g.emitter` throws particles at a `rate`, ballistic
  (`speed`, `heading`, `spread`, `gravity`) or carried to a point
  (`to_x`, `to_y`, along a curve with `bend_x`, `bend_y`). Each is worked
  out on the GPU from its birth: `color_end` and `size_end` change it over
  its life, `trail = 0.3` draws that many seconds of its path behind it,
  and an emitter inside an emitter with `sub = true` bursts `burst`
  particles where each of its parent's dies (fireworks). `attract = "id
  pointer"` pulls them toward nodes' middles or the pointer
  (`attraction`, negative pushes; `attraction_reach`), `collide = "id"`
  bounces them off nodes' boxes (`bounce`): such an emitter's particles
  are stepped by a compute pass instead, the others cost no buffer at
  all. `icon` or `src` draws each as a picture tinted by its colour,
  turning with `spin`. See `examples/particles`.
- **Simulations:** a `g.shader` with `compute = "file.wgsl"` steps a
  simulation of its own on the GPU while its `clock` runs, `steps` a
  second: `fn step(cell, me) -> vec4<f32>` works out every cell of a
  `grid` from its neighbours a step before (`g_cell`), `fn agent(i, me)
  -> Agent` every one of its `agents` from all the others (`g_agent`),
  `sim.frame` 0 to set things up. Its `shade` reads `g_state(p)`,
  `g_cell` and `g_agent`, and without a `src` the same file's `shade`
  draws it; `agent_size` has Genoma draw the agents too.
  `genoma:reaction-diffusion`, `genoma:waves` and `genoma:boids` come
  built in, and as `kit.effects` nodes. See `examples/simulations`.
- **Shared transitions:** nodes with the same `morph = "name"` are one
  thing in two states. When one is shown where another of that name was
  drawn a moment ago and is no longer shown, it grows from that one's
  rectangle and roundness to its own on the `morph` timing (or
  `morph_timing`), the other's picture fading out inside the moving shape
  and its own content coming in; never scaled. A chip opens into a panel
  and closes back into it. See `examples/morph`.
- **Post-processing:** a `g.shader` child with `pass = true` is not drawn
  over its parent but run on it: the parent and its other children are
  drawn into a picture, and each pass, in the order written, draws it
  again over its own box, reading what the pass before it left with
  `g_backdrop(p)` (what its `shade` returns there replaces what was).
  A pass covers its parent unless given a width and height; a larger one
  lets a glow reach past the parent's edges. The picture is kept while
  what is in it and its passes' numbers, clock and pointer rest.
  `cache = true` is the same picture with no pass: a rich subtree beside
  something that moves costs one image a frame. See `examples/passes`.
- **Masks:** a child with `mask = true` is not drawn but used as its
  parent's mask: the parent and its other children show only where the
  mask is opaque, as much as it is. A text over a picture with
  `overlay = true` and `mask = true` lets the picture show through the
  letters; a capsule with a gradient fades what it masks in its shape.
  Both are drawn into textures kept while they rest.
- **Blend modes:** `blend = "multiply"`, `"screen"`, `"add"` or
  `"overlay"` lays a node and what it holds over what is under it that
  way: a stain, a light, a texture over a picture.
- **Blur:** `blur_radius = 6` draws a node and everything in it blurred,
  after depth has turned it, the spread in logical units as CSS's
  `blur()`. At 0, the default, nothing is done. The subtree is drawn into
  a texture of its own and blurred there (halved first for a wide
  spread); a radius and a content at rest are blurred once and kept, a
  moving radius is blurred each frame. It animates with `transition`. See
  `examples/blur`. `backdrop_blur = 10` is frosted glass: what the
  surface drew under a flat node, blurred, shows through it inside its
  shape, kept at rest as well. Other programs' windows are the
  compositor's to blur (`blur = true`, through `ext-background-effect`).
- **Shapes drawn as one:** a capsule with `combine_with = "id"` is drawn
  as one shape with the capsule of that id, by their distance fields:
  `combine = "union"` (a neck grows between them as `combine_soft`
  says: metaballs), `"subtract"` (a bite, a moon) or `"intersect"` (a
  lens). The named capsule lends its outline; give it no fill unless it
  should be drawn too. See `examples/shapes`.
  The [API reference](docs/reference.md) lists every property, function
  and service. `genoma types ~/.local/share/genoma/types` writes the whole
  API as Luau types, the kit's sources beside them; point `@genoma` at
  that directory in your interface's `.luaurc` (`{ "aliases": { "genoma":
  "…" } }`) and luau-lsp completes and checks every `require("@genoma/…")`.
- **Bindings:** any property can be a function. It is re-evaluated only when
  a signal it read changes. `g.signal`, `g.computed`, `g.effect` and
  `g.watch` make your own signals.
- **Timing:** every duration has a name in `g.timing { … }`. A literal
  duration in an interface is rejected at load. A transition names the
  timing to use, so retiming a shell is one table.
- **Input:** `on_click`, `on_wheel`, `on_drag`, `on_key`, `on_hover`,
  `on_fling` and others. The input region follows the nodes that take input.
  `on_wheel(dx, dy, steps)` is told whether the movement came in a wheel's
  steps or a touchpad's distance. `on_hover` means within: over a child,
  the pointer is still over its parent, which hears nothing until the
  pointer leaves it too.
- **Gestures:** a touchscreen's first finger is the pointer (press, drag,
  click), two fingers pinch. `on_pinch(scale, rotation, phase, x, y)`
  hears two fingers on a screen or a touchpad, `on_swipe(direction,
  speed)` a quick stroke (a drag let go moving, or a touchpad swipe),
  `on_long_press(x, y)` a press held still (the release is then no
  click), `on_drag_start` and `on_drag_end(vx, vy)` a drag's ends;
  `drag_threshold = 6` makes a press a drag only past that distance.
  Fingers resting on a touchpad (`on_hold`) stop a scroller's fling.
  `require("@genoma/kit/gestures")` has them ready: `draggable` (bounds,
  a throw that carries on, stops to settle on), `zoomable` (pinch and
  wheel, springing back past its limits) and `dismissable` (swiped away,
  or back if not far enough). See `examples/gestures`.
- **Popups inside a tree:** a `g.popup` written as a child of a node is
  taken out at load and becomes a surface of its own, anchored to that
  node (which is given an id if it has none): a menu is written inside the
  button that opens it. Popups written there may hold popups (a submenu).
  One inside a `g.each` row or a function of the output is refused; write
  it beside the surfaces with an `anchor` instead.
- **The kit:** `require("@genoma/kit")` gives `button`, `toggle`,
  `slider`, `progress`, `menu`, `dropdown`, `tooltip`, `list`, `tabs`,
  `field` and `dialog`, and the larger `tree` (virtualised, branches
  opened lazily), `calendar` (marks, ranges), `color_picker` (with a
  swatch `color_picker.button` that opens one) and `file_picker` (places,
  filter, preview), built on the nodes above and drawn in a neutral look of their
  own, and `chart` (`chart.line`, `chart.area`, `chart.bars`: up to 1008
  numbers drawn by one shader node, no node per point). A dialog is a surface, returned beside the others.
  `kit.theme.set { accent = "#e6b062", radius = 10 }` changes it for every
  component on screen, the colours moving there. Each is also
  `require("@genoma/kit/button")` and so on, and the sources are in
  [`kit/`](kit). Its times are `kit_quick`, `kit_move` and `kit_open`,
  defined with `g.timing_default`, so a shell retimes them with
  `g.timing` before or after requiring the kit. See `examples/kit`.
- **A theme of one's own:** `require("@genoma/theme").new { roles,
  derive, fixed, sizes, steps, fonts, type, timing }` gives a shell's
  tokens as values every node binds to: `t.colour("surface")`,
  `t.size("radius")` at the density step, `t.text("label", { … })`, and
  the timing set after `t.speed`. `t.set { primary = "#…" }` and a palette
  file followed with `t.source { path }` change the roles in a crossing,
  every derived colour with them, the desktop moving together.
  `kit.theme.follow(t)` gives the kit the same look. Colour arithmetic is
  `g.colour` (`lift`, `mix`, `alpha`, `hsla`, `contrast`, …). See
  `examples/theme`.
- **Waiting without nesting:** `require("@genoma/async")` runs a task
  (`async.run(function() … end)`) in which `async.process`,
  `async.sleep`, `async.call` and `async.get` (D-Bus) and
  `async.activation_token` wait for their answer as if it came at once,
  and `async.all { … }` waits for several side by side; nothing else stops
  meanwhile. A coroutine underneath; its source is in [`kit/async.luau`](kit/async.luau).
- **Keyboard:** a node with `navigate = true` is walked by the keyboard
  once its surface has the keys: Tab and Shift+Tab, or the arrows, move
  the focus among its buttons and sliders, ringed in `focus_color`; a
  focused slider takes Left and Right; Return and space press; Escape lets
  the focus go. Other keys reach the focused node and its ancestors'
  `on_key`. A press of the pointer takes the ring away. Without
  `navigate`, keys go to what catches, as `g.focus_move` and
  `g.focus_press` walk it.
- **Inspector:** `GENOMA_IPC=inspector GENOMA_INSPECT=<name> genoma
  examples/inspector` opens a panel beside another running interface (its
  `GENOMA_IPC`, `genoma` by default): its surfaces, its tree of nodes as
  they are, and the chosen node's properties, outlined on screen where it
  is drawn. It is a Genoma interface itself, on `genoma ipc genoma tree`
  and `highlight`.
- **Long lists:** `g.list { items = …, key = …, row = …, row_height = 28,
  height = 336, reveal = … }` takes `g.each`'s three functions, but mounts
  only the rows in view and a few beyond, so thousands of items cost what a
  dozen do. Every row is `row_height` tall. The list needs a height of its
  own (`height`, or `grow` in a bounded parent): its rows do not size it.
  It scrolls by itself: a wheel step is three rows, glided to; a touchpad
  moves it as the fingers do and flings it when they lift. `reveal`, a
  row's place from 1, brings that row into view the least way, which is
  what keyboard selection wants. `on_scroll(offset, room)` reports where it
  is, for a scroll bar of your own. See `examples/list`.
- **Fields:** `g.field { text = …, on_change = …, on_submit = …, focused = … }`
  is a line to type into: caret and selection by key and by pointer
  (double-click a word, triple-click the line), word moves, undo and redo,
  Ctrl+C/X/V through the clipboard, `secret` for passwords, and input methods
  (`text-input-v3`) for scripts typed by composition. Keep its text in a
  signal written by `on_change`. A surface takes the keyboard while a node
  catches, or as its `keyboard` says (`"on_demand"` for a bar with a search
  field, `"exclusive"`). See `examples/prompt`.
- **Popups:** `g.popup { anchor = "clock", side = "bottom", align = "end",
  present = …, on_dismiss = … }` opens beside the node with `id = "clock"`
  on another surface, over everything. The compositor turns it to the other
  side or slides it along when it would leave the screen. Opened by a press
  or a key, it takes the keyboard (keys reach a `catch` node's `on_key`) and
  a press elsewhere closes it, calling `on_dismiss`. `anchor` may be a
  function, so one menu serves every item of a tray.
- **Living layer:** threads that grow between nodes, budding panels joined by
  a narrowing neck, springs, and 3D tilt and depth (`rotate_x`, `rotate_y`,
  `depth`, `perspective`). See `examples/living`, `examples/budding`.

### Services

`require("@genoma/<name>")` returns one shared object per domain. It starts on
first use, and every field is a signal.

| Service | What it gives |
|---|---|
| `clock` | hour, minute, second |
| `vitals` | CPU, memory, GPU load and clocks |
| `power` | battery (UPower), power profiles |
| `audio` | PipeWire nodes, links, defaults, volume and mute |
| `media` | MPRIS players, metadata, position, controls |
| `network` | NetworkManager devices, Wi-Fi networks, join and forget |
| `bluetooth` | BlueZ adapter and devices |
| `notifications` | an `org.freedesktop.Notifications` server |
| `tray` | StatusNotifierItem with DBusMenu |
| `polkit` | a polkit authentication agent |
| `idle`, `lock` | idle notification, session lock |
| `displays` | the displays and their setup through `wlr-output-management`: modes, position, scale, transform, adaptive sync, on or off; changed or only tested, the rest kept as it is |
| `gamma` | colour temperature, brightness and gamma per display through `wlr-gamma-control`: a night light without wlsunset |
| `compositor` | workspaces, windows, the focused one, switching, closing and (through the protocols) minimizing: niri, Hyprland and Umbriel by their IPC, any other by `ext-workspace` and `wlr-foreign-toplevel-management` |
| `niri` | the same from niri alone, with niri's numeric ids (niri IPC); kept for shells written against it |
| `toplevels` | windows through `ext-foreign-toplevel-list` |
| `clipboard` | every copy's types and text, a history of texts (secrets left out), set, save to a file, clear (`ext-data-control`) |
| `spectrum` | the sound as 64 bands a channel, captured (`genoma spectrum`, PipeWire) only while held: for a visualiser, or a shader's values |
| `session` | logind: suspend, hibernate, reboot, power off, what is allowed, the moment before sleep, lock requests, inhibitors held and listed |
| `apps` | the applications the menu offers: search ranked by use, launch with actions and files, find by window class (desktop entries) |

Anything else is built from `g.process`, `g.dbus`, `g.socket`, `g.file` and
`g.timer`, in Luau. `g.pam` and `g.greetd` serve lock screens and greeters.

## Command line

| Command | |
|---|---|
| `genoma <dir> [entry]` | run the interface in `<dir>`, from `shell.luau` or another entry file |
| `genoma types [dir]` | print the API as Luau types, or write them to `dir` for luau-lsp |
| `genoma reference` | print the API reference in Markdown (`docs/reference.md`) |
| `genoma probe` | print every service's signals as they change, without UI |
| `genoma ipc <target> <function> [args…]` | call into a running interface (`g.ipc`) |
| `genoma ipc genoma tree` | every surface and node of a running interface, as JSON; `genoma ipc genoma highlight <surface> <node>` outlines one, `… highlight none` |
| `genoma new <dir>` | a shell to start from: a bar with its name and a clock that opens a popup, written with the kit; its types in `types/` with a `.luaurc` for the editor and luau-analyze; a README of these commands |
| `genoma snapshot <dir>` | the interface run without a compositor, on a screen of `--size` named `--output`, its services as they are, `--wait` seconds for timers and animations; after the load, in the order written, `--ipc "target function args"` calls, `--click <id>` presses and `--sleep <seconds>`, and `--mock @genoma/<service>=<file>` gives a module of one's own in place of a service (a cell seen with the numbers a test chooses); every surface present drawn into `<n>-<namespace>.png` and the trees written to `tree.json` in `--out`. The compositor's blur is not in the pictures |
| `genoma check [dir]` | every `.wgsl` checked as a `g.shader` compiles it and every `.luau` by the Luau compiler, each mistake with its file and line; exit status 1 when one is found. A shader that does not compile is also drawn as its mistake over the node while the interface runs |
| `genoma ipc genoma profile on` | the profiling overlay over every surface: a line each (size, frames a second, time on the main thread and on the GPU, the wait for a buffer over 1 ms, how much the last frame drew again), the textures kept and their memory, and an outline round what was drawn again; `… profile off`. `GENOMA_OVERLAY=1` starts with it on, `GENOMA_PROFILE=1` logs where the time in Luau goes |
| `genoma pam <service> [dir]` | one PAM authentication, from the terminal |
| `genoma polkit-test` | the polkit agent against one process of its own |

Environment: `GENOMA_OUTPUT` picks the output for a test run,
`GENOMA_IPC` names the IPC socket, and `GENOMA_LOG=info,genoma=debug` logs
frames and the GPUs on offer.

## Versioning

Genoma follows semantic versioning. Before 1.0, a **minor** version
(0.1 → 0.2) may break interfaces, and a **patch** version (0.1.1 → 0.1.2)
does not. Every change an interface could notice is in
[`CHANGELOG.md`](CHANGELOG.md). A breaking change is marked **Breaking** and
says what to change.

## Licence

GPL-3.0-or-later. See `LICENSE`.

That covers what is built on it too. A shell written for Genoma calls its
API and requires its Luau library (`@genoma/kit`, which is part of
Genoma), so it is a work based on Genoma: whoever distributes one does so
under the GPL-3.0-or-later or a compatible licence, with its source.
There is no exception for scripts. Bioma, the shell Genoma was made for,
is GPL-3.0 as well. Running a shell of your own without distributing it
asks nothing.
