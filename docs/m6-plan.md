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
| ~~Images from files~~: `g.image` done 2026-10-29 (PNG, JPEG, WebP, SVG; cover, contain, fill; rounded; decoded off the frame). From pixels still to come | the wallpaper, media art, the theme's thumbnails; pixels for tray and notification icons |
| ~~A text field~~: done 2026-10-29 for the vitals' filter: keys with their text and modifiers, held keys repeating; the field itself is Bioma's `components/field.luau` | the vitals' and the clock's expansions, then the launcher, settings, the lock, the greeter |
| ~~A fragment shader node~~: `g.shader` done 2026-10-29 (a WGSL `shade(p)` over a prelude, up to 128 bound numbers eased by the node at the display's rate, a clock that runs only while asked; compiled under an error scope, so a broken file draws nothing and says why). `fluid.frag` translated by hand; the lava lamp, cytoplasm and osmosis still to come | the media band (`fluid.frag`), then the lava lamp, cytoplasm, osmosis |
| Settings controls: slider, switch, dropdown, list rows | settings |
| ~~Images over the network~~: not Genoma's, as it turned out: Bioma downloads a remote cover once to its cache (`curl`, 2026-10-29) and `g.image` reads the file | media |
| ~~Folders~~: `g.folder` done 2026-10-29 (entries by name, watched) | the wallpaper library, the palettes on offer, the thumbnails Quickshell made |
| ~~A raised layer~~: `raised` done 2026-10-29 (drawn after the whole surface, text included, its shape cutting what is under it first; hit first) | the theme cell's lists over its capsules, then every dropdown in settings |
| ~~A pointer that drags, and its shape~~: done 2026-10-29: `on_drag(x, y)` to the node the press landed on while the button is held; `cursor` on a node and what it holds (`wp-cursor-shape`); a surface `above_zones`, its origin the output's; `p:signal(n)` on a process | the utility's selection rectangle, its recorder and dictation's |
| ~~Where the pointer is~~: done 2026-10-30: `on_move(x, y)` to the node under the pointer as it enters a surface and as it moves, local to whichever node handles it (entering included, so a surface that starts taking input under a still pointer learns where it is) | the quick actions' ring, summoned at the pointer and pointed by direction |
| ~~A picture blurred, and told when it is there~~: done 2026-10-30: `soften` on an image (blurred once as it is decoded, nothing per frame) and `on_ready` (its pixels drawn for the first time since `src` was set) | the wallpaper's backdrop behind niri's overview, and its crossfade |
| Drag and drop | organisms' arrange mode (on `on_drag`) |

## Steps, in dependency order

The order is Bioma's own, its PRD §10 (foundations, services, the three
cells that validate the system, identity, replacement with notifications
last, settings, launcher); what is ported at each step is what the current
code has, which is more than the PRD lists (organisms, tray, clipboard,
emoji, performance mode, privacy, the lock screen, the greeter…).

### 0. Ground

- [x] The feature list the port is measured against: taken from the current
      code, checked against the PRD's cell catalogue (§9). What the code has
      and the PRD does not is ported; what the PRD has and the code never
      built is not the port's, it is Bioma's own work (`genoma/PORT.md`).
- [x] `Bioma/genoma/` with its `shell.luau`, a start script beside
      `scripts/bioma`, and Bioma's `CLAUDE.md` saying what the directory is.
- [x] Ownership by surface: a key in the configuration that both runtimes
      read; Quickshell skips what Genoma owns.
- [x] How the runtime is found while Genoma is unpackaged (a path to its
      build); how it starts at login is step 7's.

*Done 2026-10-28.* A membrane or floating block marked `"runtime": "genoma"`
is skipped by `shell.qml`'s `membraneMatches` (the Settings page copies
blocks whole, so the key survives an edit) and found by `genoma/shell.luau`,
which reads `config/default.json` and the override as `core/Config.qml`
merges them. `scripts/genoma` runs `$GENOMA_BIN`, else `genoma` on the PATH.
Checked with a copy of Akusen's override marking DP-1's top membrane: Genoma
found that one block (and none in his real configuration), and an isolated
Bioma on Quickshell drew HDMI-A-1's bottom membrane and not DP-1's top. For
it Genoma gained `g.dir`, relative `g.file` paths from the interface's
directory, `g.parse_json` and `g.to_json`.

### 1. Core

The `core/` singletons as Luau modules of signals:
- [x] Config: `config/default.json` then `override.json`, both watched,
      written back atomically; the same keys.
- [x] Theme: the palette (matugen's or a saved one) as animated signals, so
      the theme cell still retints everything live.
- [x] Timing, Metrics, Typography from `docs/design/tokens.css`, as the
      QML singletons have them.

*Done 2026-10-28* (Bioma's `genoma/core/`, checks in `genoma/PORT.md`).
Genoma gained, for it: `g.computed` (a memo), `g.effect`, `g.env`,
`g.animated` (a number or colour carried to a target over a timing entry,
every value in motion stepped together in one batch on one ticker, so a
theme's four roles move as one and what derives from them is computed once
a step), and `g.json_set` (a document with keys set and the rest in its
order, serde_json with `preserve_order`). The derived colours match Qt's to
the byte; the palette crosses in 32 steps of 8 ms; a write leaves the rest
of the override as it was; at rest nothing wakes.

### 2. Structure

- [x] Membrane, tissue, cell: the layout engine from the configuration. A
      new cell still costs one config block.
- [x] The cell registry, the expansion grown by budding, the input surface
      that catches outside presses.

Done 2026-10-29 (Bioma 1ff83a2, 9c150eb and the floats after them; the
checks are in Bioma's `genoma/PORT.md`). What Genoma gained for it: a
catching surface leaves the other surfaces on its output their presses and
takes the keyboard; `g.ipc` and `genoma ipc`, for keybinds;
`niri.focused_output`; a signal set while another is being set; a reload on
a save anywhere in the interface's directory; a timer of a replaced load
that fires into nothing instead of a destroyed state.

### 3. Services

Bioma's own services (about fifty) in Luau on `g.process`, `g.file`,
`g.socket`, `g.dbus` and the built-in domains, each verified headless
before a cell depends on it, as Bioma's were from Prisma.

**Genoma chooses, Quickshell acts** until step 7 (Akusen, 2026-10-29): a
service that acts on the desktop (matugen, templates, gsettings, the
wallpaper's state) writes from Bioma on Genoma only the shared
configuration and state, and Bioma on Quickshell runs the effects.

**Steps 3 and 4 go phase by phase** (Akusen, 2026-10-29): the services a
phase's cells read, then those cells, then the next phase. A service is
then tried by a real cell at once, and something is on screen sooner. The
services of phase 2 (Niri, NiriConfig and Monitors reading, SystemMonitor,
Time, Calendar, Proxy reading, Apps) are done: Bioma's `genoma/PORT.md`.

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
