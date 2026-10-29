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
pub mod dbus;
mod greetd;
mod values;
pub mod io;

use genoma_core::{
    reactive::{Effect, Scope, Signal, detached, on_cleanup, untracked},
    timing::{self, Curve, Timing},
};
use genoma_scene::{Kind, Prop, Role, Scene, Ty, Value};
use genoma_services::{
    audio::Audio,
    bluetooth::{Bluetooth, DeviceCall},
    clock::Clock,
    media::{Action, Media},
    network::Network,
    niri::Niri,
    notifications::Notifications,
    polkit::Polkit,
    power::Power,
    toplevels::Toplevels,
    tray::{MenuEntry, Tray},
    vitals::Vitals,
};
use mlua::{Function, Lua, Table, UserData, UserDataFields, Value as LuaValue};

/// Pointer input for a node, as the surface saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A press outside the node that is catching (an open cell).
    Outside,
    /// A key, by its xkb name ("Escape", "Up", "Return", "a").
    Key(String),
    Hover(bool),
    Press(u32),
    Release(u32),
    /// A press and a release on the same node.
    Click(u32),
    /// Discrete wheel steps, or touchpad movement in logical units.
    Wheel(f64, f64),
    /// The fingers left the touchpad while moving: their speed over the last
    /// moment, logical units a second, for the content to carry on.
    Fling(f64, f64),
}

impl Input {
    fn handler(&self) -> &'static str {
        match self {
            Input::Outside => "on_outside",
            Input::Key(_) => "on_key",
            Input::Hover(_) => "on_hover",
            Input::Press(_) => "on_press",
            Input::Release(_) => "on_release",
            Input::Click(_) => "on_click",
            Input::Wheel(..) => "on_wheel",
            Input::Fling(..) => "on_fling",
        }
    }
}

const HANDLERS: &[&str] = &["on_hover", "on_press", "on_release", "on_click", "on_wheel", "on_fling", "on_outside", "on_key"];

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
    /// `"all"` for one surface on every output, a list of output names, or
    /// none for a single surface (on `output`, or the compositor's choice).
    pub outputs: Option<Vec<String>>,
    pub all_outputs: bool,
    /// The layer surface's namespace: what the compositor's rules match.
    /// niri decides blur xray by it (Bioma's `bioma.kdl`), so a surface keeps
    /// its compositor behaviour by keeping its name.
    pub namespace: String,
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
    pub vitals: Rc<Vitals>,
    pub power: Rc<Power>,
    pub network: Rc<Network>,
    pub media: Rc<Media>,
    pub audio: Rc<Audio>,
    pub bluetooth: Rc<Bluetooth>,
    pub toplevels: Rc<Toplevels>,
    pub tray: Rc<Tray>,
    pub polkit: Rc<Polkit>,
    pub notifications: Rc<Notifications>,
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
        // icon_theme_path, attention_icon_name, has_pixmap, tooltip_title,
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
    scope: Option<Scope>,
    /// What the mounted load's top-level code started (processes, files,
    /// sockets, timers): stopped when the next load is mounted.
    io_scope: Option<Scope>,
    host: io::HostRef,
    /// `g.dbus`'s callbacks and channel, made on first use and kept across
    /// loads.
    dbus: Rc<RefCell<Option<Rc<dbus::Dbus>>>>,
    states: Rc<RefCell<HashMap<String, Signal<StateValue>>>>,
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
            scope: None,
            io_scope: None,
            host,
            dbus: Rc::default(),
            states: Rc::default(),
            services,
            wanted: Rc::default(),
            handlers: Rc::default(),
            icon_dirs: Rc::default(),
            scenes: Vec::new(),
            outputs: Rc::default(),
        }
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
                    Input::Outside => f.call::<()>(()),
                    Input::Key(name) => f.call::<()>(name.as_str()),
                    Input::Hover(entered) => f.call::<()>(*entered),
                    Input::Press(b) | Input::Release(b) | Input::Click(b) => f.call::<()>(*b),
                    Input::Wheel(dx, dy) | Input::Fling(dx, dy) => f.call::<()>((*dx, *dy)),
                };
                if let Err(e) = result {
                    log::error!("{name}: {e}");
                }
                return;
            }
            // Hover stays with its own node: entering a child is not
            // entering every ancestor again.
            if matches!(input, Input::Hover(_)) {
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

    /// Whether an interface has required the service (`"clock"`, `"niri"`,
    /// `"vitals"`): a service nobody required has no source running.
    pub fn wants(&self, service: &str) -> bool {
        self.wanted.borrow().contains(&service)
    }

    /// Evaluates `shell.luau` and returns where its surfaces go. Nothing is
    /// mounted until [`Script::mount`]: the runtime first makes a surface for
    /// each output the description asks for.
    pub fn evaluate(&mut self) -> Result<Vec<SurfaceSpec>, String> {
        // A fresh state for each load: modules are evaluated again and
        // nothing of the old chunks survives but the `g.state` signals.
        let lua = Lua::new();
        self.install(&lua).map_err(|e| e.to_string())?;
        let path = self.dir.join("shell.luau");
        let source = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // What the top-level code starts belongs to this load, and is stopped
        // if the load fails or when the next one is mounted.
        // One g.surface, or a list of them: a shell has several surfaces on
        // each output (a membrane on each edge, the floats, the wallpaper).
        let (scope, result) = Scope::new(|| -> Result<(Vec<Table>, Vec<SurfaceSpec>), String> {
            let returned: Table =
                lua.load(&source).set_name(format!("@{}", path.display())).eval().map_err(|e| e.to_string())?;
            let is_surface = |t: &Table| t.get::<Option<bool>>("__surface").ok().flatten() == Some(true);
            let surfaces: Vec<Table> = if is_surface(&returned) {
                vec![returned]
            } else {
                returned.sequence_values::<Table>().collect::<mlua::Result<_>>().map_err(|e| e.to_string())?
            };
            if surfaces.iter().any(|t| !is_surface(t)) {
                return Err("shell.luau must return g.surface { … }, or a list of them".into());
            }
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
        let (scope, result) = Scope::new(|| -> mlua::Result<Vec<genoma_scene::NodeId>> {
            let mut holders = Vec::new();
            for (index, (spec, output, scene, size)) in targets.iter().enumerate() {
                let Some(content) = contents.get(*spec) else { return Err(err("a surface with no description")) };
                let tree: Table = match content {
                    // f(output, { width, height }), the size when the
                    // compositor has said it.
                    LuaValue::Function(f) => {
                        let lua = f.weak_lua().upgrade();
                        let info = match size {
                            Some((w, h)) => {
                                let t = lua.create_table()?;
                                t.set("width", *w)?;
                                t.set("height", *h)?;
                                LuaValue::Table(t)
                            }
                            None => LuaValue::Nil,
                        };
                        f.call((output.clone(), info))?
                    }
                    LuaValue::Table(t) => t.clone(),
                    _ => return Err(err("g.surface holds one node, or a function returning one")),
                };
                let holder = scene.borrow_mut().create(Kind::Box, Role::Group);
                // The holder takes the surface's whole span, so content that
                // grows (a membrane's row) can reach the far edge.
                let _ = scene.borrow_mut().set(holder, Prop::Grow, Value::Number(1.0), None);
                mount(scene, &handlers, index, &tree, holder)?;
                holders.push(holder);
            }
            Ok(holders)
        });
        match result {
            Ok(holders) => {
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
        let g = lua.create_table()?;
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
        // g.computed(fn), g.animated(value), g.json_set(text, path, value):
        // an interface's own modules (Bioma's core/ singletons).
        g.set("computed", lua.create_function(|_, f: Function| Ok(values::computed(f)))?)?;
        let host = self.host.clone();
        g.set("animated", lua.create_function(move |_, v: LuaValue| values::animated(&host, v))?)?;
        g.set("json_set", lua.create_function(values::json_set)?)?;
        // g.effect(fn): runs it now and again whenever what it read changes;
        // stopped with the load. For what reacts rather than draws: the
        // timing set redefined when its speed changes, a theme transition
        // started when the palette does.
        g.set(
            "effect",
            lua.create_function(|_, f: Function| {
                Effect::new(move || {
                    if let Err(e) = f.call::<()>(()) {
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
                    let value = match source.call::<LuaValue>(()) {
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
        // g.batch(fn): the signals fn sets are heard once it returns, so a
        // binding reading several of them runs once, not once for each.
        g.set(
            "batch",
            lua.create_function(|_, f: Function| genoma_core::reactive::batch(|| f.call::<LuaValue>(())))?,
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
                let options = mlua::serde::SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false);
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
        for kind in ["box", "capsule", "text", "icon", "thread", "emitter", "each"] {
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
        // g.timing { open = { duration = 0.25, curve = { … } }, … }: the one
        // place a duration may be written.
        g.set(
            "timing",
            lua.create_function(|_, set: Table| {
                for pair in set.pairs::<String, Table>() {
                    let (name, entry) = pair?;
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
                    timing::define(&name, Timing { duration, curve, delay, spring });
                }
                Ok(())
            })?,
        )?;
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
        let cache: Table = lua.create_table()?;
        let require = lua.create_function(move |lua, name: String| -> mlua::Result<LuaValue> {
            if let Some(v) = cache.get::<Option<LuaValue>>(name.as_str())? {
                return Ok(v);
            }
            let value = match name.as_str() {
                "@genoma" => LuaValue::Table(g.clone()),
                "@genoma/clock" | "@genoma/niri" | "@genoma/vitals" | "@genoma/power" | "@genoma/network" | "@genoma/media" | "@genoma/audio" | "@genoma/bluetooth" | "@genoma/toplevels" | "@genoma/tray" | "@genoma/polkit" | "@genoma/notifications" => {
                    let service = &name["@genoma/".len()..];
                    let key: &'static str = match service {
                        "clock" => "clock",
                        "niri" => "niri",
                        "power" => "power",
                        "network" => "network",
                        "media" => "media",
                        "audio" => "audio",
                        "bluetooth" => "bluetooth",
                        "toplevels" => "toplevels",
                        "tray" => "tray",
                        "polkit" => "polkit",
                        "notifications" => "notifications",
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
                        "power" => lua.create_userdata(PowerRef(services.power.clone()))?,
                        "network" => lua.create_userdata(NetworkRef(services.network.clone()))?,
                        "media" => lua.create_userdata(MediaRef(services.media.clone()))?,
                        "audio" => lua.create_userdata(AudioRef(services.audio.clone()))?,
                        "bluetooth" => lua.create_userdata(BluetoothRef(services.bluetooth.clone()))?,
                        "toplevels" => lua.create_userdata(ToplevelsRef(services.toplevels.clone()))?,
                        "tray" => lua.create_userdata(TrayRef(services.tray.clone()))?,
                        "polkit" => lua.create_userdata(PolkitRef(services.polkit.clone()))?,
                        "notifications" => lua.create_userdata(NotificationsRef(services.notifications.clone()))?,
                        _ => lua.create_userdata(VitalsRef(services.vitals.clone()))?,
                    };
                    LuaValue::UserData(data)
                }
                other if other.starts_with('@') => return Err(err(format!("no module {other}"))),
                relative => {
                    if relative.contains("..") {
                        return Err(err(format!("{relative}: modules live inside the interface directory")));
                    }
                    let path = dir.join(format!("{relative}.luau"));
                    let source = std::fs::read_to_string(&path)
                        .map_err(|e| err(format!("{}: {e}", path.display())))?;
                    lua.load(&source).set_name(format!("@{}", path.display())).eval()?
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

fn surface_spec(t: &Table) -> mlua::Result<SurfaceSpec> {
    let anchor = match t.get::<LuaValue>("anchor")? {
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
        namespace: t.get::<Option<String>>("namespace")?.unwrap_or_else(|| "genoma".into()),
        all_outputs: matches!(t.get::<LuaValue>("outputs")?, LuaValue::String(ref s) if s.to_string_lossy() == "all"),
        outputs: match t.get::<LuaValue>("outputs")? {
            LuaValue::Table(list) => Some(list.sequence_values::<String>().collect::<mlua::Result<_>>()?),
            _ => None,
        },
    })
}

fn kind_of(t: &Table) -> mlua::Result<Kind> {
    Ok(match t.get::<String>("__kind").map_err(|_| err("a child that is not a node"))?.as_str() {
        // A keyed list is a box whose children are its rows.
        "box" | "each" => Kind::Box,
        "capsule" => Kind::Capsule,
        "text" => Kind::Text,
        "icon" => Kind::Icon,
        "thread" => Kind::Thread,
        "emitter" => Kind::Emitter,
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
        Ty::Choice(options) => match v {
            LuaValue::String(s) if options.contains(&s.to_string_lossy().as_str()) => {
                Ok(Value::Text(s.to_string_lossy()))
            }
            _ => Err(wrong(&format!("one of {}", options.join(", ")))),
        },
    }
}

/// Creates the node `t` describes under `parent`, with its bindings.
fn mount(
    scene: &Rc<RefCell<Scene>>,
    handlers: &Handlers,
    target: usize,
    t: &Table,
    parent: genoma_scene::NodeId,
) -> mlua::Result<genoma_scene::NodeId> {
    let kind = kind_of(t)?;
    let each = t.get::<Option<String>>("__kind")?.as_deref() == Some("each");
    let role = role_of(t.get("role")?)?;
    let node = scene.borrow_mut().create(kind, role);
    scene.borrow_mut().set_name(node, t.get("name")?);
    scene.borrow_mut().append(parent, node);

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
        if each && matches!(key.as_str(), "items" | "key" | "row") {
            continue;
        }
        if let Some(&name) = HANDLERS.iter().find(|h| **h == key) {
            let LuaValue::Function(f) = value else {
                return Err(err(format!("{key} wants a function")));
            };
            handlers.borrow_mut().entry((target, node)).or_default().insert(name, f);
            if !matches!(name, "on_outside" | "on_key") {
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
        keyed(scene.clone(), handlers.clone(), target, node, want("items")?, want("key")?, want("row")?);
        return Ok(node);
    }
    for child in t.sequence_values::<Table>() {
        mount(scene, handlers, target, &child?, node)?;
    }
    Ok(node)
}

/// One row of a keyed list: what it was mounted from, kept while its key is.
struct Row {
    key: String,
    root: genoma_scene::NodeId,
    scope: Scope,
    item: Signal<LuaValue>,
    index: Signal<f64>,
}

/// `g.each`: a row per item, by key. A key seen again keeps its row, which
/// is handed the new item and place through `item()` and `index()`, so its
/// bindings follow and nothing is mounted again; a new key mounts a row; a
/// key gone takes its row, its bindings and its handlers with it.
fn keyed(
    scene: Rc<RefCell<Scene>>,
    handlers: Handlers,
    target: usize,
    node: genoma_scene::NodeId,
    items: Function,
    key: Function,
    row: Function,
) {
    let rows: Rc<RefCell<Vec<Row>>> = Rc::default();
    // The rows are detached from the list's effect, or each run would
    // dispose them; the interface's scope disposes them instead.
    on_cleanup({
        let rows = rows.clone();
        move || {
            for r in rows.borrow_mut().drain(..) {
                r.scope.dispose();
            }
        }
    });
    Effect::new(move || {
        let list = match items.call::<Table>(()) {
            Ok(list) => list,
            Err(e) => {
                log::error!("g.each items: {e}");
                return;
            }
        };
        let values: Vec<LuaValue> = list.sequence_values::<LuaValue>().filter_map(Result::ok).collect();
        untracked(|| {
            let mut old = std::mem::take(&mut *rows.borrow_mut());
            let mut next = Vec::with_capacity(values.len());
            for (i, value) in values.into_iter().enumerate() {
                let k = match key.call::<LuaValue>(value.clone()).and_then(|k| k.to_string()) {
                    Ok(k) => k,
                    Err(e) => {
                        log::error!("g.each key: {e}");
                        continue;
                    }
                };
                if next.iter().any(|r: &Row| r.key == k) {
                    log::error!("g.each: key {k} twice; the second is not drawn");
                    continue;
                }
                let index = (i + 1) as f64;
                if let Some(at) = old.iter().position(|r| r.key == k) {
                    let r = old.swap_remove(at);
                    r.item.set(value);
                    r.index.set(index);
                    next.push(r);
                    continue;
                }
                let (scope, mounted) = detached(|| {
                    Scope::new(|| -> mlua::Result<_> {
                        let item = Signal::new(value);
                        let index = Signal::new(index);
                        let lua = row.weak_lua().upgrade();
                        let read_item = lua.create_function(move |_, ()| Ok(item.try_get().unwrap_or(LuaValue::Nil)))?;
                        let read_index = lua.create_function(move |_, ()| Ok(index.try_get().unwrap_or(0.0)))?;
                        let t: Table = row.call((read_item, read_index))?;
                        Ok((mount(&scene, &handlers, target, &t, node)?, item, index))
                    })
                });
                match mounted {
                    Ok((root, item, index)) => next.push(Row { key: k, root, scope, item, index }),
                    Err(e) => {
                        log::error!("g.each row {k}: {e}");
                        scope.dispose();
                    }
                }
            }
            for r in old {
                r.scope.dispose();
                let gone = scene.borrow().subtree(r.root);
                handlers.borrow_mut().retain(|(t, n), _| *t != target || !gone.contains(n));
                scene.borrow_mut().remove(r.root);
            }
            let order: Vec<_> = next.iter().map(|r| r.root).collect();
            scene.borrow_mut().set_children(node, &order);
            *rows.borrow_mut() = next;
        });
    });
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

fn bind(scene: Rc<RefCell<Scene>>, node: genoma_scene::NodeId, prop: Prop, f: Function, timing_name: Option<TimingName>) {
    Effect::new(move || {
        let value = match f.call::<LuaValue>(()) {
            Ok(v) => v,
            Err(e) => {
                log::error!("binding {prop:?}: {e}");
                return;
            }
        };
        match convert(prop, &value) {
            Ok(value) => {
                let timing = timing_name.as_ref().and_then(TimingName::resolve);
                if let Err(e) = scene.borrow_mut().set(node, prop, value, timing) {
                    log::error!("binding {prop:?}: {e}");
                }
            }
            Err(e) => log::error!("binding {prop:?}: {e}"),
        }
    });
}
