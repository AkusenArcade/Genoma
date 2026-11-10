//! `@genoma/gamma`: `wlr-gamma-control` on the main thread's connection,
//! bound once an interface requires it. Each turn, every display whose
//! setting is not neutral holds a gamma control with its ramp; a display
//! back to neutral lets go of it, and the compositor restores its own.

use std::{collections::BTreeMap, os::unix::fs::FileExt, rc::Rc};

use genoma_services::gamma::{Gamma, Held, Setting, ramp};
use smithay_client_toolkit::{
    dispatch2::Dispatch2,
    reexports::{
        client::{Connection, QueueHandle, protocol::wl_output::WlOutput},
        protocols_wlr::gamma_control::v1::client::{
            zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
            zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
        },
    },
};

use super::{App, Ignore};

pub(super) struct Gm {
    service: Rc<Gamma>,
    manager: Option<ZwlrGammaControlManagerV1>,
    controls: Vec<Control>,
}

struct Control {
    name: String,
    output: WlOutput,
    handle: ZwlrGammaControlV1,
    /// Entries per channel, once the compositor says.
    size: Option<u32>,
    applied: Option<Setting>,
    failed: bool,
}

/// A display's gamma control: found by equality.
pub(super) struct GammaData;

impl App {
    /// Binds `wlr-gamma-control` for `service`, once.
    pub fn start_gamma(&mut self, service: Rc<Gamma>) {
        if self.gm.is_some() {
            return;
        }
        let manager = self.globals.bind::<ZwlrGammaControlManagerV1, _, _>(&self.qh, 1..=1, Ignore).ok();
        service.available.set(manager.is_some());
        self.gm = Some(Gm { service, manager, controls: Vec::new() });
    }

    /// Every display's ramp as its setting asks.
    pub fn sync_gamma(&mut self) {
        let Some(gm) = self.gm.as_mut() else { return };
        let Some(manager) = &gm.manager else { return };
        let outputs: Vec<(String, WlOutput)> =
            self.outputs.outputs().filter_map(|o| Some((self.outputs.info(&o)?.name?, o))).collect();
        let mut changed = false;
        // Displays gone, or back to neutral: let go.
        gm.controls.retain(|c| {
            let keep = outputs.iter().any(|(n, o)| n == &c.name && o == &c.output) && !gm.service.wanted(&c.name).is_neutral();
            if !keep {
                if !c.failed {
                    c.handle.destroy();
                }
                changed = true;
            }
            keep
        });
        for (name, output) in outputs {
            let wanted = gm.service.wanted(&name);
            if wanted.is_neutral() {
                continue;
            }
            let control = match gm.controls.iter_mut().position(|c| c.name == name) {
                Some(i) => &mut gm.controls[i],
                None => {
                    let handle = manager.get_gamma_control(&output, &self.qh, GammaData);
                    gm.controls.push(Control { name, output, handle, size: None, applied: None, failed: false });
                    continue;
                }
            };
            let Some(size) = control.size.filter(|_| !control.failed && control.applied != Some(wanted)) else { continue };
            match write_ramp(wanted, size as usize) {
                Ok(fd) => {
                    control.handle.set_gamma(std::os::fd::AsFd::as_fd(&fd));
                    control.applied = Some(wanted);
                    changed = true;
                }
                Err(e) => log::warn!("gamma: {}: {e}", control.name),
            }
        }
        if changed {
            let held: BTreeMap<String, Held> = gm
                .controls
                .iter()
                .filter_map(|c| match (c.failed, c.applied) {
                    (true, _) => Some((c.name.clone(), Held::Failed)),
                    (false, Some(s)) => Some((c.name.clone(), Held::Set(s))),
                    _ => None,
                })
                .collect();
            if gm.service.held.get_untracked().as_ref() != Some(&held) {
                gm.service.held.set(held);
            }
            let _ = self.conn.flush();
        }
    }
}

/// The ramp in a memory file, written at offset 0 and left there, where
/// the compositor reads it from.
fn write_ramp(setting: Setting, size: usize) -> std::io::Result<std::os::fd::OwnedFd> {
    let fd = rustix::fs::memfd_create("genoma-gamma", rustix::fs::MemfdFlags::CLOEXEC)?;
    let bytes: Vec<u8> = ramp(setting, size).iter().flat_map(|v| v.to_ne_bytes()).collect();
    let file = std::fs::File::from(fd);
    file.write_all_at(&bytes, 0)?;
    Ok(file.into())
}

impl Dispatch2<ZwlrGammaControlV1, App> for GammaData {
    fn event(&self, app: &mut App, handle: &ZwlrGammaControlV1, event: zwlr_gamma_control_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(gm) = app.gm.as_mut() else { return };
        let Some(control) = gm.controls.iter_mut().find(|c| &c.handle == handle) else { return };
        match event {
            zwlr_gamma_control_v1::Event::GammaSize { size } => control.size = Some(size),
            // Another program holds it, or the display has no ramp: kept as
            // failed until its setting goes back to neutral.
            zwlr_gamma_control_v1::Event::Failed => {
                log::info!("gamma: {} is not ours to change", control.name);
                handle.destroy();
                control.failed = true;
                let mut held = gm.service.held.get_untracked().unwrap_or_default();
                held.insert(control.name.clone(), Held::Failed);
                gm.service.held.set(held);
            }
            _ => {}
        }
    }
}
