//! `@genoma/compositor` where no compositor IPC is known: workspaces from
//! `ext-workspace-v1`, windows from `wlr-foreign-toplevel-management`, both
//! on the main thread's connection. Bound once an interface requires the
//! compositor; what they say goes to the service as a whole picture after
//! each `done`, and what the interface asks of it comes back as requests on
//! the next turn.
//!
//! Handles get numbers of the runtime's own as their ids: neither protocol
//! gives windows one, and a workspace's `id` is optional.

use std::rc::Rc;

use genoma_services::compositor::{Compositor, ProtocolToplevel, ProtocolWorkspace, Request};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    reexports::{
        client::{Connection, QueueHandle, WEnum, protocol::wl_output::WlOutput},
        protocols::ext::workspace::v1::client::{
            ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
            ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
            ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
        },
        protocols_wlr::foreign_toplevel::v1::client::{
            zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        },
    },
};

use super::App;

/// The protocols' objects and what they said so far.
pub(super) struct Wm {
    compositor: Rc<Compositor>,
    workspaces: Option<ExtWorkspaceManagerV1>,
    toplevels: Option<ZwlrForeignToplevelManagerV1>,
    groups: Vec<Group>,
    spaces: Vec<Space>,
    windows: Vec<Toplevel>,
    next: u64,
}

struct Group {
    handle: ExtWorkspaceGroupHandleV1,
    outputs: Vec<WlOutput>,
    /// Its workspaces, in the order they entered.
    members: Vec<ExtWorkspaceHandleV1>,
}

struct Space {
    handle: ExtWorkspaceHandleV1,
    id: String,
    name: Option<String>,
    coordinates: Vec<u32>,
    state: u32,
}

struct Toplevel {
    handle: ZwlrForeignToplevelHandleV1,
    id: String,
    title: Option<String>,
    app_id: Option<String>,
    outputs: Vec<WlOutput>,
    /// `zwlr_foreign_toplevel_handle_v1.state` values.
    state: Vec<u32>,
}

/// Every object of both protocols: the handles are found by equality.
pub(super) struct WmData;

/// A protocol array of native-endian `u32`s.
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn named(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

impl App {
    /// Binds both protocols for `compositor`, once. A compositor without
    /// one leaves its half of the model empty.
    pub fn start_wm(&mut self, compositor: Rc<Compositor>) {
        if self.wm.is_some() {
            return;
        }
        let workspaces = self.globals.bind::<ExtWorkspaceManagerV1, _, _>(&self.qh, 1..=1, WmData).ok();
        let toplevels = self.globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&self.qh, 1..=3, WmData).ok();
        log::info!(
            "compositor through the protocols: ext-workspace {}, wlr-foreign-toplevel {}",
            if workspaces.is_some() { "yes" } else { "no" },
            if toplevels.is_some() { "yes" } else { "no" },
        );
        self.wm = Some(Wm { compositor, workspaces, toplevels, groups: Vec::new(), spaces: Vec::new(), windows: Vec::new(), next: 0 });
    }

    /// What the interface asked since the last turn, sent.
    pub fn sync_wm(&mut self) {
        let Some(wm) = &self.wm else { return };
        let requests = wm.compositor.take_requests();
        if requests.is_empty() {
            return;
        }
        let seat = self.seat.clone().or_else(|| self.seats.seats().next());
        for request in requests {
            match request {
                Request::FocusWorkspace(id) => {
                    if let (Some(space), Some(manager)) = (wm.spaces.iter().find(|s| s.id == id), &wm.workspaces) {
                        space.handle.activate();
                        manager.commit();
                    }
                }
                Request::FocusWindow(id) => {
                    if let (Some(window), Some(seat)) = (wm.windows.iter().find(|w| w.id == id), &seat) {
                        if window.state.contains(&1) {
                            window.handle.unset_minimized();
                        }
                        window.handle.activate(seat);
                    }
                }
                Request::CloseWindow(id) => {
                    if let Some(window) = wm.windows.iter().find(|w| w.id == id) {
                        window.handle.close();
                    }
                }
                Request::Minimize { window, minimized } => {
                    if let Some(window) = wm.windows.iter().find(|w| w.id == window) {
                        if minimized {
                            window.handle.set_minimized();
                        } else {
                            window.handle.unset_minimized();
                        }
                    }
                }
            }
        }
        let _ = self.conn.flush();
    }

    /// The whole picture, handed to the service.
    fn publish_wm(&mut self) {
        let Some(wm) = &self.wm else { return };
        let name = |o: &WlOutput| self.outputs.info(o).and_then(|i| i.name);
        let spaces: Vec<ProtocolWorkspace> = wm
            .spaces
            .iter()
            .map(|s| {
                let group = wm.groups.iter().find(|g| g.members.contains(&s.handle));
                ProtocolWorkspace {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    coordinates: s.coordinates.clone(),
                    state: s.state,
                    output: group.and_then(|g| g.outputs.first()).and_then(name),
                    place: group.and_then(|g| g.members.iter().position(|m| m == &s.handle)).unwrap_or(0),
                }
            })
            .collect();
        let windows: Vec<ProtocolToplevel> = wm
            .windows
            .iter()
            .map(|t| ProtocolToplevel {
                id: t.id.clone(),
                title: t.title.clone(),
                app_id: t.app_id.clone(),
                outputs: t.outputs.iter().filter_map(name).collect(),
                minimized: t.state.contains(&1),
                activated: t.state.contains(&2),
            })
            .collect();
        wm.compositor.take_wayland(&spaces, &windows);
    }

    fn wm_id(&mut self) -> String {
        let wm = self.wm.as_mut().expect("bound before its events");
        wm.next += 1;
        wm.next.to_string()
    }
}

impl Dispatch2<ExtWorkspaceManagerV1, App> for WmData {
    fn event(&self, app: &mut App, _: &ExtWorkspaceManagerV1, event: ext_workspace_manager_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                let Some(wm) = app.wm.as_mut() else { return };
                wm.groups.push(Group { handle: workspace_group, outputs: Vec::new(), members: Vec::new() });
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                let id = app.wm_id();
                let Some(wm) = app.wm.as_mut() else { return };
                wm.spaces.push(Space { handle: workspace, id, name: None, coordinates: Vec::new(), state: 0 });
            }
            ext_workspace_manager_v1::Event::Done => app.publish_wm(),
            ext_workspace_manager_v1::Event::Finished => {
                if let Some(wm) = app.wm.as_mut() {
                    wm.workspaces = None;
                    wm.groups.clear();
                    wm.spaces.clear();
                }
                app.publish_wm();
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(App, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, WmData),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, WmData)
    ]);
}

impl Dispatch2<ExtWorkspaceGroupHandleV1, App> for WmData {
    fn event(&self, app: &mut App, handle: &ExtWorkspaceGroupHandleV1, event: ext_workspace_group_handle_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(wm) = app.wm.as_mut() else { return };
        let Some(i) = wm.groups.iter().position(|g| &g.handle == handle) else { return };
        let group = &mut wm.groups[i];
        // Applied at the manager's `done`.
        match event {
            ext_workspace_group_handle_v1::Event::OutputEnter { output } => group.outputs.push(output),
            ext_workspace_group_handle_v1::Event::OutputLeave { output } => group.outputs.retain(|o| o != &output),
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => group.members.push(workspace),
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => group.members.retain(|w| w != &workspace),
            ext_workspace_group_handle_v1::Event::Removed => {
                handle.destroy();
                wm.groups.remove(i);
            }
            _ => {}
        }
    }
}

impl Dispatch2<ExtWorkspaceHandleV1, App> for WmData {
    fn event(&self, app: &mut App, handle: &ExtWorkspaceHandleV1, event: ext_workspace_handle_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(wm) = app.wm.as_mut() else { return };
        let Some(i) = wm.spaces.iter().position(|s| &s.handle == handle) else { return };
        let space = &mut wm.spaces[i];
        match event {
            ext_workspace_handle_v1::Event::Name { name } => space.name = named(name),
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => space.coordinates = words(&coordinates),
            ext_workspace_handle_v1::Event::State { state } => {
                space.state = match state {
                    WEnum::Value(s) => s.bits(),
                    WEnum::Unknown(bits) => bits,
                }
            }
            ext_workspace_handle_v1::Event::Removed => {
                handle.destroy();
                wm.spaces.remove(i);
                for group in &mut wm.groups {
                    group.members.retain(|w| w != handle);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch2<ZwlrForeignToplevelManagerV1, App> for WmData {
    fn event(&self, app: &mut App, _: &ZwlrForeignToplevelManagerV1, event: zwlr_foreign_toplevel_manager_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                let id = app.wm_id();
                let Some(wm) = app.wm.as_mut() else { return };
                wm.windows.push(Toplevel { handle: toplevel, id, title: None, app_id: None, outputs: Vec::new(), state: Vec::new() });
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                if let Some(wm) = app.wm.as_mut() {
                    wm.toplevels = None;
                    wm.windows.clear();
                }
                app.publish_wm();
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(App, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, WmData)
    ]);
}

impl Dispatch2<ZwlrForeignToplevelHandleV1, App> for WmData {
    fn event(&self, app: &mut App, handle: &ZwlrForeignToplevelHandleV1, event: zwlr_foreign_toplevel_handle_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(wm) = app.wm.as_mut() else { return };
        let Some(i) = wm.windows.iter().position(|t| &t.handle == handle) else { return };
        let window = &mut wm.windows[i];
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => window.title = named(title),
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => window.app_id = named(app_id),
            zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output } => window.outputs.push(output),
            zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output } => window.outputs.retain(|o| o != &output),
            zwlr_foreign_toplevel_handle_v1::Event::State { state } => window.state = words(&state),
            zwlr_foreign_toplevel_handle_v1::Event::Done => app.publish_wm(),
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                handle.destroy();
                wm.windows.remove(i);
                app.publish_wm();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn protocol_arrays_are_native_words() {
        let bytes: Vec<u8> = [1u32, 2, 0x0102_0304].iter().flat_map(|w| w.to_ne_bytes()).collect();
        assert_eq!(super::words(&bytes), [1, 2, 0x0102_0304]);
        assert!(super::words(&[1, 2, 3]).is_empty(), "a part of a word is no word");
    }
}
