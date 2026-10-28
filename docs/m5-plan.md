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
- [x] `g.dbus`: call, get and set properties, watch signals, either bus.
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

*`g.dbus`, built 2026-10-28.* `call`, `get`, `set` and `watch`, on the bus
thread. Arguments are Luau values with the signature the method wants
(`"susssasa{sv}i"`); inside a `v` the type is guessed from the value, or
given with `g.dbus.variant("y", 2)`. Replies and signals come back as Luau
values, to the callback they were asked for, by its number: `reply(true,
…)` or `reply(false, error)`. A watch is a task on the bus thread, ended when
its load is. Checked on the real buses: `ListNames`, `GetNameOwner` (the
owner `busctl` gives), the bus's own `NameHasNoOwner` error, the portal's
nested `a{sa{sv}}`, power-profiles' `Profiles` read and `ActiveProfile`
written (and put back), `NameOwnerChanged` watched, once per name after three
reloads. A `Notify` with `as` and `a{sv}` hints, sent on a private bus and
read back with `dbus-monitor`, arrived as written, the urgency as a byte.

### 3. Network

- [x] NetworkManager: connectivity, the primary connection, Wi-Fi devices
      (state, strength, SSID, access points on scan) and wired devices
      (carrier, speed). Checked against `nmcli`.

*Built 2026-10-28,* to what Bioma's `services/Network.qml` reads from
`Quickshell.Networking`: devices (kind, state, carrier, speed, profile), the
radio's soft and hardware switches, the networks a Wi-Fi device sees (one
per SSID, the strongest access point, hidden ones left out, whether a saved
profile knows it), connectivity and its check, the last failure's reason;
and the actions: the radio, a scanner while a list is open, join with a
saved profile or a password, leave, forget. One subscription to everything
NetworkManager signals; a burst is given 60 ms, then the state is read
again and posted whole.
- The probe matched `nmcli`: `full`, eno1 with carrier at 2500 Mb/s on
  "Wired connection 1", wlan0 unavailable with its radio off.
- From Luau, the radio on and the scanner started: wlan0 went to
  `disconnected` and the three named networks `nmcli device wifi list`
  shows came through (SoToUniFi's two access points as one); then the radio
  off again. No saved Wi-Fi profile, so nothing joined.
- At rest the bus thread woke 0 times in 30 s.
- [ ] **On the laptop, with his Wi-Fi:** join with and without a password,
      the failure reason for a wrong one, leave, forget.

### 4. Media

- [x] MPRIS: players as they appear and go, metadata, status, position
      (from the rate and the last report, never polled), and the actions.
      Checked against `playerctl`.

*Built 2026-10-28,* to what Bioma's `services/Media.qml` reads. Players from
`ListNames`, then `NameOwnerChanged`; a player read again on its
`PropertiesChanged`. The position is read with the rest and on `Seeked`, and
carried forward at the rate while playing (`media:position(name)`), so a
progress bar asks on its own beat and nothing polls. Bioma's length quirk
for Firefox stays in Bioma's media service, where it is now. Checked with
Bioma's `scripts/mpris-dummy.py` (`playerctl` is not installed here; `busctl`
instead): the player appeared with its metadata and left when it was
closed; pause held the position, play resumed it, seek +30 and set_position
100 came back through `Seeked`, next changed the title and started at 0.

### 5. Audio

- [x] PipeWire: sinks and sources, the defaults, volume and mute, setting
      them; streams by application. Its loop on its own thread. Checked
      against `wpctl`.

*Built 2026-10-28,* to what Bioma's `services/Audio.qml` and `Privacy.qml`
read: every node with its class, kind, description, nickname and all its
properties, volume and mute; the default sink and source by `node.name`,
from the "default" metadata; the links between nodes. Volume, mute and the
default set back (the default through `default.configured.*`, which
WirePlumber keeps). Volumes are on `wpctl`'s scale, the cube root of what
PipeWire stores. The `pipewire` 0.10 crate on its own thread,
`genoma-pipewire`, posting into the loop.
- The probe matched `wpctl status`: HDMI 0.40, Revelator 0.60 and the
  default, the Revelator source the default input, Bioma's sinestesia
  capture stream and its two active links.
- Volume 0.5 and mute set from Luau on "USB Audio Speakers", where nothing
  plays: `wpctl` saw both, Genoma saw both come back, and both were put
  back.
- One bug found on the way: an `info` event about parameters alone carries
  an empty property dictionary, which had wiped the node's class; the
  properties are taken only when the event says they changed.
- At rest the PipeWire thread woke 0 times in 30 s.
- [x] Setting the default sink live (Akusen agreed): from Luau to HDMI and
      back to the Revelator, both seen through `default.audio.sink`. A sink
      WirePlumber thinks unavailable (USB Speakers, no jack) gets the
      configured key and stays not the default, exactly as with `wpctl
      set-default`.
- [ ] Streams by application, when something plays.

### 6. Bluetooth

- [x] BlueZ: adapter (powered, discovering), devices (connected, paired,
      battery), connect and disconnect. **Over zbus, not `bluer`:** `bluer`
      brings a tokio runtime for what is a handful of D-Bus objects, and the
      bus connection is already there. Checked against `bluetoothctl`.

*Built 2026-10-28,* to what Bioma's `services/Bluetooth.qml` reads: the
default adapter, devices with name, own name, icon, paired, trusted,
connected, blocked and battery, and the last failed call; the actions power,
discovery (while a list is open), connect, disconnect, pair, cancel pairing,
forget, trusted. One `GetManagedObjects` read again after any BlueZ signal,
the burst given 60 ms. A device is `busy` ("connecting", "pairing"…) while
its call is in flight, as Quickshell has it, since BlueZ has no such state.
- The probe matched `bluetoothctl show`: AkuLinux, powered, not
  discoverable or pairable.
- Discovery from Luau on and off: the two unnamed devices nearby that
  `bluetoothctl devices` listed came through, and `Discovering` went back to
  no.
- [ ] **With one of Akusen's devices:** pair, connect, the battery,
      disconnect, forget. Pairing a device that wants a code needs an agent;
      whether Bioma relies on one (and which) is checked then.

### 7. Windows and colours

- [x] `ext-foreign-toplevel-list`: the compositor-agnostic window list, on
      the main thread's Wayland connection, as the fallback beside niri's.
- ~~Colours from an image: a quantizer in Rust, off the main thread.~~
  *Dropped, Akusen 2026-10-28:* Bioma takes its colours from matugen and no
  longer uses `ColorQuantizer`.

*Built 2026-10-28.* Bioma does not use the compositor-agnostic list either
(its `services/Niri.qml` reads niri's IPC, which also gives geometry, pids,
workspaces and outputs); it is built as the fallback should Bioma leave
niri. Bound only once an interface requires `@genoma/toplevels`, since a
compositor serving it sends every window's every title change. The list
matched `niri msg windows` (four windows, titles and app ids), and a
binding on it re-ran as the list changed; a title changing under the test
was not caught in its six seconds.

### 8. Greeter and PAM

- [x] greetd IPC: create a session, answer its prompts, start it. Checked
      against a stand-in greetd socket, never the real greeter (never
      restart greetd from inside the session).
- [x] PAM: a direct libpam binding (§12 question 4), the conversation
      answered from Luau. **Akusen checks this one:** it asks for his
      password.

*Built 2026-10-28,* to what Bioma's `greeter.qml` and `lock.qml` use.
- **PAM:** `pam_start_confdir`, `pam_authenticate`, `pam_acct_mgmt`,
  `pam_end`, `pam_strerror` and the conversation, so Bioma's own
  `assets/pam/bioma-lock` is read from its directory. It blocks, so it runs
  on a thread; prompts wait for `auth:respond`, as Quickshell's
  `PamContext.respond`. `g.pam.start { service, directory, user, on_prompt,
  on_message, on_done }`, and `genoma pam [service] [directory]` from a
  terminal. A wrong password: `failure` after PAM's 2 s delay, from the
  command and from Luau; a missing service: `error` at once.
- **greetd:** `g.greetd { on_auth_message, on_ready, on_launched,
  on_failure, on_error }` with `create_session`, `respond`,
  `start_session`, `cancel_session`; length-prefixed JSON on
  `$GREETD_SOCK`, replies read through the loop. Against a stand-in greetd
  (scratchpad `fake-greetd.py`): a wrong password, `failure`, cancel, a new
  session, the right one, `ready`, `start_session`, `launched`, and the six
  requests it received were the ones meant.
- [ ] **Akusen:** the right password through `genoma pam`.

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
