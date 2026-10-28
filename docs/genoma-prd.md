# Genoma — Product Requirements

A Wayland shell toolkit in Rust: the code an organism is built from.

Genoma is to Bioma what Quickshell is to Bioma today. It is the layer that
opens surfaces on the compositor, draws them, feeds them the system's state and
reloads them while they run. Bioma is its first consumer and the reason it
exists, but Genoma knows nothing of membranes, tissues or cells. Those stay
Bioma's, written on top of it.

Target compositor: niri (any compositor with the protocols in §6.1 works).
Implementation language: Rust. Interface language: Luau, hot-reloaded (§7).
Language: English throughout — code, comments, identifiers, documents.
The project name **Genoma** is the only Italian word retained.

Status: design draft, 2026-10-28. Nothing built. Open questions in §12.

---

## 1. Why

Bioma on Quickshell works, and on a capable desktop its cost does not matter.
It matters on weaker or more complicated machines. On a five-year-old RTX 3060
laptop the animations run smoothly only in performance mode, and resource use
stays high. The cost is also the ceiling on what Bioma can become: the "living"
look it aims at needs motion, and on Qt every motion is paid for twice, once on
the CPU and once in the driver.

**What was measured** (desktop, Bioma at rest, clock and vitals live,
2026-10-28; `perf` with user call chains, CPU per thread from `/proc`, PSS from
`smaps_rollup`):

| | CPU, one core | Memory (PSS) |
|---|---|---|
| Bioma, Qt on OpenGL | 10.1% | 579 MB |
| Bioma, Qt on Vulkan (default since beta.58) | 6.5% | 511 MB |
| Minimal Quickshell shell, 10 windows | — | 69 MB |
| Noctalia (C++, its own EGL renderer, no Qt) | 1.3% | 119 MB |

Where Qt's time went on OpenGL: about 35% in the Mesa driver, through 11 GL
contexts (one per window). On the main thread:

| Work | Main-thread time |
|---|---|
| Item polish | 5.9% |
| JavaScript calls | 5.3% |
| Animation ticks | 3.6% |
| Re-triangulating `Shape` paths on the CPU, even with `CurveRenderer` | 3.7% |
| Allocating `Qt.vector4d` values in bindings | 1.9% |

The JavaScript engine itself was about 5% of the process.

**What this says.** A rewrite in a faster language buys little by itself: QML
is not the floor. The gains are in three architectural choices that Qt does
not let a shell make:

1. **One GPU context for every surface**, not one per window.
2. **Draw only what changed.** Damage tracking per surface, and a frame only
   when a value that something reads has changed.
3. **Shapes evaluated in the shader** (signed distance fields), so no path is
   ever triangulated on the CPU and no shape depends on the CPU at all once
   its parameters are on the GPU.

Genoma is built around those three. The language is Rust because it gives
control over memory and threads without a garbage collector, and because the
Wayland and GPU ecosystem it needs (§6) is mature in Rust.

**Targets** (§11 makes them gates): Bioma's resting desktop on Genoma at
**≤ 1.5% of a core and ≤ 150 MB PSS** excluding the wallpaper, and every
animation at the display's refresh rate on the laptop's integrated GPU.

## 2. What Genoma must keep from Quickshell

Quickshell's value is not only its renderer. Three things made Bioma possible,
and Genoma fails if it loses any of them:

1. **The system at hand.** Battery, audio, media, network, Bluetooth,
   notifications, tray, polkit, PAM, greetd, windows and workspaces, all as
   ready objects with properties that change. Bioma uses eleven such modules
   and writes its other ~39 services itself through processes, D-Bus and
   sockets. Genoma provides the eleven (§9) and the means for the rest
   (processes, D-Bus, sockets, files — §9.2).
2. **Interfaces that reload while they run.** Save a file, see the change.
   This is how Bioma is developed, and why a new cell costs one config block.
3. **Declarative binding.** A property is a function of other properties, and
   it stays true without anyone updating it.

## 3. Principles

Decision rules for the toolkit. Bioma's own principles (its PRD §3) still
govern what Bioma draws. These govern how Genoma lets it be drawn.

1. **Nothing runs at rest.** A surface where nothing changes costs zero CPU and
   zero GPU: no frame is requested, no timer ticks, no binding runs. Idle is
   measured, not assumed (§11).
2. **A frame is caused by a value.** Every frame traces back to a signal that
   changed, or to an animation in flight, which is a signal that changes with
   time. "Redraw everything every vblank" is never a mode.
3. **The CPU describes, the GPU draws.** The CPU sends parameters (sizes,
   colours, progress) and the GPU evaluates the shapes. No tessellation, no CPU
   rasterisation, no canvas.
4. **Resolution-independent by construction.** Shapes are distance fields and
   text is drawn from a glyph atlas at the current scale (or MSDF). Any
   transform, including depth, keeps edges and text sharp. This is what lifts
   the reason behind Bioma's "never `transform: scale`" rule (§8.4).
5. **Logical units everywhere; physical pixels only for hairlines.** As in
   Bioma: dimensions are logical, and only critical thin lines snap to the
   physical pixel.
6. **One owner per domain.** One service object per system domain, shared by
   every consumer. Never a poller per widget.
7. **The script describes, Rust does.** Luau code builds trees and writes
   bindings. Loops over pixels, particles, paths or large data live in Rust or
   on the GPU, never in the script.
8. **Measure before believing.** Every milestone ends with the same
   measurement as §1, and a number goes into the commit message.

## 4. Architecture

```
               ┌──────────────── Luau interface files (hot-reloaded) ───────────────┐
               │   shell.luau, cells/*.luau, … — trees of nodes + reactive bindings   │
               └───────────────▲───────────────────────────────┬─────────────────────┘
                               │ signals read                  │ node trees
┌──────────────────┐   ┌───────┴────────┐   ┌──────────────────▼──────────────┐
│ genoma-services  │──▶│  genoma-core   │──▶│          genoma-scene           │
│ UPower, PipeWire,│   │ signals, time, │   │ node tree, layout (taffy),      │
│ MPRIS, NM, BlueZ,│   │ animations,    │   │ damage, hit testing, input      │
│ niri IPC, notifs │   │ scheduler      │   │ routing                         │
└──────────────────┘   └───────▲────────┘   └──────────────────┬──────────────┘
                               │                                │ draw lists (instances)
                       ┌───────┴────────┐   ┌──────────────────▼──────────────┐
                       │ genoma-wayland │◀─▶│          genoma-render          │
                       │ layer-shell,   │   │ wgpu (Vulkan): SDF primitives,  │
                       │ lock, blur,    │   │ glyph atlas, images, particles, │
                       │ outputs, seat  │   │ custom shaders, depth           │
                       └────────────────┘   └─────────────────────────────────┘
```

**Crates** (one Cargo workspace):

| Crate | Owns |
|---|---|
| `genoma-core` | The reactive graph (signals, derived values, effects), the clock, animations as time-driven signals, the frame scheduler, the timing set. |
| `genoma-scene` | The node tree, layout through `taffy`, per-surface damage, hit testing, focus, input routing. |
| `genoma-render` | One `wgpu` device for the process; the SDF primitive pipeline, text, images, particles, custom shaders, the depth path. |
| `genoma-wayland` | Surfaces and protocols through `smithay-client-toolkit` (§6.1), outputs and fractional scale, seat (pointer, keyboard, touch), frame callbacks. |
| `genoma-script` | Luau through `mlua`: module loader, node constructors, binding capture, hot reload, sandbox. |
| `genoma-services` | The service objects (§9), each a set of signals. |
| `genoma` | The binary: `genoma run <dir>`, `genoma ipc …`, `genoma log`. |

**Threads.**
- **Main thread:** the `calloop` event loop. It runs Wayland events, service
  events, the script and the reactive graph. One thread owns all state, so no
  state is locked.
- **Render thread:** owns the GPU device and receives immutable draw lists.
- **Service threads,** where a library demands one (PipeWire's loop, for
  example). They post into the main loop and never touch state.

**One frame, start to finish:**
1. A value changes (a D-Bus signal, a niri event, the clock).
2. Its signal marks the bindings that read it.
3. Those re-run and set node properties.
4. Each changed property damages its node's rectangle on its surface.
5. Only damaged surfaces request a frame callback.
6. On the callback, layout runs for dirty subtrees only, the draw list for the
   damaged region is rebuilt, the render thread draws it, and the surface
   commits with damage.

With nothing changing, nothing in this chain runs.

## 5. Rendering

### 5.1 One device, many surfaces

Each Wayland surface gets a `wgpu::Surface` on the **same** device. Pipelines,
the glyph atlas and textures are shared. A new surface costs a swapchain and
nothing else. (Qt spent a context, a render thread and a driver dispatch per
window.)

**The device is the compositor's.** On a hybrid laptop, rendering on a
different GPU from the compositor copies every frame between GPUs. That was
measured on the RTX 3060 laptop: niri on the NVIDIA card, Qt on the Vega. So
Genoma reads the main device from `linux-dmabuf` feedback and picks the adapter
whose DRM node matches. Through Vulkan that is `VK_EXT_physical_device_drm`;
whether `wgpu` exposes it or `ash` is needed is a milestone-0 question (§12).
An override exists (`GENOMA_ADAPTER`).

### 5.2 Primitives

Everything Bioma draws decomposes into a small set of distance-field
primitives, drawn as instanced quads in one pipeline. A frame is a few draw
calls, not one per item:

| Primitive | Parameters | Bioma uses it for |
|---|---|---|
| `rect` | size, per-corner radius, border width, border gradient | capsules, panels, wells |
| `ring` | radii, start/sweep, cap, dash pattern | gauges, dials, rings, the media dial |
| `thread` | quadratic/cubic Bézier, width profile, dash, growth `0…1` | connectors, threads, rate threads |
| `disc` | radius, fill gradient | marks, dots |
| `glyph` | atlas rect, colour | all text |
| `image` | texture, fit, radius mask | wallpaper, covers, portraits |
| `shader` | user WGSL fragment + uniforms | fluid, lava, cytoplasm, osmosis |

- **Lit border, shadows, inner glow.** All of these come from the distance
  field: a border is a band of the field, and an outer shadow is the analytic
  Gaussian of a rounded rect. No offscreen pass, no blur texture.
- **Combining shapes.** Primitives can be combined: union, smooth union
  (`smin`) and subtraction. Smooth union is the basis of §8.2.
- **Custom shaders.** Written in WGSL. Bioma's existing `.frag` files are GLSL
  and can be translated through `naga`'s GLSL front end, once, at port time.

### 5.3 Text

The hardest part of the renderer, and the one that most often goes wrong in new
toolkits, so it is planned rather than discovered.

- **Shaping and layout.** `cosmic-text` 0.19, which brings shaping, fallback
  fonts, bidirectional text and colour emoji.
- **Rasterisation.** `swash` into a shared atlas keyed by font, glyph,
  physical size and subpixel offset. Text is redrawn at each scale, never
  stretched.
- **Under depth transforms** (§8.4), glyphs drawn at the transformed size come
  from the atlas at that size. MSDF is the fallback if atlas churn shows up in
  measurements. That choice is left open until a measurement decides it (§12).
- `glyphon` 0.12 (cosmic-text on wgpu) is a candidate to start with and to
  replace when the shared instance pipeline needs to own glyphs too.

### 5.4 Particles

- **Emitters are nodes.** An emitter's parameters (rate, lifetime, velocity,
  colour, attractor) are properties, so they bind to signals like any other.
- **Simulation runs in a compute shader** over a GPU buffer, and drawing is one
  instanced call. The CPU sends only the parameters, and only when they change.
  Tens of thousands of particles cost a fraction of a millisecond of GPU time
  and nothing on the CPU.
- **An emitter at rate 0 with no live particles requests no frames** (Principle
  1). A particle system is an animation in flight only while particles exist.

### 5.5 Depth

- **Every node may carry a 3D transform:** perspective, rotation about x and y,
  and translation in z. Primitives are evaluated in the node's local space after
  the inverse transform, so edges stay analytic under any projection.
- **Painter's order by default.** Nodes that ask for a depth test get a depth
  buffer for their subtree.
- **Meshes (glTF) are out of scope for v1.** "Three-dimensional" here means
  surfaces and particles in space, not models.

### 5.6 What Genoma does not draw

**Blur behind a surface is the compositor's**, through `ext-background-effect`,
exactly as in Bioma today. Genoma declares the blur region per node and keeps
it in sync with layout. It never blurs in-process, except over its own content
for shell-over-shell overlays (Bioma's `Panel.over`), where an in-process blur
of the surface's own texture is the only option.

## 6. Wayland

### 6.1 Protocols

All verified present in `smithay-client-toolkit` 0.21.1 / `wayland-protocols`
0.32.13 on 2026-10-28:

| Need | Protocol | SCTK module |
|---|---|---|
| Edge surfaces, exclusive zones, layers | `wlr-layer-shell` | `shell::wlr_layer` |
| Lock screen | `ext-session-lock-v1` | `session_lock` |
| Compositor blur | `ext-background-effect-v1` (staging) | `background_effect` |
| Windows list | `ext-foreign-toplevel-list-v1` | `foreign_toplevel_list` |
| Fractional scale | `wp-fractional-scale-v1` + `wp-viewporter` | — (protocol crate) |
| Frame pacing | frame callbacks; `wp-presentation-time`, `fifo`, `commit-timing` | `presentation_time` |
| Cursors | `wp-cursor-shape-v1` | — |
| Device selection | `linux-dmabuf` v4 feedback | `dmabuf` |
| Activation | `xdg-activation-v1` | `activation` |
| Clipboard | `wl_data_device`, `ext-data-control-v1` | `data_device_manager` |
| Text input (IME) | `zwp-text-input-v3` | — |

niri specifics (workspaces, focus, overview, outputs, its config) come from
niri's IPC socket, one `EventStream` connection, as Bioma's `services/Niri.qml`
does now. There is no polling.

### 6.2 Input

- `xkbcommon` (0.9) for keymaps, compose and key repeat.
- Pointer, touch and touchpad, with scroll phases. Kinetic scrolling is the
  toolkit's: Bioma needed `components/Glide.qml` because Qt does none on
  Wayland.
- Input regions follow the nodes that take input. The layer surface's input
  region is rebuilt from layout and never hand-maintained.
- Keyboard interactivity per layer surface: none, on-demand or exclusive,
  switchable at runtime.

## 7. Interface language: Luau

### 7.1 Why Luau

The interface language has to start in milliseconds, reload a file without
restarting the process, be fast enough for bindings, be safe to run from a
user's config, and give editors something to check. Luau (Lua 5.1 lineage,
gradual types, sandboxing, its own fast interpreter with native code
generation) through `mlua` 0.12 meets all five. LuaJIT is faster in raw loops
but untyped and unsandboxed. Bindings are not raw loops (Principle 7), so the
types win. `luau-lsp` gives completion and type errors against Genoma's type
definitions, which Genoma generates from its node and service schemas.

### 7.2 Shape of an interface file

A file returns a tree. A property set to a value is static; a property set to a
**function** is a binding. Genoma runs the function, records every signal it
read, and runs it again only when one of them changes. There is no dependency
list to maintain, as in Solid or QML.

```lua
--!strict
local g = require("@genoma")
local upower = require("@genoma/services/upower")
local theme = require("theme")          -- Bioma's own module: an animated palette

return g.capsule {
    id = "battery",
    present = function() return upower.device.isLaptopBattery end,
    width = 22, height = 22,
    radius = 11,
    border = function() return theme.state(upower.device.level) end,

    g.ring {
        inset = 4,
        sweep = function() return upower.device.percentage / 100 end,
        color = function() return theme.state(upower.device.level) end,
        transition = { sweep = "reflow" },  -- a name from the timing set, never a literal
    },
}
```

- **`present`** is conditional existence. A false value removes the node with
  its exit transition; a true value grows it from its origin.
- **`transition`** names entries of the timing set. Literal durations are
  rejected at load time, which enforces Bioma's rule in the toolkit.
- **Components are functions** returning trees, and are imported with
  `require`.
- **State.** `g.state(initial)` creates a local signal, the counterpart of a
  QML property declared on an item.

### 7.3 Hot reload

- **Watching.** `notify` (8.2) watches the interface directory. On save, the
  changed module and the modules that import it are re-evaluated.
- **Keeping state.** The new tree is reconciled against the old one by `id`
  and position. Nodes that survive keep their local state and any animation in
  flight; only the difference is applied.
- **A file that fails to load leaves the previous tree running.** The error
  goes to the log with file and line, never to a blank screen.
- **Rust is not hot-reloaded.** A change to a service or a primitive needs a
  restart, and the restart is fast.

### 7.4 Sandbox and IPC

- **Sandbox.** Interface code gets Genoma's API and the standard Luau library,
  without `io` or `os.execute`. Processes, files and sockets go through
  Genoma's own objects (§9.2), so every effect is visible in the log.
- **IPC.** `genoma ipc call <target> <function> [args]` calls a function
  registered with `g.ipc.handler { … }`. It replaces Quickshell's `IpcHandler`
  for keybinds.

## 8. The living layer

What Genoma adds that Qt made too expensive or impossible. Each item is a
toolkit primitive. Whether Bioma uses it, and how, is decided under Bioma's
principles. Movement still has to mean something: these are transitions that
express a change of state or of relation, and indicators driven by data. None
of them is a loop that runs for its own sake.

### 8.1 Connections that grow

A `thread` between two anchors has a `growth` property (0 to 1) and a tip
shape. When a relation forms (an expansion opens from its cell, a device pairs,
a notification attaches to its source), the thread **grows** from origin to
target along its path instead of appearing. When the relation ends, it
retracts toward the origin. The path can be driven by a spring between the
anchors, so a connector that follows a moving capsule bends and settles rather
than snapping to the new position.

### 8.2 Budding and reabsorption

Two capsules joined by a smooth union (§5.2) with a variable neck radius. This
is how a panel can **bud** out of its cell, like a cell dividing:
1. The neck is wide while the panel grows.
2. It narrows to a thread.
3. It parts.

Closing runs the same steps backwards, as **reabsorption**. This is Principle 5
("expanded forms grow from their origin") made literal. It costs nothing extra,
because both shapes are already distance fields.

### 8.3 Particles with data behind them

Emitters bound to live values: network throughput as a flow between two
capsules, CPU load as density, a download as particles carried into its
destination. At zero they are still, or gone.

### 8.4 Depth transitions

A surface can recede and come forward in z, tilt toward the pointer, or open
by unfolding instead of growing. Because primitives and text are
resolution-independent (Principle 4), none of this distorts the lit border or
blurs text. That was the reason for Bioma's "never `transform: scale`" rule.
Bioma's design documents decide whether the rule relaxes for depth, not this
one.

### 8.5 Physics as an animation source

Besides curves from the timing set, an animation can be a **spring** (stiffness,
damping, mass) that settles when it reaches its target. Springs are signals
like any other, and a settled spring stops requesting frames.

## 9. Services

### 9.1 The built-in domains

One object per domain, each a set of signals, created on first use and shared.
These replace the Quickshell modules Bioma imports today:

| Domain | Replaces | Built on |
|---|---|---|
| Battery, power | `Quickshell.Services.UPower` | `zbus` 6 (UPower, power-profiles-daemon) |
| Audio | `Quickshell.Services.Pipewire` | `pipewire` 0.10 |
| Media | `Quickshell.Services.Mpris` | `zbus` (MPRIS) |
| Network | `Quickshell.Networking` | `zbus` (NetworkManager) |
| Bluetooth | `Quickshell.Bluetooth` | `bluer` 0.17 |
| Notifications server | `Quickshell.Services.Notifications` | `zbus` (`org.freedesktop.Notifications`) |
| Tray | `Quickshell.Services.SystemTray` | `zbus` (StatusNotifierItem + DBusMenu) |
| Polkit agent | `Quickshell.Services.Polkit` | `zbus` (polkit agent interface) |
| PAM | `Quickshell.Services.Pam` | `pam-client` 0.5 (or a direct libpam binding — §12) |
| Greeter | `Quickshell.Services.Greetd` | `greetd_ipc` 0.10 |
| Windows, workspaces | `ToplevelManager`, `WindowManager` | `ext-foreign-toplevel-list` + niri IPC |
| Colours from an image | `ColorQuantizer` | in Rust, off the main thread |

The version numbers are the ones current on crates.io on 2026-10-28. Each
service is verified **without UI** before anything depends on it: a headless
`genoma probe` prints its signals as they change. This is the method that made
Bioma's port from Prisma reliable.

### 9.2 Everything else

Bioma writes about 39 services itself, and those keep their shape. Genoma
provides the same building blocks Quickshell's `Quickshell.Io` does:
- `g.process` (spawn, stream stdout lines, exit status);
- `g.dbus` (call, get/set properties, watch signals, on either bus);
- `g.socket` (Unix sockets, line-delimited);
- `g.file` (read, watch, write atomically — the counterpart of `FileView`);
- `g.timer`, for the rare value that only polling can give, at a declared
  interval.

### 9.3 Services in Luau

A service can be written in Luau with those blocks, exactly as Bioma's
services are QML singletons today, and moved to Rust when a measurement says
it costs. Porting Bioma does not mean rewriting 39 services in Rust.

## 10. Bioma on Genoma

- **Bioma's structure** (membrane, tissue, cell) becomes Luau modules: the
  layout engine, the registry, and one directory per cell. "A new cell costs
  one config block" must hold on Genoma too.
- **Bioma's design tokens** (`Theme`, `Timing`, `Metrics`, `Typography`)
  become Luau modules of signals. The theme is still an animated singleton, so
  retinting the desktop stays live.
- **Configuration.** Bioma keeps its two JSON layers. Genoma reads and writes
  them with `g.file`.
- **The port is incremental,** surface by surface: for a while Bioma's
  Quickshell process and a Genoma process run side by side, each owning
  different surfaces. Notifications move last, for the same reason as before:
  one owner per session.
- **The lock screen and the greeter** move only after the shell, each keeping
  its fallback (hyprlock, agreety).

## 11. Milestones and gates

Each milestone ends with a measurement in the §1 format, run on the desktop and
on the RTX 3060 laptop. A gate that fails stops the project or changes its
course before more is built on it.

| # | Milestone | Done when |
|---|---|---|
| **M0** | **Spike.** One layer surface on niri; `wgpu` device on the compositor's GPU; one `rect` with a lit border; fractional scale; frame callbacks. | At rest: **0.0% CPU** over 60 s, no frames requested. Device matches niri's on the laptop. |
| **M1** | **Core.** Signals, layout, text, the timing set, Luau with hot reload; a clock capsule. | Clock ticking once a second costs < 0.2% of a core; editing the file updates it without restart, state kept. |
| **M2** | **The gate.** niri IPC + UPower + CPU load; a membrane with the same content as Bioma's top membrane (window title, workspaces, vitals, clock). | **Go/no-go:** ≤ ⅓ of Bioma's CPU and PSS for the same membrane, measured side by side; frame time p99 < 2 ms at 165 Hz on the desktop; smooth on the laptop's iGPU. |
| M3 | **Interaction.** Pointer, keyboard, touchpad with momentum, input regions, compositor blur, multi-output, expansions growing from origin. | One Bioma cell with its expansion fully usable by keyboard and pointer. |
| M4 | **Living layer.** Growing threads, budding, particles, depth, springs (§8). | Each primitive at rest costs nothing; each in motion stays within the M2 frame budget. |
| M5 | **Services.** The rest of §9.1, each verified headless. | `genoma probe` shows every domain live; notifications tested with no other server running. |
| M6 | **Bioma.** The port (§10). | Bioma's feature list on Genoma, then the lock screen and the greeter. |

M0 to M2 are the experiment, and the numbers at M2 decide whether there is a
project. At part-time pace that is a few months. Parity with Quickshell (M5)
is the long part, likely a year or more. Text, input methods, keyboard
handling, fractional scale, multiple monitors and accessibility are what make
it long, not the renderer.

## 12. Open questions

1. **Adapter by DRM node.** Can `wgpu` 30 select an adapter by DRM major/minor,
   or does Genoma need `ash` with `VK_EXT_physical_device_drm` beside it? To
   be decided in M0.
2. **Text under depth.** Glyph atlas at transformed sizes, or MSDF. To be
   decided by measurement in M4.
3. **Render thread from the start,** or a single thread first and the split
   later? Leaning to a split from M0, because it shapes the draw-list API.
4. **PAM binding.** `pam-client` was last released in 2022. Use it, fork it,
   or bind libpam directly (it is small)?
5. **Accessibility.** AccessKit (`accesskit_unix`, AT-SPI) is the obvious
   path. Scope it in M3 or defer it past M6?
6. **Luau type definitions.** Generated from Rust (one schema, two outputs),
   or hand-written? Leaning to generated.
7. **Licence.** Bioma is GPL-3.0. Genoma as a toolkit could be GPL-3.0 too,
   or LGPL/MPL to let other shells build on it. Your decision.
8. **Scope beyond niri.** Genoma needs only standard protocols except for
   niri's IPC service. Should other compositors' IPC (Hyprland, Sway) be
   in scope as optional services?

## 13. Sources

- Measurements: Bioma session of 2026-10-28 (perf, `/proc`, `smaps_rollup`);
  Bioma memory notes on its resource profile (2026-09-27).
- Crate versions and protocol support: crates.io and the published sources of
  `smithay-client-toolkit` 0.21.1 and `wayland-protocols` 0.32.13, checked
  2026-10-28.
- Bioma's PRD (`Bioma/docs/bioma-prd.md`) for the principles Genoma serves.
