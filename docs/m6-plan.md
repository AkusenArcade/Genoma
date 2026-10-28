# M6 — Bioma on Genoma

PRD §10 and §11: **Bioma's feature list on Genoma, then the lock screen and
the greeter.** The shell keeps its name; Genoma is only the toolkit it runs
on.

## Where it lives

*Akusen, 2026-10-28:* in Bioma's repository, a new top-level directory
`genoma/`, beside the Quickshell version. Bioma on Quickshell stays feature
complete and keeps being polished meanwhile, and the two share what is
Bioma's rather than Quickshell's:
- `docs/` (the PRD and the design documents, which stay normative),
- `config/default.json`, the palettes, and the person's `override.json`,
- `assets/` (icons, fonts, sounds, PAM),
- `scripts/` (start, lock, greeter, niri's include).

Genoma stays its own repository, a dependency Bioma's `genoma/` runs on.
The AUR package leaves `genoma/` out until it is ready (its `package()`
copies a fixed list of directories). Before a release on Genoma, Genoma's
repository goes public and gets its own package: the AUR builds from public
sources.

## How it is ported

- **Surface by surface, the two side by side** (PRD §10). A surface is
  marked as Genoma's in the configuration; Bioma on Quickshell then does not
  draw it, and Genoma draws only those. Notifications move last: one owner
  per session.
- **From the current QML, never from the old mockups**: each cell is
  rebuilt from its `.qml` and a live capture of it, as M2's membrane was
  (`docs/m2-parity.md` is the method).
- **Bioma's rules hold**, with one Akusen changed on 2026-10-28: **cells
  open by budding** (PRD §10). Bioma's `docs/design/STYLE_GUIDE.md` takes
  the rule with the first cell that opens.
- **A gap in Genoma is filled when the first surface needs it**, not ahead.
- **Each surface is done when:** its parity checklist is ticked from a
  side-by-side capture, Akusen has looked at the two, and its cost is
  measured against the same surface on Quickshell.

## What Genoma still lacks, and who needs it first

| Gap | First needed by |
|---|---|
| Images from files (PNG, JPEG, WebP) and from pixels | the wallpaper (step 4.2); media art; tray and notification pixmaps |
| A text field (keyboard, layout through xkb, a cursor, selection) | the launcher, then settings, the lock, the greeter |
| A fragment shader node, Bioma's four `.frag` translated to WGSL | the media band (`fluid.frag`), then the lava lamp, cytoplasm, osmosis |
| Settings controls: slider, switch, dropdown, list rows | settings |
| Images over the network (cover art by `https:`) | media |
| Drag and drop | organisms' arrange mode |

## Steps, in dependency order

The order is Bioma's own, its PRD §10 (foundations, services, the three
cells that validate the system, identity, replacement with notifications
last, settings, launcher); what is ported at each step is what the current
code has, which is more than the PRD lists (organisms, tray, clipboard,
emoji, performance mode, privacy, the lock screen, the greeter…).

### 0. Ground

- [ ] The feature list the port is measured against: taken from the current
      code, checked against the PRD's cell catalogue (§9). What the code has
      and the PRD does not is ported; what the PRD has and the code never
      built is not the port's, it is Bioma's own work (`genoma/PORT.md`).
- [ ] `Bioma/genoma/` with its `shell.luau`, a start script beside
      `scripts/bioma`, and Bioma's `CLAUDE.md` saying what the directory is.
- [ ] Ownership by surface: a key in the configuration that both runtimes
      read; Quickshell skips what Genoma owns.
- [ ] How the runtime is found while Genoma is unpackaged (a path to its
      build), and how it starts at login once it is.

### 1. Core

The `core/` singletons as Luau modules of signals:
- [ ] Config: `config/default.json` then `override.json`, both watched,
      written back atomically; the same keys.
- [ ] Theme: the palette (matugen's or a saved one) as animated signals, so
      the theme cell still retints everything live.
- [ ] Timing, Metrics, Typography from `docs/design/tokens.css`, as the
      QML singletons have them.

### 2. Structure

- [ ] Membrane, tissue, cell: the layout engine from the configuration. A
      new cell still costs one config block.
- [ ] The cell registry, the expansion grown by budding, the input surface
      that catches outside presses.

### 3. Services

Bioma's own services (about fifty) in Luau on `g.process`, `g.file`,
`g.socket`, `g.dbus` and the built-in domains, each verified headless
before a cell depends on it, as Bioma's were from Prisma.

### 4. Cells, in Bioma's build order

1. [ ] Window title, workspaces, vitals, clock: M2's membrane, moved over and
       brought to today's QML.
2. [ ] Identity (PRD phase 3): media (the band's shader), theme and the
       wallpaper with its overview backdrop, utility.
3. [ ] Volume, connectivity, session, dock, tray.
4. [ ] The rest: actions, clipboard, downloads, drives, emoji, keyboard,
       performance, polkit, privacy, system, timer.
5. [ ] Notifications, last.
6. [ ] Settings, then the launcher.

### 5. The organisms

- [ ] The organisms, and their arrange mode (not in the PRD; built after
      it, on the wallpaper's layer).

### 6. Lock and greeter

- [ ] The lock screen on `g.pam`, as a separate process with its guard and
      its hyprlock fallback, as now.
- [ ] The greeter on `g.greetd`, with agreety as its fallback.

### 7. The switch

- [ ] Genoma public and packaged; `bioma-shell` starting Bioma on Genoma;
      the person's `override.json` read unchanged.
- [ ] The laptop: the measurements M0 to M2 left pending, and the hardware
      checks M5 left (battery, Wi-Fi join, Bluetooth pairing).

## Done when

Every surface Bioma has on Quickshell runs on Genoma, each checked by
Akusen side by side and measured; the lock screen and the greeter too, each
with its fallback; and Bioma ships on Genoma.
