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
| ~~A pointer that drags, and its shape~~: done 2026-10-29: `on_drag(x, y)` to the node the press landed on while the button is held, measured from where that node was at the press (2026-11-03: from where it is now, a node carried by its own drag ran away); `cursor` on a node and what it holds (`wp-cursor-shape`); a surface `above_zones`, its origin the output's; `p:signal(n)` on a process | the utility's selection rectangle, its recorder and dictation's |
| ~~Where the pointer is~~: done 2026-10-30: `on_move(x, y)` to the node under the pointer as it enters a surface and as it moves, local to whichever node handles it (entering included, so a surface that starts taking input under a still pointer learns where it is) | the quick actions' ring, summoned at the pointer and pointed by direction |
| ~~A picture blurred, and told when it is there~~: done 2026-10-30: `soften` on an image (blurred once as it is decoded, nothing per frame) and `on_ready` (its pixels drawn for the first time since `src` was set) | the wallpaper's backdrop behind niri's overview, and its crossfade |
| Drag and drop | organisms' arrange mode (on `on_drag`) |
| ~~Surfaces that follow the configuration~~: done 2026-11-03: a surface's `present = function(output)` (asleep: one unit, no zone, no input, no blur, tree kept) and `reserve = function(output)`; `fluid = true` hands the content a size that follows the area instead of building it again; an area change builds only that surface again; `g.map(items, key, make)` and `g.cleanup(fn)`; a capsule's `halo` | Bioma's membranes, tissues and cells followed live; auto-hide |

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

1. [x] Window title, workspaces, vitals, clock: M2's membrane, moved over and
       brought to today's QML.
2. [x] Identity (PRD phase 3): media (the band's shader), theme and the
       wallpaper with its overview backdrop, utility.
3. [x] Volume, connectivity, session, dock, tray.
4. [x] The rest: actions, clipboard, downloads, drives, emoji, keyboard,
       performance, polkit, privacy, system, timer.
5. [x] Notifications, last.
6. [x] Settings, then the launcher (and the setup), 2026-11-02.

### 5. The organisms

- [x] The organisms, and their arrange mode (not in the PRD; built after
      it, on the wallpaper's layer): all seventeen, 2026-11-02, and what
      they asked of the toolkit (tree order, gradient and lit text, runs,
      fades, shadows, stops, idle; Bioma's genoma/PORT.md).

### 6. Lock and greeter

- [x] The lock screen on `g.pam`, as a separate process with its guard and
      its hyprlock fallback, as now.
- [x] The greeter on `g.greetd`, with agreety as its fallback.

The greeter, 2026-11-02: Bioma's `genoma/greeter.luau`, a second entry
point (`genoma <dir> greeter.luau`), falling back to Quickshell's greeter
and then agreety; Akusen logged in through it under greetd the same night.

The lock, 2026-11-03: Genoma gained `@genoma/lock`, `ext-session-lock`
behind a surface's `layer = "lock"`: asked for with `lock:lock()`, the
compositor is asked and the interface mounted again with a lock surface on
every output (output changes included); `lock:unlock()` unlocks before the
surfaces go and mounts nothing again, since what follows is the
interface's to decide; `locked` once the compositor says every output is
covered, `finished` when it refused or ended it. A lock surface's first
frame waits for its pictures (at most 750 ms), so the compositor keeps the
desktop on screen until the lock is whole rather than showing it with its
pictures missing. Bioma's `genoma/lock.luau` on it was checked in a nested
niri (Bioma's `genoma/PORT.md`), then on Akusen's session the same
morning: Mod+Alt+L, his password, unlocked, LockedHint cleared.

### 7. The switch

*Akusen, 2026-11-03:* the effects first, then the package. Everything
Quickshell still does on Genoma's behalf moves to Genoma behind one key, a
top-level `"runtime": "genoma"` in the override, with which `scripts/bioma`
starts Genoma alone; he lives on it for a few days with Quickshell off, and
only then is Genoma packaged. The Quickshell version then leaves the
package and is kept in a backup folder of Bioma's repository. Genoma goes
to the AUR as `genoma`, built from tags; `bioma-shell` depends on a
precise version.

**7a. Genoma alone** (Bioma's `genoma/PORT.md` keeps the detail):
- [x] The key: `scripts/bioma` starts Genoma, and Genoma draws every block
      whatever its own `runtime`; Quickshell, if started anyway, draws
      nothing.
- [x] One way in from outside: `scripts/ipc <target> <function> …`, sent
      to whichever runtime runs; the keybinds template, `scripts/key` and
      the session's scripts go through it; Genoma answers every target
      Quickshell's `shell.qml` answers (cell, capture, actions,
      performance, audio, brightness, media, timer, sounds, …). Done
      (Bioma 0efcbcf) but for brightness, which comes with the OSD.
- [x] The names a session has one owner of: notifications, the polkit
      agent, the tray's watcher. (Genoma b8fe4fb: a name let go by another
      shell is taken.)
- [x] The look acting on the desktop: matugen and the templates, gsettings
      (icons, cursors, the proxy), the cursor theme's slots, niri's blur.
      (Bioma 092fef7, the proxy after it.)
- [x] The Locker: logind's Lock, the idle lock, the lock before sleep and
      its inhibitor, suspending after idle. (Bioma ed73630; the Lock signal,
      PrepareForSleep and the idle suspend await a real session.)
- [x] The greeter's sync, the clipboard's history, the sounds played,
      the power profile following the mains, the only printer made the
      default. (Bioma 6a195a1; the mains and the printer await hardware.)
- [x] What settings and the setup ask of Quickshell now (`monitors`,
      `organisms`, `keybinds`, `niri`, `looks`, `presets`, `actions`,
      `setup`), done by Genoma itself. (Bioma 75622a2, 5326942; the setup's
      actions await a machine to run them on.)
- [x] The OSD (volume, brightness, keyboard layout), and brightness.
      (Bioma 5326942; the brightness awaits his monitors.)
- [ ] His few days on it, Quickshell off; the cost measured whole, beside
      Quickshell's.
      The cost, 2026-11-03, on his desktop at rest, the same layout, one
      run after the other, 45 s to settle and 60 s measured: Bioma on
      Quickshell 474 MB PSS, 8.43 % of a core, niri 12.08 %; Bioma on
      Genoma 337 MB, 3.42 %, niri 5.35 %. The shell and niri together
      20.5 % against 8.8 % of a core (−57 %); memory −29 %. (His monitor
      showed 760 against 460 MB: RSS, which counts Qt's shared libraries
      whole.)

**7b. The package** (planned with Akusen 2026-11-03: an update must be all a
person on the Quickshell version has to do, then a restart of the shell):
- [x] `genoma` on the AUR from a tag, `/usr/bin/genoma` (the scripts find
      it on the PATH); the name checked free on the AUR first. (Tag v0.1.0,
      e4fe4d3; AUR b8e0ee1, 2026-11-04.)
- [x] `bioma-shell` includes `genoma/`, depends on that precise `genoma`
      version, and drops `quickshell`; the QML version leaves the package
      for a backup folder of the repository. (Bioma d9ee424: `quickshell/`,
      with assets, config, scripts and tools as symlinks.)
- [x] `"runtime": "genoma"` in `config/default.json`, not in anybody's
      override, so it holds for everyone; existing overrides stay valid.
- [x] The fallbacks without Quickshell: the lock to hyprlock (or
      swaylock), the greeter to agreety; `lock.qml` and `greeter.qml`
      paths out of `scripts/lock` and `greeter-sync`. (A failed greeter
      leaves a mark; greeter-session hands the login to agreety. QML paths
      kept only for a checkout's `quickshell/`.)
- [ ] What the first start after the update already does by itself,
      checked: `scripts/shell start` starts Genoma, `scripts/binds` moves
      bioma-binds.kdl to `scripts/ipc`, the greeter's sync copies Genoma's
      runtime into the greeter.
- [x] `post_upgrade` says to restart the shell (`scripts/shell restart`) or
      log out: the Quickshell shell running goes on until then. The
      release notes: a script of one's own calling `qs ipc call …` moves
      to `scripts/ipc`.
- [x] *(Akusen, 2026-11-04: the laptop updated from the AUR's Quickshell
      beta.60 to the beta.61 package, everything working; it also found the
      cytoplasm's cost, fixed in Genoma 0.1.1.)* The update tried on a clean machine, the laptop: the Quickshell
      version from the AUR, then the new package, then a restart, and
      everything on Genoma, lock and greeter included.
- [x] Genoma tagged and packaged (`genoma` on the AUR); `bioma-shell`
      starting Bioma on Genoma, depending on it; the person's
      `override.json` read unchanged; the Quickshell version moved to a
      backup folder and left out.
- [ ] The laptop: the measurements M0 to M2 left pending, and the hardware
      checks M5 left (battery, Wi-Fi join, Bluetooth pairing).

## Done when

Every surface Bioma has on Quickshell runs on Genoma, each checked by
Akusen side by side and measured; the lock screen and the greeter too, each
with its fallback; and Bioma ships on Genoma.
