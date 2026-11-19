//! `g.http { url, … }`: an HTTP request, its answer handed to `on_done`.
//!
//! The request runs on a thread of its own ([`crate::io::off_loop`]): the
//! loop never waits on the network. One agent for the whole process keeps
//! connections open between requests (keep-alive), so a run of calls to
//! the same server pays for its TCP and TLS handshakes once, where a `curl`
//! a call paid every time. TLS is the script's (`crate::tls`).

use std::{
    rc::Rc,
    sync::{Arc, OnceLock},
    time::Duration,
};

use mlua::{Function, Lua, LuaString, Table, UserData, UserDataMethods};

use crate::io::{HostRef, Stop, call, off_loop};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// How long a request may take when it does not say.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The most of an answer read: a page, a list, a picture.
const MOST: u64 = 64 * 1024 * 1024;

/// The process's agent, made with the first request.
fn agent() -> Result<&'static ureq::Agent, String> {
    static AGENT: OnceLock<Result<ureq::Agent, String>> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            let roots: Vec<ureq::tls::Certificate<'static>> =
                crate::tls::roots()?.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref())).collect();
            let tls = ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::Specific(Arc::new(roots)))
                .unversioned_rustls_crypto_provider(crate::tls::provider())
                .build();
            let config = ureq::Agent::config_builder()
                .tls_config(tls)
                // A 404 is an answer the interface reads, not a failure.
                .http_status_as_error(false)
                .user_agent(concat!("genoma/", env!("CARGO_PKG_VERSION")))
                .max_idle_connections_per_host(4)
                .build();
            Ok(config.new_agent())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// What comes back: the status, the body's bytes, the headers by their
/// lower-case names (several of one name joined with ", ").
struct Answer {
    status: u16,
    body: Vec<u8>,
    headers: Vec<(String, String)>,
}

fn run(method: &str, url: &str, headers: &[(String, String)], body: Option<Vec<u8>>, timeout: Duration) -> Result<Answer, String> {
    use ureq::RequestExt;
    let agent = agent()?;
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let request = builder.body(body.unwrap_or_default()).map_err(|e| format!("{url}: {e}"))?;
    let mut response = request
        .with_agent(agent)
        .configure()
        .timeout_global(Some(timeout))
        .run()
        .map_err(|e| format!("{url}: {e}"))?;
    let status = response.status().as_u16();
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        match out.iter_mut().find(|(n, _)| n == name.as_str()) {
            Some((_, v)) => {
                v.push_str(", ");
                v.push_str(&value);
            }
            None => out.push((name.as_str().to_owned(), value)),
        }
    }
    let body = response.body_mut().with_config().limit(MOST).read_to_vec().map_err(|e| format!("{url}: {e}"))?;
    Ok(Answer { status, body, headers: out })
}

/// A request under way: `:cancel()` lets its answer go unheard.
pub struct Request {
    stop: Rc<Stop>,
}

impl UserData for Request {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });
    }
}

/// `g.http { url, method, headers, body, timeout, on_done(response) }`.
pub fn request(host: &HostRef, lua: &Lua, t: Table) -> mlua::Result<Request> {
    let url: String = t.get("url")?;
    let body: Option<LuaString> = t.get("body")?;
    let method: String = match t.get::<Option<String>>("method")? {
        Some(m) => m.to_ascii_uppercase(),
        None if body.is_some() => "POST".into(),
        None => "GET".into(),
    };
    let headers: Vec<(String, String)> = match t.get::<Option<Table>>("headers")? {
        Some(h) => h.pairs::<String, String>().collect::<mlua::Result<_>>()?,
        None => Vec::new(),
    };
    let timeout = match t.get::<Option<f64>>("timeout")? {
        Some(s) if s > 0.0 => Duration::from_secs_f64(s),
        Some(_) => return Err(err("g.http: a timeout is seconds, more than 0")),
        None => TIMEOUT,
    };
    let body = body.map(|b| b.as_bytes().to_vec());
    let on_done: Option<Function> = t.get("on_done")?;
    let stop = Stop::new(host);
    let owner = stop.owner;
    let weak = lua.weak();
    off_loop(host, &stop, "http", move || run(&method, &url, &headers, body, timeout), move |result| {
        let Some(f) = &on_done else { return };
        let Some(lua) = weak.try_upgrade() else { return };
        let response = (|| -> mlua::Result<Table> {
            let r = lua.create_table()?;
            match result {
                Ok(a) => {
                    r.set("status", a.status)?;
                    r.set("ok", (200..300).contains(&a.status))?;
                    r.set("body", lua.create_string(&a.body)?)?;
                    let h = lua.create_table()?;
                    for (name, value) in a.headers {
                        h.set(name, value)?;
                    }
                    r.set("headers", h)?;
                }
                Err(e) => {
                    r.set("status", 0)?;
                    r.set("ok", false)?;
                    r.set("body", "")?;
                    r.set("headers", lua.create_table()?)?;
                    r.set("error", e)?;
                }
            }
            Ok(r)
        })();
        match response {
            Ok(r) => call(f, "g.http on_done", r, owner),
            Err(e) => log::error!("g.http: {e}"),
        }
    })?;
    Ok(Request { stop })
}
