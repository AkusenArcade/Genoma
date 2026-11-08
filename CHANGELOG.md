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
- `examples/bar`: a classic top bar written against Genoma alone.
- This changelog, and a versioning policy in the README.

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
