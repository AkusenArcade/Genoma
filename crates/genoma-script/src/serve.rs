//! `g.serve { port, on_request }`: a small HTTP server on the loopback,
//! for what comes back to the machine itself — the browser returning from
//! an OAuth login to `http://127.0.0.1:<port>/callback`, a local tool
//! talking to the interface.
//!
//! Only 127.0.0.1: nothing on the network reaches it. Like every source of
//! the script's it owns no thread: the listening socket and each
//! connection are non-blocking and read when the loop says they are
//! readable. One request a connection, answered and closed.

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    os::fd::OwnedFd,
    rc::Rc,
    time::Duration,
};

use mlua::{Function, Lua, LuaString, Table, UserData, UserDataFields, UserDataMethods};

use crate::io::{HostRef, Stop};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// The most a request's line and headers may take, and its body.
const MOST_HEAD: usize = 16 * 1024;
const MOST_BODY: usize = 1024 * 1024;
/// How long a connection may stay without finishing its request, or
/// without taking its answer.
const PATIENCE: Duration = Duration::from_secs(10);

pub struct Server {
    port: u16,
    stop: Rc<Stop>,
}

impl UserData for Server {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // server.port: the one listened on (the system's choice for port 0).
        fields.add_field_method_get("port", |_, this| Ok(this.port));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// A request, read: what `on_request` is given.
#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// `%41` and `+` as a form writes them.
fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(a), Some(b)) => {
                        out.push((a * 16 + b) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What has arrived so far: a whole request, not yet one, or one that is
/// not HTTP or is too large.
enum Parsed {
    Whole(Request),
    More,
    Wrong(u16),
}

fn parse(data: &[u8]) -> Parsed {
    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if data.len() > MOST_HEAD { Parsed::Wrong(431) } else { Parsed::More };
    };
    if end > MOST_HEAD {
        return Parsed::Wrong(431);
    }
    let head = String::from_utf8_lossy(&data[..end]);
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Parsed::Wrong(400);
    };
    if !version.starts_with("HTTP/1.") || method.is_empty() || !target.starts_with('/') {
        return Parsed::Wrong(400);
    }
    let mut headers = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { return Parsed::Wrong(400) };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = match headers.iter().find(|(n, _)| n == "content-length") {
        Some((_, v)) => match v.parse::<usize>() {
            Ok(n) if n <= MOST_BODY => n,
            Ok(_) => return Parsed::Wrong(413),
            Err(_) => return Parsed::Wrong(400),
        },
        None => 0,
    };
    let start = end + 4;
    if data.len() < start + length {
        return Parsed::More;
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let query = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (unescape(k), unescape(v)),
            None => (unescape(kv), String::new()),
        })
        .collect();
    Parsed::Whole(Request {
        method: method.to_owned(),
        path: unescape(path),
        query,
        headers,
        body: data[start..start + length].to_vec(),
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        _ => "",
    }
}

fn answer(status: u16, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    let mut typed = false;
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if lower == "content-length" || lower == "connection" || name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            continue;
        }
        typed |= lower == "content-type";
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    if !typed {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// One connection: what it has sent, then what it is owed.
struct Conn {
    stream: TcpStream,
    read: Vec<u8>,
    out: Vec<u8>,
    sent: usize,
}

/// `g.serve { port = 0, on_request(request) -> response }`.
pub fn serve(host: &HostRef, lua: &Lua, t: Table) -> mlua::Result<Server> {
    let port: u16 = t.get::<Option<u16>>("port")?.unwrap_or(0);
    let on_request: Function = t.get("on_request")?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| err(format!("g.serve 127.0.0.1:{port}: {e}")))?;
    listener.set_nonblocking(true).map_err(|e| err(format!("g.serve: {e}")))?;
    let port = listener.local_addr().map_err(|e| err(format!("g.serve: {e}")))?.port();
    let watch = OwnedFd::from(listener.try_clone().map_err(|e| err(format!("g.serve: {e}")))?);
    let stop = Stop::new(host);
    let owner = stop.owner;
    let weak = lua.weak();
    let conns: Rc<RefCell<HashMap<u64, Conn>>> = Rc::default();
    let mut next_id = 0u64;
    let (h, st, cs) = (host.clone(), stop.clone(), conns.clone());
    stop.hold(host.readable(
        watch,
        Box::new(move |_| {
            loop {
                let stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return true,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        log::warn!("g.serve: {e}");
                        return true;
                    }
                };
                if stream.set_nonblocking(true).is_err() {
                    continue;
                }
                let Ok(fd) = stream.try_clone().map(OwnedFd::from) else { continue };
                let id = next_id;
                next_id += 1;
                cs.borrow_mut().insert(id, Conn { stream, read: Vec::new(), out: Vec::new(), sent: 0 });
                // A connection that has not finished in time goes.
                let gone = cs.clone();
                st.hold(h.timer(
                    PATIENCE,
                    Box::new(move || {
                        gone.borrow_mut().remove(&id);
                        None
                    }),
                ));
                let (cs, f, weak, h2, st2) = (cs.clone(), on_request.clone(), weak.clone(), h.clone(), st.clone());
                st.hold(h.readable(
                    fd,
                    Box::new(move |_| {
                        let parsed = {
                            let mut all = cs.borrow_mut();
                            let Some(c) = all.get_mut(&id) else { return false };
                            let mut buf = [0u8; 16384];
                            loop {
                                match c.stream.read(&mut buf) {
                                    Ok(0) => {
                                        all.remove(&id);
                                        return false;
                                    }
                                    Ok(n) => c.read.extend_from_slice(&buf[..n]),
                                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                                    Err(_) => {
                                        all.remove(&id);
                                        return false;
                                    }
                                }
                            }
                            parse(&c.read)
                        };
                        let bytes = match parsed {
                            Parsed::More => return true,
                            Parsed::Wrong(status) => answer(status, &[], reason(status).as_bytes()),
                            Parsed::Whole(request) => match weak.try_upgrade() {
                                Some(lua) => respond(&lua, &f, owner, request),
                                None => return false,
                            },
                        };
                        if let Some(c) = cs.borrow_mut().get_mut(&id) {
                            c.out = bytes;
                        }
                        send(&cs, id, &h2, &st2);
                        false
                    }),
                ));
            }
        }),
    ));
    let cs = conns.clone();
    *stop.also.borrow_mut() = Some(Box::new(move || cs.borrow_mut().clear()));
    Ok(Server { port, stop })
}

/// The interface's answer to a request, as bytes on the wire.
fn respond(lua: &Lua, f: &Function, owner: Option<usize>, r: Request) -> Vec<u8> {
    let made = (|| -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("method", r.method)?;
        t.set("path", r.path)?;
        let q = lua.create_table()?;
        for (k, v) in r.query {
            q.set(k, v)?;
        }
        t.set("query", q)?;
        let h = lua.create_table()?;
        for (k, v) in r.headers {
            h.set(k, v)?;
        }
        t.set("headers", h)?;
        t.set("body", lua.create_string(&r.body)?)?;
        Ok(t)
    })();
    let Ok(request) = made else { return answer(500, &[], b"") };
    let reply = genoma_core::reactive::owned_by(owner, || f.call::<Option<Table>>(request));
    match reply {
        Ok(Some(t)) => {
            let status = t.get::<Option<u16>>("status").ok().flatten().unwrap_or(200);
            let headers: Vec<(String, String)> = t
                .get::<Option<Table>>("headers")
                .ok()
                .flatten()
                .map(|h| h.pairs::<String, String>().filter_map(Result::ok).collect())
                .unwrap_or_default();
            let body = t.get::<Option<LuaString>>("body").ok().flatten().map(|b| b.as_bytes().to_vec()).unwrap_or_default();
            answer(status, &headers, &body)
        }
        Ok(None) => answer(204, &[], b""),
        Err(e) => {
            log::error!("g.serve on_request: {e}");
            answer(500, &[], b"")
        }
    }
}

/// What a connection is owed, written as far as the kernel takes it; the
/// rest a moment later, until it is all out and the connection closes.
fn send(conns: &Rc<RefCell<HashMap<u64, Conn>>>, id: u64, host: &HostRef, stop: &Stop) {
    let done = {
        let mut all = conns.borrow_mut();
        let Some(c) = all.get_mut(&id) else { return };
        loop {
            if c.sent >= c.out.len() {
                break true;
            }
            match c.stream.write(&c.out[c.sent..]) {
                Ok(0) => break true,
                Ok(n) => c.sent += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => break false,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break true,
            }
        }
    };
    if done {
        if let Some(c) = conns.borrow_mut().remove(&id) {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
        return;
    }
    let cs = conns.clone();
    stop.hold(host.timer(
        Duration::from_millis(16),
        Box::new(move || {
            let pending = cs.borrow().get(&id).is_some_and(|c| c.sent < c.out.len());
            if !pending {
                return None;
            }
            let mut all = cs.borrow_mut();
            let c = all.get_mut(&id)?;
            match c.stream.write(&c.out[c.sent..]) {
                Ok(n) if n > 0 => c.sent += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                _ => c.sent = c.out.len(),
            }
            if c.sent >= c.out.len() {
                let _ = c.stream.shutdown(std::net::Shutdown::Both);
                all.remove(&id);
                return None;
            }
            Some(Duration::from_millis(16))
        }),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_callback_is_read_with_its_query() {
        let raw = b"GET /callback?code=abc%2F12&state=x+y HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let Parsed::Whole(r) = parse(raw) else { panic!("whole") };
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/callback");
        assert_eq!(r.query, vec![("code".into(), "abc/12".into()), ("state".into(), "x y".into())]);
        assert_eq!(r.headers, vec![("host".into(), "127.0.0.1".into())]);
        assert!(matches!(parse(b"GET /half HTTP/1.1\r\nHost: x"), Parsed::More));
        let post = b"POST /p HTTP/1.1\r\nContent-Length: 5\r\n\r\nhel";
        assert!(matches!(parse(post), Parsed::More), "the body still coming");
        let post = b"POST /p HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let Parsed::Whole(r) = parse(post) else { panic!("whole post") };
        assert_eq!(r.body, b"hello");
        assert!(matches!(parse(b"NOT HTTP\r\n\r\n"), Parsed::Wrong(400)));
        assert!(matches!(parse(b"POST / HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n"), Parsed::Wrong(413)));
    }

    #[test]
    fn an_answer_says_its_length_and_closes() {
        let a = String::from_utf8(answer(200, &[("Content-Type".into(), "text/html".into())], b"<p>ok</p>")).unwrap();
        assert!(a.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(a.contains("Content-Type: text/html\r\n") && a.contains("Content-Length: 9\r\n") && a.contains("Connection: close\r\n"));
        assert!(a.ends_with("\r\n\r\n<p>ok</p>"));
        // A header that would split the answer is dropped.
        let a = String::from_utf8(answer(200, &[("X".into(), "a\r\nEvil: 1".into())], b"")).unwrap();
        assert!(!a.contains("Evil"));
    }
}
