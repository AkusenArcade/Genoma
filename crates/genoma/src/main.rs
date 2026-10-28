//! The Genoma runtime (PRD §4):
//!
//!     genoma <interface directory>     run the interface
//!     genoma probe                     print every service as it changes, no UI
//!     genoma types                     print the Luau definitions of the nodes
//!
//! `run` loads `shell.luau` from the directory, opens its surface on the
//! compositor, and keeps it true: a signal that changes re-runs the bindings
//! that read it, a binding that changes a property damages the surface, and a
//! damaged surface asks the compositor for one frame. Saving a file in the
//! directory reloads the interface in place.

mod access;
mod wayland;

use std::{
    cell::{Cell, RefCell},
    io::Read,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use genoma_core::reactive::Effect;
use genoma_render::{Renderer, cosmic_text::FontSystem};
use genoma_script::{Script, Services};
use genoma_services::{bus::Bus, clock::Clock, niri::Niri, vitals::{self, Vitals}};
use notify::Watcher;
use smithay_client_toolkit::reexports::{
    calloop::{
        EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
        channel::{Event as ChannelEvent, channel},
        generic::Generic,
        timer::{TimeoutAction, Timer},
    },
    calloop_wayland_source::WaylandSource,
};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("GENOMA_LOG", "info")).init();
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("types") => print!("{}", genoma_scene::luau_definitions()),
        Some("probe") => probe(),
        Some("pam") => pam_check(std::env::args().nth(2), std::env::args().nth(3)),
        other => run(PathBuf::from(other.unwrap_or("examples/clock"))),
    }
}

fn fail(message: &str) -> ! {
    log::error!("{message}");
    std::process::exit(1);
}

/// The services, before anything depends on them.
fn services() -> (Services, Option<UnixStream>) {
    let (niri, stream) = match Niri::connect() {
        Ok((niri, stream)) => (Some(Rc::new(niri)), Some(stream)),
        Err(e) => {
            log::warn!("niri: {e}");
            (None, None)
        }
    };
    (Services { clock: Rc::new(Clock::new()), niri, vitals: Rc::new(Vitals::new()), power: Rc::default(), network: Rc::default(), media: Rc::default(), audio: Rc::default(), bluetooth: Rc::default(), toplevels: Rc::default(), tray: Rc::default() }, stream)
}

/// Starts the source of each service an interface asked for and that is not
/// running yet: a reload that requires a new service gets it.
struct Sources {
    services: Services,
    niri_stream: Option<UnixStream>,
    /// The D-Bus thread, started with the first service that needs it.
    bus: Option<Bus>,
    started: Vec<&'static str>,
}

impl Sources {
    fn start<D: 'static>(&mut self, wants: impl Fn(&str) -> bool, handle: &LoopHandle<'static, D>) {
        for service in ["clock", "vitals", "niri", "power", "network", "media", "audio", "bluetooth", "tray"] {
            if !wants(service) || self.started.contains(&service) {
                continue;
            }
            self.started.push(service);
            match service {
                "clock" => {
                    let clock = self.services.clock.clone();
                    handle
                        .insert_source(Timer::from_duration(Clock::until_next_second()), move |_, _, _| {
                            clock.tick();
                            TimeoutAction::ToDuration(Clock::until_next_second())
                        })
                        .expect("clock timer");
                }
                "vitals" => {
                    let vitals = self.services.vitals.clone();
                    handle
                        .insert_source(Timer::from_duration(vitals::INTERVAL), move |_, _, _| {
                            vitals.sample();
                            TimeoutAction::ToDuration(vitals::INTERVAL)
                        })
                        .expect("vitals timer");
                }
                "power" => {
                    let Some(bus) = self.bus() else { continue };
                    let power = self.services.power.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                power.apply(update);
                            }
                        })
                        .expect("power channel");
                    self.services.power.start(&bus, post);
                }
                "network" => {
                    let Some(bus) = self.bus() else { continue };
                    let network = self.services.network.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                network.apply(update);
                            }
                        })
                        .expect("network channel");
                    self.services.network.start(&bus, post);
                }
                "media" => {
                    let Some(bus) = self.bus() else { continue };
                    let media = self.services.media.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                media.apply(update);
                            }
                        })
                        .expect("media channel");
                    self.services.media.start(&bus, post);
                }
                "audio" => {
                    // Its own thread, PipeWire's loop: no bus needed.
                    let audio = self.services.audio.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                audio.apply(update);
                            }
                        })
                        .expect("audio channel");
                    self.services.audio.start(post);
                }
                "bluetooth" => {
                    let Some(bus) = self.bus() else { continue };
                    let bluetooth = self.services.bluetooth.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                bluetooth.apply(update);
                            }
                        })
                        .expect("bluetooth channel");
                    self.services.bluetooth.start(&bus, post);
                }
                "tray" => {
                    let Some(bus) = self.bus() else { continue };
                    let tray = self.services.tray.clone();
                    let (post, updates) = channel();
                    handle
                        .insert_source(updates, move |event, _, _| {
                            if let ChannelEvent::Msg(update) = event {
                                tray.apply(update);
                            }
                        })
                        .expect("tray channel");
                    self.services.tray.start(&bus, post);
                }
                _ => {
                    let (Some(niri), Some(stream)) = (self.services.niri.clone(), self.niri_stream.take()) else {
                        continue;
                    };
                    stream.set_nonblocking(true).expect("nonblocking");
                    let mut pending = Vec::new();
                    handle
                        .insert_source(Generic::new(stream, Interest::READ, Mode::Level), move |_, stream, _| {
                            let mut chunk = [0u8; 16384];
                            loop {
                                // Safety: only reading from the stream calloop holds.
                                match unsafe { stream.get_mut() }.read(&mut chunk) {
                                    Ok(0) => {
                                        log::warn!("niri closed the event stream");
                                        return Ok(PostAction::Remove);
                                    }
                                    Ok(n) => pending.extend_from_slice(&chunk[..n]),
                                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                    Err(e) => return Err(e),
                                }
                            }
                            while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                                let line: Vec<u8> = pending.drain(..=end).collect();
                                niri.line(&String::from_utf8_lossy(&line));
                            }
                            Ok(PostAction::Continue)
                        })
                        .expect("niri source");
                }
            }
        }
    }
}

impl Sources {
    fn bus(&mut self) -> Option<Bus> {
        if self.bus.is_none() {
            match Bus::shared() {
                Ok(bus) => self.bus = Some(bus),
                Err(e) => log::warn!("no bus thread: {e}"),
            }
        }
        self.bus.clone()
    }
}

/// The loop, as the script's building blocks see it (PRD §9.2).
struct LoopHost(LoopHandle<'static, wayland::App>);

impl genoma_script::io::Host for LoopHost {
    fn timer(&self, after: Duration, mut tick: Box<dyn FnMut() -> Option<Duration>>) -> Option<RegistrationToken> {
        self.0
            .insert_source(Timer::from_duration(after), move |_, _, _| match tick() {
                Some(next) => TimeoutAction::ToDuration(next),
                None => TimeoutAction::Drop,
            })
            .map_err(|e| log::error!("timer: {e}"))
            .ok()
    }

    fn readable(&self, fd: OwnedFd, mut ready: Box<dyn FnMut(BorrowedFd<'_>) -> bool>) -> Option<RegistrationToken> {
        self.0
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, fd, _| {
                Ok(if ready(fd.as_fd()) { PostAction::Continue } else { PostAction::Remove })
            })
            .map_err(|e| log::error!("readable: {e}"))
            .ok()
    }

    fn remove(&self, token: RegistrationToken) {
        self.0.remove(token);
    }

    fn channel(
        &self,
        mut deliver: Box<dyn FnMut(genoma_script::dbus::Delivery)>,
    ) -> Option<smithay_client_toolkit::reexports::calloop::channel::Sender<genoma_script::dbus::Delivery>> {
        let (sender, receiver) = channel();
        self.0
            .insert_source(receiver, move |event, _, _| {
                if let ChannelEvent::Msg(delivery) = event {
                    deliver(delivery);
                }
            })
            .map_err(|e| log::error!("channel: {e}"))
            .ok()?;
        Some(sender)
    }
}

/// `genoma pam [service] [directory]`: one authentication of the current
/// user, the answers read from the terminal without echo, and only the
/// outcome printed. Bioma's lock screen is `bioma-lock` in its `assets/pam`.
fn pam_check(service: Option<String>, directory: Option<String>) {
    use genoma_services::pam::{Event, authenticate};
    let service = service.unwrap_or_else(|| "bioma-lock".into());
    let user = std::env::var("USER").unwrap_or_default();
    let (answers, receiver) = std::sync::mpsc::channel();
    let started = std::time::Instant::now();
    let mut events = |event: Event| match event {
        Event::Prompt { message, echo } => {
            eprint!("{message}");
            let stdin = rustix::stdio::stdin();
            // Without echo for a secret, when there is a terminal to ask.
            let saved = rustix::termios::tcgetattr(stdin).ok();
            if let (Some(t), false) = (&saved, echo) {
                let mut quiet = t.clone();
                quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
                let _ = rustix::termios::tcsetattr(stdin, rustix::termios::OptionalActions::Now, &quiet);
            }
            let mut line = String::new();
            let read = std::io::stdin().read_line(&mut line);
            if let (Some(t), false) = (&saved, echo) {
                let _ = rustix::termios::tcsetattr(stdin, rustix::termios::OptionalActions::Now, t);
                eprintln!();
            }
            let _ = answers.send(read.ok().map(|_| line.trim_end_matches(['\n', '\r']).to_owned()));
        }
        Event::Message { message, error } => eprintln!("{}{message}", if error { "error: " } else { "" }),
        Event::Done(outcome) => println!("pam {service} for {user}: {} in {:.1} s", outcome.name(), started.elapsed().as_secs_f64()),
    };
    authenticate(&service, directory.as_deref(), &user, &mut events, &receiver);
}

/// Every service, without a surface: what each says, as it changes.
fn probe() {
    let (services, stream) = services();
    let mut event_loop: EventLoop<()> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();
    let s = services.clone();
    Effect::new(move || println!("clock   {:02}:{:02}:{:02}", s.clock.hour.get(), s.clock.minute.get(), s.clock.second.get()));
    let s = services.clone();
    Effect::new(move || {
        println!("vitals  cpu {:.2}  ram {:.2}  gpu {:?}", s.vitals.cpu.get(), s.vitals.ram.get(), s.vitals.gpu.get())
    });
    if let Some(niri) = services.niri.clone() {
        let n = niri.clone();
        Effect::new(move || println!("niri    title {:?}", n.title.get()));
        Effect::new(move || {
            for w in niri.workspaces.get().iter().filter(|w| w.is_active) {
                println!("niri    {:?} shows workspace {} {:?}", w.output, w.idx, w.name);
            }
        });
    }
    let p = services.power.clone();
    Effect::new(move || {
        let b = p.battery.get();
        println!(
            "power   battery present {} laptop {} level {:.2} {} empty {} s full {} s, on battery {}",
            b.present, b.laptop, b.level, b.state, b.time_to_empty, b.time_to_full, p.on_battery.get()
        )
    });
    let p = services.power.clone();
    Effect::new(move || println!("power   profile {:?} of {:?}", p.profile.get(), p.profiles.get()));
    let n = services.network.clone();
    Effect::new(move || {
        println!(
            "network connectivity {} (check {}), wifi {} (hardware {}){}",
            n.connectivity.get(),
            n.can_check.get(),
            n.wifi_enabled.get(),
            n.wifi_hardware.get(),
            match n.failure.get() {
                f if f.is_empty() => String::new(),
                f => format!(", failed: {f}"),
            }
        )
    });
    let n = services.network.clone();
    Effect::new(move || {
        for d in n.devices.get() {
            println!(
                "network device {} {} {} connected {} carrier {} {} Mb/s {:?}",
                d.interface, d.kind, d.state, d.connected, d.carrier, d.speed, d.connection
            );
        }
    });
    let n = services.network.clone();
    Effect::new(move || {
        let networks = n.networks.get();
        println!("network {} networks seen", networks.len());
        for w in networks.iter().take(5) {
            println!(
                "network   {:?} {:.2} secured {} connected {} {} MHz known {}",
                w.ssid, w.strength, w.secured, w.connected, w.frequency, w.known
            );
        }
    });
    let m = services.media.clone();
    Effect::new(move || {
        let players = m.players.get();
        println!("media   {} players", players.len());
        for p in players {
            println!(
                "media   {} ({}) {} {:?} by {:?} on {:?}, {:.1} of {:.1} s at rate {}, art {}, can play {} pause {} next {} previous {} seek {}",
                p.identity, p.name, p.status, p.title, p.artist, p.album, p.position_now(), p.length, p.rate,
                !p.art_url.is_empty(), p.can_play, p.can_pause, p.can_go_next, p.can_go_previous, p.can_seek
            );
        }
    });
    let a = services.audio.clone();
    Effect::new(move || println!("audio   default sink {:?}, source {:?}", a.default_sink.get(), a.default_source.get()));
    let a = services.audio.clone();
    Effect::new(move || {
        for n in a.nodes.get() {
            if matches!(n.kind, "sink" | "source" | "output-stream" | "input-stream") {
                println!(
                    "audio   {:>4} {:<13} {:<40} vol {:.2}{} {}",
                    n.id,
                    n.kind,
                    if n.description.is_empty() { &n.name } else { &n.description },
                    n.volume,
                    if n.muted { " muted" } else { "" },
                    n.properties.iter().find(|(k, _)| k == "application.name").map(|(_, v)| v.as_str()).unwrap_or("")
                );
            }
        }
    });
    let a = services.audio.clone();
    Effect::new(move || println!("audio   {} links, {} active", a.links.get().len(), a.links.get().iter().filter(|l| l.active).count()));
    let b = services.bluetooth.clone();
    Effect::new(move || match b.adapter.get() {
        Some(a) => println!(
            "bluetooth adapter {} {} powered {} discovering {} discoverable {} pairable {}",
            a.name, a.address, a.powered, a.discovering, a.discoverable, a.pairable
        ),
        None => println!("bluetooth no adapter"),
    });
    let b = services.bluetooth.clone();
    Effect::new(move || {
        let devices = b.devices.get();
        println!("bluetooth {} devices", devices.len());
        for d in devices.iter().take(6) {
            println!(
                "bluetooth   {} {:?} {:?} paired {} connected {} battery {:?} {}",
                d.address, d.name, d.icon, d.paired, d.connected, d.battery, d.busy
            );
        }
    });
    let t = services.tray.clone();
    Effect::new(move || {
        let items = t.items.get();
        println!("tray    {} items, serving the watcher {}", items.len(), t.serving_watcher.get());
        for i in items {
            println!(
                "tray      {} id {:?} title {:?} {} {} icon {:?}{} tooltip {:?} menu {}{}",
                i.key, i.id, i.title, i.status, i.category, i.icon_name,
                if i.has_pixmap { " (pixmap)" } else { "" },
                i.tooltip_title, !i.menu.is_empty(), if i.only_menu { " only" } else { "" }
            );
        }
    });
    let mut sources = Sources { services, niri_stream: stream, bus: None, started: Vec::new() };
    sources.start(|_| true, &handle);
    event_loop.run(None, &mut (), |_| {}).expect("event loop");
}

/// Evaluates the interface, fits the surfaces to what it asks for, and mounts
/// a tree on each. The last trees stay if any step fails.
fn load(script: &RefCell<Script>, app: &mut wayland::App) -> Result<(), String> {
    let spec = script.borrow_mut().evaluate()?;
    let targets = app.apply(spec);
    script.borrow_mut().mount(&targets)?;
    app.refit();
    Ok(())
}

fn run(dir: PathBuf) {
    let dir = dir.canonicalize().unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));
    let (mut app, queue) = wayland::App::connect();
    let pci = app.main_device_pci();
    log::info!("compositor's main device: {}", pci.as_deref().unwrap_or("unknown"));

    let fonts = Arc::new(Mutex::new(FontSystem::new()));
    let renderer = Renderer::spawn(pci.clone(), fonts.clone()).unwrap_or_else(|e| fail(&e.to_string()));
    if pci.is_some_and(|id| !renderer.adapter_info().device_pci_bus_id.eq_ignore_ascii_case(&id)) {
        log::warn!("drawing on a different GPU from the compositor's");
    }

    let mut event_loop: EventLoop<'static, wayland::App> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();
    app.start(renderer, fonts, handle.clone());

    let (services, niri_stream) = services();
    let host: genoma_script::io::HostRef = Rc::new(LoopHost(handle.clone()));
    let script = Rc::new(RefCell::new(Script::new(&dir, services.clone(), host)));
    load(&script, &mut app).unwrap_or_else(|e| fail(&e));
    log::info!("loaded {}", dir.join("shell.luau").display());
    WaylandSource::new(app.connection(), queue).insert(handle.clone()).expect("wayland source");

    let services_toplevels = services.toplevels.clone();
    let sources = Rc::new(RefCell::new(Sources { services, niri_stream, bus: None, started: Vec::new() }));
    sources.borrow_mut().start(|s| script.borrow().wants(s), &handle);
    // On the Wayland connection, so started here rather than among the
    // sources.
    if script.borrow().wants("toplevels") {
        app.start_toplevels(services_toplevels.clone());
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
    let dispatcher = script.clone();
    app.on_input(move |target, node, input| dispatcher.borrow().dispatch(target, node, input));
    let pending = Rc::new(Cell::new(false));
    let timers = handle.clone();
    let reloader = script.clone();
    let reload_sources = sources.clone();
    handle
        .insert_source(rx, move |event, _, _| {
            if !matches!(event, ChannelEvent::Msg(())) || pending.replace(true) {
                return;
            }
            let (script, pending, sources, handle) =
                (reloader.clone(), pending.clone(), reload_sources.clone(), timers.clone());
            timers
                .insert_source(Timer::from_duration(Duration::from_millis(40)), move |_, _, app| {
                    pending.set(false);
                    match load(&script, app) {
                        Ok(()) => {
                            log::info!("reloaded");
                            sources.borrow_mut().start(|s| script.borrow().wants(s), &handle);
                            if script.borrow().wants("toplevels") {
                                app.start_toplevels(sources.borrow().services.toplevels.clone());
                            }
                        }
                        Err(e) => log::error!("reload failed, the last interface stays: {e}"),
                    }
                    TimeoutAction::Drop
                })
                .expect("reload timer");
        })
        .expect("reload channel");

    // No timeout: the loop sleeps until the compositor, a service's source,
    // a scheduled beat or a saved file says something. An output that comes
    // or goes under an interface spanning them all mounts its trees again.
    event_loop
        .run(None, &mut app, |app| {
            if app.take_outputs_changed() {
                match load(&script, app) {
                    Ok(()) => log::info!("outputs changed: surfaces fitted"),
                    Err(e) => log::error!("outputs changed, reload failed: {e}"),
                }
            }
            app.after_events();
        })
        .expect("event loop");
    drop(watcher);
}
