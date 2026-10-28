# Genoma

A Wayland shell toolkit in Rust, with interfaces written in Luau and reloaded
while they run.

Genoma opens surfaces on the compositor, draws them on the GPU through
distance-field primitives, feeds them the system's state as reactive signals
and reloads them when their files change. It is built for
[Bioma](https://github.com/AkusenArcade/Bioma), which runs on Quickshell today,
and for what Bioma could not afford there: shapes that bud and grow, particles,
and depth transitions. At rest it should cost nothing at all.

**Status:** milestone 1 — signals, layout, text, transitions and hot-reloaded Luau, nothing running at rest. See
[`docs/genoma-prd.md`](docs/genoma-prd.md).

## Running

```sh
GENOMA_OUTPUT=HDMI-A-1 cargo run --release -p genoma -- examples/clock
```

`examples/clock/shell.luau` is milestone 1: a capsule with the time, its text
a binding on the clock service. Save the file and the change shows without
a restart. `GENOMA_OUTPUT` names the output (the interface's own `output`,
or the compositor's choice, without it). `GENOMA_LOG=info,genoma=debug` logs
every frame drawn and the GPUs on offer.

## Licence

GPL-3.0-or-later, the same as Bioma. See `LICENSE`.
