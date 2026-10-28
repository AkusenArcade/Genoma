# Genoma

A Wayland shell toolkit in Rust, with interfaces written in Luau and reloaded
while they run.

Genoma opens surfaces on the compositor, draws them on the GPU through
distance-field primitives, feeds them the system's state as reactive signals
and reloads them when their files change. It is built for
[Bioma](https://github.com/AkusenArcade/Bioma), which runs on Quickshell today,
and for what Bioma could not afford there: shapes that bud and grow, particles,
and depth transitions. At rest it should cost nothing at all.

**Status:** design draft. Nothing is built yet. See
[`docs/genoma-prd.md`](docs/genoma-prd.md).
