//! `@genoma/displays`: `wlr-output-management` on the main thread's
//! connection, bound once an interface requires it. The heads and their
//! modes are kept as handles here and as plain values in the service,
//! which gets the whole list after each `done`; a configuration the
//! interface asks for is planned against the heads of that moment, every
//! head named, and the compositor's answer goes back to the service.

use std::rc::Rc;

use genoma_services::displays::{Displays, Head, Mode, ModeChoice, Outcome, Planned, Request, plan, transform_name};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    reexports::{
        client::{Connection, Proxy, QueueHandle, WEnum, protocol::wl_output::Transform},
        protocols_wlr::output_management::v1::client::{
            zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
            zwlr_output_head_v1::{self, AdaptiveSyncState, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
        },
    },
};

use super::{App, Ignore};

pub(super) struct Dm {
    service: Rc<Displays>,
    manager: Option<ZwlrOutputManagerV1>,
    /// The latest `done`'s: a configuration made against an older one is
    /// cancelled.
    serial: u32,
    heads: Vec<HeadState>,
}

struct HeadState {
    handle: ZwlrOutputHeadV1,
    head: Head,
    modes: Vec<(ZwlrOutputModeV1, Mode)>,
    current: Option<ZwlrOutputModeV1>,
}

impl HeadState {
    /// The plain values, the mode in use resolved.
    fn value(&self) -> Head {
        let mut head = self.head.clone();
        head.modes = self.modes.iter().map(|(_, m)| m.clone()).collect();
        head.mode = if head.enabled { self.current.as_ref().and_then(|c| self.modes.iter().find(|(h, _)| h == c)).map(|(_, m)| m.clone()) } else { None };
        head
    }
}

/// The manager, its heads and their modes: found by equality.
pub(super) struct DmData;

/// A configuration in flight: what its answer is reported under.
pub(super) struct ConfigData {
    id: u64,
    test: bool,
    /// The first answer is the answer: niri 26.04 follows `succeeded` with
    /// `cancelled` for the same configuration, both sent before the
    /// destroy reaches it.
    answered: std::sync::atomic::AtomicBool,
}

impl App {
    /// Binds `wlr-output-management` for `service`, once.
    pub fn start_displays(&mut self, service: Rc<Displays>) {
        if self.dm.is_some() {
            return;
        }
        let manager = self.globals.bind::<ZwlrOutputManagerV1, _, _>(&self.qh, 1..=4, DmData).ok();
        service.available.set(manager.is_some());
        self.dm = Some(Dm { service, manager, serial: 0, heads: Vec::new() });
    }

    /// The configurations asked for since the last turn, sent.
    pub fn sync_displays(&mut self) {
        let Some(dm) = &self.dm else { return };
        let requests = dm.service.take_requests();
        if requests.is_empty() {
            return;
        }
        for Request::Configure { id, changes, test } in requests {
            let failed = |status| dm.service.result.set(Some(Outcome { id, test, status }));
            let Some(manager) = &dm.manager else {
                failed("failed");
                continue;
            };
            let values: Vec<Head> = dm.heads.iter().map(HeadState::value).collect();
            // Planned again: the heads may have changed since the interface
            // asked.
            let planned = match plan(&values, &changes) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("displays: {e}");
                    failed("failed");
                    continue;
                }
            };
            let config = manager.create_configuration(dm.serial, &self.qh, ConfigData { id, test, answered: std::sync::atomic::AtomicBool::new(false) });
            for p in planned {
                match p {
                    Planned::Disabled { head } => config.disable_head(&dm.heads[head].handle),
                    Planned::Enabled { head, mode, position, scale, transform, adaptive_sync } => {
                        let state = &dm.heads[head];
                        let c = config.enable_head(&state.handle, &self.qh, Ignore);
                        match mode {
                            Some(ModeChoice::Listed(i)) => c.set_mode(&state.modes[i].0),
                            Some(ModeChoice::Custom(w, h, refresh)) => c.set_custom_mode(w, h, refresh),
                            None => {}
                        }
                        c.set_position(position.0, position.1);
                        c.set_scale(scale);
                        if let Ok(t) = Transform::try_from(transform) {
                            c.set_transform(t);
                        }
                        if let Some(on) = adaptive_sync.filter(|_| c.version() >= 4) {
                            c.set_adaptive_sync(if on { AdaptiveSyncState::Enabled } else { AdaptiveSyncState::Disabled });
                        }
                    }
                }
            }
            if test {
                config.test();
            } else {
                config.apply();
            }
        }
        let _ = self.conn.flush();
    }

    fn publish_displays(&self) {
        if let Some(dm) = &self.dm {
            dm.service.set_heads(dm.heads.iter().map(HeadState::value).collect());
        }
    }
}

impl Dispatch2<ZwlrOutputManagerV1, App> for DmData {
    fn event(&self, app: &mut App, _: &ZwlrOutputManagerV1, event: zwlr_output_manager_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(dm) = app.dm.as_mut() else { return };
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                dm.heads.push(HeadState { handle: head, head: Head { scale: 1.0, transform: "normal", ..Head::default() }, modes: Vec::new(), current: None });
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                dm.serial = serial;
                app.publish_displays();
            }
            zwlr_output_manager_v1::Event::Finished => {
                dm.manager = None;
                dm.heads.clear();
                dm.service.available.set(false);
                app.publish_displays();
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(App, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, DmData)
    ]);
}

impl Dispatch2<ZwlrOutputHeadV1, App> for DmData {
    fn event(&self, app: &mut App, handle: &ZwlrOutputHeadV1, event: zwlr_output_head_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(dm) = app.dm.as_mut() else { return };
        let Some(i) = dm.heads.iter().position(|h| &h.handle == handle) else { return };
        let state = &mut dm.heads[i];
        let head = &mut state.head;
        // Applied at the manager's `done`.
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Description { description } => head.description = description,
            zwlr_output_head_v1::Event::Make { make } => head.make = make,
            zwlr_output_head_v1::Event::Model { model } => head.model = model,
            zwlr_output_head_v1::Event::SerialNumber { serial_number } => head.serial = serial_number,
            zwlr_output_head_v1::Event::PhysicalSize { width, height } => (head.width_mm, head.height_mm) = (width, height),
            zwlr_output_head_v1::Event::Mode { mode } => state.modes.push((mode, Mode::default())),
            zwlr_output_head_v1::Event::Enabled { enabled } => {
                head.enabled = enabled != 0;
                if !head.enabled {
                    state.current = None;
                }
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => state.current = Some(mode),
            zwlr_output_head_v1::Event::Position { x, y } => (head.x, head.y) = (x, y),
            zwlr_output_head_v1::Event::Transform { transform } => {
                head.transform = transform_name(match transform {
                    WEnum::Value(t) => t as u32,
                    WEnum::Unknown(v) => v,
                })
            }
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            zwlr_output_head_v1::Event::AdaptiveSync { state: s } => head.adaptive_sync = Some(matches!(s, WEnum::Value(AdaptiveSyncState::Enabled))),
            zwlr_output_head_v1::Event::Finished => {
                if handle.version() >= 3 {
                    handle.release();
                }
                dm.heads.remove(i);
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(App, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, DmData)
    ]);
}

impl Dispatch2<ZwlrOutputModeV1, App> for DmData {
    fn event(&self, app: &mut App, handle: &ZwlrOutputModeV1, event: zwlr_output_mode_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(dm) = app.dm.as_mut() else { return };
        let Some(state) = dm.heads.iter_mut().find(|h| h.modes.iter().any(|(m, _)| m == handle)) else { return };
        let Some(i) = state.modes.iter().position(|(m, _)| m == handle) else { return };
        let mode = &mut state.modes[i].1;
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => (mode.width, mode.height) = (width, height),
            zwlr_output_mode_v1::Event::Refresh { refresh } => mode.refresh = refresh as f64 / 1000.0,
            zwlr_output_mode_v1::Event::Preferred => mode.preferred = true,
            zwlr_output_mode_v1::Event::Finished => {
                if handle.version() >= 3 {
                    handle.release();
                }
                if state.current.as_ref() == Some(handle) {
                    state.current = None;
                }
                state.modes.remove(i);
            }
            _ => {}
        }
    }
}

impl Dispatch2<ZwlrOutputConfigurationV1, App> for ConfigData {
    fn event(&self, app: &mut App, config: &ZwlrOutputConfigurationV1, event: zwlr_output_configuration_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let status = match event {
            zwlr_output_configuration_v1::Event::Succeeded if self.test => "works",
            zwlr_output_configuration_v1::Event::Succeeded => "applied",
            zwlr_output_configuration_v1::Event::Failed => "failed",
            zwlr_output_configuration_v1::Event::Cancelled => "cancelled",
            _ => return,
        };
        if self.answered.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        config.destroy();
        log::info!("displays: configuration {} {status}", self.id);
        if let Some(dm) = &app.dm {
            dm.service.result.set(Some(Outcome { id: self.id, test: self.test, status }));
        }
    }
}
