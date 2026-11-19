//! `g.websocket { url, … }`: a WebSocket client, `ws://` or `wss://`.
//!
//! As every other source of the script's, it owns no thread once it is
//! open: the socket is non-blocking and the loop wakes it when it is
//! readable. Only what would hold the loop — the name looked up, the TCP
//! connection, the TLS and WebSocket handshakes — happens on a thread of
//! its own ([`crate::io::off_loop`]), which hands the open connection over
//! and ends.
//!
//! TLS is rustls (ring), checking the server against the system's
//! certificates, read once.

use std::{
    cell::RefCell,
    io::{ErrorKind, Read, Write},
    net::TcpStream,
    os::fd::OwnedFd,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};

use mlua::{Function, LuaString, Table, UserData, UserDataFields, UserDataMethods};
use tungstenite::{
    Message,
    client::IntoClientRequest,
    http::HeaderValue,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

use crate::io::{HostRef, Stop, call, connect_tcp, off_loop, CONNECT_TIMEOUT};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// A connection with TLS on it, or without.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// The system's certificates, read the first time a `wss://` is opened.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            let found = rustls_native_certs::load_native_certs();
            for e in &found.errors {
                log::warn!("g.websocket: system certificates: {e}");
            }
            let (added, _) = roots.add_parsable_certificates(found.certs);
            if added == 0 {
                return Err("no system certificates to check a server with".into());
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

/// What the thread hands over: the open socket.
type Opened = tungstenite::WebSocket<Stream>;

/// The URL, connected and shaken hands with, on the thread.
fn open(url: &str, protocols: &[String], headers: &[(String, String)]) -> Result<Opened, String> {
    let mut request = url.into_client_request().map_err(|e| format!("{url}: {e}"))?;
    if !protocols.is_empty() {
        let v = HeaderValue::from_str(&protocols.join(", ")).map_err(|e| format!("protocols: {e}"))?;
        request.headers_mut().insert("Sec-WebSocket-Protocol", v);
    }
    for (name, value) in headers {
        let n = tungstenite::http::HeaderName::from_bytes(name.as_bytes()).map_err(|e| format!("header {name}: {e}"))?;
        let v = HeaderValue::from_str(value).map_err(|e| format!("header {name}: {e}"))?;
        request.headers_mut().insert(n, v);
    }
    let uri = request.uri().clone();
    let secure = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        other => return Err(format!("{url}: not ws:// or wss:// ({})", other.unwrap_or("no scheme"))),
    };
    let name = uri.host().ok_or_else(|| format!("{url}: no host"))?.trim_start_matches('[').trim_end_matches(']').to_owned();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = connect_tcp(&format!("{}:{port}", if name.contains(':') { format!("[{name}]") } else { name.clone() }))?;
    // The handshakes block, with a limit; the socket is made non-blocking
    // once they are done.
    tcp.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
    let stream = if secure {
        let server = rustls::pki_types::ServerName::try_from(name.clone()).map_err(|e| format!("{name}: {e}"))?;
        let connection = rustls::ClientConnection::new(tls_config()?, server).map_err(|e| format!("{name}: {e}"))?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    let (socket, _) = tungstenite::client(request, stream).map_err(|e| format!("{url}: {e}"))?;
    let tcp = socket.get_ref().tcp();
    tcp.set_read_timeout(None).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(None).map_err(|e| e.to_string())?;
    tcp.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(socket)
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Connecting,
    Open,
    Closing,
    Closed,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Connecting => "connecting",
            State::Open => "open",
            State::Closing => "closing",
            State::Closed => "closed",
        }
    }
}

struct Inner {
    socket: Option<Opened>,
    state: State,
    /// Why it closed, as the other side said: its code and reason.
    said: Option<(u16, String)>,
    /// Waiting to go out: a retry is asked of the loop while it is.
    flushing: bool,
}

pub struct WebSocket {
    inner: Rc<RefCell<Inner>>,
    host: HostRef,
    stop: Rc<Stop>,
}

/// Writes what is waiting; while the kernel's buffer is full, asks the loop
/// to try again a moment later, and only then: nothing runs at rest.
fn flush(inner: &Rc<RefCell<Inner>>, host: &HostRef, stop: &Stop) {
    let pending = {
        let mut i = inner.borrow_mut();
        let Some(socket) = i.socket.as_mut() else { return };
        match socket.flush() {
            Ok(()) => false,
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => true,
            Err(_) => false,
        }
    };
    let mut i = inner.borrow_mut();
    if pending && !i.flushing {
        i.flushing = true;
        drop(i);
        let again = inner.clone();
        stop.hold(host.timer(
            Duration::from_millis(16),
            Box::new(move || {
                let mut i = again.borrow_mut();
                let Some(socket) = i.socket.as_mut() else {
                    i.flushing = false;
                    return None;
                };
                match socket.flush() {
                    Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Some(Duration::from_millis(16)),
                    _ => {
                        i.flushing = false;
                        None
                    }
                }
            }),
        ));
    }
}

impl UserData for WebSocket {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // ws.state: "connecting", "open", "closing" or "closed".
        fields.add_field_method_get("state", |_, this| Ok(this.inner.borrow().state.name()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // ws:send(text) or ws:send(bytes, { binary = true }).
        methods.add_method("send", |_, this, (data, options): (LuaString, Option<Table>)| {
            let binary = match options {
                Some(t) => t.get::<Option<bool>>("binary")?.unwrap_or(false),
                None => false,
            };
            {
                let mut i = this.inner.borrow_mut();
                if i.state != State::Open {
                    return Err(err(format!("g.websocket send: {}", i.state.name())));
                }
                let Some(socket) = i.socket.as_mut() else { return Err(err("g.websocket send: closed")) };
                let message = if binary {
                    Message::Binary(data.as_bytes().to_vec().into())
                } else {
                    Message::Text(data.to_str().map_err(|_| err("g.websocket send: text that is not UTF-8 (binary = true?)"))?.to_string().into())
                };
                match socket.write(message) {
                    Ok(()) => {}
                    Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(e) => return Err(err(format!("g.websocket send: {e}"))),
                }
            }
            flush(&this.inner, &this.host, &this.stop);
            Ok(())
        });
        // ws:close(code, reason): the closing handshake; on_close follows
        // once the other side answers.
        methods.add_method("close", |_, this, (code, reason): (Option<u16>, Option<String>)| {
            {
                let mut i = this.inner.borrow_mut();
                match i.state {
                    State::Open => {}
                    State::Connecting => {
                        i.state = State::Closed;
                        drop(i);
                        this.stop.stop();
                        return Ok(());
                    }
                    _ => return Ok(()),
                }
                i.state = State::Closing;
                if let Some(socket) = i.socket.as_mut() {
                    let frame = CloseFrame { code: CloseCode::from(code.unwrap_or(1000)), reason: reason.unwrap_or_default().into() };
                    let _ = socket.close(Some(frame));
                }
            }
            flush(&this.inner, &this.host, &this.stop);
            Ok(())
        });
    }
}

/// Everything waiting on the socket, read now: messages to `on_message`,
/// the end to `on_close`. Returns whether it is still open.
fn drain(inner: &Rc<RefCell<Inner>>, on_message: &Option<Function>, on_close: &Option<Function>, owner: Option<usize>, lua: &mlua::Lua) -> bool {
    loop {
        let read = {
            let mut i = inner.borrow_mut();
            let Some(socket) = i.socket.as_mut() else { return false };
            socket.read()
        };
        match read {
            Ok(Message::Text(text)) => {
                if let Some(f) = on_message {
                    call(f, "g.websocket on_message", (text.as_str().to_owned(), false), owner);
                }
            }
            Ok(Message::Binary(bytes)) => {
                if let (Some(f), Ok(s)) = (on_message, lua.create_string(&bytes[..])) {
                    call(f, "g.websocket on_message", (s, true), owner);
                }
            }
            Ok(Message::Close(frame)) => {
                if let Some(frame) = frame {
                    inner.borrow_mut().said = Some((u16::from(frame.code), frame.reason.as_str().to_owned()));
                }
                // tungstenite answers it; the next read ends the connection.
            }
            Ok(_) => {} // ping, pong (answered by tungstenite), raw frames
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                // Pongs and the close answer go out with what is waiting.
                if let Some(socket) = inner.borrow_mut().socket.as_mut() {
                    let _ = socket.flush();
                }
                return true;
            }
            Err(e) => {
                let (code, reason) = {
                    let mut i = inner.borrow_mut();
                    i.socket = None;
                    i.state = State::Closed;
                    match (i.said.take(), &e) {
                        (Some((code, reason)), _) => (code, reason),
                        (None, tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => (1005, String::new()),
                        // Ended without a closing handshake: 1006, and why.
                        (None, other) => (1006, other.to_string()),
                    }
                };
                if let Some(f) = on_close {
                    call(f, "g.websocket on_close", (code, reason), owner);
                }
                return false;
            }
        }
    }
}

/// `g.websocket { url, protocols, headers, on_open, on_message(data,
/// binary), on_close(code, reason) }`.
pub fn connect(host: &HostRef, lua: &mlua::Lua, t: Table) -> mlua::Result<WebSocket> {
    let url: String = t.get("url")?;
    let protocols: Vec<String> = t.get::<Option<Vec<String>>>("protocols")?.unwrap_or_default();
    let headers: Vec<(String, String)> = match t.get::<Option<Table>>("headers")? {
        Some(h) => h.pairs::<String, String>().collect::<mlua::Result<_>>()?,
        None => Vec::new(),
    };
    let on_open: Option<Function> = t.get("on_open")?;
    let on_message: Option<Function> = t.get("on_message")?;
    let on_close: Option<Function> = t.get("on_close")?;
    let inner = Rc::new(RefCell::new(Inner { socket: None, state: State::Connecting, said: None, flushing: false }));
    let stop = Stop::new(host);
    let i = inner.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || {
        let mut i = i.borrow_mut();
        i.socket = None;
        i.state = State::Closed;
    }));
    let owner = stop.owner;
    let (h, st, i) = (host.clone(), stop.clone(), inner.clone());
    let weak_lua = lua.weak();
    off_loop(host, &stop, "websocket", move || open(&url, &protocols, &headers), move |result| {
        let socket = match result {
            Ok(socket) => socket,
            Err(e) => {
                {
                    let mut inner = i.borrow_mut();
                    inner.state = State::Closed;
                }
                if let Some(f) = &on_close {
                    call(f, "g.websocket on_close", (1006u16, e), owner);
                }
                return;
            }
        };
        let watch = match socket.get_ref().tcp().try_clone() {
            Ok(c) => OwnedFd::from(c),
            Err(e) => {
                i.borrow_mut().state = State::Closed;
                if let Some(f) = &on_close {
                    call(f, "g.websocket on_close", (1006u16, e.to_string()), owner);
                }
                return;
            }
        };
        {
            let mut inner = i.borrow_mut();
            inner.socket = Some(socket);
            inner.state = State::Open;
        }
        if let Some(f) = &on_open {
            call(f, "g.websocket on_open", (), owner);
        }
        let Some(lua) = weak_lua.try_upgrade() else { return };
        // What came with the handshake is already read off the socket into
        // the buffers, and the loop would not wake for it: read it now.
        if !drain(&i, &on_message, &on_close, owner, &lua) {
            return;
        }
        let (again, weak) = (i.clone(), lua.weak());
        st.hold(h.readable(
            watch,
            Box::new(move |_| {
                let Some(lua) = weak.try_upgrade() else { return false };
                drain(&again, &on_message, &on_close, owner, &lua)
            }),
        ));
    })?;
    Ok(WebSocket { inner, host: host.clone(), stop })
}
