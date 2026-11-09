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
needs `ext-background-effect-v1`, as niri 26.04 has it. The workspace and
window services read niri's IPC. On other compositors the rest of the toolkit
works, and those services stay empty.

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
  shadow, linear/radial/conic gradients, radius, blur), and there are also `g.text`, `g.icon`,
  `g.image`, `g.shader` and particle emitters. `g.each` is a keyed list.
  `genoma types` prints every property with its type and description, as
  `.d.luau` definitions for `luau-lsp`.
- **Bindings:** any property can be a function. It is re-evaluated only when
  a signal it read changes. `g.value`, `g.computed`, `g.effect` and
  `g.watch` make your own signals.
- **Timing:** every duration has a name in `g.timing { … }`. A literal
  duration in an interface is rejected at load. A transition names the
  timing to use, so retiming a shell is one table.
- **Input:** `on_click`, `on_wheel`, `on_drag`, `on_key`, `on_hover`,
  `on_fling` and others. The input region follows the nodes that take input.
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
| `niri` | workspaces, focused window, outputs (niri IPC) |
| `toplevels` | windows through `ext-foreign-toplevel-list` |
| `clipboard` | every copy's types and text, set, save to a file, clear (`ext-data-control`) |

Anything else is built from `g.process`, `g.dbus`, `g.socket`, `g.file` and
`g.timer`, in Luau. `g.pam` and `g.greetd` serve lock screens and greeters.

## Command line

| Command | |
|---|---|
| `genoma <dir> [entry]` | run the interface in `<dir>`, from `shell.luau` or another entry file |
| `genoma types` | print the node schema as `.d.luau` |
| `genoma probe` | print every service's signals as they change, without UI |
| `genoma ipc <target> <function> [args…]` | call into a running interface (`g.ipc`) |
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
