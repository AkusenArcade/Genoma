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
//! - Any other (or `GENOMA_COMPOSITOR=wayland`): the standard protocols,
//!   `ext-workspace-v1` for workspaces and `wlr-foreign-toplevel-management`
//!   for windows. They live on the main thread's Wayland connection, so the
//!   runtime binds them, hands what they say to [`Compositor::take_wayland`]
//!   and carries out the [`Request`]s the interface leaves here. Neither
//!   protocol says which workspace a window is on, nor whether a workspace
//!   holds any: there `workspace` is nil and `vacant` false.
//!
//! Ids are strings: niri numbers, Hyprland window addresses, Umbriel's
//! stable ids and the runtime's own numbers for protocol handles all fit,
//! and an interface only hands them back.

use std::{
    cell::RefCell,
    collections::HashMap,
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
    /// The window that has, or last had, the focus on it.
    pub active_window: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub id: String,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub workspace: Option<String>,
    /// Where the compositor says it is, when it says so without a
    /// workspace (the protocols); otherwise its workspace's output.
    pub output: Option<String>,
    /// Minimized: only the protocols report it, and only they minimize.
    pub minimized: bool,
    /// The program's process, where the compositor says.
    pub pid: Option<i64>,
    pub floating: bool,
    /// Asking for attention.
    pub urgent: bool,
}

/// Where a window is laid out, in logical units on its output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// The corner, in the output's own coordinates; none when the
    /// compositor does not say (niri says it of floating windows only).
    pub pos: Option<(f64, f64)>,
    pub size: (f64, f64),
    /// Column and row, from 1, where the layout is made of columns (niri,
    /// Umbriel's scrolling layout); none when floating or elsewhere.
    pub column: Option<u64>,
    pub row: Option<u64>,
}

/// The screen being captured, as the compositor reports it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cast {
    pub id: String,
    /// Frames are flowing (niri reports a cast paused too).
    pub active: bool,
    /// The program receiving it, where the compositor says.
    pub pid: Option<i64>,
    /// Its PipeWire node, where the compositor says.
    pub node: Option<u64>,
    /// What is captured: an output by name, or a window by id; neither when
    /// the compositor does not say which.
    pub output: Option<String>,
    pub window: Option<String>,
}

/// The keyboard's layouts, as the compositor reports them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keyboard {
    /// In the compositor's order and words: niri gives xkb's descriptions
    /// ("English (US)"), Hyprland the layout codes of its main keyboard
    /// ("us", "it").
    pub layouts: Vec<String>,
    /// The one in use, an index into `layouts`.
    pub current: Option<usize>,
    /// The one in use, by its full name ("Italian"): what to show.
    pub name: Option<String>,
}

/// Where [`Compositor::switch_layout`] goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayoutTarget {
    Next,
    Prev,
    /// An index into [`Keyboard::layouts`].
    Index(usize),
}

/// Which compositor, and what talking to it needs.
pub enum Backend {
    Niri { socket: PathBuf, model: Niri },
    Hyprland { dir: PathBuf },
    Umbriel { socket: PathBuf },
    /// The standard protocols, on the runtime's connection: what the
    /// interface asks waits here for the runtime's next turn.
    Wayland { requests: RefCell<Vec<Request>> },
}

/// What the interface asks of a compositor reached through the protocols.
/// The ids are the ones [`Compositor::take_wayland`] was given.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    FocusWorkspace(String),
    FocusWindow(String),
    CloseWindow(String),
    Minimize { window: String, minimized: bool },
}

/// A workspace as `ext-workspace-v1` describes it, its output's name found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProtocolWorkspace {
    pub id: String,
    pub name: Option<String>,
    /// Its coordinates in its group, when the compositor gives them, in as
    /// many dimensions as it has (niri gives two).
    pub coordinates: Vec<u32>,
    /// `ext_workspace_handle_v1.state`: 1 active, 2 urgent, 4 hidden.
    pub state: u32,
    /// Its group's first output.
    pub output: Option<String>,
    /// Its place among its group's workspaces, in the order they entered.
    pub place: usize,
}

/// A window as `wlr-foreign-toplevel-management` describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProtocolToplevel {
    pub id: String,
    pub title: Option<String>,
    pub app_id: Option<String>,
    /// The outputs it is on, by name.
    pub outputs: Vec<String>,
    pub minimized: bool,
    /// Holds the keyboard.
    pub activated: bool,
}

pub struct Compositor {
    pub backend: Backend,
    pub workspaces: Signal<Vec<Workspace>>,
    pub windows: Signal<Vec<Window>>,
    pub focused: Signal<Option<String>>,
    /// The focused window's title, or nothing when no window has focus.
    pub title: Memo<Option<String>>,
    /// The keyboard layouts: niri and Hyprland say; empty elsewhere.
    pub keyboard: Signal<Keyboard>,
    /// Each window's layout, by id: apart from `windows`, because it
    /// changes on every frame of a resize and a title must not follow it.
    pub layouts: Signal<HashMap<String, Layout>>,
    /// The compositor's overview is open (niri, Umbriel).
    pub overview: Signal<bool>,
    pub casts: Signal<Vec<Cast>>,
    /// The screenshots the compositor took itself since the start, and the
    /// last one's path, when it saved it (niri).
    pub screenshot: Signal<(u64, Option<String>)>,
    /// Hyprland's windows asking for attention, until they are focused:
    /// its clients do not say, only an event.
    hyprland_urgent: RefCell<Vec<String>>,
    /// Umbriel's outputs' corners, by name, and its last window list: its
    /// windows are placed in the whole layout's coordinates, and either may
    /// come first.
    umbriel_outputs: RefCell<HashMap<String, (f64, f64)>>,
    umbriel_windows: RefCell<Value>,
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
        Compositor {
            backend,
            workspaces: Signal::new(Vec::new()),
            windows,
            focused,
            title,
            keyboard: Signal::new(Keyboard::default()),
            layouts: Signal::new(HashMap::new()),
            overview: Signal::new(false),
            casts: Signal::new(Vec::new()),
            screenshot: Signal::new((0, None)),
            hyprland_urgent: RefCell::new(Vec::new()),
            umbriel_outputs: RefCell::new(HashMap::new()),
            umbriel_windows: RefCell::new(Value::Null),
        }
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
            stream.write_all(b"{\"cmd\":\"subscribe\",\"events\":[\"workspaces\",\"windows\",\"outputs\",\"overview\"]}\n")?;
            return Ok((Compositor::new(Backend::Umbriel { socket }), stream));
        }
        Err(unknown())
    }

    /// The compositor through the standard protocols: what [`connect`]
    /// falls back on, or what `GENOMA_COMPOSITOR=wayland` asks for. The
    /// runtime binds the protocols once an interface requires it.
    ///
    /// [`connect`]: Compositor::connect
    pub fn wayland() -> Compositor {
        Compositor::new(Backend::Wayland { requests: RefCell::new(Vec::new()) })
    }

    /// "niri", "hyprland", "umbriel" or "wayland".
    pub fn name(&self) -> &'static str {
        match self.backend {
            Backend::Niri { .. } => "niri",
            Backend::Hyprland { .. } => "hyprland",
            Backend::Umbriel { .. } => "umbriel",
            Backend::Wayland { .. } => "wayland",
        }
    }

    /// Whether windows can be minimized here: only through the protocols.
    pub fn can_minimize(&self) -> bool {
        matches!(self.backend, Backend::Wayland { .. })
    }

    /// One line of the event stream.
    pub fn line(&self, line: &str) {
        match &self.backend {
            Backend::Niri { model, .. } => {
                if line.contains("\"KeyboardLayout") {
                    self.niri_keyboard(line);
                    return;
                }
                if self.niri_extra(line) {
                    return;
                }
                model.line(line);
                self.take_niri(model);
            }
            Backend::Hyprland { .. } => self.hyprland_event(line),
            Backend::Umbriel { .. } => self.umbriel_event(line),
            // No stream: the runtime calls take_wayland.
            Backend::Wayland { .. } => {}
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
                urgent: w.is_urgent,
                active_window: w.active_window_id.map(|i| i.to_string()),
            })
            .collect();
        let windows: Vec<Window> = model
            .windows
            .get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|w| Window {
                id: w.id.to_string(),
                title: w.title,
                app_id: w.app_id,
                workspace: w.workspace_id.map(|i| i.to_string()),
                pid: w.pid,
                floating: w.is_floating,
                urgent: w.is_urgent,
                ..Window::default()
            })
            .collect();
        let focused = model.focused.get_untracked().flatten().map(|i| i.to_string());
        let layouts = model
            .layouts
            .get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, l)| (id.to_string(), Layout { pos: l.pos, size: l.size, column: l.column, row: l.row }))
            .collect();
        batch(|| {
            self.set_workspaces(workspaces);
            self.set_windows(windows);
            self.set_focused(focused);
            self.set_layouts(layouts);
        });
    }

    /// What niri says that only the compositor service follows: the
    /// overview, screencasts and its own screenshots. True when the line
    /// was one of them.
    fn niri_extra(&self, line: &str) -> bool {
        if !(line.contains("\"Overview") || line.contains("\"Cast") || line.contains("\"ScreenshotCaptured")) {
            return false;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else { return false };
        let Some((kind, body)) = event.as_object().and_then(|o| o.iter().next()) else { return false };
        let cast = |v: &Value| -> Option<Cast> {
            let target = v.get("target");
            Some(Cast {
                id: id_of(v.get("stream_id")?)?,
                active: v.get("is_active").and_then(Value::as_bool).unwrap_or(true),
                pid: v.get("pid").and_then(Value::as_i64),
                node: v.get("pw_node_id").and_then(Value::as_u64),
                output: target.and_then(|t| text(t.get("Output")?, "name")),
                window: target.and_then(|t| id_of(t.get("Window")?.get("id")?)),
            })
        };
        match kind.as_str() {
            "OverviewOpenedOrClosed" => self.set_overview(body.get("is_open").and_then(Value::as_bool).unwrap_or(false)),
            "CastsChanged" => {
                self.set_casts(body.get("casts").and_then(Value::as_array).into_iter().flatten().filter_map(cast).collect())
            }
            "CastStartedOrChanged" => {
                let Some(c) = body.get("cast").and_then(cast) else { return true };
                let mut list: Vec<Cast> = self.casts.get_untracked().unwrap_or_default().into_iter().filter(|o| o.id != c.id).collect();
                list.push(c);
                self.set_casts(list);
            }
            "CastStopped" => {
                let Some(id) = body.get("stream_id").and_then(id_of) else { return true };
                self.set_casts(self.casts.get_untracked().unwrap_or_default().into_iter().filter(|o| o.id != id).collect());
            }
            "ScreenshotCaptured" => {
                let (n, _) = self.screenshot.get_untracked().unwrap_or_default();
                self.screenshot.set((n + 1, text(body, "path")));
            }
            _ => return false,
        }
        true
    }

    /// `KeyboardLayoutsChanged` carries the whole list, `KeyboardLayoutSwitched`
    /// only the index.
    fn niri_keyboard(&self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return };
        let mut keyboard = self.keyboard.get_untracked().unwrap_or_default();
        if let Some(layouts) = event.get("KeyboardLayoutsChanged").and_then(|e| e.get("keyboard_layouts")) {
            keyboard.layouts = layouts
                .get("names")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect();
            keyboard.current = layouts.get("current_idx").and_then(Value::as_u64).map(|i| i as usize);
        } else if let Some(idx) = event.get("KeyboardLayoutSwitched").and_then(|e| e.get("idx")).and_then(Value::as_u64) {
            keyboard.current = Some(idx as usize);
        } else {
            return;
        }
        keyboard.name = keyboard.current.and_then(|i| keyboard.layouts.get(i).cloned());
        self.set_keyboard(keyboard);
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
        // Each monitor's corner, by its number: clients are placed in the
        // whole layout's coordinates.
        let corner = |monitor: i64| {
            monitors
                .iter()
                .find(|m| m.get("id").and_then(Value::as_i64) == Some(monitor))
                .map(|m| (m.get("x").and_then(Value::as_f64).unwrap_or(0.0), m.get("y").and_then(Value::as_f64).unwrap_or(0.0)))
        };
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
                    active_window: text(w, "lastwindow").filter(|a| a.trim_start_matches("0x").bytes().any(|b| b != b'0')),
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
        let mapped: Vec<&Value> =
            clients.as_array().into_iter().flatten().filter(|c| c.get("mapped").and_then(Value::as_bool) != Some(false)).collect();
        let focused = text(&active, "address");
        // A window focused is a window answered.
        let urgent: Vec<String> =
            self.hyprland_urgent.borrow().iter().filter(|a| Some(*a) != focused.as_ref()).cloned().collect();
        *self.hyprland_urgent.borrow_mut() = urgent.clone();
        let windows: Vec<Window> = mapped
            .iter()
            .filter_map(|c| {
                let id = text(c, "address")?;
                Some(Window {
                    urgent: urgent.contains(&id),
                    id,
                    title: text(c, "title"),
                    app_id: text(c, "class"),
                    workspace: c.get("workspace")?.get("id")?.as_i64().map(|i| i.to_string()),
                    pid: c.get("pid").and_then(Value::as_i64).filter(|p| *p > 0),
                    floating: c.get("floating").and_then(Value::as_bool).unwrap_or(false),
                    ..Window::default()
                })
            })
            .collect();
        let layouts = mapped
            .iter()
            .filter_map(|c| {
                let (x, y) = pair(c.get("at"))?;
                let (ox, oy) = corner(c.get("monitor")?.as_i64()?)?;
                Some((text(c, "address")?, Layout { pos: Some((x - ox, y - oy)), size: pair(c.get("size"))?, column: None, row: None }))
            })
            .collect();
        batch(|| {
            self.set_workspaces(workspaces);
            self.set_windows(windows);
            self.set_focused(focused);
            self.set_layouts(layouts);
        });
        self.hyprland_keyboard();
    }

    /// The main keyboard's layouts, asked again.
    fn hyprland_keyboard(&self) {
        self.set_keyboard(hyprland_keyboard_of(&self.hyprland_json("devices")));
    }

    /// An event, `NAME>>DATA`: a title or the focus changed in place, any
    /// other change asks for the state again.
    fn hyprland_event(&self, line: &str) {
        let Some((name, data)) = line.trim_end().split_once(">>") else { return };
        match name {
            "activewindowv2" => {
                let id = (!data.is_empty() && data != ",").then(|| format!("0x{}", data.trim_start_matches("0x")));
                if let Some(id) = &id
                    && self.hyprland_urgent.borrow().contains(id)
                {
                    self.hyprland_urgent.borrow_mut().retain(|a| a != id);
                    self.windows.update(|list| list.iter().map(|w| if &w.id == id { Window { urgent: false, ..w.clone() } } else { w.clone() }).collect());
                }
                self.set_focused(id);
            }
            "urgent" => {
                let id = format!("0x{}", data.trim_start_matches("0x"));
                if self.focused.get_untracked().flatten().as_ref() == Some(&id) || self.hyprland_urgent.borrow().contains(&id) {
                    return;
                }
                self.hyprland_urgent.borrow_mut().push(id.clone());
                self.windows.update(|list| list.iter().map(|w| if w.id == id { Window { urgent: true, ..w.clone() } } else { w.clone() }).collect());
            }
            // `screencast>>STATE,OWNER`: one cast at a time, of a monitor
            // (owner 0) or a window (1), with no more said.
            "screencast" => {
                let on = data.split(',').next() == Some("1");
                self.set_casts(if on { vec![Cast { id: "hyprland".into(), active: true, ..Cast::default() }] } else { Vec::new() });
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
            "activelayout" => self.hyprland_keyboard(),
            "workspacev2" | "focusedmonv2" | "createworkspacev2" | "destroyworkspacev2" | "moveworkspacev2" | "renameworkspace"
            | "openwindow" | "closewindow" | "movewindowv2" | "monitoraddedv2" | "monitorremovedv2" | "configreloaded"
            | "changefloatingmode" | "fullscreen" => {
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
                            active_window: w.get("focused_window").and_then(id_of),
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
                            pid: w.get("pid").and_then(Value::as_i64).filter(|p| *p > 0),
                            floating: w.get("floating").and_then(Value::as_bool).unwrap_or(false),
                            urgent: w.get("urgent").and_then(Value::as_bool).unwrap_or(false),
                            ..Window::default()
                        })
                    })
                    .collect();
                // `active` is the window holding the keyboard; `focused` only
                // its workspace's remembered one.
                let focused = list.iter().find(|w| w.get("active").and_then(Value::as_bool) == Some(true)).and_then(|w| id_of(w.get("id")?));
                *self.umbriel_windows.borrow_mut() = Value::Array(list);
                batch(|| {
                    self.set_windows(windows);
                    self.set_focused(focused);
                    self.umbriel_layouts();
                });
            }
            Some("outputs") => {
                let corners = data
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|o| {
                        let p = o.get("position")?;
                        Some((text(o, "name")?, (p.get("x")?.as_f64()?, p.get("y")?.as_f64()?)))
                    })
                    .collect();
                *self.umbriel_outputs.borrow_mut() = corners;
                self.umbriel_layouts();
            }
            Some("overview") => {
                self.set_overview(data.and_then(|d| d.get("open")).and_then(Value::as_bool).unwrap_or(false));
            }
            _ => {}
        }
    }

    /// The windows' places, on their outputs: x and y come in the whole
    /// layout's coordinates, column and row from 0 (-1 floating).
    fn umbriel_layouts(&self) {
        let corners = self.umbriel_outputs.borrow();
        let layouts = self
            .umbriel_windows
            .borrow()
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| {
                let number = |key: &str| w.get(key).and_then(Value::as_f64);
                let (ox, oy) = text(w, "output").and_then(|o| corners.get(&o).copied()).unwrap_or((0.0, 0.0));
                let place = |key: &str| w.get(key).and_then(Value::as_i64).filter(|n| *n >= 0).map(|n| n as u64 + 1);
                let floating = w.get("floating").and_then(Value::as_bool) == Some(true);
                Some((
                    id_of(w.get("id")?)?,
                    Layout {
                        pos: Some((number("x")? - ox, number("y")? - oy)),
                        size: (number("w")?, number("h")?),
                        column: place("column").filter(|_| !floating),
                        row: place("row").filter(|_| !floating),
                    },
                ))
            })
            .collect();
        drop(corners);
        self.set_layouts(layouts);
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

    // ---- The standard protocols ---------------------------------------------

    /// What the protocols say now, as the common model. The focused output
    /// is the activated window's; with none activated, the one focused
    /// before, or the only one there is.
    pub fn take_wayland(&self, spaces: &[ProtocolWorkspace], toplevels: &[ProtocolToplevel]) {
        let windows: Vec<Window> = toplevels
            .iter()
            .map(|t| Window {
                id: t.id.clone(),
                title: t.title.clone(),
                app_id: t.app_id.clone(),
                workspace: None,
                output: t.outputs.first().cloned(),
                minimized: t.minimized,
                ..Window::default()
            })
            .collect();
        let focused = toplevels.iter().find(|t| t.activated).map(|t| t.id.clone());
        let mut outputs: Vec<&str> = spaces.iter().filter_map(|w| w.output.as_deref()).collect();
        outputs.dedup();
        let focused_output = toplevels
            .iter()
            .find(|t| t.activated)
            .and_then(|t| t.outputs.first().cloned())
            .or_else(|| self.workspaces.get_untracked().unwrap_or_default().into_iter().find(|w| w.focused).and_then(|w| w.output))
            .or_else(|| (outputs.len() == 1).then(|| outputs[0].to_owned()));
        let shown: Vec<&ProtocolWorkspace> = spaces.iter().filter(|w| w.state & 4 == 0).collect();
        // Its place on its output: by coordinates, whatever their
        // dimensions, then by the order the workspaces entered.
        let order = |w: &ProtocolWorkspace| (w.coordinates.clone(), w.place);
        let workspaces: Vec<Workspace> = shown
            .iter()
            .map(|w| {
                let active = w.state & 1 != 0;
                let before = shown.iter().filter(|o| o.output == w.output && order(o) < order(w)).count();
                Workspace {
                    id: w.id.clone(),
                    idx: before as u64 + 1,
                    // A number is the place, not a name (niri names its
                    // unnamed workspaces so).
                    name: w.name.clone().filter(|n| !n.bytes().all(|b| b.is_ascii_digit())),
                    output: w.output.clone(),
                    active,
                    focused: active && w.output.is_some() && w.output == focused_output,
                    vacant: false,
                    urgent: w.state & 2 != 0,
                    active_window: None,
                }
            })
            .collect();
        batch(|| {
            self.set_workspaces(workspaces);
            self.set_windows(windows);
            self.set_focused(focused);
        });
    }

    /// What the interface asked since the runtime last looked.
    pub fn take_requests(&self) -> Vec<Request> {
        match &self.backend {
            Backend::Wayland { requests } => std::mem::take(&mut *requests.borrow_mut()),
            _ => Vec::new(),
        }
    }

    /// Left for the runtime, once the id is known to be a window's or a
    /// workspace's.
    fn ask_runtime(&self, request: Request) -> Result<(), String> {
        let Backend::Wayland { requests } = &self.backend else { return Err("not the protocols".into()) };
        let known = match &request {
            Request::FocusWorkspace(id) => self.workspaces.get_untracked().unwrap_or_default().iter().any(|w| &w.id == id),
            Request::FocusWindow(id) | Request::CloseWindow(id) | Request::Minimize { window: id, .. } => {
                self.windows.get_untracked().unwrap_or_default().iter().any(|w| &w.id == id)
            }
        };
        if !known {
            return Err(match &request {
                Request::FocusWorkspace(id) => format!("no workspace {id}"),
                Request::FocusWindow(id) | Request::CloseWindow(id) | Request::Minimize { window: id, .. } => format!("no window {id}"),
            });
        }
        requests.borrow_mut().push(request);
        Ok(())
    }

    /// Minimizes `id`, or brings it back with `minimized` false.
    pub fn minimize_window(&self, id: &str, minimized: bool) -> Result<(), String> {
        if !self.can_minimize() {
            return Err(format!("{} does not minimize windows", self.name()));
        }
        self.ask_runtime(Request::Minimize { window: id.to_owned(), minimized })
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

    fn set_layouts(&self, layouts: HashMap<String, Layout>) {
        if self.layouts.get_untracked().as_ref() != Some(&layouts) {
            self.layouts.set(layouts);
        }
    }

    fn set_overview(&self, open: bool) {
        if self.overview.get_untracked() != Some(open) {
            self.overview.set(open);
        }
    }

    fn set_casts(&self, list: Vec<Cast>) {
        if self.casts.get_untracked().as_ref() != Some(&list) {
            self.casts.set(list);
        }
    }

    /// Where `id` is laid out, when the compositor says.
    pub fn layout(&self, id: &str) -> Option<Layout> {
        self.layouts.get().get(id).cloned()
    }

    fn set_keyboard(&self, keyboard: Keyboard) {
        if self.keyboard.get_untracked().as_ref() != Some(&keyboard) {
            self.keyboard.set(keyboard);
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
        (self.output_of(&window).as_deref() == Some(output)).then_some(window)
    }

    /// Where `window` is: what the compositor said, or its workspace's
    /// output.
    pub fn output_of(&self, window: &Window) -> Option<String> {
        if window.output.is_some() {
            return window.output.clone();
        }
        let id = window.workspace.as_ref()?;
        self.workspaces.get().into_iter().find(|w| &w.id == id)?.output
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
            Backend::Wayland { .. } => self.ask_runtime(Request::FocusWorkspace(id.to_owned())),
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
            Backend::Wayland { .. } => self.ask_runtime(Request::FocusWindow(id.to_owned())),
        }
    }

    /// The workspace `delta` places on from the one shown on `output` (the
    /// focused output without one): down is positive. Focusing it there
    /// moves the keyboard to that output, as a wheel over its workspaces
    /// means. Past the first or the last, nothing happens: niri keeps an
    /// empty one at the end, the others do not make one.
    pub fn step_workspace(&self, output: Option<&str>, delta: i64) -> Result<(), String> {
        let spaces = self.workspaces.get_untracked().unwrap_or_default();
        let output = match output {
            Some(o) => o.to_owned(),
            None => spaces.iter().find(|w| w.focused).and_then(|w| w.output.clone()).ok_or("no focused output")?,
        };
        let mut on: Vec<&Workspace> = spaces.iter().filter(|w| w.output.as_deref() == Some(output.as_str())).collect();
        on.sort_by_key(|w| w.idx);
        let Some(at) = on.iter().position(|w| w.active) else { return Err(format!("no workspace shown on {output}")) };
        let to = at as i64 + delta;
        if to < 0 || to >= on.len() as i64 || delta == 0 {
            return Ok(());
        }
        self.focus_workspace(&on[to as usize].id.clone())
    }

    /// Centres the focused window on its screen (niri, Umbriel; Hyprland
    /// centres a floating one only).
    pub fn center_window(&self) -> Result<(), String> {
        match &self.backend {
            Backend::Niri { socket, .. } => Self::niri_action(socket, "{\"CenterWindow\":{\"id\":null}}"),
            Backend::Hyprland { .. } => self.hyprland_dispatch("hl.dsp.window.center()", "centerwindow"),
            Backend::Umbriel { .. } => self.umbriel_action(serde_json::json!({ "action": "window-center" })),
            Backend::Wayland { .. } => Err(format!("{} does not centre windows", self.name())),
        }
    }

    /// Switches the keyboard layout. niri and Hyprland only; the new one
    /// comes back through the event stream as [`Compositor::keyboard`].
    pub fn switch_layout(&self, target: LayoutTarget) -> Result<(), String> {
        if let LayoutTarget::Index(i) = target {
            let count = self.keyboard.get_untracked().unwrap_or_default().layouts.len();
            if i >= count {
                return Err(format!("no layout {} (there are {count})", i + 1));
            }
        }
        match &self.backend {
            Backend::Niri { socket, .. } => {
                let layout = match target {
                    LayoutTarget::Next => "\"Next\"".to_owned(),
                    LayoutTarget::Prev => "\"Prev\"".to_owned(),
                    LayoutTarget::Index(i) => format!("{{\"Index\":{i}}}"),
                };
                Self::niri_action(socket, &format!("{{\"SwitchLayout\":{{\"layout\":{layout}}}}}"))
            }
            Backend::Hyprland { .. } => {
                let to = match target {
                    LayoutTarget::Next => "next".to_owned(),
                    LayoutTarget::Prev => "prev".to_owned(),
                    LayoutTarget::Index(i) => i.to_string(),
                };
                let reply = self.hyprland(&format!("switchxkblayout all {to}"))?;
                if reply.trim() == "ok" { Ok(()) } else { Err(reply.trim().to_owned()) }
            }
            _ => Err(format!("{} does not switch keyboard layouts", self.name())),
        }
    }

    /// Whether [`Compositor::power_monitors`] does anything: through each
    /// compositor's IPC, not through the protocols.
    pub fn can_power_monitors(&self) -> bool {
        !matches!(self.backend, Backend::Wayland { .. })
    }

    /// Every monitor's power, through DPMS: off blanks them with the session
    /// still running, on brings them back. niri brings them back by itself
    /// at the first input; Hyprland and Umbriel wait to be asked.
    pub fn power_monitors(&self, on: bool) -> Result<(), String> {
        match &self.backend {
            Backend::Niri { socket, .. } => {
                Self::niri_action(socket, if on { "{\"PowerOnMonitors\":{}}" } else { "{\"PowerOffMonitors\":{}}" })
            }
            Backend::Hyprland { .. } => {
                let (lua, classic) = if on { ("enable", "on") } else { ("disable", "off") };
                self.hyprland_dispatch(&format!("hl.dsp.dpms({{ action = \"{lua}\" }})"), &format!("dpms {classic}"))
            }
            Backend::Umbriel { .. } => {
                self.umbriel_action(serde_json::json!({ "action": if on { "dpms-on" } else { "dpms-off" } }))
            }
            Backend::Wayland { .. } => Err(format!("{} does not power monitors", self.name())),
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
            Backend::Wayland { .. } => self.ask_runtime(Request::CloseWindow(id.to_owned())),
        }
    }
}

/// `j/devices`: the main keyboard's layouts (the first keyboard when none
/// is marked main). `active_layout_index` is missing before Hyprland 0.47;
/// the name is there in every version.
fn hyprland_keyboard_of(devices: &Value) -> Keyboard {
    let keyboards = devices.get("keyboards").and_then(Value::as_array).cloned().unwrap_or_default();
    let Some(main) = keyboards.iter().find(|k| k.get("main").and_then(Value::as_bool) == Some(true)).or(keyboards.first()) else {
        return Keyboard::default();
    };
    let layouts: Vec<String> =
        main.get("layout").and_then(Value::as_str).unwrap_or("").split(',').map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let current = main.get("active_layout_index").and_then(Value::as_u64).map(|i| i as usize).filter(|i| *i < layouts.len());
    Keyboard { layouts, current, name: text(main, "active_keymap") }
}

/// A window address, checked to be one: it goes into a Lua string.
fn hyprland_address(id: &str) -> Result<&str, String> {
    let hex = id.strip_prefix("0x").unwrap_or("");
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("no Hyprland window {id}"));
    }
    Ok(id)
}

fn pair(v: Option<&Value>) -> Option<(f64, f64)> {
    let a = v?.as_array()?;
    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
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
        c.windows.set(vec![Window { id: "0x55aa".into(), title: Some("a".into()), workspace: Some("1".into()), ..Window::default() }]);
        c.line("activewindowv2>>55aa\n");
        assert_eq!(c.focused.get_untracked().flatten().as_deref(), Some("0x55aa"));
        c.line("windowtitlev2>>55aa,b, with a comma\n");
        assert_eq!(c.title.get().as_deref(), Some("b, with a comma"));
        c.line("activewindowv2>>\n");
        assert_eq!(c.title.get(), None);
    }

    #[test]
    fn the_protocols_reach_the_common_model() {
        let c = Compositor::wayland();
        let space = |id: &str, name: &str, state, output: &str, place| ProtocolWorkspace {
            id: id.into(),
            name: Some(name.into()),
            state,
            output: Some(output.into()),
            place,
            ..ProtocolWorkspace::default()
        };
        let spaces = [
            ProtocolWorkspace { coordinates: vec![0, 1], ..space("2", "web", 0, "DP-1", 0) },
            ProtocolWorkspace { coordinates: vec![0, 0], ..space("1", "1", 1, "DP-1", 1) },
            space("3", "3", 1 | 2, "HDMI-A-1", 0),
            space("4", "scratch", 4, "HDMI-A-1", 1),
        ];
        let window = |id: &str, output: &str, activated| ProtocolToplevel {
            id: id.into(),
            title: Some(format!("t{id}")),
            outputs: vec![output.into()],
            activated,
            ..ProtocolToplevel::default()
        };
        c.take_wayland(&spaces, &[window("7", "DP-1", false), window("8", "HDMI-A-1", true)]);
        assert_eq!(c.workspaces.get_untracked().unwrap().len(), 3, "a hidden workspace is left out");
        assert_eq!(c.on_output(Some("DP-1")).iter().map(|w| w.name.clone()).collect::<Vec<_>>(), [None, Some("web".into())]);
        assert_eq!(c.focused_output().as_deref(), Some("HDMI-A-1"));
        assert!(c.active("HDMI-A-1").is_some_and(|w| w.idx == 1));
        assert_eq!(c.on_output(Some("DP-1")).iter().map(|w| (w.id.as_str(), w.idx)).collect::<Vec<_>>(), [("1", 1), ("2", 2)], "coordinates before entry order");
        assert!(c.on_output(Some("HDMI-A-1"))[0].urgent);
        assert_eq!(c.focused_on("HDMI-A-1").map(|w| w.id), Some("8".into()));
        assert_eq!(c.focused_on("DP-1"), None);
        // Nothing activated: the output focused before stays so.
        c.take_wayland(&spaces, &[window("7", "DP-1", false)]);
        assert_eq!((c.focused_output().as_deref(), c.title.get()), (Some("HDMI-A-1"), None));
        assert_eq!(c.focus_window("8"), Err("no window 8".into()));
        assert_eq!(c.minimize_window("7", true), Ok(()));
        assert_eq!(c.focus_workspace("2"), Ok(()));
        assert_eq!(
            c.take_requests(),
            [Request::Minimize { window: "7".into(), minimized: true }, Request::FocusWorkspace("2".into())]
        );
        assert!(c.take_requests().is_empty());
    }

    #[test]
    fn keyboard_layouts_from_niri_and_hyprland() {
        let c = Compositor::new(Backend::Niri { socket: PathBuf::from("/nonexistent"), model: Niri::model() });
        c.line(r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","Italian"],"current_idx":0}}}"#);
        assert_eq!(c.keyboard.get_untracked().unwrap().name.as_deref(), Some("English (US)"));
        c.line(r#"{"KeyboardLayoutSwitched":{"idx":1}}"#);
        let k = c.keyboard.get_untracked().unwrap();
        assert_eq!((k.layouts.len(), k.current, k.name.as_deref()), (2, Some(1), Some("Italian")));
        assert_eq!(c.switch_layout(LayoutTarget::Index(2)), Err("no layout 3 (there are 2)".into()));
        let devices = serde_json::json!({ "keyboards": [
            { "name": "virtual", "layout": "us", "active_keymap": "English (US)", "main": false },
            { "name": "kbd", "layout": "us, it", "active_keymap": "Italian", "active_layout_index": 1, "main": true },
        ] });
        assert_eq!(
            hyprland_keyboard_of(&devices),
            Keyboard { layouts: vec!["us".into(), "it".into()], current: Some(1), name: Some("Italian".into()) }
        );
        assert_eq!(hyprland_keyboard_of(&Value::Null), Keyboard::default());
    }

    #[test]
    fn niri_says_pids_floating_urgency_layouts_and_the_rest() {
        let c = Compositor::new(Backend::Niri { socket: PathBuf::from("/nonexistent"), model: Niri::model() });
        c.line(r#"{"WorkspacesChanged":{"workspaces":[{"id":5,"idx":1,"output":"DP-1","is_active":true,"is_focused":true,"active_window_id":9},{"id":6,"idx":2,"output":"DP-1","is_active":false,"is_focused":false,"active_window_id":null}]}}"#);
        c.line(r#"{"WindowsChanged":{"windows":[{"id":9,"title":"t","app_id":"a","pid":42,"workspace_id":5,"is_focused":true,"is_floating":false,"is_urgent":false,"layout":{"pos_in_scrolling_layout":[2,1],"tile_size":[800.0,1000.0],"window_size":[796,996],"tile_pos_in_workspace_view":[16.0,50.0],"window_offset_in_tile":[2,2]}}]}}"#);
        let w = c.focused_window().unwrap();
        assert_eq!((w.pid, w.floating, w.urgent), (Some(42), false, false));
        assert_eq!(c.active("DP-1").and_then(|w| w.active_window).as_deref(), Some("9"));
        assert_eq!(c.layout("9"), Some(Layout { pos: Some((16.0, 50.0)), size: (800.0, 1000.0), column: Some(2), row: Some(1) }));

        // A resize moves the layout and nothing else.
        let windows = c.windows.get_untracked();
        c.line(r#"{"WindowLayoutsChanged":{"changes":[[9,{"pos_in_scrolling_layout":null,"tile_size":[400.0,300.0],"window_size":[400,300],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[0,0]}]]}}"#);
        assert_eq!(c.layout("9"), Some(Layout { pos: None, size: (400.0, 300.0), column: None, row: None }));
        assert_eq!(c.windows.get_untracked(), windows);

        c.line(r#"{"WindowUrgencyChanged":{"id":9,"urgent":true}}"#);
        c.line(r#"{"WorkspaceUrgencyChanged":{"id":6,"urgent":true}}"#);
        assert!(c.focused_window().unwrap().urgent && c.on_output(Some("DP-1"))[1].urgent);
        c.line(r#"{"WorkspaceActiveWindowChanged":{"workspace_id":5,"active_window_id":null}}"#);
        assert!(c.active("DP-1").is_some_and(|w| w.active_window.is_none() && w.vacant));

        c.line(r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#);
        assert!(c.overview.get_untracked().unwrap());
        c.line(r#"{"CastsChanged":{"casts":[{"stream_id":3,"session_id":1,"kind":"PipeWire","target":{"Output":{"name":"DP-1"}},"is_active":true,"pid":77,"pw_node_id":120}]}}"#);
        c.line(r#"{"CastStartedOrChanged":{"cast":{"stream_id":4,"target":{"Window":{"id":9}},"is_active":false}}}"#);
        let casts = c.casts.get_untracked().unwrap();
        assert_eq!(casts[0], Cast { id: "3".into(), active: true, pid: Some(77), node: Some(120), output: Some("DP-1".into()), window: None });
        assert_eq!((casts[1].window.as_deref(), casts[1].active), (Some("9"), false));
        c.line(r#"{"CastStopped":{"stream_id":3}}"#);
        assert_eq!(c.casts.get_untracked().unwrap().len(), 1);
        c.line(r#"{"ScreenshotCaptured":{"path":"/tmp/s.png"}}"#);
        assert_eq!(c.screenshot.get_untracked().unwrap(), (1, Some("/tmp/s.png".into())));
        c.line(r#"{"WindowClosed":{"id":9}}"#);
        assert_eq!(c.layout("9"), None);
    }

    #[test]
    fn hyprland_urgency_lasts_until_the_focus() {
        let c = Compositor::new(Backend::Hyprland { dir: PathBuf::from("/nonexistent") });
        c.windows.set(vec![Window { id: "0x1".into(), ..Window::default() }, Window { id: "0x2".into(), ..Window::default() }]);
        c.line("activewindowv2>>1\n");
        c.line("urgent>>1\n");
        assert!(!c.windows.get_untracked().unwrap()[0].urgent, "the focused window is not asking");
        c.line("urgent>>2\n");
        assert!(c.windows.get_untracked().unwrap()[1].urgent);
        c.line("activewindowv2>>2\n");
        assert!(!c.windows.get_untracked().unwrap()[1].urgent);
        c.line("screencast>>1,0\n");
        assert_eq!(c.casts.get_untracked().unwrap().len(), 1);
        c.line("screencast>>0,0\n");
        assert!(c.casts.get_untracked().unwrap().is_empty());
    }

    #[test]
    fn umbriel_places_windows_on_their_outputs() {
        let c = umbriel();
        // As Umbriel 0.1.0 sends them: x and y across the whole layout,
        // column and row from 0, -1 floating.
        c.line(r#"{"event":"windows","data":[{"id":"a","output":"B","x":1608,"y":10,"w":624,"h":700,"column":0,"row":0,"floating":false,"pid":7,"urgent":true,"active":true},{"id":"f","output":"A","x":328,"y":18,"w":624,"h":700,"column":-1,"row":-1,"floating":true}]}"#);
        c.line(r#"{"event":"outputs","data":[{"name":"A","position":{"x":0,"y":0}},{"name":"B","position":{"x":1280,"y":0}}]}"#);
        assert_eq!(c.layout("a"), Some(Layout { pos: Some((328.0, 10.0)), size: (624.0, 700.0), column: Some(1), row: Some(1) }));
        assert_eq!(c.layout("f").map(|l| (l.pos, l.column, l.row)), Some((Some((328.0, 18.0)), None, None)));
        let a = c.focused_window().unwrap();
        assert_eq!((a.pid, a.urgent, a.floating), (Some(7), true, false));
        c.line(r#"{"event":"overview","data":{"open":true}}"#);
        assert!(c.overview.get_untracked().unwrap());
        c.line(r#"{"event":"workspaces","data":[{"id":"B:1","index":1,"output":"B","active":true,"focused":true,"focused_window":"a"},{"id":"B:2","index":2,"output":"B","active":false,"focused":false,"focused_window":""}]}"#);
        assert_eq!(c.on_output(Some("B")).iter().map(|w| w.active_window.clone()).collect::<Vec<_>>(), [Some("a".into()), None]);
    }

    #[test]
    fn a_workspace_step_stays_on_its_output_and_inside_the_list() {
        let c = Compositor::wayland();
        let space = |id: &str, state, output: &str, place| ProtocolWorkspace { id: id.into(), state, output: Some(output.into()), place, ..ProtocolWorkspace::default() };
        c.take_wayland(&[space("1", 1, "A", 0), space("2", 0, "A", 1), space("3", 1, "B", 0), space("4", 0, "B", 1)], &[]);
        assert_eq!(c.step_workspace(Some("B"), 1), Ok(()));
        assert_eq!(c.step_workspace(Some("B"), -1), Ok(()), "past the first: nothing");
        assert_eq!(c.step_workspace(Some("A"), 5), Ok(()), "past the last: nothing");
        assert_eq!(c.take_requests(), [Request::FocusWorkspace("4".into())]);
        assert_eq!(c.step_workspace(Some("C"), 1), Err("no workspace shown on C".into()));
    }

    /// A socket that answers each connection with `reply` and hands back
    /// what it was sent, one request a connection.
    fn listener(path: &std::path::Path, reply: &'static str, count: usize) -> std::thread::JoinHandle<Vec<String>> {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            (0..count)
                .map(|_| {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = String::new();
                    BufReader::new(stream.try_clone().unwrap()).read_line(&mut request).unwrap();
                    stream.write_all(reply.as_bytes()).unwrap();
                    request.trim_end().to_owned()
                })
                .collect()
        })
    }

    #[test]
    fn monitors_power_off_and_on_in_each_tongue() {
        let dir = std::env::temp_dir().join(format!("genoma-power-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let socket = dir.join("niri.sock");
        let said = listener(&socket, "{\"Ok\":\"Handled\"}\n", 2);
        let c = Compositor::new(Backend::Niri { socket, model: Niri::model() });
        assert!(c.can_power_monitors());
        assert_eq!((c.power_monitors(false), c.power_monitors(true)), (Ok(()), Ok(())));
        assert_eq!(said.join().unwrap(), ["{\"Action\":{\"PowerOffMonitors\":{}}}", "{\"Action\":{\"PowerOnMonitors\":{}}}"]);

        let socket = dir.join("umbriel.sock");
        let said = listener(&socket, "{\"ok\":true}\n", 1);
        let c = Compositor::new(Backend::Umbriel { socket });
        assert_eq!(c.power_monitors(false), Ok(()));
        let sent: Value = serde_json::from_str(&said.join().unwrap()[0]).unwrap();
        assert_eq!(sent, serde_json::json!({ "cmd": "action", "action": "dpms-off" }));

        // Hyprland in hyprlang refuses the Lua form, then takes the classic.
        let hypr = dir.join("hypr");
        std::fs::create_dir_all(&hypr).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(hypr.join(".socket.sock")).unwrap();
        let said = std::thread::spawn(move || {
            ["unknown request", "ok"]
                .iter()
                .map(|reply| {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buf = [0u8; 256];
                    let n = stream.read(&mut buf).unwrap();
                    stream.write_all(reply.as_bytes()).unwrap();
                    String::from_utf8_lossy(&buf[..n]).into_owned()
                })
                .collect::<Vec<_>>()
        });
        let c = Compositor::new(Backend::Hyprland { dir: hypr });
        assert_eq!(c.power_monitors(true), Ok(()));
        assert_eq!(said.join().unwrap(), ["dispatch hl.dsp.dpms({ action = \"enable\" })", "dispatch dpms on"]);

        assert!(!Compositor::wayland().can_power_monitors());
        assert_eq!(Compositor::wayland().power_monitors(false), Err("wayland does not power monitors".into()));
        std::fs::remove_dir_all(&dir).ok();
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
