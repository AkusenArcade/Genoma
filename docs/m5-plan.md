# M5 — services

PRD §9 and §11: the rest of the built-in domains (§9.1) and the building
blocks Bioma's own services are written with (§9.2). **Done when `genoma
probe` shows every domain live, and notifications are tested with no other
server running.**

The method is Bioma's from the Prisma port: each service is verified
**without UI** before anything depends on it. `genoma probe <domain>` prints
its signals as they change, and the check is done against the system's own
tools (`upower`, `nmcli`, `bluetoothctl`, `playerctl`, `wpctl`, `notify-send`).

## Shape of a service

- One object per domain, a set of signals, created on first use and shared
  (as `clock`, `niri` and `vitals` are).
- **D-Bus:** one `zbus` connection per bus for the whole process. Its thread
  is zbus's; it posts plain, `Send` updates into the main loop through a
  `calloop` channel, and only the main thread sets signals. A service
  subscribes to change signals, never polls.
- A library that brings its own loop (PipeWire) gets its own thread, and
  posts the same way (PRD §4).
- A service whose source nobody required costs nothing: no connection, no
  thread.
- Luau sees each as `require("@genoma/<domain>")`, its fields read through
  signals, as `niri` and `vitals` are now.

## Steps, in dependency order

### 1. The bus, and battery and power

*Built 2026-10-28.* zbus 6 no longer keeps a thread of its own: its
connections run on whichever thread is inside `block_on`. So `genoma-bus` is
that thread, started with the first service that needs a bus; each bus is
connected once, on first use, and a service's tasks run there and post into
the loop through a `calloop` channel.

- [x] The shared bus connections and the channel into the loop.
- [x] UPower: the display device (percentage, state, time to empty and to
      full, whether it is a laptop battery), and `on_battery`.
- [x] power-profiles-daemon: the active profile, the ones offered, and
      setting it.
- Checked against `upower -i` and `powerprofilesctl`: the probe matched
  both, and `powerprofilesctl set balanced`, then `performance`, came
  through as two signals. `power:set_profile("balanced")` from Luau moved
  the daemon and came back as `power.profile`. At rest the bus thread woke
  0 times in 30 s.
- [ ] **On the laptop:** the battery half (the desktop has none), unplugging
      the charger, and time to empty.

### 2. Building blocks (§9.2)

Bioma writes about 39 services of its own; they need these, not Rust:
- [x] `g.process`: spawn, stdout lines as they come, exit status.
- [x] `g.file`: read, watch, write atomically (`FileView`'s counterpart).
- [x] `g.socket`: Unix sockets, line-delimited.
- [ ] `g.dbus`: call, get and set properties, watch signals, either bus.
- [x] `g.timer`: a declared interval, for what only polling can give.

*Built 2026-10-28, all but D-Bus.* None owns a thread: the script asks the
loop, through a `Host` the runtime implements, for a timer or a wake when a
descriptor is readable. A process's pipes are read once per wake and its exit
comes from a pidfd, reported after its last line, with 128 plus the signal
for a killed one. A file is watched through inotify on its directory, so an
atomic replace is seen. What a load's top-level code starts belongs to that
load: the next mounted load stops it, a failed one stops its own, and a
child gets `PR_SET_PDEATHSIG` so it dies with Genoma too. Paths take `~/`
and a leading `$NAME`. `print` goes to the log.

Checked with an interface that used all four: stdout and stderr lines as
written, the last one without a newline, then `exit 3`; a stream of three
lines 0.3 s apart in order; stdin through `cat`; niri's socket answering
`"FocusedOutput"`; the watched file following a write, an atomic replace and
a removal on screen; `f:write` leaving no temporary file; a `sleep` killed
by a reload and another by killing Genoma, no zombie.

### 3. Network

- [ ] NetworkManager: connectivity, the primary connection, Wi-Fi devices
      (state, strength, SSID, access points on scan) and wired devices
      (carrier, speed). Checked against `nmcli`.

### 4. Media

- [ ] MPRIS: players as they appear and go, metadata, status, position
      (from the rate and the last report, never polled), and the actions.
      Checked against `playerctl`.

### 5. Audio

- [ ] PipeWire: sinks and sources, the defaults, volume and mute, setting
      them; streams by application. Its loop on its own thread. Checked
      against `wpctl`.

### 6. Bluetooth

- [ ] BlueZ: adapter (powered, discovering), devices (connected, paired,
      battery), connect and disconnect. **Over zbus, not `bluer`:** `bluer`
      brings a tokio runtime for what is a handful of D-Bus objects, and the
      bus connection is already there. Checked against `bluetoothctl`.

### 7. Windows and colours

- [ ] `ext-foreign-toplevel-list`: the compositor-agnostic window list, on
      the main thread's Wayland connection, as the fallback beside niri's.
- [ ] Colours from an image: a quantizer in Rust, off the main thread.

### 8. Greeter and PAM

- [ ] greetd IPC: create a session, answer its prompts, start it. Checked
      against a stand-in greetd socket, never the real greeter (never
      restart greetd from inside the session).
- [ ] PAM: a direct libpam binding (§12 question 4), the conversation
      answered from Luau. **Akusen checks this one:** it asks for his
      password.

### 9. Tray and polkit

- [ ] StatusNotifierItem host (and watcher when none is running) with
      DBusMenu.
- [ ] Polkit agent.
- Both have one owner per session, and Bioma owns them now. They are tested
  on a private session bus (`dbus-run-session`) where that is enough, and
  with Akusen where it is not (polkit registers on the system bus).

### 10. Notifications, last

- [ ] `org.freedesktop.Notifications` server: notify, replace, close,
      actions, hints, and the signals back.
- Tested inside `dbus-run-session`: on that bus there is no other server,
  which is the gate's condition, and Bioma keeps its own on the real one.
  `notify-send` sends inside the same session.

## Done when

- `genoma probe` shows every domain live, each checked against its system
  tool;
- notifications tested on a bus with no other server;
- at rest, with every service started: no wake-ups beyond what the domains
  themselves signal, measured as M0 measured them.
