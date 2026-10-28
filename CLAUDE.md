# Genoma

A Wayland shell toolkit in **Rust**, with interfaces in **Luau** (through
`mlua`), hot-reloaded. It is the planned successor to Quickshell under
**Bioma** (`~/Documents/Development/Bioma`), its first and only consumer for
now. The shell is and stays **Bioma**; Genoma is only the toolkit's name.
Bioma rebuilt on Genoma is still Bioma.

When a cell of Bioma is reproduced here (an example, a measurement, the
port), the reference is Bioma's **current** QML and a capture of the live
desktop, never `docs/design/CELLS.md` or the mockups alone: those show early
versions, and the cells have moved on.

The specification is `docs/genoma-prd.md`. It is the source of truth: read the
relevant section before implementing, and do not re-decide what it has closed.
Its §12 lists what is still open. When one of those questions is settled,
update the PRD in the same change.

## Language

English everywhere: code, comments, identifiers, documents, commit messages.
`Genoma` is the only Italian word retained.

## Rules that are easy to break by accident

- **Nothing runs at rest.** A surface where nothing changes requests no frame,
  runs no timer and evaluates no binding. Check it with a measurement, never by
  reading the code.
- **Every frame is caused by a value**, either a signal that changed or an
  animation in flight. Never redraw on every vblank.
- **The CPU describes, the GPU draws.** No tessellation, no CPU rasterisation.
  New shapes are distance-field primitives.
- **One wgpu device for the whole process, on the compositor's GPU.** It is
  chosen from linux-dmabuf feedback. A hybrid laptop made this rule: niri on
  NVIDIA and Qt on the Vega copied every frame between GPUs.
- **Logical units everywhere**; only hairlines snap to the physical pixel.
- **Durations come from the timing set.** Luau code naming a literal duration
  is rejected at load time.
- **Genoma knows nothing of Bioma.** Membrane, tissue and cell are Bioma's
  Luau modules, never Genoma types. A primitive belongs in Genoma when a shell
  other than Bioma could use it.
- **One service object per domain**, shared by everyone.
- **Blur behind a surface is the compositor's** (`ext-background-effect`).
  Never blur in-process, except over a surface's own content.

## Method

The same one that worked for Bioma:
- **Every milestone ends with a measurement**: CPU per thread from `/proc`, PSS
  from `smaps_rollup`, and `perf record -e cpu-clock:u --call-graph dwarf
  --user-callchains -p <pid>` (perf_event_paranoid is 2 here). Put the numbers
  in the commit message.
- **Verify each service headlessly** (`genoma probe`) before any interface
  depends on it.
- **Test on both machines**: the desktop (RX 9070, 165 + 120 Hz) and the RTX
  3060 laptop (Vega iGPU + NVIDIA, 2560×1600 @ 60 Hz, scale 1.5).
