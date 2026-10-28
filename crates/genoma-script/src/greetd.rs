//! `g.greetd` (PRD §9.1): the greeter's side of greetd's IPC, as Bioma's
//! `greeter.qml` uses Quickshell's `Greetd`: create a session for a user,
//! answer its authentication messages, and start it.
//!
//! greetd's protocol is a Unix socket (`$GREETD_SOCK`) carrying JSON, each
//! message after its length as a native-endian u32. Requests are written as
//! they are made; replies are read when the socket is readable, through the
//! loop, and matched to the requests in order.

use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    rc::Rc,
};

use mlua::{Function, IntoLuaMulti, Table, UserData, UserDataFields, UserDataMethods};
use serde_json::{Value, json};

use crate::io::{HostRef, Stop};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

fn call(f: &Option<Function>, what: &str, args: impl IntoLuaMulti) {
    if let Some(f) = f {
        if let Err(e) = f.call::<()>(args) {
            log::error!("g.greetd {what}: {e}");
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Asked {
    Create,
    Respond,
    Start,
    Cancel,
}

struct Handlers {
    on_auth_message: Option<Function>,
    on_ready: Option<Function>,
    on_launched: Option<Function>,
    on_failure: Option<Function>,
    on_error: Option<Function>,
}

struct Connection {
    stream: RefCell<Option<UnixStream>>,
    asked: RefCell<VecDeque<Asked>>,
    buffer: RefCell<Vec<u8>>,
    handlers: Handlers,
}

impl Connection {
    fn send(&self, asked: Asked, request: Value) -> mlua::Result<()> {
        let body = serde_json::to_vec(&request).map_err(|e| err(e.to_string()))?;
        let mut frame = (body.len() as u32).to_ne_bytes().to_vec();
        frame.extend_from_slice(&body);
        let mut stream = self.stream.borrow_mut();
        let Some(stream) = stream.as_mut() else { return Err(err("g.greetd: not connected")) };
        // Small, and the socket's buffer has room: written whole.
        stream.set_nonblocking(false).ok();
        let written = stream.write_all(&frame);
        stream.set_nonblocking(true).ok();
        written.map_err(|e| err(format!("g.greetd: {e}")))?;
        self.asked.borrow_mut().push_back(asked);
        Ok(())
    }

    /// Every whole reply in the buffer, to its handler.
    fn replies(&self) {
        loop {
            let reply = {
                let mut buffer = self.buffer.borrow_mut();
                if buffer.len() < 4 {
                    return;
                }
                let length = u32::from_ne_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
                if buffer.len() < 4 + length {
                    return;
                }
                let body: Vec<u8> = buffer.drain(..4 + length).skip(4).collect();
                serde_json::from_slice::<Value>(&body)
            };
            let asked = self.asked.borrow_mut().pop_front();
            let h = &self.handlers;
            let Ok(reply) = reply else {
                call(&h.on_error, "on_error", "a reply that is not JSON");
                continue;
            };
            let text = |key: &str| reply.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
            match reply.get("type").and_then(Value::as_str) {
                Some("success") => match asked {
                    Some(Asked::Start) => call(&h.on_launched, "on_launched", ()),
                    Some(Asked::Cancel) => {}
                    _ => call(&h.on_ready, "on_ready", ()),
                },
                // "visible", "secret", "info" or "error": the first two want
                // an answer, a secret without echo.
                Some("auth_message") => {
                    call(&h.on_auth_message, "on_auth_message", (text("auth_message"), text("auth_message_type")))
                }
                Some("error") => {
                    if text("error_type") == "auth_error" {
                        call(&h.on_failure, "on_failure", text("description"));
                    } else {
                        call(&h.on_error, "on_error", text("description"));
                    }
                }
                _ => call(&h.on_error, "on_error", "a reply of no known type"),
            }
        }
    }
}

pub struct Greetd {
    connection: Option<Rc<Connection>>,
    stop: Rc<Stop>,
}

impl UserData for Greetd {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // Whether greetd is there to talk to: a greeter run outside greetd
        // has no $GREETD_SOCK.
        fields.add_field_method_get("available", |_, this| {
            Ok(this.connection.as_ref().is_some_and(|c| c.stream.borrow().is_some()))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        fn connection(this: &Greetd) -> mlua::Result<&Rc<Connection>> {
            this.connection.as_ref().ok_or_else(|| err("g.greetd: greetd is not running"))
        }
        methods.add_method("create_session", |_, this, user: String| {
            connection(this)?.send(Asked::Create, json!({ "type": "create_session", "username": user }))
        });
        // greetd:respond(text): the answer to the last auth message; nil
        // only acknowledges an informational one.
        methods.add_method("respond", |_, this, answer: Option<String>| {
            connection(this)?.send(Asked::Respond, json!({ "type": "post_auth_message_response", "response": answer }))
        });
        // greetd:start_session({ "niri-session" }, { "XDG_SESSION_TYPE=wayland" })
        methods.add_method("start_session", |_, this, (command, env): (Vec<String>, Option<Vec<String>>)| {
            connection(this)?.send(Asked::Start, json!({ "type": "start_session", "cmd": command, "env": env.unwrap_or_default() }))
        });
        methods.add_method("cancel_session", |_, this, ()| {
            connection(this)?.send(Asked::Cancel, json!({ "type": "cancel_session" }))
        });
        methods.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.greetd { on_auth_message = function(message, kind) … end, on_ready =
/// function() … end, on_launched = …, on_failure = function(description) …
/// end, on_error = … }`.
pub fn connect(host: &HostRef, t: Table) -> mlua::Result<Greetd> {
    let stop = Stop::new(host);
    let handlers = Handlers {
        on_auth_message: t.get("on_auth_message")?,
        on_ready: t.get("on_ready")?,
        on_launched: t.get("on_launched")?,
        on_failure: t.get("on_failure")?,
        on_error: t.get("on_error")?,
    };
    let Some(path) = std::env::var_os("GREETD_SOCK") else {
        return Ok(Greetd { connection: None, stop });
    };
    let stream = UnixStream::connect(&path).map_err(|e| err(format!("g.greetd: {e}")))?;
    stream.set_nonblocking(true).map_err(|e| err(format!("g.greetd: {e}")))?;
    let watch = OwnedFd::from(stream.try_clone().map_err(|e| err(format!("g.greetd: {e}")))?);
    let connection = Rc::new(Connection {
        stream: RefCell::new(Some(stream)),
        asked: RefCell::default(),
        buffer: RefCell::default(),
        handlers,
    });
    let c = connection.clone();
    stop.hold(host.readable(
        watch,
        Box::new(move |_| {
            let mut chunk = [0u8; 8192];
            loop {
                let read = match c.stream.borrow_mut().as_mut() {
                    Some(s) => s.read(&mut chunk),
                    None => return false,
                };
                match read {
                    Ok(0) => {
                        c.stream.borrow_mut().take();
                        call(&c.handlers.on_error, "on_error", "greetd closed the connection");
                        return false;
                    }
                    Ok(n) => c.buffer.borrow_mut().extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        c.stream.borrow_mut().take();
                        call(&c.handlers.on_error, "on_error", e.to_string());
                        return false;
                    }
                }
            }
            c.replies();
            true
        }),
    ));
    let c = connection.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        c.stream.borrow_mut().take();
    }));
    Ok(Greetd { connection: Some(connection), stop })
}
