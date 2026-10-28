# Genoma

A Wayland shell toolkit in Rust, with interfaces written in Luau and reloaded
while they run.

Genoma opens surfaces on the compositor, draws them on the GPU through
distance-field primitives, feeds them the system's state as reactive signals
and reloads them when their files change. It is built for
[Bioma](https://github.com/AkusenArcade/Bioma), which runs on Quickshell today,
and for what Bioma could not afford there: shapes that bud and grow, particles,
and depth transitions. At rest it should cost nothing at all.

**Status:** milestone 0 — one layer surface, one capsule, nothing running at rest. See
[`docs/genoma-prd.md`](docs/genoma-prd.md).

## Running milestone 0

```sh
GENOMA_OUTPUT=HDMI-A-1 cargo run --release -p genoma
```

It opens one capsule at the top of the named output (the compositor chooses
without `GENOMA_OUTPUT`) and logs every frame it draws. Hovering the capsule
lights its rim, and that is the only thing that makes it draw again.
`GENOMA_LOG=debug` also lists the GPUs on offer and the one chosen.

## Licence

GPL-3.0-or-later, the same as Bioma. See `LICENSE`.
