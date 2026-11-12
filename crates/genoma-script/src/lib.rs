//! Genoma's interface language (PRD §7): Luau files that return trees, with
//! properties that are either values or bindings.
//!
//! A property set to a function is a binding. It runs inside an effect, so
//! whatever signal it reads (a service's field, a `g.state`) wakes it, and
//! only that. Everything a load creates belongs to one scope, and the next
//! load disposes it: a reload never leaves an old binding running.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

mod auth;
mod colour;
mod profile;
pub mod dbus;
mod greetd;
mod list;
mod values;
pub mod io;

use genoma_core::{
    reactive::{Effect, Scope, Signal, current_owner, detached, on_cleanup, owned_by},
    timing::{self, Curve, Timing},
};
use genoma_scene::{Kind, Prop, Role, Scene, Ty, Value};
use genoma_services::{
    apps::{App as AppEntry, Apps},
    session::{Action as SessionAction, Session},
    spectrum::Spectrum,
    audio::Audio,
    bluetooth::{Bluetooth, DeviceCall},
    clipboard::{Clipboard, Request as ClipboardRequest, TEXT_TYPES},
    compositor::Compositor,
    clock::Clock,
    idle::Idle,
    lock::Lock,
    media::{Action, Media},
    network::Network,
    niri::Niri,
    notifications::Notifications,
    polkit::Polkit,
    power::Power,
    toplevels::Toplevels,
    displays::{Change, Displays, Head},
    gamma::{Gamma, Held, Setting},
    tray::{MenuEntry, Tray},
    vitals::Vitals,
};
use mlua::{Function, Lua, Table, UserData, UserDataFields, Value as LuaValue};

/// A key as a field reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub name: String,
    pub text: Option<String>,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// The logo key (Super), niri's Mod.
    pub logo: bool,
    /// The xkb keycode: where the key is, whatever the layout and Shift
    /// make of it (Qt's nativeScanCode).
    pub code: u32,
}

/// Pointer input for a node, as the surface saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A press outside the node that is catching (an open cell).
    Outside,
    /// A key, by its xkb name ("Escape", "Up", "Return", "a"), with the text
    /// it types and the modifiers held.
    Key(Key),
    Hover(bool),
    /// The button, and where on the node, logical units from its top-left
    /// corner: a progress line seeks to where it was pressed.
    Press(u32, f32, f32),
    Release(u32, f32, f32),
    /// A press and a release on the same node.
    Click(u32, f32, f32),
    /// The pointer moving while a button is held, to the node the press
    /// landed on, logical units from its top-left corner wherever the
    /// pointer has gone since: a rectangle being drawn.
    Drag(f32, f32),
    /// Where the pointer is over the node, entering it or moving on it: in
    /// surface coordinates here, made local to whichever node handles it, so
    /// a field that reaches past what it holds reads the point in its own
    /// frame (the quick actions' ring, pointing by direction).
    Move(f32, f32),
    /// An image's pixels drawn for the first time since its `src` was set:
    /// a picture that should appear only once it is there (a wallpaper's
    /// crossfade). Not pointer input, and never passed up to a parent.
    Ready,
    /// The keyboard's focus came to the node (true) or left it: it shows
    /// that it is the one Enter would press. Never passed up.
    Focus(bool),
    /// A node inside took the keyboard's focus, at this rectangle (x, y,
    /// width, height), local to whoever handles it: the nearest ancestor
    /// with on_focus_within, a well that scrolls it into view.
    FocusWithin(f32, f32, f32, f32),
    /// A node's laid-out size, logical units, after the first layout and
    /// whenever it changes: what only the layout knows (the height of a
    /// column of wrapped text). Not pointer input, never passed up.
    Size(f32, f32),
    /// A field's text after the person changed it (typing, erasing,
    /// pasting, undoing): what the interface keeps. Never passed up.
    Change(String),
    /// Return in a field, with its text. Never passed up.
    Submit(String),
    /// Discrete wheel steps, or touchpad movement in logical units; true
    /// for steps, so a high-resolution wheel's fractions read as steps.
    Wheel(f64, f64, bool),
    /// The fingers left the touchpad while moving: their speed over the last
    /// moment, logical units a second, for the content to carry on.
    Fling(f64, f64),
    /// Something dropped on the node (drag and drop from another program,
    /// or from the shell itself). Never passed up: the node is the target
    /// the runtime found.
    Drop(Dropped),
    /// Something being dragged came over the node (true) or left it.
    DropHover(bool),
}

/// What a drop brought: the files of a `text/uri-list` as paths, the
/// text when there was some, and where it fell, surface coordinates.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dropped {
    pub files: Vec<String>,
    pub uris: Vec<String>,
    pub text: Option<String>,
    pub x: f32,
    pub y: f32,
}

impl Input {
    fn handler(&self) -> &'static str {
        match self {
            Input::Outside => "on_outside",
            Input::Key(_) => "on_key",
            Input::Hover(_) => "on_hover",
            Input::Press(..) => "on_press",
            Input::Release(..) => "on_release",
            Input::Click(..) => "on_click",
            Input::Drag(..) => "on_drag",
            Input::Move(..) => "on_move",
            Input::Ready => "on_ready",
            Input::Size(..) => "on_size",
            Input::Focus(_) => "on_focus",
            Input::FocusWithin(..) => "on_focus_within",
            Input::Change(_) => "on_change",
            Input::Submit(_) => "on_submit",
            Input::Drop(_) => "on_drop",
            Input::DropHover(_) => "on_drop_hover",
            Input::Wheel(..) => "on_wheel",
            Input::Fling(..) => "on_fling",
        }
    }
}

const HANDLERS: &[&str] = &[
    "on_hover", "on_press", "on_release", "on_click", "on_drag", "on_move", "on_ready", "on_size", "on_focus", "on_focus_within", "on_wheel",
    "on_fling", "on_outside", "on_key", "on_change", "on_submit", "on_drop", "on_drop_hover",
];

/// What `g.focus_*` asks of the keyboard's focus, done by the host once the
/// key that asked has been handled, on the surface the key went to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FocusRequest {
    /// To the next (1) or previous (-1) button or slider of the open cell.
    Move(i32),
    /// Press the one that has it, as a click.
    Press,
    /// Let it go.
    Clear,
}

/// The requests waiting, and whether a node has the focus now.
pub type FocusQueue = (Rc<RefCell<Vec<FocusRequest>>>, Rc<std::cell::Cell<bool>>);

/// Activation tokens asked for (`g.activation_token`), by number, each with
/// the app id it is for: the runtime asks the compositor and answers with
/// [`Script::activation_done`].
pub type ActivationQueue = Rc<RefCell<Vec<(u64, Option<String>)>>>;

/// Handlers by (target, node): node ids of different scenes can coincide.
type Handlers = Rc<RefCell<HashMap<(usize, genoma_scene::NodeId), HashMap<&'static str, Function>>>>;

/// Where and how the shell's surface sits on the output.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceSpec {
    pub anchor: Vec<String>,
    /// Top, right, bottom, left.
    pub margin: [i32; 4],
    pub layer: String,
    pub output: Option<String>,
    /// Keep windows out of the surface's strip (a membrane on its edge).
    pub reserve: bool,
    /// The strip to keep windows out of, logical units, when the surface is
    /// larger than its strip (a `full` membrane); otherwise its height.
    pub reserve_strip: Option<i32>,
    /// The size of its output, whatever its content: a membrane whose cells
    /// open over the windows, as Bioma's are, so the surface never resizes.
    pub full: bool,
    /// Over what the other surfaces keep for themselves, rather than inside
    /// what they leave: a surface that is the whole output, its origin the
    /// output's (a selection drawn over the desktop).
    pub above_zones: bool,
    /// `"all"` for one surface on every output, a list of output names, or
    /// none for a single surface (on `output`, or the compositor's choice).
    pub outputs: Option<Vec<String>>,
    pub all_outputs: bool,
    /// The layer surface's namespace: what the compositor's rules match.
    /// niri decides blur xray by it (Bioma's `bioma.kdl`), so a surface keeps
    /// its compositor behaviour by keeping its name.
    pub namespace: String,
    /// A popup (`g.popup`) rather than a layer surface: where it opens.
    pub popup: Option<PopupSpec>,
    /// A window (`g.window`) rather than a layer surface: what it is called
    /// and the size it asks for.
    pub window: Option<WindowSpec>,
    /// When it takes the keyboard: "none" (only while a node catches),
    /// "on_demand" (when pressed: a bar with a search field), "exclusive"
    /// (always).
    pub keyboard: String,
}

/// A window the compositor manages as it does any program's: tiled or
/// floating, in its alt-tab, decorated by it where it decorates.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSpec {
    /// Its title, when the description wrote one; a function's comes as a
    /// wish.
    pub title: Option<String>,
    /// What the compositor's rules match, as an application's id.
    pub app_id: String,
    /// The size it asks for first, logical units; the compositor may give
    /// another.
    pub size: (u32, u32),
    pub min: Option<(u32, u32)>,
    pub max: Option<(u32, u32)>,
    /// Asked to close, it closes itself: it has no `on_close` to decide.
    /// With one, it stays until its `present` lets it go (a question
    /// first, unsaved work).
    pub closes_itself: bool,
}

/// Where a popup opens: beside a node of another surface on its output,
/// the compositor turning or sliding it to keep it on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct PopupSpec {
    /// The `id` of the node it opens from; `None` when a function gives
    /// it (the runtime then reads it from the surface's wish).
    pub anchor: Option<String>,
    /// The side of that node it opens on: "bottom", "top", "left", "right".
    pub side: String,
    /// Along that side: "start", "center", "end".
    pub align: String,
    /// Logical units between the node and the popup.
    pub gap: i32,
    /// Takes the keyboard, and a press anywhere else closes it.
    pub grab: bool,
}

/// An output as the compositor describes it, for `g.outputs()`.
#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub size: Option<(u32, u32)>,
}

/// A `g.state` value: what a signal can hold across reloads.
///
/// A table is held by reference, and equal only to itself: a service sets a
/// new list rather than changing the one it set, or nothing hears of it.
/// One from a load that has been replaced reads as nil.
#[derive(Clone, Debug)]
enum StateValue {
    Nil,
    Bool(bool),
    Number(f64),
    Text(String),
    Table(Table),
}

impl PartialEq for StateValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (StateValue::Nil, StateValue::Nil) => true,
            (StateValue::Bool(a), StateValue::Bool(b)) => a == b,
            (StateValue::Number(a), StateValue::Number(b)) => a == b,
            (StateValue::Text(a), StateValue::Text(b)) => a == b,
            // mlua asserts that two values it compares share a state.
            (StateValue::Table(a), StateValue::Table(b)) => {
                a.weak_lua() == b.weak_lua() && a.weak_lua().try_upgrade().is_some() && a.to_pointer() == b.to_pointer()
            }
            _ => false,
        }
    }
}

impl StateValue {
    fn from_lua(v: &LuaValue) -> StateValue {
        match v {
            LuaValue::Boolean(b) => StateValue::Bool(*b),
            LuaValue::Integer(i) => StateValue::Number(*i as f64),
            LuaValue::Number(n) => StateValue::Number(*n),
            LuaValue::String(s) => StateValue::Text(s.to_string_lossy()),
            LuaValue::Table(t) => StateValue::Table(t.clone()),
            _ => StateValue::Nil,
        }
    }

    fn into_lua(self, lua: &Lua) -> mlua::Result<LuaValue> {
        Ok(match self {
            StateValue::Nil => LuaValue::Nil,
            StateValue::Bool(b) => LuaValue::Boolean(b),
            StateValue::Number(n) => LuaValue::Number(n),
            StateValue::Text(t) => LuaValue::String(lua.create_string(&t)?),
            StateValue::Table(t) if *t.weak_lua() == lua.weak() => LuaValue::Table(t),
            StateValue::Table(_) => LuaValue::Nil,
        })
    }
}

struct State(Signal<StateValue>);

impl UserData for State {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // The userdata is borrowed only to copy the signal out: setting it
        // runs the effects that read it, and they read it through this same
        // userdata, which a borrow held across the set would lock.
        fields.add_field_function_get("value", |lua, ud: mlua::AnyUserData| {
            let signal = ud.borrow::<State>()?.0;
            signal.try_get().unwrap_or(StateValue::Nil).into_lua(lua)
        });
    }

    // The setter is a metamethod, not a field setter: mlua guards a field
    // setter as FnMut, and setting one signal runs the effects that set
    // another through this same function, which the guard refuses.
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(
            mlua::MetaMethod::NewIndex,
            |_, (ud, key, v): (mlua::AnyUserData, String, LuaValue)| {
                if key != "value" {
                    return Err(mlua::Error::runtime(format!("a signal has no field '{key}'")));
                }
                let signal = ud.borrow::<State>()?.0;
                signal.set(StateValue::from_lua(&v));
                Ok(())
            },
        );
    }
}

/// The services an interface may require. The runtime makes them; a load
/// records which ones it asked for, and only those get their sources.
#[derive(Clone)]
pub struct Services {
    pub clock: Rc<Clock>,
    pub niri: Option<Rc<Niri>>,
    /// Whichever compositor runs (niri, Hyprland, Umbriel), as one model.
    pub compositor: Option<Rc<Compositor>>,
    pub vitals: Rc<Vitals>,
    pub power: Rc<Power>,
    pub network: Rc<Network>,
    pub media: Rc<Media>,
    pub audio: Rc<Audio>,
    pub bluetooth: Rc<Bluetooth>,
    pub toplevels: Rc<Toplevels>,
    /// wlr-output-management: the displays and their setup.
    pub displays: Rc<Displays>,
    /// wlr-gamma-control: colour temperature and brightness.
    pub gamma: Rc<Gamma>,
    pub idle: Rc<Idle>,
    pub lock: Rc<Lock>,
    pub tray: Rc<Tray>,
    pub polkit: Rc<Polkit>,
    pub notifications: Rc<Notifications>,
    pub clipboard: Rc<Clipboard>,
    /// The applications the menu offers.
    pub apps: Rc<Apps>,
    /// logind: what the machine can do, sleep, inhibitors.
    pub session: Rc<Session>,
    /// The sound as bands.
    pub spectrum: Rc<Spectrum>,
}

struct NiriRef(Rc<Niri>);

impl UserData for NiriRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("title", |_, this| Ok(this.0.title.get()));
        // niri.focused_output → "DP-1": where a shortcut means.
        fields.add_field_method_get("focused_output", |_, this| Ok(this.0.focused_output()));
        // niri.focused → { title = …, app_id = … } or nil, on any output.
        fields.add_field_method_get("focused", |lua, this| {
            let Some(w) = this.0.focused_window() else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("title", w.title)?;
            t.set("app_id", w.app_id)?;
            Ok(LuaValue::Table(t))
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // niri:active("DP-1") → { idx = 2, name = "web" } or nil
        methods.add_method("active", |lua, this, output: String| {
            let Some(w) = this.0.active(&output) else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("idx", w.idx)?;
            t.set("name", w.name)?;
            Ok(LuaValue::Table(t))
        });
        // niri:workspaces("DP-1") → { { id, idx, name, active }, … } in order
        methods.add_method("workspaces", |lua, this, output: String| {
            let list = lua.create_table()?;
            for (i, w) in this.0.on_output(&output).into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", w.id)?;
                t.set("idx", w.idx)?;
                t.set("name", w.name)?;
                t.set("active", w.is_active)?;
                t.set("vacant", w.vacant)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // niri:focus_workspace(id): the action, on its own connection.
        methods.add_method("focus_workspace", |_, this, id: u64| {
            this.0.focus_workspace(id).map_err(err)
        });
        // niri:focused_on("DP-1") → { title = …, app_id = … } or nil
        methods.add_method("focused_on", |lua, this, output: String| {
            let Some(w) = this.0.focused_on(&output) else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("title", w.title)?;
            t.set("app_id", w.app_id)?;
            Ok(LuaValue::Table(t))
        });
    }
}

struct CompositorRef(Rc<Compositor>);

/// An id handed back by the interface: a string, or a number it read as one.
fn id_arg(v: LuaValue) -> mlua::Result<String> {
    match v {
        LuaValue::String(s) => Ok(s.to_string_lossy()),
        LuaValue::Integer(n) => Ok(n.to_string()),
        LuaValue::Number(n) if n.fract() == 0.0 => Ok((n as i64).to_string()),
        _ => Err(err("an id: a string or a whole number")),
    }
}

fn window_table(lua: &Lua, c: &Compositor, w: genoma_services::compositor::Window) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("output", c.output_of(&w))?;
    t.set("id", w.id)?;
    t.set("title", w.title)?;
    t.set("app_id", w.app_id)?;
    t.set("workspace", w.workspace)?;
    t.set("minimized", w.minimized)?;
    Ok(t)
}

impl UserData for CompositorRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // compositor.name → "niri", "hyprland", "umbriel", or "wayland" (the
        // standard protocols, where no IPC is known).
        fields.add_field_method_get("name", |_, this| Ok(this.0.name()));
        // compositor.can_minimize → whether :minimize_window does anything.
        fields.add_field_method_get("can_minimize", |_, this| Ok(this.0.can_minimize()));
        // compositor.title → the focused window's title, or nil.
        fields.add_field_method_get("title", |_, this| Ok(this.0.title.get()));
        // compositor.focused_output → "DP-1": where a shortcut means.
        fields.add_field_method_get("focused_output", |_, this| Ok(this.0.focused_output()));
        // compositor.focused → { id, title, app_id, workspace, output,
        // minimized } or nil.
        fields.add_field_method_get("focused", |lua, this| match this.0.focused_window() {
            Some(w) => Ok(LuaValue::Table(window_table(lua, &this.0, w)?)),
            None => Ok(LuaValue::Nil),
        });
        // compositor.windows → { { id, title, app_id, workspace, output,
        // minimized }, … }
        fields.add_field_method_get("windows", |lua, this| {
            let list = lua.create_table()?;
            for (i, w) in this.0.windows.get().into_iter().enumerate() {
                list.set(i + 1, window_table(lua, &this.0, w)?)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // compositor:workspaces("DP-1") → { { id, idx, name, output, active,
        // focused, vacant, urgent }, … } in order; every output's without one.
        methods.add_method("workspaces", |lua, this, output: Option<String>| {
            let list = lua.create_table()?;
            for (i, w) in this.0.on_output(output.as_deref()).into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", w.id)?;
                t.set("idx", w.idx)?;
                t.set("name", w.name)?;
                t.set("output", w.output)?;
                t.set("active", w.active)?;
                t.set("focused", w.focused)?;
                t.set("vacant", w.vacant)?;
                t.set("urgent", w.urgent)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // compositor:active("DP-1") → { id, idx, name } or nil
        methods.add_method("active", |lua, this, output: String| {
            let Some(w) = this.0.active(&output) else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("id", w.id)?;
            t.set("idx", w.idx)?;
            t.set("name", w.name)?;
            Ok(LuaValue::Table(t))
        });
        // compositor:focused_on("DP-1") → the focused window if it is there.
        methods.add_method("focused_on", |lua, this, output: String| match this.0.focused_on(&output) {
            Some(w) => Ok(LuaValue::Table(window_table(lua, &this.0, w)?)),
            None => Ok(LuaValue::Nil),
        });
        // compositor:focus_workspace(id), :focus_window(id), :close_window(id):
        // each on a connection of its own (through the protocols, sent on the
        // runtime's next turn).
        methods.add_method("focus_workspace", |_, this, id: LuaValue| this.0.focus_workspace(&id_arg(id)?).map_err(err));
        methods.add_method("focus_window", |_, this, id: LuaValue| this.0.focus_window(&id_arg(id)?).map_err(err));
        methods.add_method("close_window", |_, this, id: LuaValue| this.0.close_window(&id_arg(id)?).map_err(err));
        // compositor:minimize_window(id, minimized = true): only where
        // compositor.can_minimize; elsewhere an error.
        methods.add_method("minimize_window", |_, this, (id, minimized): (LuaValue, Option<bool>)| {
            this.0.minimize_window(&id_arg(id)?, minimized.unwrap_or(true)).map_err(err)
        });
    }
}

struct VitalsRef(Rc<Vitals>);

impl UserData for VitalsRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("cpu", |_, this| Ok(this.0.cpu.get()));
        fields.add_field_method_get("ram", |_, this| Ok(this.0.ram.get()));
        fields.add_field_method_get("gpu", |_, this| Ok(this.0.gpu.get()));
        fields.add_field_method_get("gpu_clock", |_, this| Ok(this.0.gpu_clock.get()));
        fields.add_field_method_get("cpu_clock", |_, this| Ok(this.0.cpu_clock.get()));
    }
}

struct PowerRef(Rc<Power>);

impl UserData for PowerRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // power.battery → { present, laptop, level 0…1, state, time_to_empty,
        // time_to_full }, the display device as UPower sums it.
        fields.add_field_method_get("battery", |lua, this| {
            let b = this.0.battery.get();
            let t = lua.create_table()?;
            t.set("present", b.present)?;
            t.set("laptop", b.laptop)?;
            t.set("level", b.level)?;
            t.set("state", b.state)?;
            t.set("time_to_empty", b.time_to_empty)?;
            t.set("time_to_full", b.time_to_full)?;
            t.set("energy_rate", b.energy_rate)?;
            Ok(t)
        });
        fields.add_field_method_get("on_battery", |_, this| Ok(this.0.on_battery.get()));
        fields.add_field_method_get("profile", |_, this| Ok(this.0.profile.get()));
        fields.add_field_method_get("profiles", |_, this| Ok(this.0.profiles.get()));
        // power.degraded: why performance is held back, or "".
        fields.add_field_method_get("degraded", |_, this| Ok(this.0.degraded.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // power:set_profile("balanced"): the daemon's answer comes back as
        // power.profile.
        methods.add_method("set_profile", |_, this, profile: String| {
            this.0.set_profile(profile);
            Ok(())
        });
    }
}

struct NetworkRef(Rc<Network>);

impl UserData for NetworkRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // "unknown", "none", "portal", "limited", "full"
        fields.add_field_method_get("connectivity", |_, this| Ok(this.0.connectivity.get()));
        fields.add_field_method_get("can_check", |_, this| Ok(this.0.can_check.get()));
        fields.add_field_method_get("wifi_enabled", |_, this| Ok(this.0.wifi_enabled.get()));
        fields.add_field_method_get("wifi_hardware", |_, this| Ok(this.0.wifi_hardware.get()));
        fields.add_field_method_get("failure", |_, this| Ok(this.0.failure.get()));
        // network.devices → { { interface, kind, state, connected, carrier,
        // speed, connection }, … }, the connected ones first.
        fields.add_field_method_get("devices", |lua, this| {
            let list = lua.create_table()?;
            for (i, d) in this.0.devices.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("interface", d.interface)?;
                t.set("kind", d.kind)?;
                t.set("state", d.state)?;
                t.set("connected", d.connected)?;
                t.set("carrier", d.carrier)?;
                t.set("speed", d.speed)?;
                t.set("connection", d.connection)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // network.networks → { { ssid, strength 0…1, secured, connected,
        // frequency, known }, … }: the connected one first, then by strength.
        fields.add_field_method_get("networks", |lua, this| {
            let list = lua.create_table()?;
            for (i, n) in this.0.networks.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("ssid", n.ssid)?;
                t.set("strength", n.strength)?;
                t.set("secured", n.secured)?;
                t.set("connected", n.connected)?;
                t.set("frequency", n.frequency)?;
                t.set("known", n.known)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("set_wifi_enabled", |_, this, on: bool| {
            this.0.set_wifi_enabled(on);
            Ok(())
        });
        // network:set_scanning(true) while a list is on screen, false after.
        methods.add_method("set_scanning", |_, this, on: bool| {
            this.0.set_scanning(on);
            Ok(())
        });
        methods.add_method("check_connectivity", |_, this, ()| {
            this.0.check_connectivity();
            Ok(())
        });
        // network:join("ssid") through its saved profile, or
        // network:join("ssid", "password") with a new one.
        methods.add_method("join", |_, this, (ssid, password): (String, Option<String>)| {
            this.0.join(ssid, password);
            Ok(())
        });
        methods.add_method("leave", |_, this, ()| {
            this.0.leave();
            Ok(())
        });
        methods.add_method("forget", |_, this, ssid: String| {
            this.0.forget(ssid);
            Ok(())
        });
    }
}

struct MediaRef(Rc<Media>);

impl UserData for MediaRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // media.players → { { name, identity, desktop_entry, status, title,
        // artist, album, album_artist, art_url, length, track_id, rate,
        // can_play, can_pause, can_go_next, can_go_previous, can_seek,
        // can_control, position_supported }, … }, in the order they came.
        fields.add_field_method_get("players", |lua, this| {
            let list = lua.create_table()?;
            for (i, p) in this.0.players.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("name", p.name)?;
                t.set("identity", p.identity)?;
                t.set("desktop_entry", p.desktop_entry)?;
                t.set("status", p.status)?;
                t.set("title", p.title)?;
                t.set("artist", p.artist)?;
                t.set("album", p.album)?;
                t.set("album_artist", p.album_artist)?;
                t.set("art_url", p.art_url)?;
                t.set("length", p.length)?;
                t.set("track_id", p.track_id)?;
                t.set("rate", p.rate)?;
                t.set("can_play", p.can_play)?;
                t.set("can_pause", p.can_pause)?;
                t.set("can_go_next", p.can_go_next)?;
                t.set("can_go_previous", p.can_go_previous)?;
                t.set("can_seek", p.can_seek)?;
                t.set("can_control", p.can_control)?;
                t.set("position_supported", p.position_supported)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // media:position(name) → seconds now: the last reading carried forward
        // at the rate. It is not a signal: what draws progress asks it on its
        // own beat.
        methods.add_method("position", |_, this, name: String| {
            let players = this.0.players.get_untracked().unwrap_or_default();
            Ok(players.iter().find(|p| p.name == name).map(|p| p.position_now()))
        });
        for (method, action) in [
            ("play_pause", 0u8),
            ("play", 1),
            ("pause", 2),
            ("stop", 3),
            ("next", 4),
            ("previous", 5),
        ] {
            methods.add_method(method, move |_, this, name: String| {
                let action = match action {
                    0 => Action::PlayPause,
                    1 => Action::Play,
                    2 => Action::Pause,
                    3 => Action::Stop,
                    4 => Action::Next,
                    _ => Action::Previous,
                };
                this.0.act(name, action);
                Ok(())
            });
        }
        // media:seek(name, seconds) forward or back; media:set_position(name,
        // seconds) from the start.
        methods.add_method("seek", |_, this, (name, by): (String, f64)| {
            this.0.act(name, Action::Seek(by));
            Ok(())
        });
        methods.add_method("set_position", |_, this, (name, at): (String, f64)| {
            this.0.act(name, Action::SetPosition(at));
            Ok(())
        });
    }
}

struct AudioRef(Rc<Audio>);

impl UserData for AudioRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // audio.nodes → { { id, name, description, nickname, media_class,
        // kind, properties = { … }, has_volume, volume, channels, muted }, … }
        fields.add_field_method_get("nodes", |lua, this| {
            let list = lua.create_table()?;
            for (i, n) in this.0.nodes.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", n.id)?;
                t.set("name", n.name)?;
                t.set("description", n.description)?;
                t.set("nickname", n.nickname)?;
                t.set("media_class", n.media_class)?;
                t.set("kind", n.kind)?;
                let props = lua.create_table()?;
                for (k, v) in n.properties {
                    props.set(k, v)?;
                }
                t.set("properties", props)?;
                t.set("has_volume", n.has_volume)?;
                t.set("volume", n.volume)?;
                t.set("channels", n.channels)?;
                t.set("muted", n.muted)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // audio.links → { { id, output, input, active }, … }: from the output
        // node to the input node, which is how Privacy tells who captures.
        fields.add_field_method_get("links", |lua, this| {
            let list = lua.create_table()?;
            for (i, l) in this.0.links.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", l.id)?;
                t.set("output", l.output)?;
                t.set("input", l.input)?;
                t.set("active", l.active)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // The defaults by `node.name`, as PipeWire keeps them.
        fields.add_field_method_get("default_sink", |_, this| Ok(this.0.default_sink.get()));
        fields.add_field_method_get("default_source", |_, this| Ok(this.0.default_source.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("set_volume", |_, this, (id, volume): (u32, f64)| {
            this.0.set_volume(id, volume);
            Ok(())
        });
        methods.add_method("set_mute", |_, this, (id, muted): (u32, bool)| {
            this.0.set_mute(id, muted);
            Ok(())
        });
        // audio:set_default(id): a sink or a source, by its kind.
        methods.add_method("set_default", |_, this, id: u32| {
            this.0.set_default(id);
            Ok(())
        });
    }
}

struct BluetoothRef(Rc<Bluetooth>);

impl UserData for BluetoothRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // bluetooth.adapter → { name, address, powered, discovering,
        // discoverable, pairable }, or nil without one.
        fields.add_field_method_get("adapter", |lua, this| {
            let Some(a) = this.0.adapter.get() else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("name", a.name)?;
            t.set("address", a.address)?;
            t.set("powered", a.powered)?;
            t.set("discovering", a.discovering)?;
            t.set("discoverable", a.discoverable)?;
            t.set("pairable", a.pairable)?;
            Ok(LuaValue::Table(t))
        });
        // bluetooth.devices → { { address, name, device_name, icon, paired,
        // trusted, connected, blocked, battery (0…1 or nil), busy }, … }
        fields.add_field_method_get("devices", |lua, this| {
            let list = lua.create_table()?;
            for (i, d) in this.0.devices.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("address", d.address)?;
                t.set("name", d.name)?;
                t.set("device_name", d.device_name)?;
                t.set("icon", d.icon)?;
                t.set("paired", d.paired)?;
                t.set("trusted", d.trusted)?;
                t.set("connected", d.connected)?;
                t.set("blocked", d.blocked)?;
                t.set("battery", d.battery)?;
                t.set("busy", d.busy)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // bluetooth.failure → { address, error } for the last failed call.
        fields.add_field_method_get("failure", |lua, this| {
            let Some((address, error)) = this.0.failure.get() else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("address", address)?;
            t.set("error", error)?;
            Ok(LuaValue::Table(t))
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("set_powered", |_, this, on: bool| {
            this.0.set_powered(on);
            Ok(())
        });
        // bluetooth:set_discovering(true) while a list is open, false after.
        methods.add_method("set_discovering", |_, this, on: bool| {
            this.0.set_discovering(on);
            Ok(())
        });
        methods.add_method("forget", |_, this, address: String| {
            this.0.forget(&address);
            Ok(())
        });
        methods.add_method("set_trusted", |_, this, (address, on): (String, bool)| {
            this.0.on_device(&address, DeviceCall::Trusted(on));
            Ok(())
        });
        for (method, call) in [("connect", 0u8), ("disconnect", 1), ("pair", 2), ("cancel_pair", 3)] {
            methods.add_method(method, move |_, this, address: String| {
                let call = match call {
                    0 => DeviceCall::Connect,
                    1 => DeviceCall::Disconnect,
                    2 => DeviceCall::Pair,
                    _ => DeviceCall::CancelPair,
                };
                this.0.on_device(&address, call);
                Ok(())
            });
        }
    }
}

struct IdleRef(Rc<Idle>);

impl UserData for IdleRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers ext-idle-notify.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // No input for the seconds `after` asked, and nothing inhibiting idle.
        fields.add_field_method_get("idle", |_, this| Ok(this.0.is_idle(genoma_services::idle::DEFAULT)));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // idle:after(seconds): idle is declared after that long without
        // input; 0 stops asking.
        methods.add_method("after", |_, this, seconds: f64| {
            this.0.set_timeout(genoma_services::idle::DEFAULT, seconds.max(0.0).round() as u32);
            Ok(())
        });
        // idle:watch(name, seconds): a timeout of its own, beside the others;
        // 0 stops asking. idle:is_idle(name) says whether it has passed.
        methods.add_method("watch", |_, this, (name, seconds): (String, f64)| {
            this.0.set_timeout(&name, seconds.max(0.0).round() as u32);
            Ok(())
        });
        methods.add_method("is_idle", |_, this, name: String| Ok(this.0.is_idle(&name)));
    }
}

struct LockRef(Rc<Lock>);

impl UserData for LockRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers ext-session-lock.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // Every output is covered, as the compositor says.
        fields.add_field_method_get("locked", |_, this| Ok(this.0.locked.get()));
        // The compositor refused the lock, or ended it on its own.
        fields.add_field_method_get("finished", |_, this| Ok(this.0.finished.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // lock:lock(): the compositor is asked to lock the session, and the
        // interface's "lock" surfaces are made, one per output. Once.
        methods.add_method("lock", |_, this, ()| {
            this.0.wanted.set(true);
            Ok(())
        });
        // lock:unlock(): the session opens again and the lock surfaces go.
        // Only the interface can say it; a process that dies leaves the
        // compositor holding the outputs locked.
        methods.add_method("unlock", |_, this, ()| {
            this.0.wanted.set(false);
            Ok(())
        });
    }
}

struct ClipboardRef(Rc<Clipboard>);

impl UserData for ClipboardRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers ext-data-control.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // clipboard.types → { "text/plain;charset=utf-8", "image/png", … }:
        // what the current copy is offered as; empty with nothing copied.
        fields.add_field_method_get("types", |_, this| Ok(this.0.types.get()));
        // clipboard.history → { { text, at }, … }: the texts copied, newest
        // first, each once; in memory only, a copy a password manager marks
        // secret left out.
        fields.add_field_method_get("history", |lua, this| {
            let list = lua.create_table()?;
            for (i, c) in this.0.history.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("text", c.text)?;
                t.set("at", c.at)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        // The current copy as text, when it is text and up to 1 MiB; "".
        fields.add_field_method_get("text", |_, this| Ok(this.0.text.get()));
        // Moves on every copy, the same text copied twice included.
        fields.add_field_method_get("changed", |_, this| Ok(this.0.changed.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // clipboard:set_text("…"): the shell owns the clipboard, the text
        // offered as every text type programs look for. It stays after the
        // program that copied before is gone.
        methods.add_method("set_text", |_, this, text: String| {
            let types = TEXT_TYPES.iter().map(|t| t.to_string()).collect();
            this.0.ask(ClipboardRequest::Set { types, data: text.into_bytes() });
            Ok(())
        });
        // clipboard:set("image/png", "~/…/shot.png"): a file's content on the
        // clipboard, offered as the type (or list of types) given. The file
        // is read now; changing it afterwards changes nothing.
        methods.add_method("set", |_, this, (types, path): (LuaValue, String)| {
            let types: Vec<String> = match types {
                LuaValue::String(s) => vec![s.to_string_lossy()],
                LuaValue::Table(t) => t.sequence_values::<String>().collect::<mlua::Result<_>>()?,
                _ => return Err(err("clipboard:set: a type or a list of types, then a path")),
            };
            let data = std::fs::read(crate::io::expand(&path)).map_err(|e| err(&format!("clipboard:set: {path}: {e}")))?;
            this.0.ask(ClipboardRequest::Set { types, data });
            Ok(())
        });
        // clipboard:clear(): nothing on the clipboard.
        methods.add_method("clear", |_, this, ()| {
            this.0.ask(ClipboardRequest::Clear);
            Ok(())
        });
        // clipboard:save("image/png", "~/…/1.png") → true when the current
        // copy is offered in that type: the program that copied writes it
        // into the file, which is complete once that program is done.
        // clipboard:forget(i): the history's entry i (from 1) taken out.
        methods.add_method("forget", |_, this, index: usize| {
            this.0.forget(index.saturating_sub(1));
            Ok(())
        });
        methods.add_method("clear_history", |_, this, ()| {
            this.0.clear_history();
            Ok(())
        });
        // clipboard:keep(n): how many texts the history keeps (50).
        methods.add_method("keep", |_, this, n: usize| {
            this.0.history_limit.set(n.max(1));
            Ok(())
        });
        methods.add_method("save", |_, this, (mime, path): (String, String)| {
            if !this.0.types.get_untracked().unwrap_or_default().contains(&mime) {
                return Ok(false);
            }
            this.0.ask(ClipboardRequest::Save { mime, path: crate::io::expand(&path) });
            Ok(true)
        });
    }
}

struct AppsRef(Rc<Apps>);

/// One application as Luau sees it.
fn app_table(lua: &Lua, a: AppEntry) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("id", a.id)?;
    t.set("name", a.name)?;
    t.set("generic_name", a.generic_name)?;
    t.set("comment", a.comment)?;
    t.set("icon", a.icon)?;
    t.set("exec", a.exec)?;
    t.set("terminal", a.terminal)?;
    t.set("startup_class", a.startup_class)?;
    t.set("keywords", a.keywords)?;
    t.set("categories", a.categories)?;
    t.set("file", a.file.display().to_string())?;
    let actions = lua.create_table()?;
    for (i, x) in a.actions.into_iter().enumerate() {
        let at = lua.create_table()?;
        at.set("id", x.id)?;
        at.set("name", x.name)?;
        at.set("icon", x.icon)?;
        actions.set(i + 1, at)?;
    }
    t.set("actions", actions)?;
    Ok(t)
}

impl UserData for AppsRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // apps.list → { app, … } by name: what the menu offers.
        fields.add_field_method_get("list", |lua, this| {
            let list = lua.create_table()?;
            for (i, a) in this.0.all().into_iter().enumerate() {
                list.set(i + 1, app_table(lua, a)?)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // apps:search("fire", 20) → the best matches first; with nothing
        // typed, the most used first. A binding that calls it follows the list.
        methods.add_method("search", |lua, this, (query, limit): (String, Option<usize>)| {
            let list = lua.create_table()?;
            for (i, a) in this.0.search(&query, limit.unwrap_or(0)).into_iter().enumerate() {
                list.set(i + 1, app_table(lua, a)?)?;
            }
            Ok(list)
        });
        // apps:find("firefox") → the application a window's app_id or class,
        // or a process's name, belongs to, or nil.
        methods.add_method("find", |lua, this, name: String| match this.0.find(&name) {
            Some(a) => Ok(LuaValue::Table(app_table(lua, a)?)),
            None => Ok(LuaValue::Nil),
        });
        // apps:launch(id, action, { files… }): started out of the shell's
        // session, and counted for the ranking.
        // apps:launch(id, action?, files?, token?): the token from
        // g.activation_token gives its window the focus.
        methods.add_method(
            "launch",
            |_, this, (id, action, files, token): (String, Option<String>, Option<Vec<String>>, Option<String>)| {
                this.0.launch(&id, action.as_deref(), &files.unwrap_or_default(), token.as_deref()).map_err(err)
            },
        );
        // apps:usage(id) → { count, last }: how often and when last started.
        methods.add_method("usage", |lua, this, id: String| {
            let u = this.0.usage(&id);
            let t = lua.create_table()?;
            t.set("count", u.count)?;
            t.set("last", u.last)?;
            Ok(t)
        });
        // apps:refresh(): read again, after an installation.
        methods.add_method("refresh", |_, this, ()| {
            this.0.refresh();
            Ok(())
        });
    }
}

struct SpectrumRef(Rc<Spectrum>);

/// A hold on the spectrum: `:release()` lets it go.
struct SpectrumHold(Rc<Spectrum>, std::cell::Cell<bool>);

impl UserData for SpectrumHold {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("release", |_, this, ()| {
            if this.1.replace(false) {
                this.0.release();
            }
            Ok(())
        });
    }
}

impl UserData for SpectrumRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // spectrum.left / .right → { 0…1, … } low to high, 64 a channel;
        // empty while nobody holds it.
        fields.add_field_method_get("left", |_, this| Ok(this.0.left.get()));
        fields.add_field_method_get("right", |_, this| Ok(this.0.right.get()));
        // The loudest band of either, 0…1.
        fields.add_field_method_get("level", |_, this| Ok(this.0.level.get()));
        // Whether the capture runs.
        fields.add_field_method_get("running", |_, this| Ok(this.0.wanted.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // spectrum:hold() → a hold: the capture runs while any is held.
        methods.add_method("hold", |_, this, ()| {
            this.0.hold();
            Ok(SpectrumHold(this.0.clone(), std::cell::Cell::new(true)))
        });
        // spectrum:listen_to("input" | "output"): the microphone or what
        // leaves the machine, from the next start.
        methods.add_method("listen_to", |_, this, source: String| {
            this.0.input.set(source == "input");
            Ok(())
        });
    }
}

struct SessionRef(Rc<Session>);

/// An inhibitor held: `:release()` lets it go.
struct Inhibition(Rc<Session>, u64);

impl UserData for Inhibition {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether logind has given it and it is not let go.
        fields.add_field_method_get("held", |_, this| Ok(this.0.holds(this.1)));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("release", |_, this, ()| {
            this.0.release(this.1);
            Ok(())
        });
    }
}

impl UserData for SessionRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether logind answers.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // session.can → { suspend, hibernate, hybrid_sleep, reboot, power_off },
        // each "yes", "no", "challenge" (a password, through polkit) or "na".
        fields.add_field_method_get("can", |lua, this| {
            let c = this.0.can.get();
            let t = lua.create_table()?;
            t.set("suspend", c.suspend)?;
            t.set("hibernate", c.hibernate)?;
            t.set("hybrid_sleep", c.hybrid_sleep)?;
            t.set("reboot", c.reboot)?;
            t.set("power_off", c.power_off)?;
            Ok(t)
        });
        // True from logind's PrepareForSleep(true) to (false): lock now.
        fields.add_field_method_get("preparing_for_sleep", |_, this| Ok(this.0.preparing_for_sleep.get()));
        fields.add_field_method_get("preparing_for_shutdown", |_, this| Ok(this.0.preparing_for_shutdown.get()));
        // Moves on each request to lock (or unlock) this session.
        fields.add_field_method_get("lock_requests", |_, this| Ok(this.0.lock_requests.get()));
        fields.add_field_method_get("unlock_requests", |_, this| Ok(this.0.unlock_requests.get()));
        // session.inhibitors → { { what, who, why, mode, uid, pid }, … }
        fields.add_field_method_get("inhibitors", |lua, this| {
            let list = lua.create_table()?;
            for (i, x) in this.0.inhibitors.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("what", x.what)?;
                t.set("who", x.who)?;
                t.set("why", x.why)?;
                t.set("mode", x.mode)?;
                t.set("uid", x.uid)?;
                t.set("pid", x.pid)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        for (name, action) in [
            ("suspend", SessionAction::Suspend),
            ("hibernate", SessionAction::Hibernate),
            ("hybrid_sleep", SessionAction::HybridSleep),
            ("reboot", SessionAction::Reboot),
            ("power_off", SessionAction::PowerOff),
        ] {
            methods.add_method(name, move |_, this, ()| {
                this.0.act(action);
                Ok(())
            });
        }
        // session:lock_session(): logind asked to lock this session; every
        // listener (a lock screen, lock_requests) hears it.
        methods.add_method("lock_session", |_, this, ()| {
            this.0.lock_session();
            Ok(())
        });
        // session:inhibit("sleep", "Locking first", "delay") → an inhibitor,
        // held until :release().
        methods.add_method("inhibit", |_, this, (what, why, mode): (String, String, Option<String>)| {
            let who = std::env::args().next().and_then(|a| std::path::Path::new(&a).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "genoma".into());
            let handle = this.0.inhibit(what, why, mode.unwrap_or_else(|| "block".into()), who);
            Ok(Inhibition(this.0.clone(), handle))
        });
    }
}

struct ToplevelsRef(Rc<Toplevels>);

impl UserData for ToplevelsRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers ext-foreign-toplevel-list.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // toplevels.list → { { identifier, title, app_id }, … }
        fields.add_field_method_get("list", |lua, this| {
            let list = lua.create_table()?;
            for (i, w) in this.0.list.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("identifier", w.identifier)?;
                t.set("title", w.title)?;
                t.set("app_id", w.app_id)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
    }
}

struct DisplaysRef(Rc<Displays>);

fn mode_table(lua: &Lua, m: &genoma_services::displays::Mode) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("width", m.width)?;
    t.set("height", m.height)?;
    t.set("refresh", m.refresh)?;
    t.set("preferred", m.preferred)?;
    Ok(t)
}

fn head_table(lua: &Lua, h: Head) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("name", h.name)?;
    t.set("description", h.description)?;
    t.set("make", h.make)?;
    t.set("model", h.model)?;
    t.set("serial", h.serial)?;
    t.set("width_mm", h.width_mm)?;
    t.set("height_mm", h.height_mm)?;
    t.set("enabled", h.enabled)?;
    let modes = lua.create_table()?;
    for (i, m) in h.modes.iter().enumerate() {
        modes.set(i + 1, mode_table(lua, m)?)?;
    }
    t.set("modes", modes)?;
    if let Some(m) = &h.mode {
        t.set("mode", mode_table(lua, m)?)?;
    }
    t.set("x", h.x)?;
    t.set("y", h.y)?;
    t.set("scale", h.scale)?;
    t.set("transform", h.transform)?;
    t.set("adaptive_sync", h.adaptive_sync)?;
    Ok(t)
}

/// `{ ["DP-1"] = { enabled, mode = { width, height, refresh }, position =
/// { x, y }, scale, transform, adaptive_sync } }` as changes.
fn changes_arg(t: Table) -> mlua::Result<Vec<Change>> {
    let mut out = Vec::new();
    for pair in t.pairs::<String, Table>() {
        let (name, c) = pair?;
        let mode = match c.get::<Option<Table>>("mode")? {
            Some(m) => Some((
                m.get::<Option<i32>>("width")?.map_or_else(|| m.get(1), Ok)?,
                m.get::<Option<i32>>("height")?.map_or_else(|| m.get(2), Ok)?,
                m.get::<Option<f64>>("refresh")?.or(m.get::<Option<f64>>(3)?),
            )),
            None => None,
        };
        let position = match c.get::<Option<Table>>("position")? {
            Some(p) => Some((p.get::<Option<i32>>("x")?.map_or_else(|| p.get(1), Ok)?, p.get::<Option<i32>>("y")?.map_or_else(|| p.get(2), Ok)?)),
            None => None,
        };
        out.push(Change {
            name,
            enabled: c.get("enabled")?,
            mode,
            position,
            scale: c.get("scale")?,
            transform: c.get("transform")?,
            adaptive_sync: c.get("adaptive_sync")?,
        });
    }
    Ok(out)
}

impl UserData for DisplaysRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers wlr-output-management.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // displays.heads → { { name, description, make, model, serial,
        // width_mm, height_mm, enabled, modes, mode, x, y, scale, transform,
        // adaptive_sync }, … }
        fields.add_field_method_get("heads", |lua, this| {
            let list = lua.create_table()?;
            for (i, h) in this.0.heads.get().into_iter().enumerate() {
                list.set(i + 1, head_table(lua, h)?)?;
            }
            Ok(list)
        });
        // displays.result → { id, test, status } for the latest
        // configuration, or nil.
        fields.add_field_method_get("result", |lua, this| match this.0.result.get() {
            Some(o) => {
                let t = lua.create_table()?;
                t.set("id", o.id)?;
                t.set("test", o.test)?;
                t.set("status", o.status)?;
                Ok(LuaValue::Table(t))
            }
            None => Ok(LuaValue::Nil),
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // displays:apply(changes) → id: the heads not named keep their setup;
        // the answer comes as displays.result with this id.
        methods.add_method("apply", |_, this, changes: Table| this.0.configure(changes_arg(changes)?, false).map_err(err));
        // displays:test(changes) → id: only asks whether it would work.
        methods.add_method("test", |_, this, changes: Table| this.0.configure(changes_arg(changes)?, true).map_err(err));
    }
}

struct GammaRef(Rc<Gamma>);

fn setting_table(lua: &Lua, s: Setting) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("temperature", s.temperature)?;
    t.set("brightness", s.brightness)?;
    t.set("gamma", s.gamma)?;
    Ok(t)
}

impl UserData for GammaRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether the compositor offers wlr-gamma-control.
        fields.add_field_method_get("available", |_, this| Ok(this.0.available.get()));
        // gamma.held → { ["DP-1"] = { temperature, brightness, gamma } or
        // "failed" }: the displays whose ramp is ours, or held by another
        // program. A display at its own ramp is not listed.
        fields.add_field_method_get("held", |lua, this| {
            let t = lua.create_table()?;
            for (name, held) in this.0.held.get() {
                match held {
                    Held::Set(s) => t.set(name, setting_table(lua, s)?)?,
                    Held::Failed => t.set(name, "failed")?,
                }
            }
            Ok(t)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // gamma:get("DP-1") → { temperature, brightness, gamma }: what that
        // display is asked to show (every display's without one).
        methods.add_method("get", |lua, this, output: Option<String>| {
            let _ = this.0.settings.get();
            setting_table(lua, this.0.wanted(output.as_deref().unwrap_or("*")))
        });
        // gamma:set({ temperature = 4000, brightness = 0.9, gamma = 1 },
        // "DP-1"): what is left out stays as it was; without an output, every
        // display without its own setting.
        methods.add_method("set", |_, this, (t, output): (Table, Option<String>)| {
            let now = this.0.wanted(output.as_deref().unwrap_or("*"));
            let setting = Setting {
                temperature: t.get::<Option<f64>>("temperature")?.unwrap_or(now.temperature),
                brightness: t.get::<Option<f64>>("brightness")?.unwrap_or(now.brightness),
                gamma: t.get::<Option<f64>>("gamma")?.unwrap_or(now.gamma),
            };
            this.0.set(output.as_deref(), setting);
            Ok(())
        });
        // gamma:reset("DP-1"): that display back to every display's setting;
        // gamma:reset(): every display back to its own ramp.
        methods.add_method("reset", |_, this, output: Option<String>| {
            this.0.reset(output.as_deref());
            Ok(())
        });
    }
}

struct TrayRef(Rc<Tray>);

fn menu_table(lua: &Lua, e: &MenuEntry) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("id", e.id)?;
    t.set("label", e.label.as_str())?;
    t.set("enabled", e.enabled)?;
    t.set("visible", e.visible)?;
    t.set("separator", e.separator)?;
    t.set("toggle_type", e.toggle_type.as_str())?;
    t.set("toggle_state", e.toggle_state)?;
    t.set("icon_name", e.icon_name.as_str())?;
    let children = lua.create_table()?;
    for (i, c) in e.children.iter().enumerate() {
        children.set(i + 1, menu_table(lua, c)?)?;
    }
    t.set("children", children)?;
    Ok(t)
}

impl UserData for TrayRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // tray.items → { { key, id, title, status, category, icon_name,
        // icon_theme_path, attention_icon_name, has_pixmap, pixmap, tooltip_title,
        // tooltip_description, only_menu, has_menu }, … }
        fields.add_field_method_get("items", |lua, this| {
            let list = lua.create_table()?;
            for (i, it) in this.0.items.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("key", it.key)?;
                t.set("id", it.id)?;
                t.set("title", it.title)?;
                t.set("status", it.status)?;
                t.set("category", it.category)?;
                t.set("icon_name", it.icon_name)?;
                t.set("icon_theme_path", it.icon_theme_path)?;
                t.set("attention_icon_name", it.attention_icon_name)?;
                t.set("has_pixmap", it.has_pixmap)?;
                t.set("pixmap", it.pixmap)?;
                t.set("tooltip_title", it.tooltip_title)?;
                t.set("tooltip_description", it.tooltip_description)?;
                t.set("only_menu", it.only_menu)?;
                t.set("has_menu", !it.menu.is_empty())?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
        fields.add_field_method_get("serving_watcher", |_, this| Ok(this.0.serving_watcher.get()));
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // tray:menu(key) → the opened menu's tree, { id, label, …, children },
        // or nil before tray:open_menu(key) has read it.
        methods.add_method("menu", |lua, this, key: String| {
            match this.0.menus.get().get(&key) {
                Some(tree) => Ok(LuaValue::Table(menu_table(lua, tree)?)),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("open_menu", |_, this, key: String| {
            this.0.open_menu(&key);
            Ok(())
        });
        methods.add_method("close_menu", |_, this, key: String| {
            this.0.close_menu(&key);
            Ok(())
        });
        methods.add_method("menu_click", |_, this, (key, id): (String, i32)| {
            this.0.menu_click(&key, id);
            Ok(())
        });
        for (method, call) in [("activate", "Activate"), ("secondary_activate", "SecondaryActivate"), ("context_menu", "ContextMenu")] {
            methods.add_method(method, move |_, this, (key, x, y): (String, Option<i32>, Option<i32>)| {
                this.0.act(&key, call, x.unwrap_or(0), y.unwrap_or(0));
                Ok(())
            });
        }
        methods.add_method("scroll", |_, this, (key, delta, orientation): (String, i32, Option<String>)| {
            this.0.scroll(&key, delta, orientation.unwrap_or_else(|| "vertical".into()));
            Ok(())
        });
    }
}

struct PolkitRef(Rc<Polkit>);

impl UserData for PolkitRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("registered", |_, this| Ok(this.0.registered.get()));
        // Why it is not registered: another agent holds the session.
        fields.add_field_method_get("error", |_, this| Ok(this.0.error.get()));
        // polkit.request → { cookie, action_id, message, icon_name, user,
        // prompt, echo, failed, info }, or nil when nothing is asked.
        fields.add_field_method_get("request", |lua, this| {
            let Some(r) = this.0.request.get() else { return Ok(LuaValue::Nil) };
            let t = lua.create_table()?;
            t.set("cookie", r.cookie)?;
            t.set("action_id", r.action_id)?;
            t.set("message", r.message)?;
            t.set("icon_name", r.icon_name)?;
            t.set("user", r.user)?;
            t.set("prompt", r.prompt)?;
            t.set("echo", r.echo)?;
            t.set("failed", r.failed)?;
            t.set("info", r.info)?;
            Ok(LuaValue::Table(t))
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // polkit:retry(): registers again when not registered, after another
        // agent has let the session go.
        methods.add_method("retry", |_, this, ()| {
            this.0.retry();
            Ok(())
        });
        // polkit:respond(cookie, password); polkit:cancel(cookie).
        methods.add_method("respond", |_, this, (cookie, answer): (String, String)| {
            this.0.respond(&cookie, answer);
            Ok(())
        });
        methods.add_method("cancel", |_, this, cookie: String| {
            this.0.cancel(&cookie);
            Ok(())
        });
    }
}

struct NotificationsRef(Rc<Notifications>);

impl UserData for NotificationsRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether Genoma owns the session's notification name.
        fields.add_field_method_get("serving", |_, this| Ok(this.0.serving.get()));
        fields.add_field_method_get("error", |_, this| Ok(this.0.error.get()));
        // notifications.list → { { id, app_name, app_icon, summary, body,
        // actions = { { key, label }, … }, urgency (0 low, 1 normal, 2
        // critical), expire_timeout, desktop_entry, category, image_path,
        // has_image, resident, transient, value, sound_name, sound_file,
        // suppress_sound, replaced }, … }, in arrival order.
        fields.add_field_method_get("list", |lua, this| {
            let list = lua.create_table()?;
            for (i, n) in this.0.list.get().into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", n.id)?;
                t.set("app_name", n.app_name)?;
                t.set("app_icon", n.app_icon)?;
                t.set("summary", n.summary)?;
                t.set("body", n.body)?;
                let actions = lua.create_table()?;
                for (j, (key, label)) in n.actions.into_iter().enumerate() {
                    let a = lua.create_table()?;
                    a.set("key", key)?;
                    a.set("label", label)?;
                    actions.set(j + 1, a)?;
                }
                t.set("actions", actions)?;
                t.set("urgency", n.urgency)?;
                t.set("expire_timeout", n.expire_timeout)?;
                t.set("desktop_entry", n.desktop_entry)?;
                t.set("category", n.category)?;
                t.set("image_path", n.image_path)?;
                t.set("has_image", n.image.is_some())?;
                t.set("resident", n.resident)?;
                t.set("transient", n.transient)?;
                t.set("value", n.value)?;
                t.set("sound_name", n.sound_name)?;
                t.set("sound_file", n.sound_file)?;
                t.set("suppress_sound", n.suppress_sound)?;
                t.set("replaced", n.replaced)?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        });
    }

    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        // notifications:close(id, "expired" | "dismissed")
        methods.add_method("close", |_, this, (id, reason): (u32, Option<String>)| {
            this.0.close(id, reason.as_deref().unwrap_or("dismissed"));
            Ok(())
        });
        // notifications:invoke(id, key): the action; then dismissed unless
        // the notification is resident.
        methods.add_method("invoke", |_, this, (id, key): (u32, String)| {
            this.0.invoke(id, key);
            Ok(())
        });
    }
}

struct ClockRef(Rc<Clock>);

impl UserData for ClockRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("hour", |_, this| Ok(this.0.hour.get()));
        fields.add_field_method_get("minute", |_, this| Ok(this.0.minute.get()));
        fields.add_field_method_get("second", |_, this| Ok(this.0.second.get()));
    }
}

pub struct Script {
    lua: Lua,
    /// A file evaluated and not yet mounted: its Luau state, the surface
    /// table it returned, and the scope of what its top-level code started.
    pending: Option<(Lua, Vec<Table>, Scope)>,
    dir: PathBuf,
    /// The file the interface starts from: `shell.luau`, or another entry
    /// point of the same interface (Bioma's greeter).
    entry: String,
    scope: Option<Scope>,
    /// What the mounted load's top-level code started (processes, files,
    /// sockets, timers): stopped when the next load is mounted.
    io_scope: Option<Scope>,
    host: io::HostRef,
    /// `g.dbus`'s callbacks and channel, made on first use and kept across
    /// loads.
    dbus: Rc<RefCell<Option<Rc<dbus::Dbus>>>>,
    states: Rc<RefCell<HashMap<String, Signal<StateValue>>>>,
    /// The load's own scope, which owns every module: a module is evaluated
    /// once per load and cached, so what it creates must live as long as the
    /// load, wherever it was first required from.
    module_owner: Rc<std::cell::Cell<Option<usize>>>,
    services: Services,
    wanted: Rc<RefCell<Vec<&'static str>>>,
    handlers: Handlers,
    icon_dirs: Rc<RefCell<Vec<PathBuf>>>,
    /// The scenes mounted last, in the order of the targets: a handler is
    /// found by (target, node).
    scenes: Vec<Rc<RefCell<Scene>>>,
    /// The outputs at the moment of the load: a load is evaluated again
    /// whenever they change.
    outputs: Rc<RefCell<Vec<OutputInfo>>>,
    /// What each target's surface asks for now, by target: whether it has
    /// anything to be (`present`) and the strip it keeps (`reserve`), when
    /// the description gave either as a function of the output; and whether
    /// that changed since the runtime last took it.
    wishes: Rc<RefCell<Vec<Wish>>>,
    wishes_changed: Rc<std::cell::Cell<bool>>,
    focus: FocusQueue,
    /// Modules to give in place of `@genoma/…` ones (`genoma snapshot
    /// --mock`): a test's fake service, by name, read from this path.
    mocks: Rc<RefCell<HashMap<String, PathBuf>>>,
    /// Activation tokens asked for and not yet given, and who waits for each.
    activation: ActivationQueue,
    activation_waiting: Rc<RefCell<HashMap<u64, Function>>>,
    activation_next: Rc<std::cell::Cell<u64>>,
    /// The mounted load's surface contents, each target's own scope, and
    /// the load's owner: one target can be built again alone.
    contents: Vec<LuaValue>,
    target_scopes: Vec<Option<Scope>>,
    load_owner: Option<usize>,
    /// By surface description: whether its content takes its size as it
    /// changes (`fluid = true`); and by target, that size, for those that do.
    fluid: Vec<bool>,
    /// By surface description: whether it is a window, whose content is
    /// stretched across the size the compositor gave it.
    windows: Vec<bool>,
    /// By surface description: a popup's `on_dismiss`, called when the
    /// compositor closed it (a press elsewhere).
    dismissers: Vec<Option<Function>>,
    sizes: Vec<Option<Signal<(f64, f64)>>>,
}

/// A surface's wish, as its description's functions answer it: `present`
/// false puts the surface to sleep (nothing drawn, nothing kept, nothing
/// taking the pointer; its tree stays mounted), and `reserve`, when the
/// function gave one, is the strip to keep windows out of (`None` for
/// none) in place of the description's.
#[derive(Clone, Debug, PartialEq)]
pub struct Wish {
    pub present: bool,
    pub reserve: Option<Option<i32>>,
    /// A popup's anchor, when its description gave a function for it: the
    /// id of the node it opens from now.
    pub anchor: Option<String>,
    /// A window's title, when its description gave a function for it.
    pub title: Option<String>,
}

impl Default for Wish {
    fn default() -> Self {
        Wish { present: true, reserve: None, anchor: None, title: None }
    }
}

/// One tree to mount: the output it is for, and the scene it goes into.
/// The output's logical size rides along: a surface spanning its output lays
/// out against its length (Bioma's membranes).
pub type Target = (usize, Option<String>, Rc<RefCell<Scene>>, Option<(u32, u32)>);

/// Where `g.ipc` keeps its targets, in each load's state.
const IPC: &str = "genoma.ipc";

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

impl Script {
    pub fn new(dir: &Path, services: Services, host: io::HostRef) -> Script {
        Script {
            lua: Lua::new(),
            pending: None,
            dir: dir.to_owned(),
            entry: "shell.luau".into(),
            scope: None,
            io_scope: None,
            host,
            dbus: Rc::default(),
            states: Rc::default(),
            module_owner: Rc::default(),
            services,
            wanted: Rc::default(),
            handlers: Rc::default(),
            icon_dirs: Rc::default(),
            scenes: Vec::new(),
            outputs: Rc::default(),
            wishes: Rc::default(),
            wishes_changed: Rc::default(),
            focus: (Rc::default(), Rc::default()),
            mocks: Rc::default(),
            activation: Rc::default(),
            activation_waiting: Rc::default(),
            activation_next: Rc::default(),
            contents: Vec::new(),
            target_scopes: Vec::new(),
            load_owner: None,
            fluid: Vec::new(),
            dismissers: Vec::new(),
            windows: Vec::new(),
            sizes: Vec::new(),
        }
    }

    /// Builds target `index`'s tree again for its area, alone: a surface the
    /// compositor gave another area (another surface took or let go of a
    /// zone) is laid out for what it has, and nothing else in the interface
    /// is touched. On an error its last tree stays.
    pub fn remount(&mut self, index: usize, target: &Target) -> Result<(), String> {
        let (spec, output, scene, size) = target;
        // A fluid surface is told its new size, and its tree follows it.
        if let (Some(Some(signal)), Some((w, h))) = (self.sizes.get(index), size) {
            signal.set((*w as f64, *h as f64));
            return Ok(());
        }
        let Some(content) = self.contents.get(*spec).cloned() else { return Err("a surface with no description".into()) };
        if let Some(old) = self.target_scopes.get_mut(index).and_then(Option::take) {
            old.dispose();
        }
        self.handlers.borrow_mut().retain(|(t, _), _| *t != index);
        let handlers = self.handlers.clone();
        let stretch = self.windows.get(*spec).copied().unwrap_or(false);
        let (scope, result) = owned_by(self.load_owner, || Scope::new(|| build_tree(&content, output, size, None, stretch, scene, &handlers, index)));
        match result {
            Ok(holder) => {
                let mut s = scene.borrow_mut();
                let root = s.root();
                s.clear();
                s.append(root, holder);
                s.set_icon_dirs(self.icon_dirs.borrow().clone());
                if let Some(slot) = self.target_scopes.get_mut(index) {
                    *slot = Some(scope);
                }
                Ok(())
            }
            Err(e) => {
                scope.dispose();
                Err(e.to_string())
            }
        }
    }

    /// Every target's wish, once something changed since the last call.
    /// The keyboard focus's requests and state, shared with the host.
    pub fn focus_queue(&self) -> FocusQueue {
        (self.focus.0.clone(), self.focus.1.clone())
    }

    /// `require(name)` gives the module at `path` instead, from the next
    /// load: a fake service for a test (`@genoma/vitals` with the numbers
    /// a cell is to be seen with).
    pub fn mock(&mut self, name: &str, path: PathBuf) {
        self.mocks.borrow_mut().insert(name.to_owned(), path);
    }

    /// Where `g.activation_token` leaves its requests for the host.
    pub fn activation_queue(&self) -> ActivationQueue {
        self.activation.clone()
    }

    /// The compositor's token for request `id`, or none when it gave none
    /// (no xdg-activation): told to whoever asked.
    pub fn activation_done(&self, id: u64, token: Option<String>) {
        let waiting = self.activation_waiting.borrow_mut().remove(&id);
        if let Some(f) = waiting
            && let Err(e) = f.call::<()>(token)
        {
            log::error!("g.activation_token on_token: {e}");
        }
    }

    pub fn take_wishes(&self) -> Option<Vec<Wish>> {
        self.wishes_changed.replace(false).then(|| self.wishes.borrow().clone())
    }

    /// What `g.outputs()` answers in the next load.
    pub fn set_outputs(&mut self, outputs: Vec<OutputInfo>) {
        *self.outputs.borrow_mut() = outputs;
    }

    /// Delivers pointer input to the node hit in target `at`'s tree, or to
    /// the nearest ancestor that handles it: a wheel over a row reaches the
    /// list.
    pub fn dispatch(&self, target: usize, node: genoma_scene::NodeId, input: Input) {
        let Some(scene) = self.scenes.get(target) else { return };
        let name = input.handler();
        let mut at = Some(node);
        while let Some(id) = at {
            let handler = self.handlers.borrow().get(&(target, id)).and_then(|h| h.get(name)).cloned();
            if let Some(f) = handler {
                let result = match &input {
                    Input::Outside | Input::Ready => f.call::<()>(()),
                    // on_key(name, text, { ctrl, shift, alt, logo, code })
                    Input::Key(key) => (|| {
                        let lua = f.weak_lua().upgrade();
                        let mods = lua.create_table()?;
                        mods.set("ctrl", key.ctrl)?;
                        mods.set("shift", key.shift)?;
                        mods.set("alt", key.alt)?;
                        mods.set("logo", key.logo)?;
                        mods.set("code", key.code)?;
                        f.call::<()>((key.name.as_str(), key.text.clone(), mods))
                    })(),
                    Input::Hover(entered) => f.call::<()>(*entered),
                    Input::Press(b, x, y) | Input::Release(b, x, y) | Input::Click(b, x, y) => f.call::<()>((*b, *x, *y)),
                    Input::Drag(x, y) => f.call::<()>((*x, *y)),
                    Input::Move(x, y) => {
                        let (lx, ly) = scene.borrow().local(id, *x, *y);
                        f.call::<()>((lx, ly))
                    }
                    Input::Wheel(dx, dy, steps) => f.call::<()>((*dx, *dy, *steps)),
                    Input::Fling(dx, dy) => f.call::<()>((*dx, *dy)),
                    Input::Size(w, h) => f.call::<()>((*w, *h)),
                    Input::Change(text) | Input::Submit(text) => f.call::<()>(text.as_str()),
                    Input::Focus(on) | Input::DropHover(on) => f.call::<()>(*on),
                    Input::Drop(d) => (|| {
                        let lua = f.weak_lua().upgrade();
                        let t = lua.create_table()?;
                        t.set("files", d.files.clone())?;
                        t.set("uris", d.uris.clone())?;
                        t.set("text", d.text.clone())?;
                        let (lx, ly) = scene.borrow().local(id, d.x, d.y);
                        t.set("x", lx)?;
                        t.set("y", ly)?;
                        f.call::<()>(t)
                    })(),
                    Input::FocusWithin(x, y, w, h) => {
                        let (lx, ly) = scene.borrow().local(id, *x, *y);
                        f.call::<()>((lx, ly, *w, *h))
                    }
                };
                if let Err(e) = result {
                    log::error!("{name}: {e}");
                }
                return;
            }
            // Hover stays with its own node: the host tells each node the
            // pointer enters or leaves, the ancestors of the one hit
            // included, so it never climbs to a parent here.
            if matches!(
                input,
                Input::Hover(_) | Input::Ready | Input::Size(..) | Input::Focus(_) | Input::Change(_) | Input::Submit(_) | Input::Drop(_) | Input::DropHover(_)
            ) {
                return;
            }
            at = scene.borrow().parent(id);
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A call from outside (`genoma ipc`): the function `g.ipc` registered
    /// under `target`, with the words as strings. Its value, as text, is
    /// the reply.
    pub fn ipc(&self, target: &str, function: &str, args: &[String]) -> Result<String, String> {
        let targets: Table = self.lua.named_registry_value(IPC).map_err(|_| "nothing answers yet".to_owned())?;
        let Some(functions) = targets.get::<Option<Table>>(target).map_err(|e| e.to_string())? else {
            return Err(format!("no target \"{target}\""));
        };
        let Some(f) = functions.get::<Option<Function>>(function).map_err(|e| e.to_string())? else {
            return Err(format!("\"{target}\" has no function \"{function}\""));
        };
        let args = args.iter().map(|a| self.lua.create_string(a).map(LuaValue::String)).collect::<mlua::Result<Vec<_>>>();
        let value: LuaValue = f.call(mlua::MultiValue::from_iter(args.map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
        Ok(match value {
            LuaValue::Nil => String::new(),
            other => other.to_string().map_err(|e| e.to_string())?,
        })
    }

    /// The compositor closed a popup (description `spec`, on `output`): its
    /// `on_dismiss` is told, and is where the interface lets it go.
    pub fn dismissed(&self, spec: usize, output: Option<String>) {
        if let Some(Some(f)) = self.dismissers.get(spec) {
            if let Err(e) = f.call::<()>(output) {
                log::error!("g.popup on_dismiss / g.window on_close: {e}");
            }
        }
    }

    /// Whether an interface has required the service (`"clock"`, `"niri"`,
    /// `"vitals"`): a service nobody required has no source running.
    pub fn wants(&self, service: &str) -> bool {
        self.wanted.borrow().contains(&service)
    }

    /// Starts from `entry` instead of `shell.luau`: another entry point of
    /// the same interface, its modules shared.
    pub fn set_entry(&mut self, entry: &str) {
        self.entry = entry.to_owned();
    }

    /// Evaluates the entry (`shell.luau`) and returns where its surfaces go. Nothing is
    /// mounted until [`Script::mount`]: the runtime first makes a surface for
    /// each output the description asks for.
    pub fn evaluate(&mut self) -> Result<Vec<SurfaceSpec>, String> {
        // A fresh state for each load: modules are evaluated again and
        // nothing of the old chunks survives but the `g.state` signals.
        let lua = Lua::new();
        self.install(&lua).map_err(|e| e.to_string())?;
        let path = self.dir.join(&self.entry);
        let source = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // What the top-level code starts belongs to this load, and is stopped
        // if the load fails or when the next one is mounted.
        // One g.surface, or a list of them: a shell has several surfaces on
        // each output (a membrane on each edge, the floats, the wallpaper).
        let module_owner = self.module_owner.clone();
        let (scope, result) = Scope::new(|| -> Result<(Vec<Table>, Vec<SurfaceSpec>), String> {
            module_owner.set(current_owner());
            let returned: Table =
                lua.load(&source).set_name(format!("@{}", path.display())).eval().map_err(|e| e.to_string())?;
            let is_surface = |t: &Table| t.get::<Option<bool>>("__surface").ok().flatten() == Some(true);
            let surfaces: Vec<Table> = if is_surface(&returned) {
                vec![returned]
            } else {
                returned.sequence_values::<Table>().collect::<mlua::Result<_>>().map_err(|e| e.to_string())?
            };
            if surfaces.iter().any(|t| !is_surface(t)) {
                return Err(format!("{} must return g.surface {{ … }}, or a list of them", self.entry));
            }
            let surfaces = hoist_popups(surfaces).map_err(|e| e.to_string())?;
            let specs = surfaces.iter().map(surface_spec).collect::<mlua::Result<Vec<_>>>().map_err(|e| e.to_string())?;
            Ok((surfaces, specs))
        });
        match result {
            Ok((surfaces, specs)) => {
                if let Some((_, _, stale)) = self.pending.replace((lua, surfaces, scope)) {
                    stale.dispose();
                }
                Ok(specs)
            }
            Err(e) => {
                scope.dispose();
                Err(e)
            }
        }
    }

    /// Mounts the evaluated file's content into each target, replacing the
    /// last trees. Content that is a function is called with each target's
    /// output; a table is one tree for one surface. On an error the last
    /// trees stay as they were, and so do the scenes.
    pub fn mount(&mut self, targets: &[Target]) -> Result<(), String> {
        let Some((lua, surfaces, io_scope)) = self.pending.take() else { return Err("nothing evaluated".into()) };
        let mut contents: Vec<LuaValue> = Vec::with_capacity(surfaces.len());
        for (i, surface) in surfaces.iter().enumerate() {
            let content: LuaValue = match surface.get(1) {
                Ok(content) => content,
                Err(e) => {
                    io_scope.dispose();
                    return Err(e.to_string());
                }
            };
            if matches!(content, LuaValue::Table(_)) && targets.iter().filter(|t| t.0 == i).count() > 1 {
                io_scope.dispose();
                return Err("g.surface for several outputs holds a function of the output".into());
            }
            contents.push(content);
        }
        let handlers: Handlers = Rc::default();
        let (wishes, wishes_changed) = (self.wishes.clone(), self.wishes_changed.clone());
        let fluid: Vec<bool> = surfaces.iter().map(|t| t.get::<Option<bool>>("fluid").ok().flatten() == Some(true)).collect();
        let windows: Vec<bool> = surfaces.iter().map(|t| t.get::<Option<bool>>("__window").ok().flatten() == Some(true)).collect();
        // A popup's on_dismiss, a window's on_close: the compositor closed it,
        // or asks to.
        let dismissers: Vec<Option<Function>> = surfaces
            .iter()
            .map(|t| {
                let key = if t.get::<Option<bool>>("__window").ok().flatten() == Some(true) { "on_close" } else { "on_dismiss" };
                t.get::<Option<Function>>(key).ok().flatten()
            })
            .collect();
        let (scope, result) = Scope::new(|| -> mlua::Result<(Vec<genoma_scene::NodeId>, Vec<Scope>, Option<usize>, Vec<Option<Signal<(f64, f64)>>>)> {
            let load_owner = current_owner();
            let mut holders = Vec::new();
            // Each target's tree in a scope of its own, inside the load's:
            // one can be built again alone.
            let mut target_scopes: Vec<Scope> = Vec::new();
            let mut sizes: Vec<Option<Signal<(f64, f64)>>> = Vec::new();
            for (index, (spec, output, scene, size)) in targets.iter().enumerate() {
                let Some(content) = contents.get(*spec) else { return Err(err("a surface with no description")) };
                let signal = match (fluid.get(*spec), size) {
                    (Some(true), Some((w, h))) => Some(Signal::new((*w as f64, *h as f64))),
                    _ => None,
                };
                sizes.push(signal);
                let stretch = windows.get(*spec).copied().unwrap_or(false);
                let (target_scope, built) = Scope::new(|| build_tree(content, output, size, signal, stretch, scene, &handlers, index));
                match built {
                    Ok(holder) => {
                        holders.push(holder);
                        target_scopes.push(target_scope);
                    }
                    Err(e) => {
                        target_scope.dispose();
                        return Err(e);
                    }
                }
            }
            // present = function(output), reserve = function(output): asked
            // again whenever what they read changes.
            *wishes.borrow_mut() = vec![Wish::default(); targets.len()];
            wishes_changed.set(true);
            for (index, (spec, output, _, _)) in targets.iter().enumerate() {
                let Some(surface) = surfaces.get(*spec) else { continue };
                let present = surface.get::<Option<Function>>("present")?;
                let reserve = match surface.get::<LuaValue>("reserve")? {
                    LuaValue::Function(f) => Some(f),
                    _ => None,
                };
                let anchor = match surface.get::<LuaValue>("anchor")? {
                    LuaValue::Function(f) if surface.get::<Option<bool>>("__popup")? == Some(true) => Some(f),
                    _ => None,
                };
                let title = match surface.get::<LuaValue>("title")? {
                    LuaValue::Function(f) if surface.get::<Option<bool>>("__window")? == Some(true) => Some(f),
                    _ => None,
                };
                if present.is_none() && reserve.is_none() && anchor.is_none() && title.is_none() {
                    continue;
                }
                let (wishes, changed, output) = (wishes.clone(), wishes_changed.clone(), output.clone());
                Effect::new(move || {
                    let is_present = match &present {
                        Some(f) => f.call::<Option<bool>>(output.clone()).unwrap_or_else(|e| {
                            log::error!("g.surface present: {e}");
                            Some(true)
                        }) != Some(false),
                        None => true,
                    };
                    let reserving = reserve.as_ref().map(|f| match f.call::<LuaValue>(output.clone()) {
                        Ok(LuaValue::Integer(n)) if n > 0 => Some(n as i32),
                        Ok(LuaValue::Number(n)) if n > 0.0 => Some(n.round() as i32),
                        Ok(_) => None,
                        Err(e) => {
                            log::error!("g.surface reserve: {e}");
                            None
                        }
                    });
                    let anchoring = anchor.as_ref().and_then(|f| match f.call::<Option<String>>(output.clone()) {
                        Ok(id) => id,
                        Err(e) => {
                            log::error!("g.popup anchor: {e}");
                            None
                        }
                    });
                    let titled = title.as_ref().and_then(|f| match f.call::<Option<String>>(output.clone()) {
                        Ok(t) => t,
                        Err(e) => {
                            log::error!("g.window title: {e}");
                            None
                        }
                    });
                    let wish = Wish { present: is_present, reserve: reserving, anchor: anchoring, title: titled };
                    let mut all = wishes.borrow_mut();
                    if let Some(w) = all.get_mut(index) {
                        if *w != wish {
                            *w = wish;
                            changed.set(true);
                        }
                    }
                });
            }
            Ok((holders, target_scopes, load_owner, sizes))
        });
        match result {
            Ok((holders, target_scopes, load_owner, sizes)) => {
                self.sizes = sizes;
                self.fluid = fluid;
                self.windows = windows;
                self.dismissers = dismissers;
                self.target_scopes = target_scopes.into_iter().map(Some).collect();
                self.load_owner = load_owner;
                if let Some(old) = self.scope.take() {
                    old.dispose();
                }
                if let Some(old) = self.io_scope.replace(io_scope) {
                    old.dispose();
                }
                for ((_, _, scene, _), holder) in targets.iter().zip(holders) {
                    let mut s = scene.borrow_mut();
                    let root = s.root();
                    s.clear();
                    s.append(root, holder);
                    s.set_icon_dirs(self.icon_dirs.borrow().clone());
                }
                self.scenes = targets.iter().map(|(_, _, s, _)| s.clone()).collect();
                self.contents = contents;
                self.scope = Some(scope);
                self.lua = lua;
                // The map itself, not its contents: a keyed list adds and
                // drops its rows' handlers in it after the mount.
                self.handlers = handlers;
                Ok(())
            }
            Err(e) => {
                scope.dispose();
                io_scope.dispose();
                Err(e.to_string())
            }
        }
    }

    fn install(&self, lua: &Lua) -> mlua::Result<()> {
        list::set_host(self.host.clone());
        let g = lua.create_table()?;
        // g.focus_move(1 | -1), g.focus_press(), g.focus_clear(): the
        // keyboard's way through the open cell's buttons, done once the key
        // that asked is handled. g.has_focus(): whether one has it.
        for (name, request) in [("focus_press", FocusRequest::Press), ("focus_clear", FocusRequest::Clear)] {
            let queue = self.focus.0.clone();
            g.set(name, lua.create_function(move |_, ()| {
                queue.borrow_mut().push(request);
                Ok(())
            })?)?;
        }
        let queue = self.focus.0.clone();
        g.set("focus_move", lua.create_function(move |_, step: i32| {
            queue.borrow_mut().push(FocusRequest::Move(if step < 0 { -1 } else { 1 }));
            Ok(())
        })?)?;
        let held = self.focus.1.clone();
        g.set("has_focus", lua.create_function(move |_, ()| Ok(held.get()))?)?;
        // g.activation_token { app_id = "…", on_token = function(token) … end }:
        // a token from the compositor for a program about to start, its
        // window then given the focus (XDG_ACTIVATION_TOKEN in its
        // environment). nil where the compositor gives none. The requests of
        // an older load go with it.
        self.activation_waiting.borrow_mut().clear();
        self.activation.borrow_mut().clear();
        let (queue, waiting, next) = (self.activation.clone(), self.activation_waiting.clone(), self.activation_next.clone());
        g.set(
            "activation_token",
            lua.create_function(move |_, t: Table| {
                let on_token: Function = t.get("on_token").map_err(|_| err("g.activation_token needs on_token, a function"))?;
                let id = next.get() + 1;
                next.set(id);
                waiting.borrow_mut().insert(id, on_token);
                queue.borrow_mut().push((id, t.get::<Option<String>>("app_id")?));
                Ok(())
            })?,
        )?;
        // g.ipc("cell", { toggle = function(domain) … end }): what a call
        // from outside reaches, for this load.
        lua.set_named_registry_value(IPC, lua.create_table()?)?;
        // g.outputs() → { { name, description, make, model, width, height } }:
        // the outputs now, in the compositor's order. A change reloads.
        let outputs = self.outputs.clone();
        g.set(
            "outputs",
            lua.create_function(move |lua, ()| {
                let list = lua.create_table()?;
                for (i, o) in outputs.borrow().iter().enumerate() {
                    let t = lua.create_table()?;
                    t.set("name", o.name.as_str())?;
                    t.set("description", o.description.as_str())?;
                    t.set("make", o.make.as_str())?;
                    t.set("model", o.model.as_str())?;
                    if let Some((w, h)) = o.size {
                        t.set("width", w)?;
                        t.set("height", h)?;
                    }
                    list.set(i + 1, t)?;
                }
                Ok(list)
            })?,
        )?;
        g.set(
            "ipc",
            lua.create_function(|lua, (target, functions): (String, Table)| {
                let targets: Table = lua.named_registry_value(IPC)?;
                targets.set(target, functions)
            })?,
        )?;
        // The building blocks of a service written in Luau (PRD §9.2).
        let host = self.host.clone();
        g.set("timer", lua.create_function(move |_, t: Table| io::timer(&host, t))?)?;
        let host = self.host.clone();
        g.set("process", lua.create_function(move |_, t: Table| io::process(&host, t))?)?;
        // A relative path is the interface's own: `../config/default.json`
        // from Bioma's genoma/ directory.
        let (host, dir) = (self.host.clone(), self.dir.clone());
        g.set(
            "file",
            lua.create_function(move |_, t: Table| {
                let path: String = t.get("path")?;
                if !(path.starts_with('/') || path.starts_with('~') || path.starts_with('$')) {
                    t.set("path", dir.join(&path).display().to_string())?;
                }
                io::file(&host, t)
            })?,
        )?;
        let (host, dir) = (self.host.clone(), self.dir.clone());
        g.set(
            "folder",
            lua.create_function(move |_, t: Table| {
                let path: String = t.get("path")?;
                if !(path.starts_with('/') || path.starts_with('~') || path.starts_with('$')) {
                    t.set("path", dir.join(&path).display().to_string())?;
                }
                io::folder(&host, t)
            })?,
        )?;
        // g.computed(fn), g.animated(value), g.json_set(text, path, value):
        // an interface's own modules (Bioma's core/ singletons).
        g.set("computed", lua.create_function(|_, f: Function| Ok(values::computed(f)))?)?;
        // g.map(items, key, make): a keyed list of things made once per key.
        g.set("map", lua.create_function(values::map)?)?;
        // g.cleanup(fn): fn runs when what is being made now is disposed (a
        // row of g.map or g.each gone, the load replaced).
        g.set(
            "cleanup",
            lua.create_function(|_, f: Function| {
                on_cleanup(move || {
                    if let Err(e) = f.call::<()>(()) {
                        log::error!("g.cleanup: {e}");
                    }
                });
                Ok(())
            })?,
        )?;
        let host = self.host.clone();
        g.set("animated", lua.create_function(move |_, v: LuaValue| values::animated(&host, v))?)?;
        g.set("json_set", lua.create_function(values::json_set)?)?;
        // g.array(t): t, marked as a list, so that empty it is still written
        // as [] and not as {}. The mark is a protected metatable: a marked
        // table is copied by hand, not with table.clone.
        g.set(
            "array",
            lua.create_function(|lua, t: Option<mlua::Table>| {
                let t = match t {
                    Some(t) => t,
                    None => lua.create_table()?,
                };
                t.set_metatable(Some(mlua::LuaSerdeExt::array_metatable(lua)))?;
                Ok(t)
            })?,
        )?;
        // g.effect(fn): runs it now and again whenever what it read changes;
        // stopped with the load. For what reacts rather than draws: the
        // timing set redefined when its speed changes, a theme transition
        // started when the palette does.
        g.set(
            "effect",
            lua.create_function(|_, f: Function| {
                Effect::new(move || {
                    if let Err(e) = profile::timed("effect", &f, || f.call::<()>(())) {
                        log::error!("g.effect: {e}");
                    }
                });
                Ok(())
            })?,
        )?;
        // g.watch(source, handler): handler(value) now and whenever what
        // source read changes — QML's onXChanged. Only the source is
        // tracked; the handler runs untracked, and what it starts (a
        // g.after) is not the watch's, so the next change does not stop it.
        g.set(
            "watch",
            lua.create_function(|_, (source, handler): (Function, Function)| {
                Effect::new(move || {
                    let value = match profile::timed("watch", &source, || source.call::<LuaValue>(())) {
                        Ok(v) => v,
                        Err(e) => {
                            log::error!("g.watch source: {e}");
                            return;
                        }
                    };
                    let handler = handler.clone();
                    genoma_core::reactive::untracked(|| {
                        detached(|| {
                            if let Err(e) = handler.call::<()>(value) {
                                log::error!("g.watch: {e}");
                            }
                        })
                    });
                });
                Ok(())
            })?,
        )?;
        // g.desktop_entries() → { { id, name, generic_name, comment, icon,
        // exec, startup_class, path, terminal, keywords, categories, file } }:
        // the applications the menu offers, read now.
        g.set(
            "desktop_entries",
            lua.create_function(|lua, ()| {
                let list = lua.create_table()?;
                for (i, e) in genoma_services::icons::desktop_entries().into_iter().enumerate() {
                    let t = lua.create_table()?;
                    t.set("id", e.id)?;
                    t.set("name", e.name)?;
                    t.set("generic_name", e.generic_name)?;
                    t.set("comment", e.comment)?;
                    t.set("icon", e.icon)?;
                    t.set("exec", e.exec)?;
                    t.set("startup_class", e.startup_class)?;
                    t.set("path", e.path)?;
                    t.set("terminal", e.terminal)?;
                    t.set("keywords", e.keywords)?;
                    t.set("categories", e.categories)?;
                    t.set("file", e.file.display().to_string())?;
                    list.set(i + 1, t)?;
                }
                Ok(list)
            })?,
        )?;
        // g.icon_path("firefox") → the file the icon theme gives for it, or nil.
        g.set(
            "icon_path",
            lua.create_function(|_, name: String| {
                Ok(genoma_services::icons::icon_path(&name).map(|p| p.display().to_string()))
            })?,
        )?;
        // g.batch(fn): the signals fn sets are heard once it returns, so a
        // binding reading several of them runs once, not once for each.
        g.set(
            "batch",
            lua.create_function(|_, f: Function| genoma_core::reactive::batch(|| f.call::<LuaValue>(())))?,
        )?;
        // g.lasting(fn): what fn makes (a computed, a signal) belongs to the
        // load, as a module's own state does, not to the binding or effect
        // that happens to be running: a memo made the first time a binding
        // asks for it outlives that binding's next run.
        let lasting_owner = self.module_owner.clone();
        g.set(
            "lasting",
            lua.create_function(move |_, f: Function| owned_by(lasting_owner.get(), || f.call::<LuaValue>(())))?,
        )?;
        // g.crossing("theme", fn): a batch whose colours cross rather than
        // jump. What `fn` changes reaches every binding once; each colour a
        // binding then sets, whatever transition its node names, is
        // carried from the old to the new over that timing entry by the
        // scene. A palette crossed by stepping its roles in Luau re-ran
        // every binding at every step and filled the frame. Called from
        // inside a binding or an effect, the bindings would run after it,
        // once the wave under way settles: call it from a timer or a
        // callback (g.after(0, …)).
        g.set(
            "crossing",
            lua.create_function(|_, (name, f): (String, Function)| {
                let timing = timing::get(&name).ok_or_else(|| err(format!("g.crossing: no timing named {name}")))?;
                let previous = CROSSING.with(|c| c.replace(Some(timing)));
                let result = genoma_core::reactive::batch(|| f.call::<LuaValue>(()));
                CROSSING.with(|c| *c.borrow_mut() = previous);
                result
            })?,
        )?;
        // g.untracked(fn): its result, without what it read becoming a
        // dependency of the binding around it.
        g.set(
            "untracked",
            lua.create_function(|_, f: Function| genoma_core::reactive::untracked(|| f.call::<LuaValue>(())))?,
        )?;
        // g.env("XDG_CONFIG_HOME") → its value or nil: the sandbox has no
        // environment of its own.
        g.set("env", lua.create_function(|_, name: String| Ok(std::env::var(name).ok()))?)?;
        // g.colour.lift("#…", 0.05), g.colour.contrast(a, b), …: colour
        // arithmetic, Qt's, for what a theme derives (colour.rs).
        g.set("colour", colour::table(lua)?)?;
        // g.signal(value): a value bindings follow, made anew by each load
        // (g.state is the one kept across loads).
        g.set("signal", lua.create_function(|_, initial: LuaValue| Ok(State(Signal::new(StateValue::from_lua(&initial)))))?)?;
        let host = self.host.clone();
        g.set("after", lua.create_function(move |_, args: (f64, Function)| io::after(&host, args))?)?;
        // g.now(): seconds on a monotonic clock, for measuring a wait.
        let epoch = std::time::Instant::now();
        g.set("now", lua.create_function(move |_, ()| Ok(epoch.elapsed().as_secs_f64()))?)?;
        // g.text_width { text, font, size, weight, italic, tabular, tracking }:
        // logical units, as the text would be drawn.
        let host = self.host.clone();
        g.set(
            "text_width",
            lua.create_function(move |_, t: Table| {
                let text: String = t.get("text")?;
                let font: Option<String> = t.get("font")?;
                let style = genoma_scene::TextStyle {
                    font: font.as_deref(),
                    italic: t.get::<Option<bool>>("italic")?.unwrap_or(false),
                    tabular: t.get::<Option<bool>>("tabular")?.unwrap_or(false),
                    weight: t.get::<Option<f64>>("weight")?.map(|w| w.clamp(100.0, 900.0) as u16),
                    tracking: t.get::<Option<f32>>("tracking")?.unwrap_or(0.0),
                };
                Ok(host.measure(&text, t.get("size")?, &style))
            })?,
        )?;
        // g.text_height { text, font, size, weight, italic, tabular, tracking,
        // width, runs }: logical units, as the text would be drawn wrapped
        // within `width` (a text node with `wrap` and that `max_width`).
        let host = self.host.clone();
        g.set(
            "text_height",
            lua.create_function(move |_, t: Table| {
                let text: String = t.get("text")?;
                let font: Option<String> = t.get("font")?;
                let style = genoma_scene::TextStyle {
                    font: font.as_deref(),
                    italic: t.get::<Option<bool>>("italic")?.unwrap_or(false),
                    tabular: t.get::<Option<bool>>("tabular")?.unwrap_or(false),
                    weight: t.get::<Option<f64>>("weight")?.map(|w| w.clamp(100.0, 900.0) as u16),
                    tracking: t.get::<Option<f32>>("tracking")?.unwrap_or(0.0),
                };
                let runs = match convert(genoma_scene::Prop::Runs, &t.get::<LuaValue>("runs")?) {
                    Ok(genoma_scene::Value::Runs(runs)) => runs,
                    Ok(_) => Vec::new(),
                    Err(e) => return Err(mlua::Error::runtime(e)),
                };
                Ok(host.measure_wrapped(&text, t.get("size")?, &style, t.get("width")?, &runs))
            })?,
        )?;
        // g.quit(code): the interface is done and the process ends, with that
        // exit status (0 by default): a greeter once the session it started
        // has taken over. The compositor takes its surfaces down.
        g.set(
            "quit",
            lua.create_function(|_, code: Option<i32>| -> mlua::Result<()> {
                log::info!("quit, as the interface asked");
                std::process::exit(code.unwrap_or(0))
            })?,
        )?;
        // g.dir: the interface's directory.
        g.set("dir", self.dir.display().to_string())?;
        // g.parse_json(text) → the value; g.to_json(value, pretty) → text.
        g.set(
            "parse_json",
            lua.create_function(|lua, text: String| {
                use mlua::LuaSerdeExt;
                let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| err(format!("g.parse_json: {e}")))?;
                // A JSON null is nil, as Luau reads absence: niri's
                // `"id": null` is no focused window, not a truthy value.
                // And a list is a plain table: mlua's marker for one is a
                // protected metatable, which table.clone refuses. An emptied
                // list is written back as a list by g.json_set all the same.
                let options = mlua::serde::SerializeOptions::new()
                    .serialize_none_to_null(false)
                    .serialize_unit_to_null(false)
                    .set_array_metatable(false);
                lua.to_value_with(&value, options)
            })?,
        )?;
        g.set(
            "to_json",
            lua.create_function(|lua, (value, pretty): (LuaValue, Option<bool>)| {
                use mlua::LuaSerdeExt;
                let value: serde_json::Value = lua.from_value(value)?;
                let text = if pretty.unwrap_or(false) { serde_json::to_string_pretty(&value) } else { serde_json::to_string(&value) };
                text.map_err(|e| err(format!("g.to_json: {e}")))
            })?,
        )?;
        let host = self.host.clone();
        g.set("socket", lua.create_function(move |_, t: Table| io::socket(&host, t))?)?;
        let host = self.host.clone();
        g.set("greetd", lua.create_function(move |_, t: Table| greetd::connect(&host, t))?)?;
        let dbus_table = lua.create_table()?;
        let get_dbus = {
            let (host, cell) = (self.host.clone(), self.dbus.clone());
            move || -> mlua::Result<Rc<dbus::Dbus>> {
                if let Some(d) = cell.borrow().as_ref() {
                    return Ok(d.clone());
                }
                let d = dbus::Dbus::new(&host)?;
                *cell.borrow_mut() = Some(d.clone());
                Ok(d)
            }
        };
        let f = get_dbus.clone();
        dbus_table.set("call", lua.create_function(move |_, t: Table| dbus::call_from(&f()?, t))?)?;
        let f = get_dbus.clone();
        dbus_table.set("get", lua.create_function(move |_, t: Table| dbus::get_from(&f()?, t))?)?;
        let f = get_dbus.clone();
        dbus_table.set("set", lua.create_function(move |_, t: Table| dbus::set_from(&f()?, t))?)?;
        let f = get_dbus.clone();
        dbus_table.set("watch", lua.create_function(move |_, t: Table| dbus::watch_from(&f()?, t))?)?;
        // g.pam.start { … }: PAM, through the same channel into the loop.
        let pam_table = lua.create_table()?;
        let f = get_dbus;
        pam_table.set("start", lua.create_function(move |_, t: Table| auth::start(&f()?, t))?)?;
        g.set("pam", pam_table)?;
        // g.dbus.variant("y", 2): a value with the type it takes in a `v`.
        dbus_table.set(
            "variant",
            lua.create_function(|lua, (signature, value): (String, LuaValue)| {
                let t = lua.create_table()?;
                t.raw_set("__dbus_signature", signature)?;
                t.raw_set("__dbus_value", value)?;
                Ok(t)
            })?,
        )?;
        g.set("dbus", dbus_table)?;
        for kind in ["box", "capsule", "text", "icon", "image", "thread", "emitter", "shader", "field", "each", "list", "path"] {
            g.set(
                kind,
                lua.create_function(move |_, t: Table| {
                    t.set("__kind", kind)?;
                    Ok(t)
                })?,
            )?;
        }
        g.set(
            "surface",
            lua.create_function(|_, t: Table| {
                t.set("__surface", true)?;
                Ok(t)
            })?,
        )?;
        // g.popup { anchor = "clock", side = "bottom", … }: a surface that
        // opens beside a node of another one, over everything, for as long
        // as its `present` says.
        g.set(
            "popup",
            lua.create_function(|_, t: Table| {
                t.set("__surface", true)?;
                t.set("__popup", true)?;
                Ok(t)
            })?,
        )?;
        // g.window { title = "Settings", app_id = "…", width, height, … }:
        // a surface the compositor manages as a program's window, for as
        // long as its `present` says; `on_close` hears the compositor ask
        // to close it.
        g.set(
            "window",
            lua.create_function(|_, t: Table| {
                t.set("__surface", true)?;
                t.set("__window", true)?;
                Ok(t)
            })?,
        )?;
        // g.timing { open = { duration = 0.25, curve = { … } }, … }: the one
        // place a duration may be written. g.timing_default { … } defines
        // only the names not defined yet: what a library offers, which a
        // shell may have retimed before requiring it, or retime after.
        for (name, keep) in [("timing", false), ("timing_default", true)] {
            g.set(
                name,
                lua.create_function(move |_, set: Table| {
                    for pair in set.pairs::<String, Table>() {
                        let (name, entry) = pair?;
                        if keep && timing::get(&name).is_some() {
                            continue;
                        }
                        timing::define(&name, timing_entry(&name, &entry)?);
                    }
                    Ok(())
                })?,
            )?;
        }
        // g.icon_dir("~/…/assets/icons"): where icon names are looked up.
        let icon_dirs = self.icon_dirs.clone();
        g.set(
            "icon_dir",
            lua.create_function(move |_, dirs: mlua::Variadic<String>| {
                let home = std::env::var("HOME").unwrap_or_default();
                let dirs = dirs
                    .iter()
                    .map(|d| PathBuf::from(d.strip_prefix("~/").map_or(d.clone(), |r| format!("{home}/{r}"))))
                    .collect();
                *icon_dirs.borrow_mut() = dirs;
                Ok(())
            })?,
        )?;
        // g.app_icon("firefox") → the icon file's path, or nil.
        g.set(
            "app_icon",
            lua.create_function(|_, app_id: Option<String>| {
                Ok(app_id
                    .and_then(|id| genoma_services::icons::app_icon(&id))
                    .map(|p| p.to_string_lossy().into_owned()))
            })?,
        )?;
        // g.json("~/.config/bioma/generated/palette.json"): a data file read at
        // load, only from the user's configuration directory.
        g.set(
            "json",
            lua.create_function(|lua, path: String| {
                use mlua::LuaSerdeExt;
                let home = std::env::var("HOME").map_err(|_| err("no HOME"))?;
                let config = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
                let path = match path.strip_prefix("~/") {
                    Some(rest) => format!("{home}/{rest}"),
                    None => path,
                };
                let resolved = std::fs::canonicalize(&path).map_err(|e| err(format!("{path}: {e}")))?;
                if !resolved.starts_with(&config) {
                    return Err(err(format!("{path}: g.json reads only inside {config}")));
                }
                let text = std::fs::read_to_string(&resolved).map_err(|e| err(format!("{path}: {e}")))?;
                let value: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| err(format!("{path}: {e}")))?;
                lua.to_value(&value)
            })?,
        )?;
        // g.state("key", initial): a signal kept across reloads by its key.
        let states = self.states.clone();
        g.set(
            "state",
            lua.create_function(move |_, (key, initial): (String, LuaValue)| {
                let mut states = states.borrow_mut();
                let signal = *states
                    .entry(key)
                    .or_insert_with(|| detached(|| Signal::new(StateValue::from_lua(&initial))));
                Ok(State(signal))
            })?,
        )?;

        let dir = self.dir.clone();
        let services = self.services.clone();
        let wanted = self.wanted.clone();
        let module_owner = self.module_owner.clone();
        let mocks = self.mocks.clone();
        let cache: Table = lua.create_table()?;
        let require = lua.create_function(move |lua, name: String| -> mlua::Result<LuaValue> {
            if let Some(v) = cache.get::<Option<LuaValue>>(name.as_str())? {
                return Ok(v);
            }
            let mocked = mocks.borrow().get(&name).cloned();
            let value = match name.as_str() {
                _ if mocked.is_some() => {
                    let path = mocked.expect("checked");
                    let source = std::fs::read_to_string(&path).map_err(|e| err(format!("mock {name}: {}: {e}", path.display())))?;
                    owned_by(module_owner.get(), || lua.load(&source).set_name(format!("@{}", path.display())).eval())?
                }
                "@genoma" => LuaValue::Table(g.clone()),
                "@genoma/clock" | "@genoma/niri" | "@genoma/compositor" | "@genoma/vitals" | "@genoma/power" | "@genoma/network" | "@genoma/media" | "@genoma/audio" | "@genoma/bluetooth" | "@genoma/toplevels" | "@genoma/displays" | "@genoma/gamma" | "@genoma/idle" | "@genoma/lock" | "@genoma/tray" | "@genoma/polkit" | "@genoma/notifications" | "@genoma/clipboard" | "@genoma/apps" | "@genoma/session" | "@genoma/spectrum" => {
                    let service = &name["@genoma/".len()..];
                    let key: &'static str = match service {
                        "clock" => "clock",
                        "niri" => "niri",
                        "compositor" => "compositor",
                        "power" => "power",
                        "network" => "network",
                        "media" => "media",
                        "audio" => "audio",
                        "bluetooth" => "bluetooth",
                        "toplevels" => "toplevels",
                        "displays" => "displays",
                        "gamma" => "gamma",
                        "idle" => "idle",
                        "lock" => "lock",
                        "tray" => "tray",
                        "polkit" => "polkit",
                        "notifications" => "notifications",
                        "clipboard" => "clipboard",
                        "apps" => "apps",
                        "session" => "session",
                        "spectrum" => "spectrum",
                        _ => "vitals",
                    };
                    if !wanted.borrow().contains(&key) {
                        wanted.borrow_mut().push(key);
                    }
                    let data = match key {
                        "clock" => lua.create_userdata(ClockRef(services.clock.clone()))?,
                        "niri" => match &services.niri {
                            Some(niri) => lua.create_userdata(NiriRef(niri.clone()))?,
                            None => return Err(err("niri's socket is not available")),
                        },
                        "compositor" => match &services.compositor {
                            Some(c) => lua.create_userdata(CompositorRef(c.clone()))?,
                            None => return Err(err("no compositor service")),
                        },
                        "power" => lua.create_userdata(PowerRef(services.power.clone()))?,
                        "network" => lua.create_userdata(NetworkRef(services.network.clone()))?,
                        "media" => lua.create_userdata(MediaRef(services.media.clone()))?,
                        "audio" => lua.create_userdata(AudioRef(services.audio.clone()))?,
                        "bluetooth" => lua.create_userdata(BluetoothRef(services.bluetooth.clone()))?,
                        "toplevels" => lua.create_userdata(ToplevelsRef(services.toplevels.clone()))?,
                        "displays" => lua.create_userdata(DisplaysRef(services.displays.clone()))?,
                        "gamma" => lua.create_userdata(GammaRef(services.gamma.clone()))?,
                        "idle" => lua.create_userdata(IdleRef(services.idle.clone()))?,
                        "lock" => lua.create_userdata(LockRef(services.lock.clone()))?,
                        "tray" => lua.create_userdata(TrayRef(services.tray.clone()))?,
                        "polkit" => lua.create_userdata(PolkitRef(services.polkit.clone()))?,
                        "notifications" => lua.create_userdata(NotificationsRef(services.notifications.clone()))?,
                        "clipboard" => lua.create_userdata(ClipboardRef(services.clipboard.clone()))?,
                        "apps" => lua.create_userdata(AppsRef(services.apps.clone()))?,
                        "session" => lua.create_userdata(SessionRef(services.session.clone()))?,
                        "spectrum" => lua.create_userdata(SpectrumRef(services.spectrum.clone()))?,
                        _ => lua.create_userdata(VitalsRef(services.vitals.clone()))?,
                    };
                    LuaValue::UserData(data)
                }
                kit if kit == "@genoma/kit"
                    || kit.starts_with("@genoma/kit/")
                    || kit == "@genoma/async"
                    || kit == "@genoma/theme" =>
                {
                    // @genoma/async and @genoma/theme ship with the kit,
                    // modules of their own (the kit's look is kit/theme;
                    // a shell's own themes are kit/themes).
                    let part = match kit {
                        "@genoma/async" => "async",
                        "@genoma/theme" => "themes",
                        _ => kit.strip_prefix("@genoma/kit/").unwrap_or("init"),
                    };
                    let source = kit_source(part).ok_or_else(|| err(format!("no module {kit}")))?;
                    owned_by(module_owner.get(), || lua.load(&source).set_name(kit).eval())?
                }
                other if other.starts_with('@') => return Err(err(format!("no module {other}"))),
                relative => {
                    if relative.contains("..") {
                        return Err(err(format!("{relative}: modules live inside the interface directory")));
                    }
                    let path = dir.join(format!("{relative}.luau"));
                    let source = std::fs::read_to_string(&path)
                        .map_err(|e| err(format!("{}: {e}", path.display())))?;
                    owned_by(module_owner.get(), || lua.load(&source).set_name(format!("@{}", path.display())).eval())?
                }
            };
            cache.set(name.as_str(), value.clone())?;
            Ok(value)
        })?;
        lua.globals().set("require", require)?;
        // print goes to the log, under the interface's name, rather than to
        // a stdout nobody reads.
        let print = lua.create_function(|lua, args: mlua::MultiValue| {
            let tostring: Function = lua.globals().get("tostring")?;
            let mut parts = Vec::with_capacity(args.len());
            for v in args {
                parts.push(tostring.call::<String>(v)?);
            }
            log::info!(target: "luau", "{}", parts.join("\t"));
            Ok(())
        })?;
        lua.globals().set("print", print)?;
        lua.sandbox(true)?;
        Ok(())
    }
}

/// Genoma's Luau API as types: what `genoma types` prints after the nodes'
/// and `genoma reference` documents.
pub const API: &str = include_str!("../api.luau");

/// Each `require("@genoma/<name>")` service and the type it has in [`API`].
pub const SERVICES: &[(&str, &str)] = &[
    ("clock", "Clock"),
    ("niri", "Niri"),
    ("compositor", "Compositor"),
    ("vitals", "Vitals"),
    ("power", "Power"),
    ("network", "Network"),
    ("media", "Media"),
    ("audio", "Audio"),
    ("bluetooth", "Bluetooth"),
    ("toplevels", "Toplevels"),
    ("displays", "Displays"),
    ("gamma", "Gamma"),
    ("idle", "Idle"),
    ("lock", "Lock"),
    ("tray", "Tray"),
    ("polkit", "Polkit"),
    ("notifications", "Notifications"),
    ("clipboard", "Clipboard"),
    ("apps", "Apps"),
    ("session", "Session"),
    ("spectrum", "Spectrum"),
];

/// The kit's modules by name, as built in.
/// A Luau file compiled, not run: a syntax mistake and its line
/// (`genoma check`). Types are luau-analyze's business (`genoma types`).
pub fn check_luau(name: &str, source: &str) -> Result<(), String> {
    let lua = Lua::new();
    lua.load(source).set_name(format!("={name}")).into_function().map(|_| ()).map_err(|e| {
        let message = match e {
            mlua::Error::SyntaxError { message, .. } => message,
            other => other.to_string(),
        };
        // "name:3: …" as "line 3: …", as a shader's mistake is told.
        match message.strip_prefix(&format!("{name}:")).and_then(|rest| rest.split_once(": ")) {
            Some((line, what)) if line.chars().all(|c| c.is_ascii_digit()) => format!("line {line}: {what}"),
            _ => message,
        }
    })
}

pub fn kit_modules() -> &'static [(&'static str, &'static str)] {
    KIT
}

/// The component kit's modules, built in. `GENOMA_KIT_DIR` reads them from
/// a directory instead, for working on the kit itself: read at each load,
/// so saving the interface's own files brings an edit of the kit in.
const KIT: &[(&str, &str)] = &[
    ("init", include_str!("../../../kit/init.luau")),
    ("theme", include_str!("../../../kit/theme.luau")),
    ("button", include_str!("../../../kit/button.luau")),
    ("toggle", include_str!("../../../kit/toggle.luau")),
    ("slider", include_str!("../../../kit/slider.luau")),
    ("progress", include_str!("../../../kit/progress.luau")),
    ("menu", include_str!("../../../kit/menu.luau")),
    ("dropdown", include_str!("../../../kit/dropdown.luau")),
    ("tooltip", include_str!("../../../kit/tooltip.luau")),
    ("list", include_str!("../../../kit/list.luau")),
    ("dialog", include_str!("../../../kit/dialog.luau")),
    ("tabs", include_str!("../../../kit/tabs.luau")),
    ("effects", include_str!("../../../kit/effects.luau")),
    ("field", include_str!("../../../kit/field.luau")),
    ("titlebar", include_str!("../../../kit/titlebar.luau")),
    ("async", include_str!("../../../kit/async.luau")),
    ("chart", include_str!("../../../kit/chart.luau")),
    ("themes", include_str!("../../../kit/themes.luau")),
];

fn kit_source(part: &str) -> Option<String> {
    let (_, built_in) = KIT.iter().find(|(name, _)| *name == part)?;
    if let Some(dir) = std::env::var_os("GENOMA_KIT_DIR") {
        let path = Path::new(&dir).join(format!("{part}.luau"));
        match std::fs::read_to_string(&path) {
            Ok(source) => return Some(source),
            Err(e) => log::warn!("{}: {e}; the built-in kit's", path.display()),
        }
    }
    Some((*built_in).to_owned())
}

/// One entry of `g.timing`: a duration and a curve, or a spring.
fn timing_entry(name: &str, entry: &Table) -> mlua::Result<Timing> {
    let spring = match entry.get::<Option<Table>>("spring")? {
        Some(t) => Some(genoma_core::timing::Spring {
            stiffness: t.get::<Option<f64>>("stiffness")?.unwrap_or(300.0),
            damping: t.get::<Option<f64>>("damping")?.unwrap_or(26.0),
            mass: t.get::<Option<f64>>("mass")?.unwrap_or(1.0),
        }),
        None => None,
    };
    let duration: f64 = match spring {
        Some(_) => entry.get::<Option<f64>>("duration")?.unwrap_or(0.0),
        None => entry.get("duration")?,
    };
    let curve = match entry.get::<Option<Vec<f64>>>("curve")? {
        Some(c) if c.len() == 4 => Curve { x1: c[0], y1: c[1], x2: c[2], y2: c[3] },
        Some(_) => return Err(err(format!("timing {name}: a curve is four numbers"))),
        None => Curve::LINEAR,
    };
    let delay: f64 = entry.get::<Option<f64>>("delay")?.unwrap_or(0.0);
    Ok(Timing { duration, curve, delay, spring })
}

/// The popups written inside a tree, taken out to be surfaces of their own
/// beside the others: a menu declared in the button that opens it. One
/// without an `anchor` is anchored to the node it was written in, which is
/// given an id if it has none. Only the tree as written is searched; a
/// popup in a row of `g.each` or in a function of the output is refused at
/// mount.
fn hoist_popups(surfaces: Vec<Table>) -> mlua::Result<Vec<Table>> {
    fn is_popup(t: &Table) -> bool {
        t.raw_get::<Option<bool>>("__popup").ok().flatten() == Some(true)
    }
    fn walk(node: &Table, found: &mut Vec<Table>, made: &mut usize) -> mlua::Result<()> {
        let children: Vec<LuaValue> = node.sequence_values::<LuaValue>().collect::<mlua::Result<_>>()?;
        if !children.iter().any(|c| matches!(c, LuaValue::Table(t) if is_popup(t))) {
            for child in &children {
                if let LuaValue::Table(t) = child {
                    walk(t, found, made)?;
                }
            }
            return Ok(());
        }
        let mut kept = Vec::with_capacity(children.len());
        for child in children {
            match child {
                LuaValue::Table(t) if is_popup(&t) => {
                    if matches!(t.raw_get::<LuaValue>("anchor")?, LuaValue::Nil) {
                        let id = match node.raw_get::<LuaValue>("id")? {
                            LuaValue::String(id) => id.to_string_lossy(),
                            _ => {
                                *made += 1;
                                let id = format!("__popup_anchor_{made}");
                                node.raw_set("id", id.as_str())?;
                                id
                            }
                        };
                        t.raw_set("anchor", id)?;
                    }
                    found.push(t);
                }
                other => kept.push(other),
            }
        }
        let len = node.raw_len();
        for i in 1..=len {
            node.raw_set(i, kept.get(i - 1).cloned().unwrap_or(LuaValue::Nil))?;
        }
        for child in &kept {
            if let LuaValue::Table(t) = child {
                walk(t, found, made)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::with_capacity(surfaces.len());
    let mut made = 0;
    let mut queue: std::collections::VecDeque<Table> = surfaces.into();
    while let Some(surface) = queue.pop_front() {
        let mut found = Vec::new();
        // The surface's own content, and below it; a popup's content may
        // hold popups of its own (a submenu).
        if let LuaValue::Table(content) = surface.raw_get::<LuaValue>(1)? {
            if is_popup(&content) {
                return Err(err("a surface's content is a node, not a g.popup"));
            }
            walk(&content, &mut found, &mut made)?;
        }
        out.push(surface);
        queue.extend(found);
    }
    Ok(out)
}

fn surface_spec(t: &Table) -> mlua::Result<SurfaceSpec> {
    // A popup's anchor is a node, read by popup_spec.
    let is_popup = t.get::<Option<bool>>("__popup")? == Some(true);
    let is_window = t.get::<Option<bool>>("__window")? == Some(true);
    let anchor = match t.get::<LuaValue>("anchor")? {
        _ if is_popup || is_window => Vec::new(),
        LuaValue::String(s) => vec![s.to_string_lossy()],
        LuaValue::Table(list) => list.sequence_values::<String>().collect::<mlua::Result<_>>()?,
        LuaValue::Nil => vec!["top".into()],
        _ => return Err(err("anchor is an edge or a list of edges")),
    };
    let mut margin = [0; 4];
    if let Some(m) = t.get::<Option<Table>>("margin")? {
        for (i, side) in ["top", "right", "bottom", "left"].iter().enumerate() {
            margin[i] = m.get::<Option<i32>>(*side)?.unwrap_or(0);
        }
    }
    Ok(SurfaceSpec {
        anchor,
        margin,
        layer: t.get::<Option<String>>("layer")?.unwrap_or_else(|| "top".into()),
        output: t.get("output")?,
        reserve: match t.get::<LuaValue>("reserve")? {
            LuaValue::Boolean(b) => b,
            LuaValue::Integer(n) => n > 0,
            LuaValue::Number(n) => n > 0.0,
            _ => false,
        },
        reserve_strip: match t.get::<LuaValue>("reserve")? {
            LuaValue::Integer(n) => Some(n as i32),
            LuaValue::Number(n) => Some(n.round() as i32),
            _ => None,
        },
        full: t.get::<Option<bool>>("full")?.unwrap_or(false),
        above_zones: t.get::<Option<bool>>("above_zones")?.unwrap_or(false),
        namespace: t.get::<Option<String>>("namespace")?.unwrap_or_else(|| "genoma".into()),
        all_outputs: matches!(t.get::<LuaValue>("outputs")?, LuaValue::String(ref s) if s.to_string_lossy() == "all"),
        outputs: match t.get::<LuaValue>("outputs")? {
            LuaValue::Table(list) => Some(list.sequence_values::<String>().collect::<mlua::Result<_>>()?),
            _ => None,
        },
        popup: popup_spec(t)?,
        window: window_spec(t)?,
        keyboard: match t.get::<Option<String>>("keyboard")?.as_deref() {
            None | Some("none") => "none".into(),
            Some(k @ ("on_demand" | "exclusive")) => k.into(),
            Some(other) => return Err(err(format!("keyboard: no mode {other} (none, on_demand, exclusive)"))),
        },
    })
}

fn window_spec(t: &Table) -> mlua::Result<Option<WindowSpec>> {
    if t.get::<Option<bool>>("__window")? != Some(true) {
        return Ok(None);
    }
    let pair = |key: &str| -> mlua::Result<Option<(u32, u32)>> {
        match t.get::<LuaValue>(key)? {
            LuaValue::Nil => Ok(None),
            LuaValue::Table(p) => {
                let w = p.get::<f64>(1).map_err(|_| err(format!("g.window {key}: {{ width, height }}")))?;
                let h = p.get::<f64>(2).map_err(|_| err(format!("g.window {key}: {{ width, height }}")))?;
                Ok(Some((w.round().max(1.0) as u32, h.round().max(1.0) as u32)))
            }
            _ => Err(err(format!("g.window {key}: {{ width, height }}"))),
        }
    };
    let side = |key: &str, default: f64| -> mlua::Result<u32> {
        Ok(t.get::<Option<f64>>(key)?.unwrap_or(default).round().max(1.0) as u32)
    };
    Ok(Some(WindowSpec {
        title: match t.get::<LuaValue>("title")? {
            LuaValue::String(s) => Some(s.to_string_lossy()),
            LuaValue::Nil | LuaValue::Function(_) => None,
            _ => return Err(err("g.window title: a string, or a function giving one")),
        },
        app_id: t.get::<Option<String>>("app_id")?.unwrap_or_else(|| "genoma".into()),
        size: (side("width", 640.0)?, side("height", 480.0)?),
        min: pair("min")?,
        max: pair("max")?,
        closes_itself: !matches!(t.get::<LuaValue>("on_close")?, LuaValue::Function(_)),
    }))
}

fn popup_spec(t: &Table) -> mlua::Result<Option<PopupSpec>> {
    if t.get::<Option<bool>>("__popup")? != Some(true) {
        return Ok(None);
    }
    let anchor = match t.get::<LuaValue>("anchor")? {
        LuaValue::String(s) => Some(s.to_string_lossy()),
        LuaValue::Function(_) => None,
        _ => return Err(err("g.popup needs an anchor: the id of the node it opens from, or a function giving it")),
    };
    let side = t.get::<Option<String>>("side")?.unwrap_or_else(|| "bottom".into());
    if !matches!(side.as_str(), "bottom" | "top" | "left" | "right") {
        return Err(err(format!("g.popup side: no side {side} (bottom, top, left, right)")));
    }
    let align = t.get::<Option<String>>("align")?.unwrap_or_else(|| "center".into());
    if !matches!(align.as_str(), "start" | "center" | "end") {
        return Err(err(format!("g.popup align: no alignment {align} (start, center, end)")));
    }
    Ok(Some(PopupSpec {
        anchor,
        side,
        align,
        gap: t.get::<Option<f64>>("gap")?.map_or(0, |g| g.round() as i32),
        grab: t.get::<Option<bool>>("grab")?.unwrap_or(true),
    }))
}

fn kind_of(t: &Table) -> mlua::Result<Kind> {
    if t.get::<Option<bool>>("__popup")? == Some(true) {
        return Err(err(
            "a g.popup inside a g.each row or a function of the output: write it in the tree a surface returns, or beside the surfaces",
        ));
    }
    if t.get::<Option<bool>>("__window")? == Some(true) {
        return Err(err("a g.window inside a tree: a window is a surface, returned beside the others"));
    }
    Ok(match t.get::<String>("__kind").map_err(|_| err("a child that is not a node"))?.as_str() {
        // A keyed list is a box whose children are its rows.
        "box" | "each" | "list" => Kind::Box,
        "path" => Kind::Path,
        "capsule" => Kind::Capsule,
        "text" => Kind::Text,
        "icon" => Kind::Icon,
        "image" => Kind::Image,
        "thread" => Kind::Thread,
        "emitter" => Kind::Emitter,
        "shader" => Kind::Shader,
        "field" => Kind::Field,
        other => return Err(err(format!("unknown node {other}"))),
    })
}

fn role_of(name: Option<String>) -> mlua::Result<Role> {
    Ok(match name.as_deref() {
        None => Role::None,
        Some("group") => Role::Group,
        Some("label") => Role::Label,
        Some("button") => Role::Button,
        Some("slider") => Role::Slider,
        Some("timer") => Role::Timer,
        Some("status") => Role::Status,
        Some(other) => return Err(err(format!("unknown role {other}"))),
    })
}

fn color(text: &str) -> Option<[f32; 4]> {
    let hex = text.strip_prefix('#')?;
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    match hex.len() {
        6 => Some([channel(0)?, channel(2)?, channel(4)?, 1.0]),
        8 => Some([channel(0)?, channel(2)?, channel(4)?, channel(6)?]),
        _ => None,
    }
}

/// A Luau value as the property wants it, by the type the schema gives it.
fn convert(prop: Prop, v: &LuaValue) -> Result<Value, String> {
    let info = prop.info();
    let wrong = |what: &str| format!("{} wants {what}", info.name);
    match info.ty {
        Ty::Number => match v {
            LuaValue::Integer(i) => Ok(Value::Number(*i as f32)),
            LuaValue::Number(n) => Ok(Value::Number(*n as f32)),
            _ => Err(wrong("a number")),
        },
        Ty::Color => match v {
            LuaValue::String(s) => color(&s.to_string_lossy())
                .map(Value::Color)
                .ok_or_else(|| wrong("a colour, \"#rrggbb\" or \"#rrggbbaa\"")),
            _ => Err(wrong("a colour string")),
        },
        Ty::Bool => match v {
            LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
            LuaValue::Nil => Ok(Value::Bool(false)),
            _ => Err(wrong("a boolean")),
        },
        Ty::Text => match v {
            LuaValue::String(s) => Ok(Value::Text(s.to_string_lossy())),
            LuaValue::Integer(i) => Ok(Value::Text(i.to_string())),
            LuaValue::Number(n) => Ok(Value::Text(n.to_string())),
            LuaValue::Nil => Ok(Value::Text(String::new())),
            _ => Err(wrong("a string")),
        },
        Ty::Numbers => match v {
            LuaValue::Table(t) => t
                .sequence_values::<f64>()
                .map(|n| n.map(|n| n as f32))
                .collect::<mlua::Result<Vec<f32>>>()
                .map(Value::Numbers)
                .map_err(|_| wrong("a list of numbers")),
            LuaValue::Nil => Ok(Value::Numbers(Vec::new())),
            // One number is a list of one (`slice = 16`).
            LuaValue::Integer(n) => Ok(Value::Numbers(vec![*n as f32])),
            LuaValue::Number(n) => Ok(Value::Numbers(vec![*n as f32])),
            _ => Err(wrong("a number or a list of numbers")),
        },
        Ty::Stops => match v {
            LuaValue::Table(t) => {
                let mut stops = Vec::new();
                for stop in t.sequence_values::<Table>() {
                    let s = stop.map_err(|_| wrong("a list of { position, colour }"))?;
                    let at: f64 = s.get(1).map_err(|_| wrong("a stop's position, a number"))?;
                    let c: String = s.get(2).map_err(|_| wrong("a stop's colour, \"#rrggbb\""))?;
                    let c = color(&c).ok_or_else(|| wrong("a stop's colour, \"#rrggbb\""))?;
                    stops.push((at as f32, c));
                }
                Ok(Value::Stops(stops))
            }
            LuaValue::Nil => Ok(Value::Stops(Vec::new())),
            _ => Err(wrong("a list of { position, colour }")),
        },
        Ty::Runs => match v {
            LuaValue::Table(t) => {
                let mut runs = Vec::new();
                for run in t.sequence_values::<Table>() {
                    let r = run.map_err(|_| wrong("a list of runs"))?;
                    let get = |k: &str| r.get::<LuaValue>(k).map_err(|_| wrong("a list of runs"));
                    let number = |k: &str| -> Result<Option<f64>, String> {
                        Ok(match get(k)? {
                            LuaValue::Integer(i) => Some(i as f64),
                            LuaValue::Number(n) => Some(n),
                            _ => None,
                        })
                    };
                    let (Some(from), Some(to)) = (number("from")?, number("to")?) else {
                        return Err(wrong("runs with `from` and `to`"));
                    };
                    runs.push(genoma_scene::TextRun {
                        from: from.max(0.0) as usize,
                        to: to.max(0.0) as usize,
                        weight: number("weight")?.map(|w| w as u16),
                        italic: match get("italic")? {
                            LuaValue::Boolean(b) => Some(b),
                            _ => None,
                        },
                        font: match get("font")? {
                            LuaValue::String(s) => Some(s.to_string_lossy()),
                            _ => None,
                        },
                        size: number("size")?.map(|s| s as f32),
                        color: match get("color")? {
                            LuaValue::String(s) => Some(color(&s.to_string_lossy()).ok_or_else(|| wrong("a run colour, \"#rrggbb\""))?),
                            _ => None,
                        },
                        strike: matches!(get("strike")?, LuaValue::Boolean(true)),
                    });
                }
                Ok(Value::Runs(runs))
            }
            LuaValue::Nil => Ok(Value::Runs(Vec::new())),
            _ => Err(wrong("a list of runs")),
        },
        Ty::Choice(options) => match v {
            LuaValue::String(s) if options.contains(&s.to_string_lossy().as_str()) => {
                Ok(Value::Text(s.to_string_lossy()))
            }
            _ => Err(wrong(&format!("one of {}", options.join(", ")))),
        },
    }
}

/// Creates the node `t` describes under `parent`, with its bindings.
/// One target's tree: its surface's content (a node, or a function of the
/// output and its size) mounted under a holder of its own, not yet in the
/// scene's root.
#[allow(clippy::too_many_arguments)]
fn build_tree(
    content: &LuaValue,
    output: &Option<String>,
    size: &Option<(u32, u32)>,
    fluid: Option<Signal<(f64, f64)>>,
    stretch: bool,
    scene: &Rc<RefCell<Scene>>,
    handlers: &Handlers,
    index: usize,
) -> mlua::Result<genoma_scene::NodeId> {
    let tree: Table = match content {
        // f(output, { width, height }), the size when the compositor has
        // said it. A fluid surface's size is read through, so a binding that
        // read it follows a new area instead of the tree being built again.
        LuaValue::Function(f) => {
            let lua = f.weak_lua().upgrade();
            let info = match (size, fluid) {
                (Some(_), Some(signal)) => {
                    let t = lua.create_table()?;
                    let meta = lua.create_table()?;
                    meta.set(
                        "__index",
                        lua.create_function(move |_, (_, key): (LuaValue, String)| {
                            let (w, h) = signal.try_get().unwrap_or((0.0, 0.0));
                            Ok(match key.as_str() {
                                "width" => LuaValue::Number(w),
                                "height" => LuaValue::Number(h),
                                _ => LuaValue::Nil,
                            })
                        })?,
                    )?;
                    t.set_metatable(Some(meta))?;
                    LuaValue::Table(t)
                }
                (Some((w, h)), None) => {
                    let t = lua.create_table()?;
                    t.set("width", *w)?;
                    t.set("height", *h)?;
                    LuaValue::Table(t)
                }
                (None, _) => LuaValue::Nil,
            };
            f.call((output.clone(), info))?
        }
        LuaValue::Table(t) => t.clone(),
        _ => return Err(err("g.surface holds one node, or a function returning one")),
    };
    let holder = scene.borrow_mut().create(Kind::Box, Role::Group);
    // The holder takes the surface's whole span, so content that grows (a
    // membrane's row) can reach the far edge.
    let _ = scene.borrow_mut().set(holder, Prop::Grow, Value::Number(1.0), None);
    // A window's content fills it, across and down: the holder is a column
    // that stretches it, and a content that grows takes the height.
    if stretch {
        let mut s = scene.borrow_mut();
        let _ = s.set(holder, Prop::Direction, Value::Text("column".into()), None);
        let _ = s.set(holder, Prop::Align, Value::Text("stretch".into()), None);
    }
    mount(scene, handlers, index, &tree, holder)?;
    Ok(holder)
}

fn mount(
    scene: &Rc<RefCell<Scene>>,
    handlers: &Handlers,
    target: usize,
    t: &Table,
    parent: genoma_scene::NodeId,
) -> mlua::Result<genoma_scene::NodeId> {
    let kind = kind_of(t)?;
    let made_as = t.get::<Option<String>>("__kind")?;
    let each = made_as.as_deref() == Some("each");
    let list = made_as.as_deref() == Some("list");
    let role = role_of(t.get("role")?)?;
    let node = scene.borrow_mut().create(kind, role);
    scene.borrow_mut().set_name(node, t.get("name")?);
    // Only a string names a node for the runtime; an interface may keep
    // other ids of its own there.
    if let LuaValue::String(id) = t.get::<LuaValue>("id")? {
        scene.borrow_mut().set_id(node, Some(id.to_string_lossy()));
    }
    scene.borrow_mut().append(parent, node);
    if list {
        for taken in ["on_wheel", "on_fling"] {
            if t.contains_key(taken)? {
                return Err(err(format!("g.list scrolls by itself: {taken} is its own")));
            }
        }
        // A column of rows as wide as the list, inside it; what the
        // interface sets says otherwise.
        let mut scene = scene.borrow_mut();
        let _ = scene.set(node, Prop::Direction, Value::Text("column".into()), None);
        let _ = scene.set(node, Prop::Align, Value::Text("stretch".into()), None);
        let _ = scene.set(node, Prop::Clip, Value::Bool(true), None);
    }

    // transition = { fill = "theme" }: names from the timing set, never
    // numbers. Checked here, at load, not when the value first moves.
    let mut transitions: HashMap<Prop, TimingName> = HashMap::new();
    if let Some(table) = t.get::<Option<Table>>("transition")? {
        for pair in table.pairs::<String, LuaValue>() {
            let (key, value) = pair?;
            let prop = Prop::from_name(&key).ok_or_else(|| err(format!("transition: no property {key}")))?;
            let name = match value {
                LuaValue::String(s) => {
                    let name = s.to_string_lossy();
                    if timing::get(&name).is_none() {
                        return Err(err(format!("transition.{key}: no timing named {name}")));
                    }
                    TimingName::Fixed(name)
                }
                // A function chooses the entry when the value changes: an
                // opening and a closing move on different curves.
                LuaValue::Function(f) => TimingName::Chosen(f),
                _ => {
                    return Err(err(format!(
                        "transition.{key}: durations come from the timing set; name an entry"
                    )));
                }
            };
            transitions.insert(prop, name);
        }
    }

    for pair in t.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        let LuaValue::String(key) = key else { continue };
        let key = key.to_string_lossy();
        if matches!(key.as_str(), "__kind" | "role" | "name" | "id" | "transition") {
            continue;
        }
        if (each || list) && matches!(key.as_str(), "items" | "key" | "row") {
            continue;
        }
        if list && matches!(key.as_str(), "row_height" | "reveal" | "on_scroll") {
            continue;
        }
        if let Some(&name) = HANDLERS.iter().find(|h| **h == key) {
            let LuaValue::Function(f) = value else {
                return Err(err(format!("{key} wants a function")));
            };
            handlers.borrow_mut().entry((target, node)).or_default().insert(name, f);
            if name == "on_drop" {
                scene.borrow_mut().set_drop_target(node, true);
            }
            if name == "on_size" {
                scene.borrow_mut().watch_size(node);
            } else if !matches!(name, "on_outside" | "on_key" | "on_ready" | "on_focus" | "on_focus_within") {
                scene.borrow_mut().set_interactive(node, true);
            }
            continue;
        }
        let prop = Prop::from_name(&key).ok_or_else(|| err(format!("{kind:?} has no property {key}")))?;
        let timing_name = transitions.get(&prop).cloned();
        match value {
            LuaValue::Function(f) => bind(scene.clone(), node, prop, f, timing_name),
            v => {
                let value = convert(prop, &v).map_err(err)?;
                scene.borrow_mut().set(node, prop, value, None).map_err(|e| err(e.0))?;
            }
        }
    }

    if each {
        if t.raw_len() > 0 {
            return Err(err("g.each draws its children from row, and holds none of its own"));
        }
        let want = |name: &str| -> mlua::Result<Function> {
            t.get::<Option<Function>>(name)?.ok_or_else(|| err(format!("g.each needs {name}, a function")))
        };
        list::each(scene.clone(), handlers.clone(), target, node, want("items")?, want("key")?, want("row")?);
        return Ok(node);
    }
    if list {
        if t.raw_len() > 0 {
            return Err(err("g.list draws its children from row, and holds none of its own"));
        }
        list::list(scene.clone(), handlers.clone(), target, node, t)?;
        return Ok(node);
    }
    for child in t.sequence_values::<Table>() {
        mount(scene, handlers, target, &child?, node)?;
    }
    Ok(node)
}

/// A property that follows a function: an effect that calls it and sets the
/// result, again whenever something it read changes.
/// A transition's timing: an entry's name, or a function that names one.
#[derive(Clone)]
enum TimingName {
    Fixed(String),
    Chosen(Function),
}

impl TimingName {
    fn resolve(&self) -> Option<Timing> {
        match self {
            TimingName::Fixed(name) => timing::get(name),
            TimingName::Chosen(f) => {
                let name = genoma_core::reactive::untracked(|| f.call::<String>(()));
                match name {
                    Ok(name) => timing::get(&name).or_else(|| {
                        log::error!("transition: no timing named {name}");
                        None
                    }),
                    Err(e) => {
                        log::error!("transition: {e}");
                        None
                    }
                }
            }
        }
    }
}

thread_local! {
    /// The timing colours cross over while a `g.crossing` is under way.
    static CROSSING: RefCell<Option<Timing>> = const { RefCell::new(None) };
}

fn bind(scene: Rc<RefCell<Scene>>, node: genoma_scene::NodeId, prop: Prop, f: Function, timing_name: Option<TimingName>) {
    Effect::new(move || {
        let value = match profile::timed("bind", &f, || f.call::<LuaValue>(())) {
            Ok(v) => v,
            Err(e) => {
                log::error!("binding {prop:?}: {e}");
                return;
            }
        };
        match convert(prop, &value) {
            Ok(value) => {
                // A colour set while a crossing is under way crosses over
                // its timing even where the node names one of its own (a
                // control's hover fade): the desktop crosses together, not
                // the controls first and the rest after.
                let crossing = if prop.info().ty == Ty::Color { CROSSING.with(|c| c.borrow().clone()) } else { None };
                let timing = crossing.or_else(|| timing_name.as_ref().and_then(TimingName::resolve));
                if let Err(e) = scene.borrow_mut().set(node, prop, value, timing) {
                    log::error!("binding {prop:?}: {e}");
                }
            }
            Err(e) => log::error!("binding {prop:?}: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(source: &str) -> mlua::Result<SurfaceSpec> {
        let lua = Lua::new();
        let t: Table = lua.load(source).eval()?;
        surface_spec(&t)
    }

    /// `source` mounted under a fresh scene's root, in a scope of its own.
    fn mounted(lua: &Lua, source: &str) -> mlua::Result<(Rc<RefCell<Scene>>, Handlers, genoma_scene::NodeId, Scope)> {
        let fonts = std::sync::Arc::new(std::sync::Mutex::new(genoma_render::cosmic_text::FontSystem::new_with_fonts(std::iter::empty())));
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        let handlers: Handlers = Rc::default();
        let t: Table = lua.load(source).eval()?;
        let root = scene.borrow().root();
        let (scope, node) = Scope::new(|| mount(&scene, &handlers, 0, &t, root));
        Ok((scene, handlers, node?, scope))
    }

    #[test]
    fn a_luau_mistake_is_told_at_its_line() {
        assert_eq!(check_luau("a.luau", "local x = 1\nreturn x").map_err(|e| e), Ok(()));
        let said = check_luau("a.luau", "local x = {\nreturn x\n").unwrap_err();
        assert!(said.starts_with("line 2: "), "{said}");
    }

    #[test]
    fn a_binding_follows_what_it_reads_until_its_scope_goes() {
        let lua = Lua::new();
        let level = genoma_core::reactive::Signal::new(10.0_f64);
        lua.globals().set("level", lua.create_function(move |_, ()| Ok(level.get())).unwrap()).unwrap();
        let (scene, handlers, node, scope) = mounted(
            &lua,
            r#"return { __kind = "box", width = function() return level() end, on_click = function() end,
                { __kind = "text", id = "label", text = function() return "at " .. level() end } }"#,
        )
        .unwrap();
        let width = || scene.borrow().value(node, Prop::Width);
        assert_eq!(width(), Some(Value::Number(10.0)));
        level.set(25.0);
        assert_eq!(width(), Some(Value::Number(25.0)));
        // Text reads back through the inspector.
        let label = scene.borrow().inspect().into_iter().find(|n| n.id.as_deref() == Some("label")).and_then(|n| n.text);
        assert_eq!(label.as_deref(), Some("at 25"), "Luau writes a whole number without its point");
        assert!(handlers.borrow().get(&(0, node)).is_some_and(|h| h.contains_key("on_click")));
        scope.dispose();
        level.set(40.0);
        assert_eq!(width(), Some(Value::Number(25.0)), "a disposed binding no longer follows");
    }

    #[test]
    fn a_wrong_value_is_refused_with_what_was_wanted() {
        let lua = Lua::new();
        let e = mounted(&lua, r##"return { __kind = "capsule", fill = "#335" }"##).err().expect("refused").to_string();
        assert!(e.contains("fill wants a colour"), "{e}");
        let e = mounted(&lua, r#"return { __kind = "box", direction = "diagonal" }"#).err().expect("refused").to_string();
        assert!(e.contains("diagonal") || e.contains("direction"), "{e}");
        let e = mounted(&lua, r#"return { __kind = "box", { 42 } }"#).err().expect("refused").to_string();
        assert!(e.contains("not a node"), "{e}");
    }

    #[test]
    fn a_popup_reads_its_anchor_as_a_node() {
        let s = spec(r#"return { __surface = true, __popup = true, anchor = "clock", side = "left", align = "end", gap = 6.4 }"#).unwrap();
        assert!(s.anchor.is_empty(), "no edges: the anchor is a node");
        assert_eq!(
            s.popup,
            Some(PopupSpec { anchor: Some("clock".into()), side: "left".into(), align: "end".into(), gap: 6, grab: true })
        );
        // A function gives the anchor at run time.
        let s = spec(r#"return { __surface = true, __popup = true, anchor = function() return "tray" end }"#).unwrap();
        let popup = s.popup.unwrap();
        assert_eq!((popup.anchor, popup.side.as_str(), popup.align.as_str()), (None, "bottom", "center"));
    }

    #[test]
    fn a_window_reads_its_size_and_whether_it_closes_itself() {
        let s = spec(r#"return { __surface = true, __window = true, title = "Settings", app_id = "x.settings", width = 720.4, height = 620, min = { 480, 360 } }"#).unwrap();
        assert!(s.anchor.is_empty(), "a window has no edges");
        assert_eq!(
            s.window,
            Some(WindowSpec {
                title: Some("Settings".into()),
                app_id: "x.settings".into(),
                size: (720, 620),
                min: Some((480, 360)),
                max: None,
                closes_itself: true,
            })
        );
        // A title function comes as a wish; an on_close decides.
        let s = spec(r#"return { __surface = true, __window = true, title = function() return "t" end, on_close = function() end }"#).unwrap();
        let w = s.window.unwrap();
        assert_eq!((w.title, w.app_id.as_str(), w.size, w.closes_itself), (None, "genoma", (640, 480), false));
        assert!(spec(r#"return { __surface = true, __window = true, min = 3 }"#).is_err());
    }

    #[test]
    fn a_popup_written_in_a_tree_becomes_a_surface_anchored_to_its_parent() {
        let lua = Lua::new();
        let returned: Table = lua
            .load(
                r#"return { __surface = true, {
                    __kind = "box",
                    { __kind = "capsule", id = "save",
                        { __surface = true, __popup = true, { __kind = "box" } } },
                    { __kind = "capsule",
                        { __surface = true, __popup = true, side = "right",
                            { __kind = "box",
                                { __kind = "capsule", { __surface = true, __popup = true, { __kind = "box" } } } } } },
                    { __kind = "text" },
                } }"#,
            )
            .eval()
            .unwrap();
        let surfaces = hoist_popups(vec![returned]).unwrap();
        assert_eq!(surfaces.len(), 4, "the surface, two popups, and one inside a popup");
        let anchors: Vec<String> = surfaces[1..].iter().map(|t| t.get::<String>("anchor").unwrap()).collect();
        assert_eq!(anchors[0], "save", "an id of its own is kept");
        assert!(anchors[1].starts_with("__popup_anchor_") && anchors[2].starts_with("__popup_anchor_") && anchors[1] != anchors[2]);
        // The tree keeps its other children, in order, and loses the popups.
        let content: Table = surfaces[0].get(1).unwrap();
        assert_eq!(content.raw_len(), 3);
        let save: Table = content.get(1).unwrap();
        assert_eq!(save.raw_len(), 0);
        let last: Table = content.get(3).unwrap();
        assert_eq!(last.get::<String>("__kind").unwrap(), "text");
        for s in &surfaces {
            surface_spec(s).unwrap();
        }
    }

    #[test]
    fn every_kit_module_compiles() {
        let lua = Lua::new();
        for (name, source) in KIT {
            lua.load(*source).set_name(*name).into_function().unwrap_or_else(|e| panic!("kit/{name}: {e}"));
        }
        assert!(kit_source("button").is_some() && kit_source("nothing").is_none());
        assert!(kit_source("async").is_some_and(|s| s.contains("function M.run")), "@genoma/async ships with the kit");
    }

    #[test]
    fn a_popup_without_a_place_is_refused() {
        assert!(spec(r#"return { __surface = true, __popup = true }"#).is_err());
        assert!(spec(r#"return { __surface = true, __popup = true, anchor = "a", side = "under" }"#).is_err());
        assert!(spec(r#"return { __surface = true, __popup = true, anchor = "a", align = "middle" }"#).is_err());
    }

    #[test]
    fn a_surface_is_no_popup() {
        let s = spec(r#"return { __surface = true, anchor = { "top", "left" } }"#).unwrap();
        assert_eq!(s.anchor, vec!["top".to_string(), "left".to_string()]);
        assert_eq!(s.popup, None);
    }
}
