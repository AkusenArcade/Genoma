//! niri, through its own IPC socket: one connection, one `"EventStream"`
//! request, newline-delimited JSON for the life of the session (Bioma's
//! `services/Niri.qml`). Nothing polls.

use std::{
    io::Write,
    os::unix::net::UnixStream,
};

use genoma_core::reactive::{Memo, Signal, batch};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: u64,
    pub idx: u64,
    pub name: Option<String>,
    pub output: Option<String>,
    pub is_active: bool,
    pub is_focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub workspace_id: Option<u64>,
}

pub struct Niri {
    pub workspaces: Signal<Vec<Workspace>>,
    pub windows: Signal<Vec<Window>>,
    pub focused: Signal<Option<u64>>,
    /// The focused window's title, or nothing when no window has focus.
    pub title: Memo<Option<String>>,
}

fn workspace(v: &Value) -> Option<Workspace> {
    Some(Workspace {
        id: v.get("id")?.as_u64()?,
        idx: v.get("idx")?.as_u64()?,
        name: v.get("name").and_then(Value::as_str).map(str::to_owned),
        output: v.get("output").and_then(Value::as_str).map(str::to_owned),
        is_active: v.get("is_active").and_then(Value::as_bool).unwrap_or(false),
        is_focused: v.get("is_focused").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn window(v: &Value) -> Option<(Window, bool)> {
    Some((
        Window {
            id: v.get("id")?.as_u64()?,
            title: v.get("title").and_then(Value::as_str).map(str::to_owned),
            app_id: v.get("app_id").and_then(Value::as_str).map(str::to_owned),
            workspace_id: v.get("workspace_id").and_then(Value::as_u64),
        },
        v.get("is_focused").and_then(Value::as_bool).unwrap_or(false),
    ))
}

impl Niri {
    /// Connects to `$NIRI_SOCKET` and asks for the event stream. The caller
    /// reads lines from the returned stream and hands each to [`Niri::line`].
    pub fn connect() -> std::io::Result<(Niri, UnixStream)> {
        let path = std::env::var_os("NIRI_SOCKET")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "NIRI_SOCKET is not set"))?;
        let mut stream = UnixStream::connect(path)?;
        stream.write_all(b"\"EventStream\"\n")?;
        let workspaces = Signal::new(Vec::new());
        let windows: Signal<Vec<Window>> = Signal::new(Vec::new());
        let focused: Signal<Option<u64>> = Signal::new(None);
        let title = Memo::new(move || {
            let id = focused.get()?;
            windows.get().into_iter().find(|w| w.id == id)?.title
        });
        Ok((Niri { workspaces, windows, focused, title }, stream))
    }

    /// The workspace shown on `output` now.
    pub fn active(&self, output: &str) -> Option<Workspace> {
        self.workspaces.get().into_iter().find(|w| w.is_active && w.output.as_deref() == Some(output))
    }

    /// One line of the event stream.
    pub fn line(&self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else { return };
        let Some((kind, body)) = event.as_object().and_then(|o| o.iter().next()) else { return };
        batch(|| match kind.as_str() {
            "WorkspacesChanged" => {
                let list = body.get("workspaces").and_then(Value::as_array);
                self.workspaces.set(list.into_iter().flatten().filter_map(workspace).collect());
            }
            "WorkspaceActivated" => {
                let (Some(id), focused) = (
                    body.get("id").and_then(Value::as_u64),
                    body.get("focused").and_then(Value::as_bool).unwrap_or(false),
                ) else {
                    return;
                };
                self.workspaces.update(|list| {
                    let output = list.iter().find(|w| w.id == id).and_then(|w| w.output.clone());
                    list.iter()
                        .map(|w| {
                            let mut w = w.clone();
                            if w.output == output {
                                w.is_active = w.id == id;
                            }
                            if focused {
                                w.is_focused = w.id == id;
                            }
                            w
                        })
                        .collect()
                });
            }
            "WindowsChanged" => {
                let list = body.get("windows").and_then(Value::as_array);
                let parsed: Vec<_> = list.into_iter().flatten().filter_map(window).collect();
                if let Some((w, _)) = parsed.iter().find(|(_, f)| *f) {
                    self.focused.set(Some(w.id));
                }
                self.windows.set(parsed.into_iter().map(|(w, _)| w).collect());
            }
            "WindowOpenedOrChanged" => {
                let Some((w, focused)) = body.get("window").and_then(window) else { return };
                let id = w.id;
                self.windows.update(|list| {
                    let mut list: Vec<_> = list.iter().filter(|x| x.id != id).cloned().collect();
                    list.push(w);
                    list
                });
                if focused {
                    self.focused.set(Some(id));
                }
            }
            "WindowClosed" => {
                let Some(id) = body.get("id").and_then(Value::as_u64) else { return };
                self.windows.update(|list| list.iter().filter(|x| x.id != id).cloned().collect());
                if self.focused.get_untracked() == Some(Some(id)) {
                    self.focused.set(None);
                }
            }
            "WindowFocusChanged" => self.focused.set(body.get("id").and_then(Value::as_u64)),
            _ => {}
        });
    }
}
