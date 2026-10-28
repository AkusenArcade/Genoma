//! The Genoma runtime (PRD §4):
//!
//!     genoma <interface directory>
//!
//! Loads `shell.luau` from the directory, opens its surface on the
//! compositor, and keeps it true: a signal that changes re-runs the bindings
//! that read it, a binding that changes a property damages the surface, and a
//! damaged surface asks the compositor for one frame. Saving a file in the
//! directory reloads the interface in place.

mod wayland;

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use genoma_render::{Renderer, cosmic_text::FontSystem};
use genoma_scene::Scene;
use genoma_script::Script;
use genoma_services::clock::Clock;
use notify::Watcher;
use smithay_client_toolkit::reexports::{
    calloop::{
        EventLoop,
        channel::{Event as ChannelEvent, channel},
        timer::{TimeoutAction, Timer},
    },
    calloop_wayland_source::WaylandSource,
};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("GENOMA_LOG", "info")).init();
    let dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("examples/clock"));
    let dir = dir.canonicalize().unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));

    let (mut app, queue) = wayland::App::connect();
    let pci = app.main_device_pci();
    log::info!("compositor's main device: {}", pci.as_deref().unwrap_or("unknown"));

    let fonts = Arc::new(Mutex::new(FontSystem::new()));
    let renderer = Renderer::spawn(pci.clone(), fonts.clone()).unwrap_or_else(|e| fail(&e.to_string()));
    if pci.is_some_and(|id| !renderer.adapter_info().device_pci_bus_id.eq_ignore_ascii_case(&id)) {
        log::warn!("drawing on a different GPU from the compositor's");
    }

    let scene = Rc::new(RefCell::new(Scene::new(fonts)));
    let clock = Rc::new(Clock::new());
    let mut script = Script::new(&dir, scene.clone(), clock.clone());
    let spec = script.load().unwrap_or_else(|e| fail(&e));
    log::info!("loaded {}", dir.join("shell.luau").display());
    app.open(renderer, scene, spec, &queue.handle());

    let mut event_loop: EventLoop<wayland::App> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();
    WaylandSource::new(app.connection(), queue).insert(handle.clone()).expect("wayland source");

    // The clock ticks only for an interface that reads it.
    if script.clock_wanted() {
        let clock = clock.clone();
        handle
            .insert_source(Timer::from_duration(Clock::until_next_second()), move |_, _, _| {
                clock.tick();
                TimeoutAction::ToDuration(Clock::until_next_second())
            })
            .expect("clock timer");
    }

    // Hot reload: a save in the directory, settled for a moment because an
    // editor's save is often several events.
    let (tx, rx) = channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|e| e.kind.is_modify() || e.kind.is_create()) {
            let _ = tx.send(());
        }
    })
    .expect("watcher");
    watcher.watch(&dir, notify::RecursiveMode::NonRecursive).expect("watch");
    let script = Rc::new(RefCell::new(script));
    let pending = Rc::new(Cell::new(false));
    let timers = handle.clone();
    handle
        .insert_source(rx, move |event, _, _| {
            if !matches!(event, ChannelEvent::Msg(())) || pending.replace(true) {
                return;
            }
            let script = script.clone();
            let pending = pending.clone();
            timers
                .insert_source(Timer::from_duration(Duration::from_millis(40)), move |_, _, app| {
                    pending.set(false);
                    match script.borrow_mut().load() {
                        Ok(spec) => {
                            log::info!("reloaded");
                            app.respec(spec);
                        }
                        Err(e) => log::error!("reload failed, the last interface stays: {e}"),
                    }
                    TimeoutAction::Drop
                })
                .expect("reload timer");
        })
        .expect("reload channel");

    // No timeout: the loop sleeps until the compositor, a timer that an
    // interface asked for, or a saved file says something.
    event_loop.run(None, &mut app, |app| app.after_events()).expect("event loop");
    drop(watcher);
}

fn fail(message: &str) -> ! {
    log::error!("{message}");
    std::process::exit(1);
}
