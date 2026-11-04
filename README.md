<h1 align="center">
  <img src="docs/media/logo.png" width="240" alt="Genoma — A Living Toolkit">
</h1>

A Wayland shell toolkit in Rust, with interfaces written in Luau and reloaded
while they run.

Genoma opens surfaces on the compositor, draws them on the GPU through
distance-field primitives, feeds them the system's state as reactive signals
and reloads them when their files change. It is built for
[Bioma](https://github.com/AkusenArcade/Bioma), which ran on Quickshell before
it, and for what Bioma could not afford there: shapes that bud and grow,
particles, and depth transitions. At rest it should cost nothing at all.

**Status:** 0.1.0, the first release. Bioma runs on it whole: its shell, its
lock screen and its greeter. The toolkit's API is Bioma's for now and will
change; see [`docs/genoma-prd.md`](docs/genoma-prd.md).

## Installing

On Arch, from the AUR: `genoma`, which puts the runtime at `/usr/bin/genoma`.
Bioma's package (`bioma-shell`) depends on it, so installing Bioma brings it.

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
