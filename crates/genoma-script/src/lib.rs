//! Genoma's interface language (PRD §7): Luau files that return trees, with
//! properties that are either values or bindings.
//!
//! A property set to a function is a binding. It runs inside an effect, so
//! whatever signal it reads (a service's field, a `g.state`) wakes it, and
//! only that. Everything a load creates belongs to one scope, and the next
//! load disposes it: a reload never leaves an old binding running.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use genoma_core::{
    reactive::{Effect, Scope, Signal, detached},
    timing::{self, Curve, Timing},
};
use genoma_scene::{Kind, Prop, Role, Scene, Value};
use genoma_services::clock::Clock;
use mlua::{Function, Lua, Table, UserData, UserDataFields, Value as LuaValue};

/// Where and how the shell's surface sits on the output.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceSpec {
    pub anchor: Vec<String>,
    /// Top, right, bottom, left.
    pub margin: [i32; 4],
    pub layer: String,
    pub output: Option<String>,
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
        fields.add_field_method_get("value", |lua, this| {
            this.0.try_get().unwrap_or(StateValue::Nil).into_lua(lua)
        });
        fields.add_field_method_set("value", |_, this, v: LuaValue| {
            this.0.set(StateValue::from_lua(&v));
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
    dir: PathBuf,
    scene: Rc<RefCell<Scene>>,
    scope: Option<Scope>,
    states: Rc<RefCell<HashMap<String, Signal<StateValue>>>>,
    clock: Rc<Clock>,
    clock_wanted: Rc<Cell<bool>>,
}

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

impl Script {
    pub fn new(dir: &Path, scene: Rc<RefCell<Scene>>, clock: Rc<Clock>) -> Script {
        Script {
            lua: Lua::new(),
            dir: dir.to_owned(),
            scene,
            scope: None,
            states: Rc::default(),
            clock,
            clock_wanted: Rc::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the interface reads the clock: if not, nothing ticks it.
    pub fn clock_wanted(&self) -> bool {
        self.clock_wanted.get()
    }

    /// Evaluates `shell.luau` and mounts its tree, replacing the last one.
    /// On an error the last tree stays as it was, and so does the scene.
    pub fn load(&mut self) -> Result<SurfaceSpec, String> {
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
        let content: Table = surface
            .get(1)
            .map_err(|_| "g.surface holds one node, its content".to_string())?;

        // Mount into a new scope first; only a tree that mounted replaces
        // the old one.
        let scene = self.scene.clone();
        let root = scene.borrow().root();
        let (scope, result) = Scope::new(|| {
            let holder = scene.borrow_mut().create(Kind::Box, Role::Group);
            mount(&scene, &content, holder).map(|_| holder)
        });
        match result {
            Ok(holder) => {
                if let Some(old) = self.scope.take() {
                    old.dispose();
                }
                let mut s = scene.borrow_mut();
                s.clear();
                s.append(root, holder);
                drop(s);
                self.scope = Some(scope);
                self.lua = lua;
                Ok(spec)
            }
            Err(e) => {
                scope.dispose();
                Err(e.to_string())
            }
        }
    }

    fn install(&self, lua: &Lua) -> mlua::Result<()> {
        let g = lua.create_table()?;
        for kind in ["box", "capsule", "text"] {
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
                    let duration: f64 = entry.get("duration")?;
                    let curve = match entry.get::<Option<Vec<f64>>>("curve")? {
                        Some(c) if c.len() == 4 => Curve { x1: c[0], y1: c[1], x2: c[2], y2: c[3] },
                        Some(_) => return Err(err(format!("timing {name}: a curve is four numbers"))),
                        None => Curve::LINEAR,
                    };
                    timing::define(&name, Timing { duration, curve });
                }
                Ok(())
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
        let clock = self.clock.clone();
        let wanted = self.clock_wanted.clone();
        let cache: Table = lua.create_table()?;
        let require = lua.create_function(move |lua, name: String| -> mlua::Result<LuaValue> {
            if let Some(v) = cache.get::<Option<LuaValue>>(name.as_str())? {
                return Ok(v);
            }
            let value = match name.as_str() {
                "@genoma" => LuaValue::Table(g.clone()),
                "@genoma/clock" => {
                    wanted.set(true);
                    LuaValue::UserData(lua.create_userdata(ClockRef(clock.clone()))?)
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
    })
}

fn kind_of(t: &Table) -> mlua::Result<Kind> {
    Ok(match t.get::<String>("__kind").map_err(|_| err("a child that is not a node"))?.as_str() {
        "box" => Kind::Box,
        "capsule" => Kind::Capsule,
        "text" => Kind::Text,
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

/// A Luau value as the property wants it.
fn convert(prop: Prop, v: &LuaValue) -> Result<Value, String> {
    let number = || match v {
        LuaValue::Integer(i) => Ok(*i as f32),
        LuaValue::Number(n) => Ok(*n as f32),
        _ => Err(format!("{prop:?} wants a number")),
    };
    match prop {
        Prop::Width | Prop::Height | Prop::MinWidth | Prop::Padding | Prop::PaddingX
        | Prop::PaddingY | Prop::Gap | Prop::Radius | Prop::FontSize => number().map(Value::Number),
        Prop::Fill | Prop::RimTop | Prop::RimBottom | Prop::Color => match v {
            LuaValue::String(s) => color(&s.to_string_lossy())
                .map(Value::Color)
                .ok_or_else(|| format!("{prop:?}: colours are \"#rrggbb\" or \"#rrggbbaa\"")),
            _ => Err(format!("{prop:?} wants a colour string")),
        },
        Prop::Present | Prop::Tabular => match v {
            LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
            LuaValue::Nil => Ok(Value::Bool(false)),
            _ => Err(format!("{prop:?} wants a boolean")),
        },
        Prop::Text => match v {
            LuaValue::String(s) => Ok(Value::Text(s.to_string_lossy())),
            LuaValue::Integer(i) => Ok(Value::Text(i.to_string())),
            LuaValue::Number(n) => Ok(Value::Text(n.to_string())),
            _ => Err("text wants a string".into()),
        },
        Prop::Direction | Prop::Align | Prop::Justify | Prop::Font => match v {
            LuaValue::String(s) => Ok(Value::Text(s.to_string_lossy())),
            _ => Err(format!("{prop:?} wants a string")),
        },
    }
}

/// Creates the node `t` describes under `parent`, with its bindings.
fn mount(scene: &Rc<RefCell<Scene>>, t: &Table, parent: genoma_scene::NodeId) -> mlua::Result<()> {
    let kind = kind_of(t)?;
    let role = role_of(t.get("role")?)?;
    let node = scene.borrow_mut().create(kind, role);
    scene.borrow_mut().set_name(node, t.get("name")?);
    scene.borrow_mut().append(parent, node);

    // transition = { fill = "theme" }: names from the timing set, never
    // numbers. Checked here, at load, not when the value first moves.
    let mut transitions: HashMap<Prop, String> = HashMap::new();
    if let Some(table) = t.get::<Option<Table>>("transition")? {
        for pair in table.pairs::<String, LuaValue>() {
            let (key, value) = pair?;
            let prop = Prop::from_name(&key).ok_or_else(|| err(format!("transition: no property {key}")))?;
            let name = match value {
                LuaValue::String(s) => s.to_string_lossy(),
                _ => {
                    return Err(err(format!(
                        "transition.{key}: durations come from the timing set; name an entry"
                    )));
                }
            };
            if timing::get(&name).is_none() {
                return Err(err(format!("transition.{key}: no timing named {name}")));
            }
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
        mount(scene, &child?, node)?;
    }
    Ok(())
}

/// A property that follows a function: an effect that calls it and sets the
/// result, again whenever something it read changes.
fn bind(scene: Rc<RefCell<Scene>>, node: genoma_scene::NodeId, prop: Prop, f: Function, timing_name: Option<String>) {
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
                let timing = timing_name.as_deref().and_then(timing::get);
                if let Err(e) = scene.borrow_mut().set(node, prop, value, timing) {
                    log::error!("binding {prop:?}: {e}");
                }
            }
            Err(e) => log::error!("binding {prop:?}: {e}"),
        }
    });
}
