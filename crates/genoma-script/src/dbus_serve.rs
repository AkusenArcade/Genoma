//! `g.dbus.serve { … }`: an object on the bus, its interfaces written in
//! Luau — MPRIS for a player, FileManager1 for a file manager.
//!
//! Each served object has a connection of its own, made on the bus
//! thread: the process's shared connection runs zbus's object server for
//! Genoma's own services, which would answer calls meant for this one. On
//! that connection one task merges three streams — the calls that arrive,
//! the answers Luau gives, and what Luau asks to send — so nothing waits
//! on anything else.
//!
//! A call goes to the main thread through `g.dbus`'s channel and comes back
//! with its answer; `Introspect` and `Ping` are answered on the bus thread
//! from the description, without a trip. `org.freedesktop.DBus.Properties`
//! is served from each property's `get` and `set`; `obj:changed { … }`
//! sends `PropertiesChanged` with the values as they are now.

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    str::FromStr,
    sync::{Arc, Mutex},
};

use futures_channel::mpsc::{UnboundedSender, unbounded};
use futures_util::StreamExt;
use genoma_core::reactive::on_cleanup;
use mlua::{Function, Lua, MultiValue, Table, UserData, UserDataMethods, Value as LuaValue};
use zbus::{
    MessageStream,
    message::Type,
    wire::{ObjectPath, Signature},
};

use crate::dbus::{Dbus, Dyn, arguments, body, from_lua, to_lua, which_bus};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const PEER: &str = "org.freedesktop.DBus.Peer";

/// An answer on its way back to the bus thread: the call it answers, and
/// the body's signature and values, or an error's name and message.
type Answer = (u32, Result<(String, Vec<Dyn>), (String, String)>);

/// What the main thread asks the bus thread to send.
enum Command {
    /// interface, member, signature, arguments.
    Signal(String, String, String, Vec<Dyn>),
}

/// One of the three streams the bus task listens to.
enum Event {
    Call(zbus::Result<zbus::Message>),
    Answer(Answer),
    Command(Command),
}

/// A signature's complete types, one by one: "sa{sv}x" is s, a{sv}, x.
fn complete_types(signature: &str) -> Result<Vec<String>, String> {
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    match Signature::from_str(&format!("({signature})")).map_err(|e| format!("signature {signature}: {e}"))? {
        Signature::Structure(fields) => Ok(fields.iter().map(|s| s.to_string()).collect()),
        other => Ok(vec![other.to_string()]),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// What `Introspect` answers, made once from the description.
fn introspection(interfaces: &Table) -> mlua::Result<String> {
    let mut xml = String::from(
        "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n \
         \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n<node>\n",
    );
    xml.push_str("  <interface name=\"org.freedesktop.DBus.Introspectable\"><method name=\"Introspect\"><arg type=\"s\" direction=\"out\"/></method></interface>\n");
    xml.push_str("  <interface name=\"org.freedesktop.DBus.Peer\"><method name=\"Ping\"/></interface>\n");
    xml.push_str(concat!(
        "  <interface name=\"org.freedesktop.DBus.Properties\">",
        "<method name=\"Get\"><arg type=\"s\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/><arg type=\"v\" direction=\"out\"/></method>",
        "<method name=\"GetAll\"><arg type=\"s\" direction=\"in\"/><arg type=\"a{sv}\" direction=\"out\"/></method>",
        "<method name=\"Set\"><arg type=\"s\" direction=\"in\"/><arg type=\"s\" direction=\"in\"/><arg type=\"v\" direction=\"in\"/></method>",
        "<signal name=\"PropertiesChanged\"><arg type=\"s\"/><arg type=\"a{sv}\"/><arg type=\"as\"/></signal>",
        "</interface>\n"
    ));
    for pair in interfaces.pairs::<String, Table>() {
        let (name, def) = pair?;
        xml.push_str(&format!("  <interface name=\"{}\">\n", escape(&name)));
        if let Some(methods) = def.get::<Option<Table>>("methods")? {
            for pair in methods.pairs::<String, Table>() {
                let (method, m) = pair?;
                xml.push_str(&format!("    <method name=\"{}\">", escape(&method)));
                for t in complete_types(&m.get::<Option<String>>("args")?.unwrap_or_default()).map_err(err)? {
                    xml.push_str(&format!("<arg type=\"{}\" direction=\"in\"/>", escape(&t)));
                }
                for t in complete_types(&m.get::<Option<String>>("returns")?.unwrap_or_default()).map_err(err)? {
                    xml.push_str(&format!("<arg type=\"{}\" direction=\"out\"/>", escape(&t)));
                }
                xml.push_str("</method>\n");
            }
        }
        if let Some(properties) = def.get::<Option<Table>>("properties")? {
            for pair in properties.pairs::<String, Table>() {
                let (property, p) = pair?;
                let signature: String = p.get("signature")?;
                let access = if p.get::<Option<Function>>("set")?.is_some() { "readwrite" } else { "read" };
                xml.push_str(&format!("    <property name=\"{}\" type=\"{}\" access=\"{access}\"/>\n", escape(&property), escape(&signature)));
            }
        }
        if let Some(signals) = def.get::<Option<Table>>("signals")? {
            for pair in signals.pairs::<String, String>() {
                let (signal, signature) = pair?;
                xml.push_str(&format!("    <signal name=\"{}\">", escape(&signal)));
                for t in complete_types(&signature).map_err(err)? {
                    xml.push_str(&format!("<arg type=\"{}\"/>", escape(&t)));
                }
                xml.push_str("</signal>\n");
            }
        }
        xml.push_str("  </interface>\n");
    }
    xml.push_str("</node>\n");
    Ok(xml)
}

/// A property's value now, with its signature, as a variant takes it.
fn property(interfaces: &Table, interface: &str, name: &str) -> Result<Dyn, (String, String)> {
    let unknown = || ("org.freedesktop.DBus.Error.UnknownProperty".to_owned(), format!("no property {interface}.{name}"));
    let failed = |e: mlua::Error| ("org.freedesktop.DBus.Error.Failed".to_owned(), e.to_string());
    let def = interfaces.get::<Option<Table>>(interface).map_err(failed)?.ok_or_else(unknown)?;
    let properties = def.get::<Option<Table>>("properties").map_err(failed)?.ok_or_else(unknown)?;
    let p = properties.get::<Option<Table>>(name).map_err(failed)?.ok_or_else(unknown)?;
    let signature: String = p.get("signature").map_err(failed)?;
    let get: Function = p.get("get").map_err(failed)?;
    let value: LuaValue = get.call(()).map_err(failed)?;
    Ok(Dyn::Typed(signature, Box::new(from_lua(&value))))
}

fn all_properties(interfaces: &Table, interface: &str) -> Result<Dyn, (String, String)> {
    let failed = |e: mlua::Error| ("org.freedesktop.DBus.Error.Failed".to_owned(), e.to_string());
    let mut out = Vec::new();
    if let Some(def) = interfaces.get::<Option<Table>>(interface).map_err(failed)?
        && let Some(properties) = def.get::<Option<Table>>("properties").map_err(failed)?
    {
        for pair in properties.pairs::<String, Table>() {
            let (name, _) = pair.map_err(failed)?;
            out.push((Dyn::Str(name.clone()), property(interfaces, interface, &name)?));
        }
    }
    Ok(Dyn::Map(out))
}

/// The main thread's answer to one call: what Luau says, or why not.
fn answer_call(lua: &Lua, interfaces: &Table, interface: &str, member: &str, args: &[Dyn]) -> Result<(String, Vec<Dyn>), (String, String)> {
    let failed = |e: mlua::Error| ("org.freedesktop.DBus.Error.Failed".to_owned(), e.to_string());
    let text = |i: usize| match args.get(i) {
        Some(Dyn::Str(s)) => Ok(s.clone()),
        _ => Err(("org.freedesktop.DBus.Error.InvalidArgs".to_owned(), "a name was expected".to_owned())),
    };
    if interface == PROPERTIES {
        return match member {
            "Get" => Ok(("v".into(), vec![property(interfaces, &text(0)?, &text(1)?)?])),
            "GetAll" => Ok(("a{sv}".into(), vec![all_properties(interfaces, &text(0)?)?])),
            "Set" => {
                let (iface, name) = (text(0)?, text(1)?);
                let set = interfaces
                    .get::<Option<Table>>(iface.as_str())
                    .map_err(failed)?
                    .and_then(|d| d.get::<Option<Table>>("properties").ok().flatten())
                    .and_then(|p| p.get::<Option<Table>>(name.as_str()).ok().flatten())
                    .and_then(|p| p.get::<Option<Function>>("set").ok().flatten())
                    .ok_or_else(|| ("org.freedesktop.DBus.Error.PropertyReadOnly".to_owned(), format!("{iface}.{name} cannot be set")))?;
                let value = to_lua(lua, args.get(2).unwrap_or(&Dyn::Nil)).map_err(failed)?;
                set.call::<()>(value).map_err(failed)?;
                Ok((String::new(), Vec::new()))
            }
            _ => Err(("org.freedesktop.DBus.Error.UnknownMethod".into(), format!("no method {interface}.{member}"))),
        };
    }
    let method = interfaces
        .get::<Option<Table>>(interface)
        .map_err(failed)?
        .and_then(|d| d.get::<Option<Table>>("methods").ok().flatten())
        .and_then(|m| m.get::<Option<Table>>(member).ok().flatten())
        .ok_or_else(|| ("org.freedesktop.DBus.Error.UnknownMethod".to_owned(), format!("no method {interface}.{member}")))?;
    let call: Function = method.get("call").map_err(failed)?;
    let returns: String = method.get::<Option<String>>("returns").map_err(failed)?.unwrap_or_default();
    let values = args.iter().map(|a| to_lua(lua, a)).collect::<mlua::Result<Vec<_>>>().map_err(failed)?;
    let out: MultiValue = call.call(MultiValue::from_iter(values)).map_err(failed)?;
    let wanted = complete_types(&returns).map_err(|e| ("org.freedesktop.DBus.Error.Failed".to_owned(), e))?.len();
    let out: Vec<Dyn> = out.iter().take(wanted).map(from_lua).collect();
    if out.len() < wanted {
        return Err(("org.freedesktop.DBus.Error.Failed".into(), format!("{member} returned {} of {wanted} values", out.len())));
    }
    Ok((returns, out))
}

/// A served object: `:emit`, `:changed`, `:close`.
pub struct Served {
    commands: UnboundedSender<Command>,
    interfaces: Table,
    task: Rc<RefCell<Option<zruntime::Task<(), zruntime::Shared>>>>,
}

impl UserData for Served {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // obj:emit(interface, signal, …): its arguments in the signature
        // the description gives the signal.
        methods.add_method("emit", |_, this, (interface, signal, rest): (String, String, MultiValue)| {
            let signature: String = this
                .interfaces
                .get::<Option<Table>>(interface.as_str())?
                .and_then(|d| d.get::<Option<Table>>("signals").ok().flatten())
                .and_then(|s| s.get::<Option<String>>(signal.as_str()).ok().flatten())
                .ok_or_else(|| err(format!("obj:emit: no signal {interface}.{signal} in the description")))?;
            let args = rest.iter().map(from_lua).collect();
            let _ = this.commands.unbounded_send(Command::Signal(interface, signal, signature, args));
            Ok(())
        });
        // obj:changed(interface, { "PlaybackStatus", … }): PropertiesChanged
        // with each one's value now.
        methods.add_method("changed", |_, this, (interface, names): (String, Vec<String>)| {
            let mut changed = Vec::new();
            for name in names {
                let value = property(&this.interfaces, &interface, &name).map_err(|(_, m)| err(format!("obj:changed: {m}")))?;
                changed.push((Dyn::Str(name), value));
            }
            let args = vec![Dyn::Str(interface), Dyn::Map(changed), Dyn::List(Vec::new())];
            let _ = this.commands.unbounded_send(Command::Signal(PROPERTIES.into(), "PropertiesChanged".into(), "sa{sv}as".into(), args));
            Ok(())
        });
        // obj:close(): off the bus, its name let go.
        methods.add_method("close", |_, this, ()| {
            this.task.borrow_mut().take();
            Ok(())
        });
    }
}

/// `g.dbus.serve { bus, name, path, interfaces, on_error }`.
pub fn serve(dbus: &Rc<Dbus>, lua: &Lua, t: Table) -> mlua::Result<Served> {
    let system = which_bus(&t)?;
    let name: Option<String> = t.get("name")?;
    let path: String = t.get("path")?;
    ObjectPath::try_from(path.as_str()).map_err(|e| err(format!("g.dbus.serve: path {path}: {e}")))?;
    let interfaces: Table = t.get("interfaces")?;
    let on_error: Option<Function> = t.get("on_error")?;
    let xml = introspection(&interfaces)?;

    // Answers flow from the main thread back to the bus task.
    let (answers, answers_in) = unbounded::<Answer>();
    let (commands, commands_in) = unbounded::<Command>();

    // The main thread's side: each call, delivered as
    // [serial, interface, member, args…] (or ["error", message]).
    let (defs, weak) = (interfaces.clone(), lua.weak());
    let handler = lua.create_function(move |_, values: MultiValue| {
        let Some(lua) = weak.try_upgrade() else { return Ok(()) };
        let values: Vec<Dyn> = values.iter().map(from_lua).collect();
        match values.as_slice() {
            [Dyn::Str(kind), Dyn::Str(message)] if kind == "error" => {
                log::warn!("g.dbus.serve: {message}");
                if let Some(f) = &on_error {
                    let _ = f.call::<()>(message.clone());
                }
            }
            [Dyn::Int(serial), Dyn::Str(interface), Dyn::Str(member), args @ ..] => {
                let result = answer_call(&lua, &defs, interface, member, args);
                if let Err((name, message)) = &result {
                    log::debug!("g.dbus.serve: {interface}.{member}: {name}: {message}");
                }
                let _ = answers.unbounded_send((*serial as u32, result));
            }
            _ => {}
        }
        Ok(())
    })?;
    let id = dbus.wait(handler, false);
    let out = dbus.sender.clone();

    let task = dbus.bus.task("g.dbus serve", async move {
        let say = |message: String| {
            let _ = out.send(crate::dbus::Delivery { id, result: Ok(vec![Dyn::Str("error".into()), Dyn::Str(message)]), last: false });
        };
        let built = if system { zbus::Connection::system().await } else { zbus::Connection::session().await };
        let conn = match built {
            Ok(c) => c,
            Err(e) => return say(format!("no bus: {e}")),
        };
        let calls = MessageStream::from(&conn);
        if let Some(name) = &name {
            match conn.request_name_with_flags(name.as_str(), zbus::fdo::RequestNameFlags::DO_NOT_QUEUE).await {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner) => {}
                Ok(other) => return say(format!("{name}: not ours ({other:?})")),
                Err(e) => return say(format!("{name}: {e}")),
            }
        }
        let pending: Arc<Mutex<HashMap<u32, zbus::Message>>> = Arc::default();
        let events = futures_util::stream::select(
            futures_util::stream::select(calls.map(Event::Call), answers_in.map(Event::Answer)),
            commands_in.map(Event::Command),
        );
        futures_util::pin_mut!(events);
        while let Some(event) = events.next().await {
            match event {
                Event::Call(Ok(message)) => {
                    if message.message_type() != Type::MethodCall {
                        continue;
                    }
                    let header = message.header();
                    if header.path().map(|p| p.as_str()) != Some(path.as_str()) {
                        let _ = conn
                            .reply_error(&header, "org.freedesktop.DBus.Error.UnknownObject", &format!("no object at {}", header.path().map_or("", |p| p.as_str())))
                            .await;
                        continue;
                    }
                    let interface = header.interface().map(|i| i.as_str().to_owned()).unwrap_or_default();
                    let member = header.member().map(|m| m.as_str().to_owned()).unwrap_or_default();
                    match (interface.as_str(), member.as_str()) {
                        (INTROSPECTABLE, "Introspect") => {
                            let _ = conn.reply(&header, &xml).await;
                        }
                        (PEER, "Ping") => {
                            let _ = conn.reply(&header, &()).await;
                        }
                        (PEER, "GetMachineId") => {
                            let id = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
                            let _ = conn.reply(&header, &id.trim()).await;
                        }
                        _ => {
                            let args = match arguments(&message) {
                                Ok(a) => a,
                                Err(e) => {
                                    let _ = conn.reply_error(&header, "org.freedesktop.DBus.Error.InvalidArgs", &e).await;
                                    continue;
                                }
                            };
                            let serial = header.primary().serial_num().get();
                            if let Ok(mut p) = pending.lock() {
                                p.insert(serial, message.clone());
                            }
                            let mut values = vec![Dyn::Int(i64::from(serial)), Dyn::Str(interface), Dyn::Str(member)];
                            values.extend(args);
                            let _ = out.send(crate::dbus::Delivery { id, result: Ok(values), last: false });
                        }
                    }
                }
                Event::Call(Err(e)) => log::debug!("g.dbus.serve: {e}"),
                Event::Answer((serial, result)) => {
                    let Some(message) = pending.lock().ok().and_then(|mut p| p.remove(&serial)) else { continue };
                    let header = message.header();
                    let sent = match result {
                        Ok((signature, values)) => match body(&signature, &values) {
                            Ok(Some(b)) => conn.reply(&header, &b).await,
                            Ok(None) => conn.reply(&header, &()).await,
                            Err(e) => conn.reply_error(&header, "org.freedesktop.DBus.Error.Failed", &e).await,
                        },
                        Err((name, text)) => conn.reply_error(&header, name.as_str(), &text).await,
                    };
                    if let Err(e) = sent {
                        log::warn!("g.dbus.serve: a reply: {e}");
                    }
                }
                Event::Command(Command::Signal(interface, member, signature, args)) => {
                    let sent = match body(&signature, &args) {
                        Ok(Some(b)) => conn.emit_signal(None::<&str>, path.as_str(), interface.as_str(), member.as_str(), &b).await,
                        Ok(None) => conn.emit_signal(None::<&str>, path.as_str(), interface.as_str(), member.as_str(), &()).await,
                        Err(e) => {
                            say(format!("{interface}.{member}: {e}"));
                            continue;
                        }
                    };
                    if let Err(e) = sent {
                        log::warn!("g.dbus.serve: {interface}.{member}: {e}");
                    }
                }
            }
        }
    });
    let task = Rc::new(RefCell::new(Some(task)));
    // The next load drops it: the connection closes and the name goes.
    let t2 = task.clone();
    on_cleanup(move || {
        t2.borrow_mut().take();
    });
    Ok(Served { commands, interfaces, task })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_comes_apart_in_its_complete_types() {
        assert_eq!(complete_types("sa{sv}x").unwrap(), vec!["s", "a{sv}", "x"]);
        assert_eq!(complete_types("").unwrap(), Vec::<String>::new());
        assert_eq!(complete_types("(ii)as").unwrap(), vec!["(ii)", "as"]);
        assert!(complete_types("a{").is_err());
    }

    #[test]
    fn the_description_introspects() {
        let lua = Lua::new();
        let interfaces: Table = lua
            .load(
                r#"return { ["org.example.Thing"] = {
                    methods = { Seek = { args = "x", call = function() end }, Name = { returns = "s", call = function() return "x" end } },
                    properties = { Volume = { signature = "d", get = function() return 1 end, set = function() end },
                                   Status = { signature = "s", get = function() return "Playing" end } },
                    signals = { Seeked = "x" },
                } }"#,
            )
            .eval()
            .unwrap();
        let xml = introspection(&interfaces).unwrap();
        assert!(xml.contains("<interface name=\"org.example.Thing\">"));
        assert!(xml.contains("<method name=\"Seek\"><arg type=\"x\" direction=\"in\"/></method>"));
        assert!(xml.contains("<method name=\"Name\"><arg type=\"s\" direction=\"out\"/></method>"));
        assert!(xml.contains("<property name=\"Volume\" type=\"d\" access=\"readwrite\"/>"));
        assert!(xml.contains("<property name=\"Status\" type=\"s\" access=\"read\"/>"));
        assert!(xml.contains("<signal name=\"Seeked\"><arg type=\"x\"/></signal>"));
        // And the properties come back as variants of their signature.
        let Dyn::Map(all) = all_properties(&interfaces, "org.example.Thing").unwrap() else { panic!("a map") };
        assert!(all.iter().any(|(k, v)| matches!((k, v), (Dyn::Str(k), Dyn::Typed(s, _)) if k == "Volume" && s == "d")));
    }
}
