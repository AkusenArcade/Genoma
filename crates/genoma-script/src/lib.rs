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

pub mod dbus;
pub mod io;

use genoma_core::{
    reactive::{Effect, Scope, Signal, detached, on_cleanup, untracked},
    timing::{self, Curve, Timing},
};
use genoma_scene::{Kind, Prop, Role, Scene, Ty, Value};
use genoma_services::{clock::Clock, niri::Niri, power::Power, vitals::Vitals};
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
    /// `"all"` for one surface on every output, a list of output names, or
    /// none for a single surface (on `output`, or the compositor's choice).
    pub outputs: Option<Vec<String>>,
    pub all_outputs: bool,
    /// The layer surface's namespace: what the compositor's rules match.
    /// niri decides blur xray by it (Bioma's `bioma.kdl`), so a surface keeps
    /// its compositor behaviour by keeping its name.
    pub namespace: String,
}

/// A `g.state` value: what a signal can hold across reloads.
#[derive(Clone, Debug, PartialEq)]
enum StateValue {
    Nil,
    Bool(bool),
    Number(f64),
    Text(String),
}

impl StateValue {
    fn from_lua(v: &LuaValue) -> StateValue {
        match v {
            LuaValue::Boolean(b) => StateValue::Bool(*b),
            LuaValue::Integer(i) => StateValue::Number(*i as f64),
            LuaValue::Number(n) => StateValue::Number(*n),
            LuaValue::String(s) => StateValue::Text(s.to_string_lossy()),
            _ => StateValue::Nil,
        }
    }

    fn into_lua(self, lua: &Lua) -> mlua::Result<LuaValue> {
        Ok(match self {
            StateValue::Nil => LuaValue::Nil,
            StateValue::Bool(b) => LuaValue::Boolean(b),
            StateValue::Number(n) => LuaValue::Number(n),
            StateValue::Text(t) => LuaValue::String(lua.create_string(&t)?),
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
        fields.add_field_function_set("value", |_, ud: mlua::AnyUserData, v: LuaValue| {
            let signal = ud.borrow::<State>()?.0;
            signal.set(StateValue::from_lua(&v));
            Ok(())
        });
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
}

struct NiriRef(Rc<Niri>);

impl UserData for NiriRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("title", |_, this| Ok(this.0.title.get()));
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
    pending: Option<(Lua, Table, Scope)>,
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
}

/// One tree to mount: the output it is for, and the scene it goes into.
pub type Target = (Option<String>, Rc<RefCell<Scene>>);

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
        }
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

    /// Whether an interface has required the service (`"clock"`, `"niri"`,
    /// `"vitals"`): a service nobody required has no source running.
    pub fn wants(&self, service: &str) -> bool {
        self.wanted.borrow().contains(&service)
    }

    /// Evaluates `shell.luau` and returns where its surfaces go. Nothing is
    /// mounted until [`Script::mount`]: the runtime first makes a surface for
    /// each output the description asks for.
    pub fn evaluate(&mut self) -> Result<SurfaceSpec, String> {
        // A fresh state for each load: modules are evaluated again and
        // nothing of the old chunks survives but the `g.state` signals.
        let lua = Lua::new();
        self.install(&lua).map_err(|e| e.to_string())?;
        let path = self.dir.join("shell.luau");
        let source = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // What the top-level code starts belongs to this load, and is stopped
        // if the load fails or when the next one is mounted.
        let (scope, result) = Scope::new(|| -> Result<(Table, SurfaceSpec), String> {
            let surface: Table =
                lua.load(&source).set_name(format!("@{}", path.display())).eval().map_err(|e| e.to_string())?;
            if surface.get::<Option<bool>>("__surface").ok().flatten() != Some(true) {
                return Err("shell.luau must return g.surface { … }".into());
            }
            let spec = surface_spec(&surface).map_err(|e| e.to_string())?;
            Ok((surface, spec))
        });
        match result {
            Ok((surface, spec)) => {
                if let Some((_, _, stale)) = self.pending.replace((lua, surface, scope)) {
                    stale.dispose();
                }
                Ok(spec)
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
        let Some((lua, surface, io_scope)) = self.pending.take() else { return Err("nothing evaluated".into()) };
        let content: LuaValue = match surface.get(1) {
            Ok(content) => content,
            Err(e) => {
                io_scope.dispose();
                return Err(e.to_string());
            }
        };
        if matches!(content, LuaValue::Table(_)) && targets.len() > 1 {
            io_scope.dispose();
            return Err("g.surface for several outputs holds a function of the output".into());
        }
        let handlers: Handlers = Rc::default();
        let (scope, result) = Scope::new(|| -> mlua::Result<Vec<genoma_scene::NodeId>> {
            let mut holders = Vec::new();
            for (index, (output, scene)) in targets.iter().enumerate() {
                let tree: Table = match &content {
                    LuaValue::Function(f) => f.call(output.clone())?,
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
                for ((_, scene), holder) in targets.iter().zip(holders) {
                    let mut s = scene.borrow_mut();
                    let root = s.root();
                    s.clear();
                    s.append(root, holder);
                    s.set_icon_dirs(self.icon_dirs.borrow().clone());
                }
                self.scenes = targets.iter().map(|(_, s)| s.clone()).collect();
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
        // The building blocks of a service written in Luau (PRD §9.2).
        let host = self.host.clone();
        g.set("timer", lua.create_function(move |_, t: Table| io::timer(&host, t))?)?;
        let host = self.host.clone();
        g.set("process", lua.create_function(move |_, t: Table| io::process(&host, t))?)?;
        let host = self.host.clone();
        g.set("file", lua.create_function(move |_, t: Table| io::file(&host, t))?)?;
        let host = self.host.clone();
        g.set("socket", lua.create_function(move |_, t: Table| io::socket(&host, t))?)?;
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
        let f = get_dbus;
        dbus_table.set("watch", lua.create_function(move |_, t: Table| dbus::watch_from(&f()?, t))?)?;
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
                "@genoma/clock" | "@genoma/niri" | "@genoma/vitals" | "@genoma/power" => {
                    let service = &name["@genoma/".len()..];
                    let key: &'static str = match service {
                        "clock" => "clock",
                        "niri" => "niri",
                        "power" => "power",
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
        reserve: t.get::<Option<bool>>("reserve")?.unwrap_or(false),
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
