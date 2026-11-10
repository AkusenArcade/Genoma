//! `g.dbus` (PRD §9.2): call a method, get and set a property, watch a
//! signal, on either bus, from Luau.
//!
//! The work runs on the bus thread. What crosses between it and the main
//! thread is a [`Dyn`], a plain value, so neither side holds the other's
//! types: arguments go as Luau values plus the D-Bus signature the method
//! wants (`"su"`, `"a{sv}"`), and replies and signals come back as Luau values,
//! delivered to the callback they were asked for by its number.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    str::FromStr,
};

use calloop::channel::Sender;
use futures_util::StreamExt;
use genoma_core::reactive::{current_owner, on_cleanup, owned_by};
use genoma_services::bus::{self, Bus};
use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value as LuaValue};
use zbus::{
    MatchRule, MessageStream,
    message::Type,
    wire::{Array, Dict, ObjectPath, Signature, Structure, StructureBuilder, Variant},
};

use crate::io::HostRef;

/// A value on its way between Luau and the bus.
#[derive(Clone, Debug)]
pub enum Dyn {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    List(Vec<Dyn>),
    Map(Vec<(Dyn, Dyn)>),
    /// A value with the signature it takes inside a variant, from
    /// `g.dbus.variant("y", 2)`: a hint whose type the receiver checks.
    Typed(String, Box<Dyn>),
}

/// A reply, an error or a signal, for callback `id`.
pub struct Delivery {
    pub id: u64,
    pub result: Result<Vec<Dyn>, String>,
    /// The last for this callback: it is dropped after.
    pub last: bool,
}

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

// ---- Luau ↔ Dyn, on the main thread ----------------------------------------

fn from_lua(v: &LuaValue) -> Dyn {
    match v {
        LuaValue::Boolean(b) => Dyn::Bool(*b),
        LuaValue::Integer(i) => Dyn::Int(*i),
        LuaValue::Number(n) => Dyn::Num(*n),
        LuaValue::String(s) => Dyn::Str(s.to_string_lossy()),
        LuaValue::Table(t) => {
            if let Ok(Some(signature)) = t.raw_get::<Option<String>>("__dbus_signature") {
                let value = t.raw_get::<LuaValue>("__dbus_value").unwrap_or(LuaValue::Nil);
                return Dyn::Typed(signature, Box::new(from_lua(&value)));
            }
            let n = t.raw_len();
            let pairs: Vec<(LuaValue, LuaValue)> = t.pairs::<LuaValue, LuaValue>().filter_map(Result::ok).collect();
            if n > 0 && pairs.len() == n {
                Dyn::List((1..=n).map(|i| from_lua(&t.raw_get::<LuaValue>(i).unwrap_or(LuaValue::Nil))).collect())
            } else {
                Dyn::Map(pairs.iter().map(|(k, v)| (from_lua(k), from_lua(v))).collect())
            }
        }
        _ => Dyn::Nil,
    }
}

fn to_lua(lua: &Lua, d: &Dyn) -> mlua::Result<LuaValue> {
    Ok(match d {
        Dyn::Nil => LuaValue::Nil,
        Dyn::Bool(b) => LuaValue::Boolean(*b),
        Dyn::Int(i) => LuaValue::Integer(*i),
        Dyn::Num(n) => LuaValue::Number(*n),
        Dyn::Str(s) => LuaValue::String(lua.create_string(s)?),
        Dyn::List(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, item)?)?;
            }
            LuaValue::Table(t)
        }
        Dyn::Map(pairs) => {
            let t = lua.create_table()?;
            for (k, v) in pairs {
                t.raw_set(to_lua(lua, k)?, to_lua(lua, v)?)?;
            }
            LuaValue::Table(t)
        }
        Dyn::Typed(_, value) => to_lua(lua, value)?,
    })
}

// ---- Dyn ↔ the wire, on the bus thread -------------------------------------

fn int(d: &Dyn) -> Result<i64, String> {
    match d {
        Dyn::Int(i) => Ok(*i),
        Dyn::Num(n) if n.fract() == 0.0 => Ok(*n as i64),
        other => Err(format!("{other:?} is not an integer")),
    }
}

fn text(d: &Dyn) -> Result<String, String> {
    match d {
        Dyn::Str(s) => Ok(s.clone()),
        other => Err(format!("{other:?} is not a string")),
    }
}

/// The signature a Luau value takes inside a variant (`v`): what a hint in
/// `a{sv}` usually is.
fn guess(d: &Dyn) -> Signature {
    let s = match d {
        Dyn::Bool(_) => "b",
        Dyn::Int(i) if i32::try_from(*i).is_ok() => "i",
        Dyn::Int(_) => "x",
        Dyn::Num(_) => "d",
        Dyn::List(items) if items.iter().all(|i| matches!(i, Dyn::Str(_))) => "as",
        Dyn::List(_) => "av",
        Dyn::Map(_) => "a{sv}",
        _ => "s",
    };
    Signature::from_str(s).expect("a fixed signature")
}

fn to_wire(d: &Dyn, sig: &Signature) -> Result<Variant<'static>, String> {
    let range = |what: &str| format!("{what} out of range");
    if let Dyn::Typed(inner, value) = d {
        let inner = Signature::from_str(inner).map_err(|e| format!("signature {inner}: {e}"))?;
        return if matches!(sig, Signature::Variant) {
            Ok(Variant::Variant(Box::new(to_wire(value, &inner)?)))
        } else {
            to_wire(value, sig)
        };
    }
    Ok(match sig {
        Signature::U8 => Variant::U8(u8::try_from(int(d)?).map_err(|_| range("byte"))?),
        Signature::Bool => match d {
            Dyn::Bool(b) => Variant::Bool(*b),
            other => return Err(format!("{other:?} is not a boolean")),
        },
        Signature::I16 => Variant::I16(i16::try_from(int(d)?).map_err(|_| range("int16"))?),
        Signature::U16 => Variant::U16(u16::try_from(int(d)?).map_err(|_| range("uint16"))?),
        Signature::I32 => Variant::I32(i32::try_from(int(d)?).map_err(|_| range("int32"))?),
        Signature::U32 => Variant::U32(u32::try_from(int(d)?).map_err(|_| range("uint32"))?),
        Signature::I64 => Variant::I64(int(d)?),
        Signature::U64 => Variant::U64(u64::try_from(int(d)?).map_err(|_| range("uint64"))?),
        Signature::F64 => match d {
            Dyn::Num(n) => Variant::F64(*n),
            Dyn::Int(i) => Variant::F64(*i as f64),
            other => return Err(format!("{other:?} is not a number")),
        },
        Signature::Str => Variant::from(text(d)?),
        Signature::ObjectPath => {
            Variant::ObjectPath(ObjectPath::try_from(text(d)?).map_err(|e| e.to_string())?)
        }
        Signature::Signature => Variant::Signature(Signature::from_str(&text(d)?).map_err(|e| e.to_string())?),
        Signature::Variant => Variant::Variant(Box::new(to_wire(d, &guess(d))?)),
        Signature::Array(child) => {
            let items: &[Dyn] = match d {
                Dyn::List(items) => items,
                Dyn::Map(pairs) if pairs.is_empty() => &[],
                other => return Err(format!("{other:?} is not a list")),
            };
            let mut array = Array::new(child);
            for item in items {
                array.append(to_wire(item, child)?).map_err(|e| e.to_string())?;
            }
            Variant::Array(array)
        }
        Signature::Dict { key, value } => {
            let mut dict = Dict::new(key, value);
            if let Dyn::Map(pairs) = d {
                for (k, v) in pairs {
                    dict.append(to_wire(k, key)?, to_wire(v, value)?).map_err(|e| e.to_string())?;
                }
            } else if !matches!(d, Dyn::List(l) if l.is_empty()) {
                return Err(format!("{d:?} is not a table of pairs"));
            }
            Variant::Dict(dict)
        }
        Signature::Structure(fields) => {
            let Dyn::List(items) = d else { return Err(format!("{d:?} is not a list of fields")) };
            let fields: Vec<&Signature> = fields.iter().collect();
            if items.len() != fields.len() {
                return Err(format!("{} fields for a structure of {}", items.len(), fields.len()));
            }
            let mut builder = StructureBuilder::new();
            for (item, field) in items.iter().zip(fields) {
                builder = builder.append_field(to_wire(item, field)?);
            }
            Variant::Structure(builder.build().map_err(|e| e.to_string())?)
        }
        other => return Err(format!("signature {other} is not supported")),
    })
}

fn from_wire(v: &Variant<'_>) -> Dyn {
    match v {
        Variant::U8(n) => Dyn::Int(*n as i64),
        Variant::Bool(b) => Dyn::Bool(*b),
        Variant::I16(n) => Dyn::Int(*n as i64),
        Variant::U16(n) => Dyn::Int(*n as i64),
        Variant::I32(n) => Dyn::Int(*n as i64),
        Variant::U32(n) => Dyn::Int(*n as i64),
        Variant::I64(n) => Dyn::Int(*n),
        Variant::U64(n) => Dyn::Int(*n as i64),
        Variant::F64(n) => Dyn::Num(*n),
        Variant::Str(s) => Dyn::Str(s.to_string()),
        Variant::Signature(s) => Dyn::Str(s.to_string()),
        Variant::ObjectPath(p) => Dyn::Str(p.as_str().to_owned()),
        Variant::Variant(inner) => from_wire(inner),
        Variant::Array(a) => Dyn::List(a.inner().iter().map(from_wire).collect()),
        Variant::Dict(d) => Dyn::Map(d.iter().map(|(k, v)| (from_wire(k), from_wire(v))).collect()),
        Variant::Structure(s) => Dyn::List(s.fields().iter().map(from_wire).collect()),
        _ => Dyn::Nil,
    }
}

/// A message's arguments, in order. A body of one argument is read as a
/// structure of one field: on the wire the two are the same bytes, since a
/// body starts aligned.
fn arguments(message: &zbus::Message) -> Result<Vec<Dyn>, String> {
    let body = message.body();
    let signature = match body.signature() {
        Signature::Unit => return Ok(Vec::new()),
        s @ Signature::Structure(_) => s.clone(),
        one => Signature::structure([one.clone()]),
    };
    let (s, _): (Structure<'_>, usize) =
        body.data().deserialize_for_dynamic_signature(&signature).map_err(|e| e.to_string())?;
    Ok(s.fields().iter().map(from_wire).collect())
}

/// The arguments as one body: a structure of them, which zbus sends as the
/// arguments themselves.
fn body(signature: &str, args: &[Dyn]) -> Result<Option<Structure<'static>>, String> {
    if signature.is_empty() {
        return if args.is_empty() { Ok(None) } else { Err("arguments without a signature".into()) };
    }
    let sig = Signature::from_str(&format!("({signature})")).map_err(|e| format!("signature {signature}: {e}"))?;
    match to_wire(&Dyn::List(args.to_vec()), &sig)? {
        Variant::Structure(s) => Ok(Some(s)),
        _ => unreachable!("a structure's signature makes a structure"),
    }
}

async fn connection(system: bool) -> zbus::Result<zbus::Connection> {
    if system { bus::system().await } else { bus::session().await }
}

struct Call {
    system: bool,
    destination: String,
    path: String,
    interface: String,
    method: String,
    signature: String,
    args: Vec<Dyn>,
}

async fn call(c: Call) -> Result<Vec<Dyn>, String> {
    let conn = connection(c.system).await.map_err(|e| e.to_string())?;
    let body = body(&c.signature, &c.args)?;
    let reply = match &body {
        Some(b) => conn.call_method(Some(c.destination.as_str()), c.path.as_str(), Some(c.interface.as_str()), c.method.as_str(), b).await,
        None => conn.call_method(Some(c.destination.as_str()), c.path.as_str(), Some(c.interface.as_str()), c.method.as_str(), &()).await,
    };
    arguments(&reply.map_err(|e| e.to_string())?)
}

// ---- The Luau side ---------------------------------------------------------

/// The callbacks waiting for the bus, by number, and the channel back.
pub struct Dbus {
    bus: Bus,
    sender: Sender<Delivery>,
    /// Each with whether it answers once, and the owner it was asked under:
    /// what its answer starts belongs to the same load.
    waiting: RefCell<HashMap<u64, (Function, bool, Option<usize>)>>,
    next: Cell<u64>,
}

impl Dbus {
    /// Made on first use, with its channel into the loop.
    pub fn new(host: &HostRef) -> mlua::Result<Rc<Dbus>> {
        let bus = Bus::shared().map_err(|e| err(format!("g.dbus: no bus thread: {e}")))?;
        let cell: Rc<RefCell<Option<std::rc::Weak<Dbus>>>> = Rc::default();
        let weak = cell.clone();
        let sender = host
            .channel(Box::new(move |delivery: Delivery| {
                let Some(dbus) = weak.borrow().as_ref().and_then(|w| w.upgrade()) else { return };
                dbus.deliver(delivery);
            }))
            .ok_or_else(|| err("g.dbus: no channel into the loop"))?;
        let dbus = Rc::new(Dbus { bus, sender, waiting: RefCell::default(), next: Cell::new(1) });
        *cell.borrow_mut() = Some(Rc::downgrade(&dbus));
        Ok(dbus)
    }

    fn deliver(&self, delivery: Delivery) {
        let (f, once, owner) = match self.waiting.borrow().get(&delivery.id) {
            Some((f, once, owner)) => (f.clone(), *once, *owner),
            None => return,
        };
        // Each load has its own Luau state: the callback's own.
        let lua = f.weak_lua().upgrade();
        let lua = &lua;
        if once || delivery.last {
            self.waiting.borrow_mut().remove(&delivery.id);
        }
        let result = match delivery.result {
            Ok(values) => values
                .iter()
                .map(|v| to_lua(lua, v))
                .collect::<mlua::Result<Vec<_>>>()
                .map(|mut values| {
                    if once {
                        values.insert(0, LuaValue::Boolean(true));
                    }
                    owned_by(owner, || f.call::<()>(mlua::MultiValue::from_iter(values)))
                }),
            Err(e) => Ok(owned_by(owner, || f.call::<()>((false, e)))),
        };
        if let Err(e) | Ok(Err(e)) = result {
            log::error!("g.dbus: {e}");
        }
    }

    /// A number for `f`, called with each delivery's values until the last
    /// (`g.pam` and the like, which share this channel into the loop).
    pub(crate) fn listen(self: &Rc<Self>, f: Function) -> (u64, Sender<Delivery>) {
        (self.wait(f, false), self.sender.clone())
    }

    /// A number for `f`, dropped with the load that asked.
    fn wait(self: &Rc<Self>, f: Function, once: bool) -> u64 {
        let id = self.next.get();
        self.next.set(id + 1);
        self.waiting.borrow_mut().insert(id, (f, once, current_owner()));
        let me = Rc::downgrade(self);
        on_cleanup(move || {
            if let Some(me) = me.upgrade() {
                me.waiting.borrow_mut().remove(&id);
            }
        });
        id
    }

    fn run(self: &Rc<Self>, reply: Option<Function>, work: impl std::future::Future<Output = Result<Vec<Dyn>, String>> + Send + 'static) {
        let id = reply.map(|f| self.wait(f, true));
        let sender = self.sender.clone();
        self.bus.spawn("g.dbus", async move {
            let result = work.await;
            match id {
                Some(id) => {
                    let _ = sender.send(Delivery { id, result, last: true });
                }
                None => {
                    if let Err(e) = result {
                        log::warn!("g.dbus: {e}");
                    }
                }
            }
            Ok(())
        });
    }
}

fn which_bus(t: &Table) -> mlua::Result<bool> {
    match t.get::<Option<String>>("bus")?.as_deref().unwrap_or("session") {
        "system" => Ok(true),
        "session" => Ok(false),
        other => Err(err(format!("g.dbus: bus is \"session\" or \"system\", not {other}"))),
    }
}

fn args(t: &Table) -> mlua::Result<Vec<Dyn>> {
    Ok(match t.get::<Option<Table>>("args")? {
        Some(a) => match from_lua(&LuaValue::Table(a)) {
            Dyn::List(items) => items,
            Dyn::Map(pairs) if pairs.is_empty() => Vec::new(),
            _ => return Err(err("g.dbus: args is a list")),
        },
        None => Vec::new(),
    })
}

/// `g.dbus.call { bus, destination, path, interface, method, signature,
/// args, reply = function(ok, …) end }`: `reply` gets true and the results,
/// or false and the error.
pub fn call_from(dbus: &Rc<Dbus>, t: Table) -> mlua::Result<()> {
    let c = Call {
        system: which_bus(&t)?,
        destination: t.get("destination")?,
        path: t.get("path")?,
        interface: t.get("interface")?,
        method: t.get("method")?,
        signature: t.get::<Option<String>>("signature")?.unwrap_or_default(),
        args: args(&t)?,
    };
    dbus.run(t.get("reply")?, call(c));
    Ok(())
}

/// `g.dbus.get { bus, destination, path, interface, property, reply =
/// function(ok, value) end }`.
pub fn get_from(dbus: &Rc<Dbus>, t: Table) -> mlua::Result<()> {
    let c = Call {
        system: which_bus(&t)?,
        destination: t.get("destination")?,
        path: t.get("path")?,
        interface: "org.freedesktop.DBus.Properties".into(),
        method: "Get".into(),
        signature: "ss".into(),
        args: vec![Dyn::Str(t.get("interface")?), Dyn::Str(t.get("property")?)],
    };
    dbus.run(t.get("reply")?, call(c));
    Ok(())
}

/// `g.dbus.set { bus, destination, path, interface, property, signature,
/// value, reply }`: `signature` is the property's type (`"b"`, `"s"`).
pub fn set_from(dbus: &Rc<Dbus>, t: Table) -> mlua::Result<()> {
    let system = which_bus(&t)?;
    let destination: String = t.get("destination")?;
    let path: String = t.get("path")?;
    let interface: String = t.get("interface")?;
    let property: String = t.get("property")?;
    let signature: String = t.get("signature")?;
    let value = from_lua(&t.get::<LuaValue>("value")?);
    dbus.run(t.get("reply")?, async move {
        let sig = Signature::from_str(&signature).map_err(|e| format!("signature {signature}: {e}"))?;
        let inner = to_wire(&value, &sig)?;
        let body = StructureBuilder::new()
            .append_field(Variant::from(interface))
            .append_field(Variant::from(property))
            .append_field(Variant::Variant(Box::new(inner)))
            .build()
            .map_err(|e| e.to_string())?;
        let conn = connection(system).await.map_err(|e| e.to_string())?;
        conn.call_method(Some(destination.as_str()), path.as_str(), Some("org.freedesktop.DBus.Properties"), "Set", &body)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Vec::new())
    });
    Ok(())
}

pub struct Watch {
    task: Rc<RefCell<Option<zruntime::Task<(), zruntime::Shared>>>>,
}

impl UserData for Watch {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("stop", |_, this, ()| {
            this.task.borrow_mut().take();
            Ok(())
        });
    }
}

/// `g.dbus.watch { bus, sender, path, interface, member, on_signal =
/// function(…) end }`: each matching signal's arguments. Any of the filters
/// may be left out.
pub fn watch_from(dbus: &Rc<Dbus>, t: Table) -> mlua::Result<Watch> {
    let system = which_bus(&t)?;
    let filter = |name: &str| t.get::<Option<String>>(name);
    let (sender_name, path, interface, member) = (filter("sender")?, filter("path")?, filter("interface")?, filter("member")?);
    let on_signal: Function = t.get("on_signal")?;
    let id = dbus.wait(on_signal, false);
    let out = dbus.sender.clone();
    let task = dbus.bus.task("g.dbus watch", async move {
        let result: Result<(), String> = async {
            let mut rule = MatchRule::builder().msg_type(Type::Signal);
            if let Some(s) = &sender_name {
                rule = rule.sender(s.as_str());
            }
            if let Some(p) = &path {
                rule = rule.path(p.as_str());
            }
            if let Some(i) = &interface {
                rule = rule.interface(i.as_str());
            }
            if let Some(m) = &member {
                rule = rule.member(m.as_str());
            }
            let rule = rule.build().map_err(|e| e.to_string())?;
            let conn = connection(system).await.map_err(|e| e.to_string())?;
            let mut stream = MessageStream::for_match_rule(rule, &conn, None).await.map_err(|e| e.to_string())?;
            while let Some(message) = stream.next().await {
                let Ok(message) = message else { continue };
                if let Ok(args) = arguments(&message) {
                    let _ = out.send(Delivery { id, result: Ok(args), last: false });
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            log::warn!("g.dbus watch: {e}");
        }
    });
    let task = Rc::new(RefCell::new(Some(task)));
    // The next load drops it, and dropping the task ends the stream.
    let t = task.clone();
    on_cleanup(move || {
        t.borrow_mut().take();
    });
    Ok(Watch { task })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Dyn {
        Dyn::Str(text.into())
    }

    /// What goes out comes back the same, through the bus's own types.
    fn round_trip(signature: &str, args: Vec<Dyn>) -> Vec<Dyn> {
        let body = body(signature, &args).expect("a body").expect("not empty");
        body.fields().iter().map(from_wire).collect()
    }

    #[test]
    fn a_hint_map_goes_out_as_a_dict_of_variants_and_comes_back() {
        let hints = Dyn::Map(vec![(s("urgency"), Dyn::Int(2)), (s("transient"), Dyn::Bool(true))]);
        let back = round_trip("sa{sv}u", vec![s("app"), hints, Dyn::Int(7)]);
        let [Dyn::Str(app), Dyn::Map(pairs), Dyn::Int(7)] = back.as_slice() else { panic!("{back:?}") };
        assert_eq!(app, "app");
        assert!(pairs.iter().any(|(k, v)| matches!((k, v), (Dyn::Str(k), Dyn::Int(2)) if k == "urgency")));
        assert!(pairs.iter().any(|(k, v)| matches!((k, v), (Dyn::Str(k), Dyn::Bool(true)) if k == "transient")));
    }

    #[test]
    fn a_variant_guesses_its_signature_unless_told() {
        assert_eq!(guess(&Dyn::Int(5)).to_string(), "i");
        assert_eq!(guess(&Dyn::Int(1 << 40)).to_string(), "x", "too big for int32");
        assert_eq!(guess(&Dyn::Num(0.5)).to_string(), "d");
        assert_eq!(guess(&Dyn::List(vec![s("a"), s("b")])).to_string(), "as");
        assert_eq!(guess(&Dyn::List(vec![Dyn::Int(1)])).to_string(), "av");
        // g.dbus.variant("y", 2): a byte, which a guess would never make.
        let typed = Dyn::Typed("y".into(), Box::new(Dyn::Int(2)));
        match to_wire(&typed, &Signature::Variant).unwrap() {
            Variant::Variant(inner) => assert!(matches!(*inner, Variant::U8(2))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn numbers_that_do_not_fit_are_refused_not_wrapped() {
        assert!(to_wire(&Dyn::Int(256), &Signature::U8).unwrap_err().contains("byte"));
        assert!(to_wire(&Dyn::Int(-1), &Signature::U32).is_err());
        assert!(to_wire(&Dyn::Num(1.5), &Signature::I32).is_err(), "not an integer");
        assert!(matches!(to_wire(&Dyn::Num(3.0), &Signature::I32).unwrap(), Variant::I32(3)), "a whole float is one");
        assert!(matches!(to_wire(&Dyn::Int(2), &Signature::F64).unwrap(), Variant::F64(f) if f == 2.0));
        assert!(to_wire(&s("x"), &Signature::Bool).is_err());
    }

    #[test]
    fn an_empty_table_is_an_empty_list_or_dict_and_structures_count_fields() {
        // Luau cannot tell an empty list from an empty map.
        assert!(matches!(to_wire(&Dyn::Map(vec![]), &Signature::from_str("as").unwrap()).unwrap(), Variant::Array(_)));
        assert!(matches!(to_wire(&Dyn::List(vec![]), &Signature::from_str("a{sv}").unwrap()).unwrap(), Variant::Dict(_)));
        let pair = Signature::from_str("(si)").unwrap();
        assert!(to_wire(&Dyn::List(vec![s("a")]), &pair).unwrap_err().contains("1 fields"));
        assert!(body("", &[]).unwrap().is_none());
        assert!(body("", &[Dyn::Int(1)]).is_err(), "arguments need a signature");
        assert!(to_wire(&s("not a path"), &Signature::ObjectPath).is_err());
    }

    #[test]
    fn a_luau_table_is_a_list_when_it_is_only_a_sequence() {
        let lua = Lua::new();
        let list: LuaValue = lua.load("return { 'a', 'b' }").eval().unwrap();
        assert!(matches!(from_lua(&list), Dyn::List(ref l) if l.len() == 2));
        let map: LuaValue = lua.load("return { 'a', key = 1 }").eval().unwrap();
        assert!(matches!(from_lua(&map), Dyn::Map(ref m) if m.len() == 2));
        let typed: LuaValue = lua.load("return { __dbus_signature = 'u', __dbus_value = 4 }").eval().unwrap();
        assert!(matches!(from_lua(&typed), Dyn::Typed(ref sig, _) if sig == "u"));
        // And back: a map's keys stay keys.
        let back = to_lua(&lua, &Dyn::Map(vec![(s("k"), Dyn::Num(1.5))])).unwrap();
        let LuaValue::Table(t) = back else { panic!() };
        assert_eq!(t.get::<f64>("k").unwrap(), 1.5);
    }
}
