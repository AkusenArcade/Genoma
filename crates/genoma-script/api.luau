-- Genoma's Luau API: `require("@genoma")` and the services under it.
-- Printed by `genoma types` after the node types; `genoma reference` turns
-- it into the API reference. A test checks every name the runtime
-- registers is here.

-- ---- Values ------------------------------------------------------------------

-- What g.signal and g.state give: a value bindings follow.
export type Signal<T> = { value: T }

-- What g.computed gives: a value derived from signals, read as .value.
export type Computed<T> = { read value: T }

-- What g.animated gives: a number or "#rrggbbaa" that moves.
export type Animated = {
    -- Where it is now.
    read value: number | string,
    -- Where it is going.
    read target: number | string,
    -- a:set(value, "theme"): there over that timing entry, its curve or
    -- its spring (one redirected keeps its speed); without one, or with
    -- one that does not move, at once.
    set: (self: Animated, value: number | string, timing: string?) -> (),
}

-- Something running that can be stopped: a timer, a watch.
export type Stoppable = { stop: (self: Stoppable) -> () }

-- ---- Input and output --------------------------------------------------------

export type Process = {
    read running: boolean,
    -- The exit status once it has ended, else nil.
    read exit_code: number?,
    -- p:write("text"): to its standard input, as given.
    write: (self: Process, text: string) -> (),
    -- p:close_input(): its standard input ends.
    close_input: (self: Process) -> (),
    -- p:kill(): the exit still comes through on_exit.
    kill: (self: Process) -> (),
    -- p:signal(2): a signal by number, for a program that ends cleanly on
    -- one. The exit still comes through on_exit.
    signal: (self: Process, number: number) -> (),
    -- p:stop(): its lines and its exit are no longer heard.
    stop: (self: Process) -> (),
}

export type WatchedFile = {
    -- The content, nil while the file does not exist; a binding that reads
    -- it follows the file.
    read text: string?,
    read path: string,
    -- f:write(text): atomically, through a file beside it and a rename.
    write: (self: WatchedFile, text: string) -> (),
    -- f:read(): read again now, and the content: /proc and /sys say
    -- nothing when they change, so a sampler asks.
    read: (self: WatchedFile) -> string?,
    stop: (self: WatchedFile) -> (),
}

-- size (bytes) and modified (seconds since 1970) with `details = true`.
export type FolderEntry = { name: string, path: string, dir: boolean, size: number?, modified: number? }

export type Folder = {
    -- By name, hidden ones left out; empty while the folder does not exist.
    -- A binding that reads it follows the folder.
    read entries: { FolderEntry },
    read path: string,
    stop: (self: Folder) -> (),
}

export type Socket = {
    read connected: boolean,
    -- s:send("text\n"): as given; a line-delimited peer wants the newline.
    send: (self: Socket, text: string) -> (),
    close: (self: Socket) -> (),
}

export type Output = {
    name: string,
    description: string,
    make: string,
    model: string,
    -- Logical units, once the compositor has said.
    width: number?,
    height: number?,
}

export type DesktopEntry = {
    id: string,
    name: string,
    generic_name: string?,
    comment: string?,
    icon: string?,
    exec: string?,
    startup_class: string?,
    path: string?,
    terminal: boolean,
    keywords: { string },
    categories: { string },
    file: string,
}

-- ---- Sessions: PAM and greetd ------------------------------------------------

export type Auth = {
    -- auth:respond(text): the answer to the prompt PAM is waiting on.
    respond: (self: Auth, text: string) -> (),
    -- auth:cancel(): ends it at the prompt it waits on.
    cancel: (self: Auth) -> (),
}

export type Greetd = {
    -- Whether greetd is there to talk to: a greeter run outside greetd has
    -- no $GREETD_SOCK.
    read available: boolean,
    -- greetd:create_session("user"): the login begins; auth messages follow.
    create_session: (self: Greetd, user: string) -> (),
    -- greetd:respond(text): the answer to the last auth message; nil only
    -- acknowledges an informational one.
    respond: (self: Greetd, text: string?) -> (),
    -- greetd:start_session({ "niri-session" }, { "XDG_SESSION_TYPE=wayland" })
    start_session: (self: Greetd, command: { string }, env: { string }?) -> (),
    cancel_session: (self: Greetd) -> (),
    close: (self: Greetd) -> (),
}

-- ---- The surfaces ------------------------------------------------------------

export type Edge = "top" | "bottom" | "left" | "right"

-- g.surface { … }: a layer surface and the node it shows. The content is a
-- node, or a function(output, size) giving one for each output.
export type Surface = {
    -- The edges it is held to; none centres it.
    anchor: (Edge | { Edge })?,
    margin: { top: number?, right: number?, bottom: number?, left: number? }?,
    -- "lock": a lock screen's, made per output while @genoma/lock holds the
    -- session.
    layer: ("background" | "bottom" | "top" | "overlay" | "lock")?,
    -- One output by name; outputs = "all" makes one on each.
    output: string?,
    outputs: "all"?,
    -- Keep windows clear of it: true for its size, a number for a strip.
    reserve: (boolean | number | (() -> boolean | number) | ((output: string?) -> boolean | number))?,
    -- Its output's size whatever its content.
    full: boolean?,
    -- Over what other surfaces keep for themselves.
    above_zones: boolean?,
    namespace: string?,
    -- When it takes the keyboard without a node catching.
    keyboard: ("none" | "on_demand" | "exclusive")?,
    -- Whether it is there now, on each output.
    present: ((() -> boolean) | ((output: string?) -> boolean))?,
    fluid: boolean?,
    [number]: any,
}

-- g.popup { … }: a surface beside a node of another one, over everything.
-- Written as a child of a node, it opens beside that node.
export type Popup = {
    -- The node it opens beside, by id (or a function giving one).
    anchor: (string | () -> string)?,
    -- "top", "bottom" (the default), "left" or "right".
    side: string?,
    -- "start", "center" (the default) or "end".
    align: string?,
    gap: number?,
    -- Whether it takes the keyboard and closes on a press outside.
    grab: boolean?,
    present: (() -> boolean)?,
    -- A press outside, or the compositor closing it.
    on_dismiss: (() -> ())?,
    [number]: any,
}

export type WindowSurface = {
    -- Its title, or a function giving it as it changes.
    title: (string | () -> string)?,
    -- What the compositor's rules match, as a program's id ("genoma").
    app_id: string?,
    -- The size it asks for first (640 × 480); the compositor may give
    -- another, and its content is laid out across what it gives.
    width: number?,
    height: number?,
    -- { width, height }
    min: { number }?,
    max: { number }?,
    present: (() -> boolean)?,
    -- The compositor asks to close it. With this, it is only told, and
    -- goes when `present` lets it go; without, it closes itself for good.
    on_close: (() -> ())?,
    [number]: any,
}

-- ---- g -----------------------------------------------------------------------

-- g.colour: arithmetic on "#rrggbb[aa]", as Qt does it (HSL, HSV,
-- rounding), so a theme derives its colours once and in one way. Every
-- colour it gives back is "#rrggbbaa".
export type Colour = {
    -- g.colour.parse("#7aa2f7") → { r, g, b, a }, 0 to 1; nil if not a colour.
    parse: (hex: string) -> { r: number, g: number, b: number, a: number }?,
    -- g.colour.format(r, g, b, a?) or g.colour.format({ r, g, b, a }).
    format: (r: number | { r: number, g: number, b: number, a: number? }, g: number?, b: number?, a: number?) -> string,
    -- g.colour.hsl("#…") → hue (-1 for a grey), saturation, lightness, alpha; 0 to 1.
    hsl: (hex: string) -> (number, number, number, number),
    hsla: (h: number, s: number, l: number, a: number?) -> string,
    -- g.colour.hsv("#…") → hue (-1 for a grey), saturation, value, alpha.
    hsv: (hex: string) -> (number, number, number, number),
    hsva: (h: number, s: number, v: number, a: number?) -> string,
    -- g.colour.alpha("#…", 0.5): the same colour at that opacity.
    alpha: (hex: string, a: number) -> string,
    -- g.colour.mix(a, b, 0.25): a quarter of the way from a to b.
    mix: (a: string, b: string, t: number) -> string,
    -- Lightness or saturation moved by an amount, negative to lower it.
    lighten: (hex: string, amount: number) -> string,
    saturate: (hex: string, amount: number) -> string,
    -- g.colour.lift("#…", 0.05, dark): away from the background's side,
    -- lighter on a dark theme (the default), darker on a light one.
    lift: (hex: string, amount: number, dark: boolean?) -> string,
    -- Rec. 709 weights on the encoded channels: below 0.5 reads as dark.
    luma: (hex: string) -> number,
    -- WCAG relative luminance, and the contrast ratio of two colours (1 to 21).
    luminance: (hex: string) -> number,
    contrast: (a: string, b: string) -> number,
}

export type Timing = {
    duration: number?,
    curve: { number }?,
    delay: number?,
    spring: { stiffness: number?, damping: number?, mass: number? }?,
}

export type G = {
    -- Lays its children out in a row or a column; draws nothing itself.
    box: (t: Node) -> Node,
    -- A box that draws: fill, rim, shadow, gradients, radius, blur.
    capsule: (t: Node) -> Node,
    -- A text in a font, size, weight and colour; wraps or ends in an ellipsis.
    text: (t: Node) -> Node,
    -- An SVG icon by name (looked up in g.icon_dir) or by path, painted in `color`.
    icon: (t: Node) -> Node,
    -- A picture from a file, fitted to its box.
    image: (t: Node) -> Node,
    -- A quadratic Bézier with a width, grown along its length.
    thread: (t: Node) -> Node,
    -- Particles born at a rate, toward a heading or to a point.
    emitter: (t: Node) -> Node,
    -- A rectangle drawn by a WGSL `shade` of one's own (`src`), or a
    -- built-in effect (`src = "genoma:noise"`).
    shader: (t: Node) -> Node,
    -- A line of text to type into.
    field: (t: Node) -> Node,
    -- Any outline: `d` (SVG path data) or `points`.
    path: (t: Node) -> Node,
    -- A row per item, kept by key.
    each: <T>(t: Each<T>) -> Node,
    -- A keyed list that scrolls, mounting only the rows in view.
    list: <T>(t: List<T>) -> Node,
    surface: (t: Surface) -> Surface,
    popup: (t: Popup) -> Popup,
    -- A window the compositor manages as a program's: tiled, in alt-tab.
    window: (t: WindowSurface) -> WindowSurface,

    -- g.signal(value): a value bindings follow, made anew by each load.
    signal: <T>(value: T) -> Signal<T>,
    -- g.state("key", initial): a signal kept across reloads by its key.
    state: <T>(key: string, initial: T) -> Signal<T>,
    -- g.computed(fn): a value derived from what fn reads, read as .value.
    computed: <T>(f: () -> T) -> Computed<T>,
    -- g.animated(value): a number or colour moved by :set over a timing.
    animated: (value: number | string) -> Animated,
    -- g.effect(fn): runs now and whenever what it read changes; stopped
    -- with the load.
    effect: (f: () -> ()) -> (),
    -- g.watch(source, handler): handler(value) now and whenever what source
    -- read changes. Only the source is tracked.
    watch: <T>(source: () -> T, handler: (value: T) -> ()) -> (),
    -- g.untracked(fn): its result, without what it read becoming a
    -- dependency of the binding around it.
    untracked: <T>(f: () -> T) -> T,
    -- g.batch(fn): the signals fn sets are heard once it returns.
    batch: (f: () -> ()) -> (),
    -- g.lasting(fn): what fn makes belongs to the load, not to the binding
    -- or effect running now.
    lasting: <T>(f: () -> T) -> T,
    -- g.cleanup(fn): fn runs when what is being made now is disposed.
    cleanup: (f: () -> ()) -> (),
    -- g.map(items, key, make): things made once per key.
    map: <T, U>(items: () -> { T }, key: (T) -> string | number, make: (item: () -> T, index: () -> number) -> U) -> () -> { U },

    -- g.timing { name = { duration, curve, delay } or { spring }, … }: the
    -- one place a duration is written.
    timing: (set: { [string]: Timing }) -> (),
    -- g.timing_default { … }: only the names not defined yet.
    timing_default: (set: { [string]: Timing }) -> (),
    -- g.crossing("theme", fn): what fn changes reaches every binding once,
    -- and the colours they set cross over that timing.
    crossing: (timing: string, f: () -> ()) -> (),

    -- g.timer { every = seconds, on_tick = fn }.
    timer: (t: { every: number, on_tick: () -> () }) -> Stoppable,
    -- g.after(seconds, fn): once, later.
    after: (seconds: number, f: () -> ()) -> Stoppable,
    -- g.now(): seconds since the interface loaded.
    now: () -> number,
    -- g.process { command, on_line, on_error_line, on_exit, env, cwd }.
    process: (t: {
        command: { string },
        on_line: ((line: string) -> ())?,
        on_error_line: ((line: string) -> ())?,
        on_exit: ((code: number) -> ())?,
        env: { [string]: string }?,
        cwd: string?,
    }) -> Process,
    -- g.file { path, watch = true }: read now and whenever it changes.
    file: (t: { path: string, watch: boolean? }) -> WatchedFile,
    -- g.folder { path, watch = true, hidden = false, details = false }: what
    -- is in a folder, followed; hidden keeps dot-files, details adds size
    -- and modified to each entry.
    folder: (t: { path: string, watch: boolean?, hidden: boolean?, details: boolean? }) -> Folder,
    -- g.socket { path, on_line, on_close }: a Unix socket, its input as lines.
    socket: (t: { path: string, on_line: ((line: string) -> ())?, on_close: (() -> ())? }) -> Socket,
    -- g.json("~/.config/…/file.json"): a data file read at load, only from
    -- the user's configuration directory.
    json: (path: string) -> any,
    -- g.parse_json(text) → the value.
    parse_json: (text: string) -> any,
    -- g.to_json(value, pretty) → text.
    to_json: (value: any, pretty: boolean?) -> string,
    -- g.json_set(text, path, value) or g.json_set(text, { { path, value } }):
    -- the document with those keys set, as two-space JSON.
    json_set: (text: string?, path: string | { { any } }, value: any?) -> string,
    -- g.array(t): t marked as a list, so empty it is written as [].
    array: <T>(t: { T }) -> { T },
    -- g.env("NAME") → its value or nil.
    env: (name: string) -> string?,
    -- Colour arithmetic: g.colour.lift, .mix, .contrast, …
    colour: Colour,
    -- The interface's directory.
    dir: string,
    -- g.outputs() → the outputs the compositor has.
    outputs: () -> { Output },
    -- g.desktop_entries() → the applications the menu offers, read now.
    desktop_entries: () -> { DesktopEntry },
    -- g.icon_dir(dir, …): where icon names are looked up, first match wins.
    icon_dir: (...string) -> (),
    -- g.icon_path("firefox") → the file the icon theme gives, or nil.
    icon_path: (name: string) -> string?,
    -- g.app_icon("firefox") → an application's icon file, or nil.
    app_icon: (app_id: string) -> string?,
    -- g.text_width { text, font, size, weight, italic, tabular, tracking }:
    -- logical units, as the text would be drawn.
    text_width: (t: { text: string, size: number, font: string?, weight: number?, italic: boolean?, tabular: boolean?, tracking: number? }) -> number,
    -- g.text_height { …, width, runs }: logical units, wrapped within width.
    text_height: (t: { text: string, size: number, width: number, font: string?, weight: number?, italic: boolean?, tabular: boolean?, tracking: number?, runs: { any }? }) -> number,
    -- g.ipc("target", { name = function(…) … end }): what
    -- `genoma ipc <target> <name> [arguments…]` calls; its return is the reply.
    ipc: (target: string, functions: { [string]: (...string) -> any }) -> (),
    -- g.quit(code): the interface is done and the process ends.
    quit: (code: number?) -> (),

    -- The keyboard's way through the open surface's buttons and sliders.
    focus_move: (by: number) -> (),
    focus_press: () -> (),
    focus_clear: () -> (),
    has_focus: () -> boolean,
    -- A token from the compositor for a program about to start, in answer
    -- to the press or key just handled: given to it (apps:launch's token, or
    -- XDG_ACTIVATION_TOKEN in g.process's env), its window takes the focus.
    -- nil where the compositor gives none.
    activation_token: (t: { app_id: string?, on_token: (token: string?) -> () }) -> (),

    dbus: {
        -- g.dbus.call { bus, destination, path, interface, method,
        -- signature, args, reply = function(ok, …) end }.
        call: (t: { [string]: any }) -> (),
        -- g.dbus.get { bus, destination, path, interface, property, reply }.
        get: (t: { [string]: any }) -> (),
        -- g.dbus.set { bus, destination, path, interface, property,
        -- signature, value, reply }.
        set: (t: { [string]: any }) -> (),
        -- g.dbus.watch { bus, sender, path, interface, member, on_signal }.
        watch: (t: { [string]: any }) -> Stoppable,
        -- g.dbus.variant("y", 2): a value with the type it takes in a `v`.
        variant: (signature: string, value: any) -> any,
    },
    pam: {
        -- g.pam.start { service, directory, user, on_prompt(message, echo),
        -- on_message(message, error), on_done(result) }.
        start: (t: { [string]: any }) -> Auth,
    },
    -- g.greetd { on_auth_message(message, kind), on_ready, on_launched,
    -- on_failure(description), on_error }.
    greetd: (t: { [string]: any }) -> Greetd,
}

-- ---- Services: require("@genoma/<name>") ------------------------------------

-- @genoma/clock
export type Clock = {
    read hour: number,
    read minute: number,
    read second: number,
}

-- @genoma/vitals: sampled every two seconds.
export type Vitals = {
    -- 0…1 of all cores.
    read cpu: number,
    -- 0…1 of the memory.
    read ram: number,
    -- 0…1, or nil where the card does not say.
    read gpu: number?,
    -- Where the card's clock sits between its lowest and highest, 0…1.
    read gpu_clock: number,
    -- The processor's clock between its lowest and highest, 0…1.
    read cpu_clock: number,
    -- °C, whole degrees: the processor package (k10temp, coretemp,
    -- zenpower, cpu_thermal) and the card `gpu` reads (its edge); nil where
    -- the machine does not say.
    read cpu_temp: number?,
    read gpu_temp: number?,
    -- Every hwmon temperature (°C), fan (RPM, to ten) and power draw (W),
    -- in the kernel's order. A temperature input with nothing on it is
    -- left out.
    read sensors: { {
        kind: "temperature" | "fan" | "power",
        -- The driver: "k10temp", "amdgpu", "nvme", "nct6799"…
        chip: string,
        -- The chip's label ("Tctl", "edge"), or the input's ("fan2").
        label: string,
        value: number,
        -- Where the chip says it runs hot, and where it is in danger.
        high: number?,
        critical: number?,
    } },
}

export type Window = {
    id: string,
    title: string?,
    app_id: string?,
    -- nil through the standard protocols, which do not say.
    workspace: string?,
    output: string?,
    -- Only the standard protocols report it.
    minimized: boolean,
}
export type Workspace = {
    id: string,
    idx: number,
    name: string?,
    output: string?,
    active: boolean,
    focused: boolean,
    -- Always false through the standard protocols, which do not say.
    vacant: boolean,
    urgent: boolean,
}

-- @genoma/compositor: niri, Hyprland or Umbriel, whichever runs; any other
-- through ext-workspace-v1 and wlr-foreign-toplevel-management.
export type Compositor = {
    -- "niri", "hyprland", "umbriel" or "wayland".
    read name: string,
    -- Whether minimize_window does anything: only through the protocols.
    read can_minimize: boolean,
    -- The focused window's title, or nil.
    read title: string?,
    -- Where a shortcut means.
    read focused_output: string?,
    read focused: Window?,
    read windows: { Window },
    -- The keyboard layouts, niri's and Hyprland's; empty elsewhere. niri
    -- names them as xkb describes them ("English (US)"), Hyprland by code
    -- ("us"); `name` is the one in use, in full, on both.
    read keyboard: { layouts: { string }, current: number?, name: string? },
    -- compositor:workspaces("DP-1"), every output's without one, in order.
    workspaces: (self: Compositor, output: string?) -> { Workspace },
    -- compositor:active("DP-1") → the workspace shown there.
    active: (self: Compositor, output: string) -> { id: string, idx: number, name: string? }?,
    -- compositor:focused_on("DP-1") → the focused window if it is there.
    focused_on: (self: Compositor, output: string) -> Window?,
    focus_workspace: (self: Compositor, id: string | number) -> (),
    focus_window: (self: Compositor, id: string | number) -> (),
    close_window: (self: Compositor, id: string | number) -> (),
    -- Minimizes (or, with false, brings back) a window.
    minimize_window: (self: Compositor, id: string | number, minimized: boolean?) -> (),
    -- "next", "prev", or a place in keyboard.layouts, from 1.
    switch_layout: (self: Compositor, to: "next" | "prev" | number) -> (),
}

-- @genoma/niri: niri's own model, with niri's numeric ids.
export type Niri = {
    read title: string?,
    read focused_output: string?,
    read focused: { title: string?, app_id: string? }?,
    workspaces: (self: Niri, output: string) -> { { id: number, idx: number, name: string?, active: boolean } },
    active: (self: Niri, output: string) -> { idx: number, name: string? }?,
    focused_on: (self: Niri, output: string) -> { title: string?, app_id: string? }?,
    focus_workspace: (self: Niri, id: number) -> (),
}

-- @genoma/power: UPower and power-profiles-daemon.
export type Power = {
    -- The display device as UPower sums it.
    read battery: {
        present: boolean,
        laptop: boolean,
        -- 0…1.
        level: number,
        -- "charging", "discharging", "empty", "full", "pending-charge",
        -- "pending-discharge" or "unknown".
        state: string,
        -- Seconds; 0 when UPower does not know.
        time_to_empty: number,
        time_to_full: number,
        -- Watts going in or out; 0 while it is still measuring.
        energy_rate: number,
    },
    read on_battery: boolean,
    -- "power-saver", "balanced", "performance", or "".
    read profile: string,
    read profiles: { string },
    -- Why performance is held back, or "".
    read degraded: string,
    -- The daemon's answer comes back as power.profile.
    set_profile: (self: Power, profile: string) -> (),
}

-- @genoma/network: NetworkManager.
export type Network = {
    read available: boolean,
    -- "unknown", "none", "portal", "limited", "full"
    read connectivity: string,
    -- Whether NetworkManager may check connectivity now.
    read can_check: boolean,
    read wifi_enabled: boolean,
    read wifi_hardware: boolean,
    -- The last failed call's error, or "".
    read failure: string,
    -- The connected ones first.
    read devices: { { interface: string, kind: string, state: string, connected: boolean, carrier: boolean, speed: number, connection: string? } },
    -- The connected one first, then by strength.
    read networks: { { ssid: string, strength: number, secured: boolean, connected: boolean, frequency: number, known: boolean } },
    set_wifi_enabled: (self: Network, on: boolean) -> (),
    -- network:set_scanning(true) while a list is on screen, false after.
    set_scanning: (self: Network, on: boolean) -> (),
    check_connectivity: (self: Network) -> (),
    -- Through its saved profile, or with a password a new one.
    join: (self: Network, ssid: string, password: string?) -> (),
    -- The Wi-Fi network joined now.
    leave: (self: Network) -> (),
    forget: (self: Network, ssid: string) -> (),
}

-- @genoma/media: MPRIS players.
export type Media = {
    read players: { { [string]: any } },
    -- Seconds now, carried forward at the rate; not a signal.
    position: (self: Media, name: string) -> number,
    -- Forward or back by seconds.
    seek: (self: Media, name: string, by: number) -> (),
    set_position: (self: Media, name: string, at: number) -> (),
}

-- @genoma/audio: PipeWire.
export type Audio = {
    read nodes: { { [string]: any } },
    -- From the output node to the input node.
    read links: { { id: number, output: number, input: number, active: boolean } },
    -- The defaults by node.name.
    read default_sink: string,
    read default_source: string,
    set_volume: (self: Audio, id: number, volume: number) -> (),
    set_mute: (self: Audio, id: number, muted: boolean) -> (),
    -- A sink or a source, by its kind.
    set_default: (self: Audio, id: number) -> (),
}

-- @genoma/bluetooth: BlueZ.
export type Bluetooth = {
    read adapter: { name: string, address: string, powered: boolean, discovering: boolean, discoverable: boolean, pairable: boolean }?,
    read devices: { { [string]: any } },
    -- The last failed call.
    read failure: { address: string, error: string }?,
    set_powered: (self: Bluetooth, on: boolean) -> (),
    -- bluetooth:set_discovering(true) while a list is open, false after.
    set_discovering: (self: Bluetooth, on: boolean) -> (),
    forget: (self: Bluetooth, address: string) -> (),
    set_trusted: (self: Bluetooth, address: string, on: boolean) -> (),
}

-- @genoma/idle: ext-idle-notify.
export type Idle = {
    read available: boolean,
    -- No input for the seconds `after` asked, and nothing inhibiting idle.
    read idle: boolean,
    -- 0 stops asking.
    after: (self: Idle, seconds: number) -> (),
    -- A timeout of its own, beside the others; 0 stops asking.
    watch: (self: Idle, name: string, seconds: number) -> (),
    is_idle: (self: Idle, name: string) -> boolean,
}

-- @genoma/lock: ext-session-lock.
export type Lock = {
    read available: boolean,
    -- Every output is covered.
    read locked: boolean,
    -- The compositor refused the lock, or ended it on its own.
    read finished: boolean,
    -- The session is locked and the interface's "lock" surfaces are made.
    lock: (self: Lock) -> (),
    unlock: (self: Lock) -> (),
}

-- @genoma/clipboard: ext-data-control.
export type Clipboard = {
    read available: boolean,
    -- What the current copy is offered as; empty with nothing copied.
    read types: { string },
    -- The current copy as text, up to 1 MiB; "".
    read text: string,
    -- Moves on every copy.
    read changed: number,
    -- The texts copied, newest first, each once; in memory only, a copy a
    -- password manager marks secret left out. `at` is seconds since the epoch.
    read history: { { text: string, at: number } },
    -- The history's entry i (from 1) taken out.
    forget: (self: Clipboard, index: number) -> (),
    clear_history: (self: Clipboard) -> (),
    -- How many texts the history keeps (50).
    keep: (self: Clipboard, count: number) -> (),
    set_text: (self: Clipboard, text: string) -> (),
    -- clipboard:set("image/png", "~/…/shot.png"): a file's content, offered
    -- as the type (or types) given.
    set: (self: Clipboard, types: string | { string }, path: string) -> (),
    clear: (self: Clipboard) -> (),
    -- clipboard:save("image/png", "~/…/1.png") → true when the copy is
    -- offered in that type.
    save: (self: Clipboard, type: string, path: string) -> boolean,
}

-- @genoma/toplevels: ext-foreign-toplevel-list.
export type Toplevels = {
    read available: boolean,
    read list: { { identifier: string, title: string?, app_id: string? } },
}

export type DisplayMode = { width: number, height: number, refresh: number, preferred: boolean }

export type Display = {
    -- The connector, as g.outputs() names it: "DP-1".
    name: string,
    description: string,
    make: string,
    model: string,
    serial: string,
    -- 0 when unknown.
    width_mm: number,
    height_mm: number,
    enabled: boolean,
    modes: { DisplayMode },
    -- The mode in use; nil when off.
    mode: DisplayMode?,
    x: number,
    y: number,
    scale: number,
    -- "normal", "90", "180", "270", "flipped", "flipped-90", "flipped-180"
    -- or "flipped-270".
    transform: string,
    -- nil when the compositor does not say.
    adaptive_sync: boolean?,
}

-- What one display changes; what is left out stays. `mode` takes a
-- DisplayMode or { width, height, refresh? } (a size not listed, or a
-- refresh not near a listed one, is asked for as a custom mode);
-- `position` takes { x = , y = } or { x, y }.
export type DisplayChange = {
    enabled: boolean?,
    mode: { [any]: number }?,
    position: { [any]: number }?,
    scale: number?,
    transform: string?,
    adaptive_sync: boolean?,
}

-- @genoma/displays: wlr-output-management, the displays and their setup.
export type Displays = {
    read available: boolean,
    read heads: { Display },
    -- The compositor's answer to the latest configuration: status
    -- "applied", "works" (a test), "failed", or "cancelled" (the displays
    -- changed meanwhile).
    read result: { id: number, test: boolean, status: string }?,
    -- displays:apply({ ["DP-1"] = { scale = 1.25 } }) → its id. Displays not
    -- named keep their setup; a mistake (no such display, every display
    -- off, a transform with no name) is an error before anything is sent.
    apply: (self: Displays, changes: { [string]: DisplayChange }) -> number,
    -- The same, only asking whether it would work.
    test: (self: Displays, changes: { [string]: DisplayChange }) -> number,
}

-- A colour ramp: Kelvin (1000…10000, 6500 changes nothing), brightness
-- (0.1…1) and gamma (1 is the display's own).
export type GammaSetting = { temperature: number, brightness: number, gamma: number }

-- @genoma/gamma: wlr-gamma-control, a night light of the shell's own. One
-- program at a time holds a display's ramp; a neutral setting lets go and
-- the compositor restores its own.
export type Gamma = {
    read available: boolean,
    -- The displays whose ramp is ours (their setting), or held by another
    -- program ("failed"); a display at its own ramp is not listed.
    read held: { [string]: GammaSetting | string },
    -- What a display is asked to show; every display's without one.
    get: (self: Gamma, output: string?) -> GammaSetting,
    -- What is left out stays; without an output, every display without
    -- its own setting.
    set: (self: Gamma, setting: { temperature: number?, brightness: number?, gamma: number? }, output: string?) -> (),
    -- An output back to every display's setting; with none, every display
    -- back to its own ramp.
    reset: (self: Gamma, output: string?) -> (),
}

-- @genoma/tray: StatusNotifierItem with DBusMenu.
export type Tray = {
    read items: { { [string]: any } },
    -- Whether Genoma serves the watcher itself.
    read serving_watcher: boolean,
    -- The opened menu's tree, or nil before open_menu has read it.
    menu: (self: Tray, key: string) -> { [string]: any }?,
    open_menu: (self: Tray, key: string) -> (),
    close_menu: (self: Tray, key: string) -> (),
    menu_click: (self: Tray, key: string, id: number) -> (),
    -- By delta steps, "vertical" or "horizontal".
    scroll: (self: Tray, key: string, delta: number, orientation: string?) -> (),
}

-- @genoma/polkit: an authentication agent.
export type Polkit = {
    read registered: boolean,
    -- Why it is not registered, or "".
    read error: string,
    read request: { [string]: any }?,
    -- Registers again, after another agent has let the session go.
    retry: (self: Polkit) -> (),
    respond: (self: Polkit, cookie: string, password: string) -> (),
    cancel: (self: Polkit, cookie: string) -> (),
}

-- @genoma/notifications: an org.freedesktop.Notifications server.
export type Notifications = {
    -- Whether Genoma owns the session's notification name.
    read serving: boolean,
    -- Why it does not, or "".
    read error: string,
    read list: { { [string]: any } },
    -- notifications:close(id, "expired" | "dismissed")
    close: (self: Notifications, id: number, reason: string?) -> (),
    -- The action; then dismissed unless the notification is resident.
    invoke: (self: Notifications, id: number, key: string) -> (),
}

export type AppAction = { id: string, name: string, icon: string }

export type AppEntry = {
    -- The desktop file's name without .desktop: "org.gnome.Nautilus".
    id: string,
    -- In the user's language where the file has it.
    name: string,
    generic_name: string,
    comment: string,
    icon: string,
    exec: string,
    terminal: boolean,
    startup_class: string,
    keywords: { string },
    categories: { string },
    file: string,
    -- What it offers beside starting: a new window, a private one.
    actions: { AppAction },
}

-- @genoma/apps: the applications the menu offers, found, ranked by use, started.
export type Apps = {
    -- Every application, by name; read the first time it is asked for.
    read list: { AppEntry },
    -- The best matches first: the name, its words, what else describes it,
    -- letters in order; the most used rise among equals. With nothing
    -- typed, the most used first. A limit of 0 is all of them.
    search: (self: Apps, query: string, limit: number?) -> { AppEntry },
    -- The application a window's app_id or class, or a process's name,
    -- belongs to, or nil.
    find: (self: Apps, name: string) -> AppEntry?,
    -- Started out of the shell's session (an action by its id, files for
    -- its %f and %u), in a terminal when it asks for one, and counted.
    -- token: from g.activation_token, so its window takes the focus.
    launch: (self: Apps, id: string, action: string?, files: { string }?, token: string?) -> (),
    -- How often and when last it was started from here (seconds since the
    -- epoch), kept in $XDG_STATE_HOME/genoma/apps.json.
    usage: (self: Apps, id: string) -> { count: number, last: number },
    -- Read again, after an installation.
    refresh: (self: Apps) -> (),
}

-- An inhibitor this interface holds.
export type Inhibition = {
    -- Whether logind has given it and it is not let go.
    read held: boolean,
    release: (self: Inhibition) -> (),
}

-- @genoma/brightness: the backlight (written through logind) and external
-- monitors over DDC/CI (through ddcutil, when installed and /dev/i2c-* may
-- be opened). DDC answers seconds after the start; nothing polls after.
export type Brightness = {
    -- The backlight first, then the monitors DDC found.
    read displays: { {
        kind: "backlight" | "ddc",
        -- "eDP-1", "DP-2"; "display-<n>" where nothing says.
        connector: string,
        -- Over DDC, the monitor's model; "" for a backlight.
        model: string,
        -- 0…1, nil until the display has answered.
        value: number?,
        -- The hardware's top step.
        max: number,
    } },
    -- Some display can be dimmed.
    read available: boolean,
    -- DDC detection is under way: more displays may come.
    read detecting: boolean,
    -- The last write's failure, or "".
    read error: string,
    -- 0…1 or nil: that connector's, the first display's without one.
    value: (self: Brightness, connector: string?) -> number?,
    -- Moves at once; the hardware follows. A dragged control sends every
    -- step: only the latest waits for a slow monitor.
    set: (self: Brightness, fraction: number, connector: string?) -> (),
    -- Reads every display again, after a change made elsewhere.
    refresh: (self: Brightness) -> (),
}

export type Volume = {
    -- "/dev/sdb1".
    device: string,
    label: string,
    -- The filesystem: "vfat", "exfat", "ext4"…
    kind: string,
    -- Bytes.
    size: number,
    mounts: { string },
    -- Bytes left, while mounted.
    free: number?,
}

-- @genoma/drives: removable drives through udisks2. Read again after any
-- udisks signal; the rate and free space once a second, only while a
-- volume is mounted.
export type Drives = {
    -- Plugged in, with at least one volume; by device.
    read list: { {
        -- Stable while plugged in: what eject takes.
        id: string,
        -- Vendor and model, else the first label, else the device.
        name: string,
        -- The whole disk, "/dev/sdb".
        device: string,
        can_power_off: boolean,
        is_loop: boolean,
        -- Bytes a second being written now, page cache included: not
        -- safe to pull while it is above 0.
        rate: number,
        -- "mounting", "unmounting", "ejecting" or "".
        busy: string,
        -- Why the last action failed, in words, or "".
        error: string,
        volumes: { Volume },
    } },
    mount: (self: Drives, device: string) -> (),
    unmount: (self: Drives, device: string) -> (),
    -- Unmounts every volume (waiting for the writing to end), then powers
    -- the drive off; a loop device is deleted.
    eject: (self: Drives, id: string) -> (),
    -- Loop devices this user set up count as drives: a stick made of a file.
    include_loops: (self: Drives, on: boolean) -> (),
}

export type User = {
    uid: number,
    login: string,
    -- The real name, or the login when there is none.
    name: string,
    -- A picture that can be read (AccountsService's, else ~/.face), or nil.
    icon: string?,
    email: string,
    language: string,
    administrator: boolean,
    locked: boolean,
    home: string,
    shell: string,
    -- How many times they logged in.
    logins: number,
}

-- @genoma/accounts: AccountsService. Without it, `me` still comes from
-- /etc/passwd and ~/.face, and nothing can be set.
export type Accounts = {
    read available: boolean,
    -- This session's user.
    read me: User,
    -- The people (no system accounts), most logged in first: a greeter's.
    read users: { User },
    -- The last change's failure, or "".
    read error: string,
    -- A new picture, copied where the greeter and the lock read it.
    set_icon: (self: Accounts, path: string) -> (),
    set_name: (self: Accounts, name: string) -> (),
}

-- @genoma/session: logind. What the machine can do and doing it, the moment before sleep, inhibitors.
export type Session = {
    -- Whether logind answers.
    read available: boolean,
    -- Each "yes", "no", "challenge" (a password, through polkit) or "na".
    read can: { suspend: string, hibernate: string, hybrid_sleep: string, reboot: string, power_off: string },
    -- True from logind's PrepareForSleep(true) to (false): the moment to
    -- lock, held open by a "delay" inhibitor on "sleep".
    read preparing_for_sleep: boolean,
    read preparing_for_shutdown: boolean,
    -- Moves on each request to lock this session (loginctl lock-session).
    read lock_requests: number,
    -- Moves on each request to unlock it.
    read unlock_requests: number,
    read inhibitors: { { what: string, who: string, why: string, mode: string, uid: number, pid: number } },
    suspend: (self: Session) -> (),
    hibernate: (self: Session) -> (),
    hybrid_sleep: (self: Session) -> (),
    reboot: (self: Session) -> (),
    power_off: (self: Session) -> (),
    -- logind asked to lock this session: every listener hears it.
    lock_session: (self: Session) -> (),
    -- session:inhibit("sleep", "Locking first", "delay"): what ("sleep",
    -- "shutdown", "idle", "handle-lid-switch", joined by ':'), why, and the
    -- mode, "block" (the default) or "delay".
    inhibit: (self: Session, what: string, why: string, mode: string?) -> Inhibition,
}

-- A hold on the spectrum: the capture runs while any is held.
export type SpectrumHold = {
    release: (self: SpectrumHold) -> (),
}

-- @genoma/spectrum: the sound as bands, captured by `genoma spectrum` only while held.
export type Spectrum = {
    -- Each channel's 64 bands, low to high (30 Hz to 16 kHz, logarithmic),
    -- 0…1; empty while nobody holds it. A shader's values take them as
    -- they are.
    read left: { number },
    read right: { number },
    -- The loudest band of either.
    read level: number,
    -- Whether the capture runs.
    read running: boolean,
    hold: (self: Spectrum) -> SpectrumHold,
    -- "output" (what leaves the machine, the default) or "input" (the
    -- microphone), from the next start.
    listen_to: (self: Spectrum, source: string) -> (),
}
