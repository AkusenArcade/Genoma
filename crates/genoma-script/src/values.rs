//! Values for an interface's own modules, beside the nodes' properties:
//!
//! - `g.computed(fn)`: a value derived from others, recomputed only when one
//!   it read changed (a configuration merged from its two layers);
//! - `g.animated(value)`: a number or a colour the runtime carries to a new
//!   value over a timing set entry, read like any signal while it moves (a
//!   theme whose every derived colour follows its four roles in transit);
//! - `g.json_set(text, path, value)`: a JSON document with one key changed,
//!   its keys in their order, so a file a person also edits is rewritten
//!   with only what changed.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use calloop::RegistrationToken;
use genoma_core::{
    reactive::{Memo, Signal, batch, on_cleanup, untracked},
    timing::{self, Timing},
};
use mlua::{Function, IntoLua, Lua, LuaSerdeExt, UserData, UserDataFields, UserDataMethods, Value as LuaValue};

use crate::io::HostRef;

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

// ---- g.computed ------------------------------------------------------------

pub struct Computed(Memo<LuaValue>);

impl UserData for Computed {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("value", |_, this| Ok(this.0.try_get().unwrap_or(LuaValue::Nil)));
    }
}

/// `g.computed(function() … end)`: read as `.value`, by bindings too.
pub fn computed(f: Function) -> Computed {
    Computed(Memo::new(move || match f.call::<LuaValue>(()) {
        Ok(v) => v,
        Err(e) => {
            log::error!("g.computed: {e}");
            LuaValue::Nil
        }
    }))
}

// ---- g.animated ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Number(f64),
    Colour([f32; 4]),
}

fn parse_colour(text: &str) -> Option<[f32; 4]> {
    let hex = text.strip_prefix('#')?;
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    match hex.len() {
        6 => Some([channel(0)?, channel(2)?, channel(4)?, 1.0]),
        8 => Some([channel(0)?, channel(2)?, channel(4)?, channel(6)?]),
        _ => None,
    }
}

fn hex(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]), b(c[3]))
}

fn from_lua(v: &LuaValue) -> mlua::Result<Kind> {
    match v {
        LuaValue::Integer(i) => Ok(Kind::Number(*i as f64)),
        LuaValue::Number(n) => Ok(Kind::Number(*n)),
        LuaValue::String(s) => parse_colour(&s.to_string_lossy())
            .map(Kind::Colour)
            .ok_or_else(|| err(format!("g.animated: {} is neither a number nor #rrggbb[aa]", s.to_string_lossy()))),
        _ => Err(err("g.animated: a number or a colour")),
    }
}

fn mix(a: Kind, b: Kind, y: f64) -> Kind {
    match (a, b) {
        (Kind::Number(a), Kind::Number(b)) => Kind::Number(a + (b - a) * y),
        (Kind::Colour(a), Kind::Colour(b)) => {
            let mut c = [0.0; 4];
            for i in 0..4 {
                c[i] = a[i] + (b[i] - a[i]) * y as f32;
            }
            Kind::Colour(c)
        }
        (_, b) => b,
    }
}

/// The step between two readings while moving: about a frame at 120 Hz.
const TICK: Duration = Duration::from_millis(8);

struct Motion {
    from: Kind,
    to: Kind,
    start: Instant,
    timing: Timing,
}

struct Inner {
    value: Signal<Option<Kind>>,
    motion: RefCell<Option<Motion>>,
    host: HostRef,
}

// One ticker for every value in motion, stepping them together in a batch:
// the theme's four roles move as one, and what derives from them is
// computed once a step, not once a role.
thread_local! {
    static MOVING: RefCell<Vec<std::rc::Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    static TICKER: Cell<Option<RegistrationToken>> = const { Cell::new(None) };
}

fn start_ticker(host: &HostRef) {
    if TICKER.with(|t| t.get()).is_some() {
        return;
    }
    let token = host.timer(
        TICK,
        Box::new(|| {
            let moving: Vec<Rc<Inner>> = MOVING.with(|m| m.borrow().iter().filter_map(|w| w.upgrade()).collect());
            let still: Vec<std::rc::Weak<Inner>> =
                batch(|| moving.iter().filter(|i| i.step()).map(Rc::downgrade).collect());
            let more = !still.is_empty();
            MOVING.with(|m| *m.borrow_mut() = still);
            if more {
                Some(TICK)
            } else {
                TICKER.with(|t| t.set(None));
                None
            }
        }),
    );
    TICKER.with(|t| t.set(token));
}

impl Inner {
    fn now(&self) -> Option<Kind> {
        self.value.get_untracked().flatten()
    }

    /// One step of the motion; `false` once it has arrived.
    fn step(&self) -> bool {
        let mut motion = self.motion.borrow_mut();
        let Some(m) = motion.as_ref() else { return false };
        let elapsed = m.start.elapsed().as_secs_f64() - m.timing.delay;
        if elapsed < 0.0 {
            return true;
        }
        let t = (elapsed / m.timing.duration.max(1e-6)).clamp(0.0, 1.0);
        let value = mix(m.from, m.to, m.timing.curve.eval(t));
        let done = t >= 1.0;
        let target = m.to;
        if done {
            motion.take();
        }
        drop(motion);
        self.value.set(Some(if done { target } else { value }));
        !done
    }
}

pub struct Animated(Rc<Inner>);

impl UserData for Animated {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // .value: where it is now, a number or "#rrggbbaa".
        fields.add_field_method_get("value", |lua, this| match this.0.value.try_get().flatten() {
            Some(Kind::Number(n)) => n.into_lua(lua),
            Some(Kind::Colour(c)) => hex(c).into_lua(lua),
            None => Ok(LuaValue::Nil),
        });
        // .target: where it is going.
        fields.add_field_method_get("target", |lua, this| {
            let target = this.0.motion.borrow().as_ref().map(|m| m.to).or_else(|| this.0.now());
            match target {
                Some(Kind::Number(n)) => n.into_lua(lua),
                Some(Kind::Colour(c)) => hex(c).into_lua(lua),
                None => Ok(LuaValue::Nil),
            }
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // a:set(value, "theme"): there over that timing set entry; without one,
        // or with an entry that does not move, at once.
        methods.add_method("set", |_, this, (target, timing_name): (LuaValue, Option<String>)| {
            let to = from_lua(&target)?;
            let inner = &this.0;
            let timing = match timing_name.as_deref() {
                Some(name) => Some(timing::get(name).ok_or_else(|| err(format!("g.animated: no timing named {name}")))?),
                None => None,
            };
            let from = untracked(|| inner.now());
            let moving = match (timing, from) {
                (Some(t), Some(from)) if t.moves() && t.spring.is_none() && from != to => Some((t, from)),
                _ => None,
            };
            let Some((timing, from)) = moving else {
                inner.motion.borrow_mut().take();
                inner.value.set(Some(to));
                return Ok(());
            };
            *inner.motion.borrow_mut() = Some(Motion { from, to, start: Instant::now(), timing });
            MOVING.with(|m| {
                let mut m = m.borrow_mut();
                if !m.iter().any(|w| w.as_ptr() == Rc::as_ptr(inner)) {
                    m.push(Rc::downgrade(inner));
                }
            });
            start_ticker(&inner.host);
            Ok(())
        });
    }
}

/// `g.animated(value)`: a number or a colour, read as `.value`.
pub fn animated(host: &HostRef, initial: LuaValue) -> mlua::Result<Animated> {
    let value = from_lua(&initial)?;
    let inner = Rc::new(Inner { value: Signal::new(Some(value)), motion: RefCell::new(None), host: host.clone() });
    // Stopped with the load that made it: out of the ticker's list, which
    // stops by itself once nothing moves.
    let me = Rc::downgrade(&inner);
    on_cleanup(move || {
        if let Some(me) = me.upgrade() {
            me.motion.borrow_mut().take();
            MOVING.with(|m| m.borrow_mut().retain(|w| w.as_ptr() != Rc::as_ptr(&me)));
        }
    });
    Ok(Animated(inner))
}

// ---- g.json_set ------------------------------------------------------------

/// An empty object where the old document had a list becomes an empty list,
/// through objects by key and lists by place.
fn keep_shape(new: &mut serde_json::Value, old: &serde_json::Value) {
    use serde_json::Value as J;
    match (new, old) {
        (n @ J::Object(_), J::Array(_)) if n.as_object().is_some_and(|m| m.is_empty()) => *n = J::Array(Vec::new()),
        (J::Object(n), J::Object(o)) => {
            for (k, v) in n.iter_mut() {
                if let Some(ov) = o.get(k) {
                    keep_shape(v, ov);
                }
            }
        }
        (J::Array(n), J::Array(o)) => {
            for (v, ov) in n.iter_mut().zip(o.iter()) {
                keep_shape(v, ov);
            }
        }
        _ => {}
    }
}

/// `g.json_set(text, path, value)` or `g.json_set(text, { { path, value },
/// … })`: the document with those keys set, the others where they were, as
/// two-space JSON with a final newline (Bioma's `Config.setMany`). A key on
/// the way that is not an object becomes one.
///
/// Luau has one table for both, and an empty one says nothing about which:
/// it is written as the document already had it there, so a list emptied
/// stays a list, however deep in the value (`g.array` marks a new one).
pub fn json_set(lua: &Lua, (text, first, second): (Option<String>, LuaValue, LuaValue)) -> mlua::Result<String> {
    let mut doc: serde_json::Value = text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| err(format!("g.json_set: {e}")))?
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let pairs: Vec<(String, LuaValue)> = match first {
        LuaValue::String(path) => vec![(path.to_string_lossy(), second)],
        LuaValue::Table(list) => list
            .sequence_values::<mlua::Table>()
            .map(|pair| {
                let pair = pair?;
                Ok((pair.get::<String>(1)?, pair.get::<LuaValue>(2)?))
            })
            .collect::<mlua::Result<_>>()?,
        _ => return Err(err("g.json_set: a path and a value, or a list of pairs")),
    };
    for (path, value) in pairs {
        let value: serde_json::Value = lua.from_value(value)?;
        let keys: Vec<&str> = path.split('.').collect();
        let mut node = &mut doc;
        for key in &keys[..keys.len() - 1] {
            if !node.is_object() {
                *node = serde_json::Value::Object(Default::default());
            }
            let map = node.as_object_mut().expect("made an object above");
            let next = map.entry(key.to_string()).or_insert_with(|| serde_json::Value::Object(Default::default()));
            if !next.is_object() {
                *next = serde_json::Value::Object(Default::default());
            }
            node = next;
        }
        if !node.is_object() {
            *node = serde_json::Value::Object(Default::default());
        }
        let map = node.as_object_mut().expect("an object");
        let mut value = value;
        if let Some(old) = map.get(keys[keys.len() - 1]) {
            keep_shape(&mut value, old);
        }
        map.insert(keys[keys.len() - 1].to_string(), value);
    }
    let mut out = serde_json::to_string_pretty(&doc).map_err(|e| err(format!("g.json_set: {e}")))?;
    out.push('\n');
    Ok(out)
}
