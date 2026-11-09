//! The compositor, whichever it is: workspaces, windows and the focused
//! window as one model, and the few actions a shell takes on them, over each
//! compositor's own IPC. The backend is found from the environment the
//! compositor gives its clients:
//!
//! - niri: `$NIRI_SOCKET`, its event stream (the `niri` service's parser).
//! - Hyprland: `$HYPRLAND_INSTANCE_SIGNATURE`; events on `.socket2.sock`,
//!   and the state asked again on `.socket.sock` when an event says it
//!   changed.
//! - Umbriel: `$UMBRIEL_SOCKET`, a subscription that sends whole snapshots.
//!
//! Ids are strings: niri numbers, Hyprland window addresses and Umbriel's
//! stable ids all fit, and an interface only hands them back.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use genoma_core::reactive::{Memo, Signal, batch};
use serde_json::Value;

use crate::niri::Niri;

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: String,
    /// Its place on its output, from 1.
    pub idx: u64,
    pub name: Option<String>,
    pub output: Option<String>,
    /// Shown on its output.
    pub active: bool,
    /// The active workspace of the focused output.
    pub focused: bool,
    /// No window on it.
    pub vacant: bool,
    pub urgent: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: String,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub workspace: Option<String>,
}

/// Which compositor, and what talking to it needs.
pub enum Backend {
    Niri { socket: PathBuf, model: Niri },
    Hyprland { dir: PathBuf },
    Umbriel { socket: PathBuf },
}

pub struct Compositor {
    pub backend: Backend,
    pub workspaces: Signal<Vec<Workspace>>,
    pub windows: Signal<Vec<Window>>,
    pub focused: Signal<Option<String>>,
    /// The focused window's title, or nothing when no window has focus.
    pub title: Memo<Option<String>>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn runtime_dir() -> PathBuf {
    env("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

/// One request on a connection of its own, the whole reply read back.
fn ask(socket: &PathBuf, request: &[u8]) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
    stream.set_write_timeout(Some(Duration::from_millis(500))).ok();
    stream.write_all(request).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).map_err(|e| e.to_string())?;
    Ok(reply)
}

/// One request and its one-line reply, for sockets that keep the
/// connection open (niri, Umbriel).
fn ask_line(socket: &PathBuf, request: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    Ok(reply)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

impl Compositor {
    fn new(backend: Backend) -> Compositor {
        let windows: Signal<Vec<Window>> = Signal::new(Vec::new());
        let focused: Signal<Option<String>> = Signal::new(None);
        let title = Memo::new(move || {
            let id = focused.get()?;
            windows.get().into_iter().find(|w| w.id == id)?.title
        });
        Compositor { backend, workspaces: Signal::new(Vec::new()), windows, focused, title }
    }

    /// Finds the compositor and opens its event stream. The caller reads
    /// lines from the returned stream and hands each to [`Compositor::line`].
    pub fn connect() -> std::io::Result<(Compositor, UnixStream)> {
        let unknown = || std::io::Error::new(std::io::ErrorKind::NotFound, "no compositor IPC known (niri, Hyprland, Umbriel)");
        if let Some(socket) = env("NIRI_SOCKET").map(PathBuf::from) {
            let mut stream = UnixStream::connect(&socket)?;
            stream.write_all(b"\"EventStream\"\n")?;
            return Ok((Compositor::new(Backend::Niri { socket, model: Niri::model() }), stream));
        }
        if let Some(signature) = env("HYPRLAND_INSTANCE_SIGNATURE") {
            let dir = [runtime_dir().join("hypr").join(&signature), PathBuf::from("/tmp/hypr").join(&signature)]
                .into_iter()
                .find(|d| d.join(".socket2.sock").exists())
                .ok_or_else(unknown)?;
            let stream = UnixStream::connect(dir.join(".socket2.sock"))?;
            let compositor = Compositor::new(Backend::Hyprland { dir });
            compositor.hyprland_refresh();
            return Ok((compositor, stream));
        }
        let umbriel = env("UMBRIEL_SOCKET").map(PathBuf::from).or_else(|| {
            let path = runtime_dir().join(format!("umbriel-{}.sock", env("WAYLAND_DISPLAY")?));
            path.exists().then_some(path)
        });
        if let Some(socket) = umbriel {
            let mut stream = UnixStream::connect(&socket)?;
            stream.write_all(b"{\"cmd\":\"subscribe\",\"events\":[\"workspaces\",\"windows\"]}\n")?;
            return Ok((Compositor::new(Backend::Umbriel { socket }), stream));
        }
        Err(unknown())
    }

    /// "niri", "hyprland" or "umbriel".
    pub fn name(&self) -> &'static str {
        match self.backend {
            Backend::Niri { .. } => "niri",
            Backend::Hyprland { .. } => "hyprland",
            Backend::Umbriel { .. } => "umbriel",
        }
    }

    /// One line of the event stream.
    pub fn line(&self, line: &str) {
        match &self.backend {
            Backend::Niri { model, .. } => {
                model.line(line);
                self.take_niri(model);
            }
            Backend::Hyprland { .. } => self.hyprland_event(line),
            Backend::Umbriel { .. } => self.umbriel_event(line),
        }
    }

    // ---- niri ---------------------------------------------------------------

    fn take_niri(&self, model: &Niri) {
        let workspaces: Vec<Workspace> = model
            .workspaces
            .get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|w| Workspace {
                id: w.id.to_string(),
                idx: w.idx,
                name: w.name,
                output: w.output,
                active: w.is_active,
                focused: w.is_focused,
                vacant: w.vacant,
                urgent: false,
            })
            .collect();
        let windows: Vec<Window> = model
            .windows
            .get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|w| Window { id: w.id.to_string(), title: w.title, app_id: w.app_id, workspace: w.workspace_id.map(|i| i.to_string()) })
            .collect();
        let focused = model.focused.get_untracked().flatten().map(|i| i.to_string());
        batch(|| {
            self.set_workspaces(workspaces);
            self.set_windows(windows);
            self.set_focused(focused);
        });
    }

    // ---- Hyprland -----------------------------------------------------------

    fn hyprland(&self, request: &str) -> Result<String, String> {
        let Backend::Hyprland { dir } = &self.backend else { return Err("not Hyprland".into()) };
        ask(&dir.join(".socket.sock"), request.as_bytes())
    }

    fn hyprland_json(&self, request: &str) -> Value {
        self.hyprland(&format!("j/{request}")).ok().and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null)
    }

    /// The whole state, asked again: workspaces, monitors, windows, focus.
    fn hyprland_refresh(&self) {
        let monitors = self.hyprland_json("monitors");
        let spaces = self.hyprland_json("workspaces");
        let clients = self.hyprland_json("clients");
        let active = self.hyprland_json("activewindow");
        let monitors = monitors.as_array().cloned().unwrap_or_default();
        let shown = |output: &str| {
            monitors
                .iter()
                .find(|m| m.get("name").and_then(Value::as_str) == Some(output))
                .and_then(|m| m.get("activeWorkspace")?.get("id")?.as_i64())
        };
        let focused_output =
            monitors.iter().find(|m| m.get("focused").and_then(Value::as_bool) == Some(true)).and_then(|m| text(m, "name"));
        let mut workspaces: Vec<Workspace> = spaces
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let id = w.get("id")?.as_i64()?;
                // Special workspaces (scratchpads) are not places to switch to.
                if id <= 0 {
                    return None;
                }
                let output = text(w, "monitor");
                let active = output.as_deref().and_then(shown) == Some(id);
                let name = text(w, "name").filter(|n| *n != id.to_string());
                Some(Workspace {
                    id: id.to_string(),
                    idx: 0,
                    name,
                    focused: active && output.is_some() && output == focused_output,
                    output,
                    active,
                    vacant: w.get("windows").and_then(Value::as_u64).unwrap_or(0) == 0,
                    urgent: false,
                })
            })
            .collect();
        // In order on each output, numbered from 1 there.
        workspaces.sort_by_key(|w| (w.output.clone(), w.id.parse::<i64>().unwrap_or(i64::MAX)));
        let mut last: Option<String> = None;
        let mut n = 0;
        for w in &mut workspaces {
            if w.output != last {
                last = w.output.clone();
                n = 0;
            }
            n += 1;
            w.idx = n;
        }
        let windows: Vec<Window> = clients
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c.get("mapped").and_then(Value::as_bool) != Some(false))
            .filter_map(|c| {
                Some(Window {
                    id: text(c, "address")?,
                    title: text(c, "title"),
                    app_id: text(c, "class"),
                    workspace: c.get("workspace")?.get("id")?.as_i64().map(|i| i.to_string()),
                })
            })
            .collect();
        let focused = text(&active, "address");
        batch(|| {
            self.set_workspaces(workspaces);
            self.set_windows(windows);
            self.set_focused(focused);
        });
    }

    /// An event, `NAME>>DATA`: a title or the focus changed in place, any
    /// other change asks for the state again.
    fn hyprland_event(&self, line: &str) {
        let Some((name, data)) = line.trim_end().split_once(">>") else { return };
        match name {
            "activewindowv2" => {
                let id = (!data.is_empty() && data != ",").then(|| format!("0x{}", data.trim_start_matches("0x")));
                self.set_focused(id);
            }
            "windowtitlev2" => {
                let Some((address, title)) = data.split_once(',') else { return };
                let id = format!("0x{}", address.trim_start_matches("0x"));
                self.windows.update(|list| {
                    list.iter().map(|w| if w.id == id { Window { title: Some(title.to_owned()), ..w.clone() } } else { w.clone() }).collect()
                });
            }
            // Said twice, the v2 forms carry the same: one is enough.
            "activewindow" | "windowtitle" | "workspace" | "focusedmon" | "createworkspace" | "destroyworkspace"
            | "moveworkspace" | "movewindow" | "monitoradded" | "monitorremoved" | "activespecial" => {}
            // Opening and closing a window are said once, without a v2.
            "workspacev2" | "focusedmonv2" | "createworkspacev2" | "destroyworkspacev2" | "moveworkspacev2" | "renameworkspace"
            | "openwindow" | "closewindow" | "movewindowv2" | "monitoraddedv2" | "monitorremovedv2" | "configreloaded" => {
                self.hyprland_refresh()
            }
            _ => {}
        }
    }

    // ---- Umbriel ------------------------------------------------------------

    fn umbriel_event(&self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return };
        let data = event.get("data");
        match event.get("event").and_then(Value::as_str) {
            Some("workspaces") => {
                let workspaces = data
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|w| {
                        Some(Workspace {
                            id: id_of(w.get("id")?)?,
                            idx: w.get("index").and_then(Value::as_u64).unwrap_or(0),
                            name: (w.get("named").and_then(Value::as_bool) == Some(true)).then(|| text(w, "name")).flatten(),
                            output: text(w, "output"),
                            active: w.get("active").and_then(Value::as_bool).unwrap_or(false),
                            focused: w.get("focused").and_then(Value::as_bool).unwrap_or(false),
                            vacant: w.get("occupied").and_then(Value::as_bool) == Some(false),
                            urgent: w.get("urgent").and_then(Value::as_bool).unwrap_or(false),
                        })
                    })
                    .collect();
                self.set_workspaces(workspaces);
            }
            Some("windows") => {
                let list = data.and_then(Value::as_array).cloned().unwrap_or_default();
                let windows: Vec<Window> = list
                    .iter()
                    .filter_map(|w| {
                        Some(Window {
                            id: id_of(w.get("id")?)?,
                            title: text(w, "title"),
                            app_id: text(w, "app_id"),
                            workspace: w.get("workspace").and_then(id_of),
                        })
                    })
                    .collect();
                // `active` is the window holding the keyboard; `focused` only
                // its workspace's remembered one.
                let focused = list.iter().find(|w| w.get("active").and_then(Value::as_bool) == Some(true)).and_then(|w| id_of(w.get("id")?));
                batch(|| {
                    self.set_windows(windows);
                    self.set_focused(focused);
                });
            }
            _ => {}
        }
    }

    fn umbriel_action(&self, action: Value) -> Result<(), String> {
        let Backend::Umbriel { socket } = &self.backend else { return Err("not Umbriel".into()) };
        let mut request = serde_json::json!({ "cmd": "action" });
        if let (Some(r), Some(a)) = (request.as_object_mut(), action.as_object()) {
            r.extend(a.clone());
        }
        let reply = ask_line(socket, &format!("{request}\n"))?;
        match serde_json::from_str::<Value>(&reply).ok().and_then(|r| r.get("err").cloned()) {
            Some(Value::String(said)) => Err(said),
            Some(other) => Err(other.to_string()),
            None => Ok(()),
        }
    }

    // ---- The model ----------------------------------------------------------

    fn set_workspaces(&self, list: Vec<Workspace>) {
        if self.workspaces.get_untracked().as_ref() != Some(&list) {
            self.workspaces.set(list);
        }
    }

    fn set_windows(&self, list: Vec<Window>) {
        if self.windows.get_untracked().as_ref() != Some(&list) {
            self.windows.set(list);
        }
    }

    fn set_focused(&self, id: Option<String>) {
        if self.focused.get_untracked().as_ref() != Some(&id) {
            self.focused.set(id);
        }
    }

    /// The output the keyboard is pointed at: the focused workspace's.
    pub fn focused_output(&self) -> Option<String> {
        self.workspaces.get().into_iter().find(|w| w.focused).and_then(|w| w.output)
    }

    /// The workspace shown on `output` now.
    pub fn active(&self, output: &str) -> Option<Workspace> {
        self.workspaces.get().into_iter().find(|w| w.active && w.output.as_deref() == Some(output))
    }

    /// The workspaces on `output` (all of them, without one), in order.
    pub fn on_output(&self, output: Option<&str>) -> Vec<Workspace> {
        let mut list: Vec<_> =
            self.workspaces.get().into_iter().filter(|w| output.is_none() || w.output.as_deref() == output).collect();
        list.sort_by(|a, b| (a.output.as_deref(), a.idx).cmp(&(b.output.as_deref(), b.idx)));
        list
    }

    /// The focused window, wherever it is.
    pub fn focused_window(&self) -> Option<Window> {
        let id = self.focused.get()?;
        self.windows.get().into_iter().find(|w| w.id == id)
    }

    /// The focused window, only if it is on `output`.
    pub fn focused_on(&self, output: &str) -> Option<Window> {
        let window = self.focused_window()?;
        let ws = self.workspaces.get().into_iter().find(|w| Some(&w.id) == window.workspace.as_ref())?;
        (ws.output.as_deref() == Some(output)).then_some(window)
    }

    fn niri_action(socket: &PathBuf, action: &str) -> Result<(), String> {
        let reply = ask_line(socket, &format!("{{\"Action\":{action}}}\n"))?;
        if reply.contains("\"Err\"") {
            return Err(reply.trim().to_owned());
        }
        Ok(())
    }

    /// A dispatch in both tongues: Hyprland configured in Lua (0.55 on)
    /// takes only `hl.dsp.*`, configured in hyprlang only the classic form,
    /// and each refuses the other without doing anything.
    fn hyprland_dispatch(&self, lua: &str, classic: &str) -> Result<(), String> {
        let reply = self.hyprland(&format!("dispatch {lua}"))?;
        if reply.trim() == "ok" {
            return Ok(());
        }
        match self.hyprland(&format!("dispatch {classic}")) {
            Ok(r) if r.trim() == "ok" => Ok(()),
            // "warning: =[C]:-1: hl.focus: window not found": the last part.
            _ => Err(reply.trim().rsplit_once(": =[C]:-1: ").map_or(reply.trim(), |(_, said)| said).to_owned()),
        }
    }

    pub fn focus_workspace(&self, id: &str) -> Result<(), String> {
        match &self.backend {
            Backend::Niri { socket, .. } => {
                let n: u64 = id.parse().map_err(|_| format!("no niri workspace {id}"))?;
                Self::niri_action(socket, &format!("{{\"FocusWorkspace\":{{\"reference\":{{\"Id\":{n}}}}}}}"))
            }
            Backend::Hyprland { .. } => {
                let n: i64 = id.parse().map_err(|_| format!("no Hyprland workspace {id}"))?;
                self.hyprland_dispatch(&format!("hl.dsp.focus({{ workspace = {n} }})"), &format!("workspace {n}"))
            },
            // Umbriel switches by place and output, not by id.
            Backend::Umbriel { .. } => {
                let ws = self.workspaces.get_untracked().unwrap_or_default().into_iter().find(|w| w.id == id);
                let ws = ws.ok_or_else(|| format!("no workspace {id}"))?;
                let mut action = serde_json::json!({ "action": "workspace-switch", "workspace": ws.idx });
                if let Some(output) = ws.output {
                    action["output"] = Value::String(output);
                }
                self.umbriel_action(action)
            }
        }
    }

    pub fn focus_window(&self, id: &str) -> Result<(), String> {
        match &self.backend {
            Backend::Niri { socket, .. } => {
                let n: u64 = id.parse().map_err(|_| format!("no niri window {id}"))?;
                Self::niri_action(socket, &format!("{{\"FocusWindow\":{{\"id\":{n}}}}}"))
            }
            Backend::Hyprland { .. } => {
                let id = hyprland_address(id)?;
                self.hyprland_dispatch(&format!("hl.dsp.focus({{ window = \"address:{id}\" }})"), &format!("focuswindow address:{id}"))
            },
            Backend::Umbriel { .. } => self.umbriel_action(serde_json::json!({ "action": "window-focus", "window": id })),
        }
    }

    pub fn close_window(&self, id: &str) -> Result<(), String> {
        match &self.backend {
            Backend::Niri { socket, .. } => {
                let n: u64 = id.parse().map_err(|_| format!("no niri window {id}"))?;
                Self::niri_action(socket, &format!("{{\"CloseWindow\":{{\"id\":{n}}}}}"))
            }
            Backend::Hyprland { .. } => {
                let id = hyprland_address(id)?;
                self.hyprland_dispatch(&format!("hl.dsp.window.close({{ window = \"address:{id}\" }})"), &format!("closewindow address:{id}"))
            },
            Backend::Umbriel { .. } => self.umbriel_action(serde_json::json!({ "action": "window-close", "window": id })),
        }
    }
}

/// A window address, checked to be one: it goes into a Lua string.
fn hyprland_address(id: &str) -> Result<&str, String> {
    let hex = id.strip_prefix("0x").unwrap_or("");
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("no Hyprland window {id}"));
    }
    Ok(id)
}

/// An id as a string, whether the compositor sent a string or a number.
fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn umbriel() -> Compositor {
        Compositor::new(Backend::Umbriel { socket: PathBuf::from("/nonexistent") })
    }

    #[test]
    fn umbriel_snapshots_replace_the_state() {
        let c = umbriel();
        c.line(r#"{"event":"workspaces","data":[{"id":"ws-1","name":"1","named":false,"index":1,"output":"DP-1","active":true,"focused":true,"occupied":true,"window_count":1,"focused_window":"w1","urgent":false},{"id":"ws-2","name":"WEB","named":true,"index":2,"output":"DP-1","active":false,"focused":false,"occupied":false}]}"#);
        c.line(r#"{"event":"windows","data":[{"id":"w1","app_id":"foot","title":"~","workspace":"ws-1","active":true,"focused":true},{"id":"w2","app_id":"firefox","title":"Web","workspace":"ws-1","active":false}]}"#);
        let spaces = c.on_output(Some("DP-1"));
        assert_eq!(spaces.len(), 2);
        assert_eq!((spaces[0].name.as_deref(), spaces[1].name.as_deref()), (None, Some("WEB")), "a number is no name");
        assert!(spaces[1].vacant && !spaces[0].vacant);
        assert_eq!(c.focused_output().as_deref(), Some("DP-1"));
        assert_eq!(c.title.get().as_deref(), Some("~"));
        assert_eq!(c.focused_on("DP-1").map(|w| w.app_id), Some(Some("foot".into())));
        assert_eq!(c.focused_on("HDMI-A-1"), None);
    }

    #[test]
    fn hyprland_titles_and_focus_change_in_place() {
        let c = Compositor::new(Backend::Hyprland { dir: PathBuf::from("/nonexistent") });
        c.windows.set(vec![Window { id: "0x55aa".into(), title: Some("a".into()), app_id: None, workspace: Some("1".into()) }]);
        c.line("activewindowv2>>55aa\n");
        assert_eq!(c.focused.get_untracked().flatten().as_deref(), Some("0x55aa"));
        c.line("windowtitlev2>>55aa,b, with a comma\n");
        assert_eq!(c.title.get().as_deref(), Some("b, with a comma"));
        c.line("activewindowv2>>\n");
        assert_eq!(c.title.get(), None);
    }

    #[test]
    fn niri_events_reach_the_common_model() {
        let c = Compositor::new(Backend::Niri { socket: PathBuf::from("/nonexistent"), model: Niri::model() });
        c.line(r#"{"WorkspacesChanged":{"workspaces":[{"id":5,"idx":1,"name":null,"output":"DP-1","is_active":true,"is_focused":true,"active_window_id":9}]}}"#);
        c.line(r#"{"WindowsChanged":{"windows":[{"id":9,"title":"t","app_id":"a","workspace_id":5,"is_focused":true}]}}"#);
        assert_eq!(c.active("DP-1").map(|w| w.id), Some("5".into()));
        assert_eq!(c.focused_window().map(|w| (w.id, w.workspace)), Some(("9".into(), Some("5".into()))));
    }
}
