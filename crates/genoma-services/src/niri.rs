//! niri, through its own IPC socket: one connection, one `"EventStream"`
//! request, newline-delimited JSON for the life of the session (Bioma's
//! `services/Niri.qml`). Nothing polls.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    time::Duration,
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
    /// No window on it: the free place.
    pub vacant: bool,
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
        vacant: v.get("active_window_id").is_none_or(Value::is_null),
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
        Ok((Niri::model(), stream))
    }

    /// The state alone, empty, filled by [`Niri::line`]: what the
    /// compositor service reads niri's stream into.
    pub fn model() -> Niri {
        let workspaces = Signal::new(Vec::new());
        let windows: Signal<Vec<Window>> = Signal::new(Vec::new());
        let focused: Signal<Option<u64>> = Signal::new(None);
        let title = Memo::new(move || {
            let id = focused.get()?;
            windows.get().into_iter().find(|w| w.id == id)?.title
        });
        Niri { workspaces, windows, focused, title }
    }

    /// The output the keyboard is pointed at: the focused workspace's.
    pub fn focused_output(&self) -> Option<String> {
        self.workspaces.get().into_iter().find(|w| w.is_focused).and_then(|w| w.output)
    }

    /// The workspace shown on `output` now.
    pub fn active(&self, output: &str) -> Option<Workspace> {
        self.workspaces.get().into_iter().find(|w| w.is_active && w.output.as_deref() == Some(output))
    }

    /// The workspaces on `output`, in their order.
    pub fn on_output(&self, output: &str) -> Vec<Workspace> {
        let mut list: Vec<_> =
            self.workspaces.get().into_iter().filter(|w| w.output.as_deref() == Some(output)).collect();
        list.sort_by_key(|w| w.idx);
        list
    }

    /// Sends one action and waits for niri's reply, on a connection of its
    /// own: the event stream is never written to. `action` is the JSON of
    /// niri's `Action`, as `{"FocusWorkspace":{"reference":{"Id":3}}}`.
    pub fn action(&self, action: &str) -> Result<(), String> {
        let path = std::env::var_os("NIRI_SOCKET").ok_or("NIRI_SOCKET is not set")?;
        let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
        stream.write_all(format!("{{\"Action\":{action}}}\n").as_bytes()).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
        if reply.contains("\"Err\"") {
            return Err(reply.trim().to_owned());
        }
        Ok(())
    }

    pub fn focus_workspace(&self, id: u64) -> Result<(), String> {
        self.action(&format!("{{\"FocusWorkspace\":{{\"reference\":{{\"Id\":{id}}}}}}}"))
    }

    /// The focused window, wherever it is.
    pub fn focused_window(&self) -> Option<Window> {
        let id = self.focused.get()?;
        self.windows.get().into_iter().find(|w| w.id == id)
    }

    /// The focused window, only if it is on `output`: a membrane's title
    /// speaks only of its own screen.
    pub fn focused_on(&self, output: &str) -> Option<Window> {
        let id = self.focused.get()?;
        let window = self.windows.get().into_iter().find(|w| w.id == id)?;
        let ws = self.workspaces.get().into_iter().find(|w| Some(w.id) == window.workspace_id)?;
        (ws.output.as_deref() == Some(output)).then_some(window)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(lines: &[&str]) -> Niri {
        let niri = Niri::model();
        for line in lines {
            niri.line(line);
        }
        niri
    }

    const WORKSPACES: &str = r#"{"WorkspacesChanged":{"workspaces":[
        {"id":1,"idx":1,"name":null,"output":"DP-1","is_active":true,"is_focused":true,"active_window_id":7},
        {"id":2,"idx":2,"name":"web","output":"DP-1","is_active":false,"is_focused":false,"active_window_id":null},
        {"id":3,"idx":1,"name":null,"output":"HDMI-A-1","is_active":true,"is_focused":false}]}}"#;
    const WINDOWS: &str = r#"{"WindowsChanged":{"windows":[
        {"id":7,"title":"Terminal","app_id":"foot","workspace_id":1,"is_focused":true},
        {"id":8,"title":"Docs","app_id":"zen","workspace_id":3,"is_focused":false}]}}"#;

    #[test]
    fn workspaces_are_read_by_output_in_order() {
        let niri = fed(&[WORKSPACES]);
        assert_eq!(niri.focused_output().as_deref(), Some("DP-1"));
        assert_eq!(niri.on_output("DP-1").iter().map(|w| w.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(niri.active("HDMI-A-1").map(|w| w.id), Some(3));
        let ws = niri.on_output("DP-1");
        assert!(!ws[0].vacant && ws[1].vacant, "a null or missing window is vacant");
        assert_eq!(ws[1].name.as_deref(), Some("web"));
        assert!(niri.on_output("DP-1").iter().all(|w| w.output.is_some()));
    }

    #[test]
    fn activating_a_workspace_moves_only_its_output() {
        let niri = fed(&[WORKSPACES, r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#]);
        assert_eq!(niri.active("DP-1").map(|w| w.id), Some(2));
        assert_eq!(niri.active("HDMI-A-1").map(|w| w.id), Some(3), "the other output keeps its own");
        assert_eq!(niri.workspaces.get().iter().filter(|w| w.is_focused).count(), 1);
        // Shown without the focus: the focus stays where it was.
        let niri = fed(&[WORKSPACES, r#"{"WorkspaceActivated":{"id":2,"focused":false}}"#]);
        assert_eq!(niri.focused_output().as_deref(), Some("DP-1"));
        assert!(niri.workspaces.get().iter().find(|w| w.id == 1).is_some_and(|w| w.is_focused));
    }

    #[test]
    fn the_focused_title_follows_opens_changes_and_closes() {
        let niri = fed(&[WORKSPACES, WINDOWS]);
        assert_eq!(niri.title.get().as_deref(), Some("Terminal"));
        assert_eq!(niri.focused_on("DP-1").map(|w| w.id), Some(7));
        assert_eq!(niri.focused_on("HDMI-A-1"), None, "a membrane speaks only of its screen");
        niri.line(r#"{"WindowOpenedOrChanged":{"window":{"id":7,"title":"vim","app_id":"foot","workspace_id":1,"is_focused":true}}}"#);
        assert_eq!(niri.title.get().as_deref(), Some("vim"));
        assert_eq!(niri.windows.get().len(), 2, "changed, not added");
        niri.line(r#"{"WindowFocusChanged":{"id":8}}"#);
        assert_eq!(niri.title.get().as_deref(), Some("Docs"));
        niri.line(r#"{"WindowClosed":{"id":8}}"#);
        assert_eq!(niri.title.get(), None);
        assert_eq!(niri.focused.get(), None);
        niri.line(r#"{"WindowFocusChanged":{"id":null}}"#);
        assert_eq!(niri.focused.get(), None);
    }

    #[test]
    fn what_is_not_understood_changes_nothing() {
        let niri = fed(&[WORKSPACES, "not json", r#"{"SomethingNew":{}}"#, r#"{"WorkspaceActivated":{}}"#, "[]"]);
        assert_eq!(niri.workspaces.get().len(), 3);
    }
}
