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

use genoma_core::{
    reactive::{Effect, Scope, Signal, detached},
    timing::{self, Curve, Timing},
};
use genoma_scene::{Kind, Prop, Role, Scene, Ty, Value};
use genoma_services::{clock::Clock, niri::Niri, vitals::Vitals};
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
    /// A file evaluated and not yet mounted: its Luau state and the surface
    /// table it returned.
    pending: Option<(Lua, Table)>,
    dir: PathBuf,
    scope: Option<Scope>,
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
    pub fn new(dir: &Path, services: Services) -> Script {
        Script {
            lua: Lua::new(),
            pending: None,
            dir: dir.to_owned(),
            scope: None,
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
        let surface: Table = lua
            .load(&source)
            .set_name(format!("@{}", path.display()))
            .eval()
            .map_err(|e| e.to_string())?;
        if surface.get::<Option<bool>>("__surface").ok().flatten() != Some(true) {
            return Err("shell.luau must return g.surface { … }".into());
        }
        let spec = surface_spec(&surface).map_err(|e| e.to_string())?;
        self.pending = Some((lua, surface));
        Ok(spec)
    }

    /// Mounts the evaluated file's content into each target, replacing the
    /// last trees. Content that is a function is called with each target's
    /// output; a table is one tree for one surface. On an error the last
    /// trees stay as they were, and so do the scenes.
    pub fn mount(&mut self, targets: &[Target]) -> Result<(), String> {
        let Some((lua, surface)) = self.pending.take() else { return Err("nothing evaluated".into()) };
        let content: LuaValue = surface.get(1).map_err(|e| e.to_string())?;
        if matches!(content, LuaValue::Table(_)) && targets.len() > 1 {
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
                *self.handlers.borrow_mut() = std::mem::take(&mut *handlers.borrow_mut());
                Ok(())
            }
            Err(e) => {
                scope.dispose();
                Err(e.to_string())
            }
        }
    }

    fn install(&self, lua: &Lua) -> mlua::Result<()> {
        let g = lua.create_table()?;
        for kind in ["box", "capsule", "text", "icon", "thread"] {
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
                "@genoma/clock" | "@genoma/niri" | "@genoma/vitals" => {
                    let service = &name["@genoma/".len()..];
                    let key: &'static str = match service {
                        "clock" => "clock",
                        "niri" => "niri",
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
        all_outputs: matches!(t.get::<LuaValue>("outputs")?, LuaValue::String(ref s) if s.to_string_lossy() == "all"),
        outputs: match t.get::<LuaValue>("outputs")? {
            LuaValue::Table(list) => Some(list.sequence_values::<String>().collect::<mlua::Result<_>>()?),
            _ => None,
        },
    })
}

fn kind_of(t: &Table) -> mlua::Result<Kind> {
    Ok(match t.get::<String>("__kind").map_err(|_| err("a child that is not a node"))?.as_str() {
        "box" => Kind::Box,
        "capsule" => Kind::Capsule,
        "text" => Kind::Text,
        "icon" => Kind::Icon,
        "thread" => Kind::Thread,
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
fn mount(scene: &Rc<RefCell<Scene>>, handlers: &Handlers, target: usize, t: &Table, parent: genoma_scene::NodeId) -> mlua::Result<()> {
    let kind = kind_of(t)?;
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

    for child in t.sequence_values::<Table>() {
        mount(scene, handlers, target, &child?, node)?;
    }
    Ok(())
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
