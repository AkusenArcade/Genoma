//! The surfaces on the compositor: one layer surface per output the
//! interface asks for, each at its output's fractional scale, each drawn only
//! when its own scene says something changed. While the interface holds the
//! session lock, its `"lock"` surfaces are lock surfaces instead, one on
//! every output.

use std::{
    cell::RefCell,
    ffi::c_void,
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
    time::{Duration, Instant},
};

use genoma_render::{Fonts, Renderer, SurfaceId};
use genoma_scene::{Blur, NodeId, Scene};
use genoma_script::{Input, SurfaceSpec, Target, Wish};
use smithay_client_toolkit::{
    background_effect::{BackgroundEffectHandler, BackgroundEffectState},
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_registry,
    dispatch2::Dispatch2,
    dmabuf::{DmabufFeedback, DmabufHandler, DmabufState},
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            LoopHandle, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        client::{
            Connection, EventQueue, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_buffer, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
        },
        protocols::{
            ext::{
                background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
                foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
                idle_notify::v1::client::{
                    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
                    ext_idle_notifier_v1::ExtIdleNotifierV1,
                },
            },
            wp::{
                fractional_scale::v1::client::{
                    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
                },
                cursor_shape::v1::client::wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
                keyboard_shortcuts_inhibit::zv1::client::{
                    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
                    zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
                },
                linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1},
                viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
            },
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{AxisScroll, PointerEvent, PointerHandler, cursor_shape::CursorShapeManager},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

/// What the compositor knows the surface as.
enum Role {
    Layer(LayerSurface),
    /// Over everything, on one output, while the session is locked; sized
    /// by the compositor alone.
    Lock(SessionLockSurface),
}

impl Role {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Role::Layer(layer) => layer.wl_surface(),
            Role::Lock(lock) => lock.wl_surface(),
        }
    }

    fn layer(&self) -> Option<&LayerSurface> {
        match self {
            Role::Layer(layer) => Some(layer),
            Role::Lock(_) => None,
        }
    }
}

/// How long a lock surface's first frame may wait for its pictures. The
/// compositor shows the lock only once every output has a frame, so a
/// first frame held back keeps the desktop on screen rather than showing a
/// lock with its pictures missing; held too long, it is a lock that is late.
const LOCK_PICTURES: Duration = Duration::from_millis(750);

/// One surface and everything about it that is its own.
struct Surface {
    id: SurfaceId,
    /// Which of the interface's surfaces it is (its place in the list).
    spec: usize,
    /// The output it was made for, by name; `None` for the compositor's
    /// choice.
    output: Option<String>,
    scene: Rc<RefCell<Scene>>,
    role: Role,
    viewport: Option<WpViewport>,
    effect: Option<ExtBackgroundEffectSurfaceV1>,
    target_made: bool,
    /// The surface's size in logical units, as last configured.
    logical: (u32, u32),
    /// What the content asks for; differs from `logical` until a configure.
    wanted: (u32, u32),
    scale: f64,
    configured: bool,
    dirty: bool,
    frame_pending: bool,
    frames: u64,
    /// Frames and build time since the last minute's account (debug log).
    tally: (u64, Duration, Instant),
    /// The blur and input regions last sent, as rectangles, so each is set
    /// only when it changes; the inputs `None` until the first is sent,
    /// because Wayland's default is the whole surface.
    blurred: Vec<[i32; 4]>,
    inputs: Option<Vec<[i32; 4]>>,
    /// What is cut out of a catching surface's input: the other surfaces'
    /// inputs on its output, as last sent.
    holes: Vec<[i32; 4]>,
    /// The compositor's shortcuts standing aside for this surface.
    inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    /// Whether the surface last asked for the keyboard.
    keyboard: bool,
    hovered: Option<NodeId>,
    /// The node the press landed on, and where that node was then: a drag
    /// is told relative to it. A node carried by its own drag moves before
    /// the frame lays it out again, and several motions arrive in one frame;
    /// measured from where it is now, each motion counted the ones before it
    /// again, and a carried organism ran away from the pointer faster and
    /// faster, until its blur region overflowed niri's i32 (an abort).
    pressed: Option<NodeId>,
    pressed_at: (f32, f32),
    /// The touchpad's recent movement: when, and how far, for the fling.
    strokes: Vec<(Instant, f64, f64)>,
    /// The surface covers its output: a cell is open, or closing.
    tall: bool,
    /// The output's logical size.
    output_size: Option<(u32, u32)>,
    /// What the compositor gave a full surface, when that is less than its
    /// output: other surfaces' zones are kept clear of it.
    area: Option<(u32, u32)>,
    /// The size the surface's tree was built for.
    built: Option<(u32, u32)>,
    /// The timer that wakes the surface for the next beat, if one is set.
    wake: Option<RegistrationToken>,
    /// The surface as assistive technology sees it, checked at most every
    /// quarter second: motion never changes it, so it is not looked at per
    /// frame.
    access: crate::access::Access,
    access_checked: Option<Instant>,
    /// Since when a lock surface's first frame has waited for its pictures.
    held: Option<Instant>,
    /// Nothing to be (its description's `present` said so): one logical
    /// unit, keeping no strip, taking no pointer, blurring nothing, its tree
    /// still mounted. A membrane on an edge nobody has configured costs this
    /// much, and comes back the moment the configuration names it.
    asleep: bool,
    /// The strip its `reserve` function asks for, in place of the
    /// description's; `None` while it has none.
    reserve_wish: Option<Option<i32>>,
}

pub struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    dmabuf: DmabufState,
    effects: BackgroundEffectState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    viewporter: Option<WpViewporter>,
    fractional: Option<WpFractionalScaleManagerV1>,
    main_device: Option<u64>,
    feedback_done: bool,

    renderer: Option<Renderer>,
    fonts: Option<Fonts>,
    /// The interface's surfaces, as it last described them.
    specs: Vec<SurfaceSpec>,
    surfaces: Vec<Surface>,
    next_id: SurfaceId,
    handle: Option<LoopHandle<'static, App>>,
    pointer: Option<wl_pointer::WlPointer>,
    /// `wp-cursor-shape`: the pointer's shape by name, drawn by the
    /// compositor in the user's cursor theme. The device is the pointer's.
    cursor_shapes: Option<CursorShapeManager>,
    cursor_device: Option<WpCursorShapeDeviceV1>,
    /// `keyboard-shortcuts-inhibit`: the compositor's shortcuts stand aside
    /// while a node asks it (a field recording a key combination). The seat
    /// is the keyboard's.
    shortcuts_inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    /// The serial of the pointer's last entry, which a shape is set with,
    /// and the shape set since: none until one is.
    cursor_serial: u32,
    cursor_shape: Option<&'static str>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// A seat whose keyboard arrived before the loop did: the keyboard is
    /// taken with key repeat, which needs the loop.
    keyboard_seat: Option<wl_seat::WlSeat>,
    /// The modifiers held now, for the keys a field reads them with.
    modifiers: Modifiers,
    /// Where pointer and keyboard input goes: the script's dispatcher, by
    /// the index of the surface's tree.
    input: Option<Box<dyn FnMut(usize, NodeId, Input)>>,
    /// Surfaces the compositor gave another area than their trees were
    /// built for (another surface took or let go of a zone): each is built
    /// again on its own, the rest of the interface untouched.
    areas_changed: Vec<SurfaceId>,
    /// The last drag not yet told: a mouse moves a thousand times a second,
    /// and a script's drag handler may move half a surface each time. Told
    /// once per turn of the loop, only where the pointer is now, and before
    /// anything else the pointer does (a release must follow its last drag).
    drag: Option<(usize, NodeId, f32, f32)>,
    /// The outputs changed under an interface that spans them all: its
    /// trees are to be mounted again.
    outputs_changed: bool,
    started: Instant,
    /// The registry's globals, for what is bound only when asked for.
    globals: smithay_client_toolkit::reexports::client::globals::GlobalList,
    /// `ext-foreign-toplevel-list`, bound once an interface requires
    /// `@genoma/toplevels`, and the service it fills.
    toplevel_list: Option<ForeignToplevelList>,
    toplevels: Option<std::rc::Rc<genoma_services::toplevels::Toplevels>>,
    /// `ext-idle-notify`, bound once an interface requires `@genoma/idle`;
    /// the service it sets, and the notification asked for with its seconds.
    idle_notifier: Option<ExtIdleNotifierV1>,
    idle: Option<std::rc::Rc<genoma_services::idle::Idle>>,
    idle_notifications: std::collections::HashMap<String, (ExtIdleNotificationV1, u32)>,
    /// `ext-session-lock`, bound once an interface requires `@genoma/lock`;
    /// the service it sets, and the lock while one is asked for or held.
    lock_state: Option<SessionLockState>,
    lock: Option<std::rc::Rc<genoma_services::lock::Lock>>,
    session_lock: Option<SessionLock>,
}

impl App {
    pub fn connect() -> (App, EventQueue<App>) {
        let conn = Connection::connect_to_env().expect("no Wayland compositor");
        let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry");
        let qh = queue.handle();
        let mut app = App {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            dmabuf: DmabufState::new(&globals, &qh),
            effects: BackgroundEffectState::new(&globals, &qh),
            compositor: CompositorState::bind(&globals, &qh).expect("wl_compositor"),
            layer_shell: LayerShell::bind(&globals, &qh).expect("wlr-layer-shell"),
            viewporter: globals.bind(&qh, 1..=1, Ignore).ok(),
            fractional: globals.bind(&qh, 1..=1, Ignore).ok(),
            conn: conn.clone(),
            qh: qh.clone(),
            main_device: None,
            feedback_done: false,
            renderer: None,
            fonts: None,
            specs: Vec::new(),
            surfaces: Vec::new(),
            next_id: 1,
            handle: None,
            pointer: None,
            cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
            shortcuts_inhibit: globals.bind(&qh, 1..=1, Ignore).ok(),
            seat: None,
            cursor_device: None,
            cursor_serial: 0,
            cursor_shape: None,
            keyboard: None,
            keyboard_seat: None,
            modifiers: Modifiers::default(),
            input: None,
            drag: None,
            areas_changed: Vec::new(),
            outputs_changed: false,
            started: Instant::now(),
            toplevel_list: None,
            toplevels: None,
            idle_notifier: None,
            idle: None,
            idle_notifications: std::collections::HashMap::new(),
            lock_state: None,
            lock: None,
            session_lock: None,
            globals,
        };
        // The compositor's main device, before any GPU is opened: rendering
        // on another one copies every frame between the two.
        match app.dmabuf.get_default_feedback(&qh) {
            Ok(_) => {
                while !app.feedback_done {
                    queue.blocking_dispatch(&mut app).expect("dispatch");
                }
            }
            Err(_) => {
                log::warn!("linux-dmabuf below version 4: no main device to match");
                queue.roundtrip(&mut app).expect("roundtrip");
            }
        }
        (app, queue)
    }

    pub fn connection(&self) -> Connection {
        self.conn.clone()
    }

    pub fn main_device_pci(&self) -> Option<String> {
        self.main_device.and_then(pci_bus_id)
    }

    /// Binds `ext-foreign-toplevel-list` and keeps `toplevels` up to date:
    /// called once an interface requires them. A compositor without the
    /// protocol leaves the list empty and `available` false.
    pub fn start_toplevels(&mut self, toplevels: std::rc::Rc<genoma_services::toplevels::Toplevels>) {
        if self.toplevel_list.is_some() {
            return;
        }
        let list = ForeignToplevelList::new(&self.globals, &self.qh);
        let offered = self.globals.contents().with_list(|g| g.iter().any(|g| g.interface == "ext_foreign_toplevel_list_v1"));
        toplevels.available.set(offered);
        self.toplevel_list = Some(list);
        self.toplevels = Some(toplevels);
    }

    pub fn start_idle(&mut self, idle: std::rc::Rc<genoma_services::idle::Idle>) {
        if self.idle.is_some() {
            return;
        }
        self.idle_notifier = self.globals.bind(&self.qh, 1..=1, Ignore).ok();
        idle.available.set(self.idle_notifier.is_some());
        self.idle = Some(idle);
    }

    pub fn start_lock(&mut self, lock: std::rc::Rc<genoma_services::lock::Lock>) {
        if self.lock.is_some() {
            return;
        }
        let state = SessionLockState::new(&self.globals, &self.qh);
        let offered = self.globals.contents().with_list(|g| g.iter().any(|g| g.interface == "ext_session_lock_manager_v1"));
        lock.available.set(offered);
        self.lock_state = Some(state);
        self.lock = Some(lock);
    }

    /// The lock as the interface wants it: asked for, the compositor is
    /// asked, and the trees are mounted again from the next pass with the
    /// lock surfaces among them; let go, the session unlocks and they go.
    pub fn sync_lock(&mut self) {
        let Some(lock) = self.lock.clone() else { return };
        let wanted = lock.wanted.get_untracked().unwrap_or(false);
        match (&self.session_lock, wanted) {
            (None, true) if !lock.finished.get_untracked().unwrap_or(false) => {
                let Some(state) = &self.lock_state else { return };
                match state.lock(&self.qh) {
                    Ok(session_lock) => {
                        log::info!("session lock asked for");
                        self.session_lock = Some(session_lock);
                        self.outputs_changed = true;
                    }
                    Err(e) => {
                        log::warn!("no session lock: {e}");
                        lock.finished.set(true);
                    }
                }
            }
            (Some(session_lock), false) => {
                // Before the surfaces go, or the outputs would be locked
                // with nothing on them for a moment.
                if session_lock.is_locked() {
                    session_lock.unlock();
                    log::info!("session unlocked");
                }
                self.session_lock = None;
                lock.locked.set(false);
                self.drop_lock_surfaces();
            }
            _ => {}
        }
    }

    /// The lock surfaces go, and nothing is mounted again: what follows an
    /// unlock is the interface's to decide (a lock screen ends), and a load
    /// would start it over.
    fn drop_lock_surfaces(&mut self) {
        let gone: Vec<SurfaceId> =
            self.surfaces.iter().filter(|s| matches!(s.role, Role::Lock(_))).map(|s| s.id).collect();
        if let Some(renderer) = &self.renderer {
            for id in &gone {
                renderer.destroy_target(*id);
            }
        }
        self.surfaces.retain(|s| !gone.contains(&s.id));
        // A drag kept for a surface's index must not reach another.
        self.drag = None;
    }

    /// A notification for the seconds the interface asks, asked again when
    /// they change: the compositor counts from the moment it is made.
    fn sync_idle(&mut self) {
        let (Some(notifier), Some(idle)) = (&self.idle_notifier, &self.idle) else { return };
        let wanted = idle.timeouts.get_untracked().unwrap_or_default();
        // Gone, or asked for another length: let go, and not idle any more.
        let stale: Vec<String> = self
            .idle_notifications
            .iter()
            .filter(|(name, (_, s))| wanted.get(*name) != Some(s))
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if let Some((old, _)) = self.idle_notifications.remove(&name) {
                old.destroy();
            }
            idle.set_idle(&name, false);
        }
        let Some(seat) = self.seats.seats().next() else { return };
        for (name, seconds) in wanted {
            if self.idle_notifications.contains_key(&name) {
                continue;
            }
            let data = IdleData(name.clone());
            let notification = notifier.get_idle_notification(seconds.saturating_mul(1000), &seat, &self.qh, data);
            self.idle_notifications.insert(name, (notification, seconds));
        }
    }


    fn publish_toplevels(&mut self) {
        let (Some(list), Some(toplevels)) = (&self.toplevel_list, &self.toplevels) else { return };
        let now: Vec<genoma_services::toplevels::Toplevel> = list
            .toplevels()
            .iter()
            .filter_map(|t| list.info(t))
            .map(|i| genoma_services::toplevels::Toplevel { identifier: i.identifier, title: i.title, app_id: i.app_id })
            .collect();
        toplevels.list.set(now);
    }

    /// Where input is delivered: (the surface's tree index, node, input).
    pub fn on_input(&mut self, f: impl FnMut(usize, NodeId, Input) + 'static) {
        self.input = Some(Box::new(f));
    }

    pub fn start(&mut self, renderer: Renderer, fonts: Fonts, handle: LoopHandle<'static, App>) {
        self.renderer = Some(renderer);
        self.fonts = Some(fonts);
        self.handle = Some(handle);
        if let Some(seat) = self.keyboard_seat.take() {
            self.take_keyboard(&seat);
        }
    }

    /// The keyboard, repeating a held key at the rate the compositor says:
    /// a held Backspace in a field deletes on.
    fn take_keyboard(&mut self, seat: &wl_seat::WlSeat) {
        let Some(handle) = self.handle.clone() else {
            self.keyboard_seat = Some(seat.clone());
            return;
        };
        let qh = self.qh.clone();
        self.seat = Some(seat.clone());
        self.keyboard = self
            .seats
            .get_keyboard_with_repeat(&qh, seat, None, handle, Box::new(|app: &mut App, _, event| app.key(&event)))
            .ok();
    }

    /// The outputs the interface asks for, by name; `[None]` for one surface
    /// where the compositor puts it.
    /// The outputs as the compositor describes them, in its order: what an
    /// interface may place its surfaces on.
    pub fn output_list(&self) -> Vec<genoma_script::OutputInfo> {
        self.outputs
            .outputs()
            .filter_map(|o| self.outputs.info(&o))
            .filter_map(|i| {
                Some(genoma_script::OutputInfo {
                    name: i.name?,
                    description: i.description.unwrap_or_default(),
                    make: i.make,
                    model: i.model,
                    size: i.logical_size.map(|(w, h)| (w.max(0) as u32, h.max(0) as u32)),
                })
            })
            .collect()
    }

    fn wanted_outputs(&self, spec: &SurfaceSpec) -> Vec<Option<String>> {
        let names: Vec<String> =
            self.outputs.outputs().filter_map(|o| self.outputs.info(&o).and_then(|i| i.name)).collect();
        // A lock surface is on every output while the lock is held, and
        // nowhere otherwise: a lock that left one output bare is no lock.
        if spec.layer == "lock" {
            return if self.session_lock.is_some() { names.into_iter().map(Some).collect() } else { Vec::new() };
        }
        if spec.all_outputs {
            return names.into_iter().map(Some).collect();
        }
        if let Some(list) = &spec.outputs {
            return list.iter().filter(|n| names.contains(n)).cloned().map(Some).collect();
        }
        // GENOMA_OUTPUT overrides the interface's output, for testing.
        vec![std::env::var("GENOMA_OUTPUT").ok().or(spec.output.clone())]
    }

    /// Makes and removes surfaces so there is one per output the description
    /// asks for, and places them all. Returns the targets to mount, in the
    /// surfaces' order.
    pub fn apply(&mut self, specs: Vec<SurfaceSpec>) -> Vec<Target> {
        // Every (surface, output) the description asks for, in its order.
        let wanted: Vec<(usize, Option<String>)> = specs
            .iter()
            .enumerate()
            .flat_map(|(i, spec)| self.wanted_outputs(spec).into_iter().map(move |o| (i, o)))
            .collect();
        // A surface whose layer or namespace changed is made again: a layer
        // surface keeps both for its life.
        let changed = |i: usize, old: &[SurfaceSpec]| {
            old.get(i).is_none_or(|o| specs.get(i).is_none_or(|n| o.layer != n.layer || o.namespace != n.namespace))
        };
        let old = std::mem::take(&mut self.specs);
        let gone: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|s| !wanted.contains(&(s.spec, s.output.clone())) || changed(s.spec, &old))
            .map(|s| s.id)
            .collect();
        if let Some(renderer) = &self.renderer {
            for id in &gone {
                renderer.destroy_target(*id);
            }
        }
        self.surfaces.retain(|s| !gone.contains(&s.id));
        // A drag kept for a surface's index must not reach another.
        self.drag = None;
        self.specs = specs;
        for (spec, output) in &wanted {
            if !self.surfaces.iter().any(|s| s.spec == *spec && &s.output == output) {
                let description = self.specs[*spec].clone();
                self.make_surface(*spec, output.clone(), &description);
            }
        }
        // In the description's order, which is the order trees are mounted in.
        self.surfaces.sort_by_key(|s| wanted.iter().position(|w| w.0 == s.spec && w.1 == s.output).unwrap_or(usize::MAX));
        for i in 0..self.surfaces.len() {
            self.place(i);
        }
        self.surfaces
            .iter_mut()
            .map(|s| {
                s.built = s.area.or(s.output_size);
                (s.spec, s.output.clone(), s.scene.clone(), s.built)
            })
            .collect()
    }

    /// Measures and places every surface again: called once their trees are
    /// mounted, since [`App::apply`] runs before there is anything to measure.
    pub fn refit(&mut self) {
        for i in 0..self.surfaces.len() {
            self.place(i);
        }
    }

    fn make_surface(&mut self, spec_index: usize, output_name: Option<String>, spec: &SurfaceSpec) {
        let fonts = self.fonts.clone().expect("started");
        let qh = self.qh.clone();
        let id = self.next_id;
        self.next_id += 1;
        let wl = self.compositor.create_surface(&qh);
        if let Some(fractional) = &self.fractional {
            fractional.get_fractional_scale(&wl, &qh, ScaleData(id));
        }
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl, &qh, Ignore));
        let effect = self.effects.get_background_effect(&wl, &qh).ok();
        if effect.is_none() {
            log::warn!("the compositor offers no ext-background-effect: no blur");
        }
        let output = output_name.as_ref().and_then(|name| {
            self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name.as_str()))
        });
        let output_size = output
            .as_ref()
            .and_then(|o| self.outputs.info(o))
            .and_then(|i| i.logical_size)
            .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
        let role = if spec.layer == "lock" {
            let (Some(session_lock), Some(output)) = (&self.session_lock, output.as_ref()) else { return };
            Role::Lock(session_lock.create_lock_surface(wl, output, &qh))
        } else {
            let layer = match spec.layer.as_str() {
                "background" => Layer::Background,
                "bottom" => Layer::Bottom,
                "overlay" => Layer::Overlay,
                _ => Layer::Top,
            };
            let layer = self.layer_shell.create_layer_surface(&qh, wl, layer, Some(spec.namespace.clone()), output.as_ref());
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            Role::Layer(layer)
        };
        self.surfaces.push(Surface {
            id,
            spec: spec_index,
            output: output_name,
            scene: Rc::new(RefCell::new(Scene::new(fonts))),
            role,
            viewport,
            effect,
            target_made: false,
            logical: (1, 1),
            wanted: (1, 1),
            scale: 1.0,
            configured: false,
            dirty: false,
            frame_pending: false,
            frames: 0,
            tally: (0, Duration::ZERO, Instant::now()),
            blurred: Vec::new(),
            inputs: None,
            holes: Vec::new(),
            inhibitor: None,
            keyboard: false,
            hovered: None,
            pressed: None,
            pressed_at: (0.0, 0.0),
            asleep: false,
            reserve_wish: None,
            strokes: Vec::new(),
            tall: false,
            output_size,
            area: None,
            built: None,
            wake: None,
            access: crate::access::Access::new(),
            access_checked: None,
            held: None,
        });
    }

    /// A surface anchored to both left and right spans its edge, as a
    /// membrane does: the compositor gives it the width and the content lays
    /// out across it.
    fn spans(&self, i: usize) -> bool {
        self.specs.get(self.surfaces[i].spec).is_some_and(|s| {
            s.anchor.iter().any(|a| a == "left") && s.anchor.iter().any(|a| a == "right")
        })
    }

    /// The size to ask the compositor for: the content's, with 0 for a
    /// spanned width, which the compositor fills.
    fn measure(&mut self, i: usize) -> (u32, u32) {
        if self.surfaces[i].asleep {
            return (1, 1);
        }
        let spans = self.spans(i);
        let s = &mut self.surfaces[i];
        let width = (spans && s.configured).then_some(s.logical.0 as f32);
        let size = s.scene.borrow_mut().layout(width, None);
        let w = if spans { 0 } else { size.0.ceil().max(1.0) as u32 };
        // An open cell, or one still closing, needs the whole output: its
        // expansion is drawn there and a press anywhere else must reach it.
        let catching = s.scene.borrow().catcher().is_some();
        s.tall = catching || (s.tall && s.scene.borrow().transitioning());
        // A full surface is its output's size from the start and stays so:
        // resizing a layer surface draws its content once at each size.
        let full = self.specs.get(self.surfaces[i].spec).is_some_and(|spec| spec.full);
        // Anchored above and below, a full surface asks for no height and is
        // given what is left between the other surfaces' zones, as it is
        // given its width: asked for its output's height, niri cut it down
        // when a zone appeared and did not give it back when the zone went.
        let tall = self.specs.get(self.surfaces[i].spec).is_some_and(|spec| {
            spec.anchor.iter().any(|a| a == "top") && spec.anchor.iter().any(|a| a == "bottom")
        });
        let s = &mut self.surfaces[i];
        if full {
            if let Some((ow, oh)) = s.output_size {
                return (if spans { 0 } else { ow }, if tall { 0 } else { oh });
            }
        }
        let h = match (s.tall, s.output_size) {
            (true, Some((_, oh))) => oh,
            _ => size.1.ceil().max(1.0) as u32,
        };
        (w, h)
    }

    fn place(&mut self, i: usize) {
        let wanted = self.measure(i);
        let Some(spec) = self.specs.get(self.surfaces[i].spec).cloned() else { return };
        let s = &mut self.surfaces[i];
        s.wanted = wanted;
        // A lock surface is the size of its output, and the compositor's
        // to place: there is nothing to ask for.
        let Some(layer) = s.role.layer() else {
            s.dirty = true;
            return;
        };
        let mut anchor = Anchor::empty();
        for edge in &spec.anchor {
            anchor |= match edge.as_str() {
                "top" => Anchor::TOP,
                "bottom" => Anchor::BOTTOM,
                "left" => Anchor::LEFT,
                "right" => Anchor::RIGHT,
                other => {
                    log::warn!("anchor: no edge {other}");
                    Anchor::empty()
                }
            };
        }
        layer.set_anchor(anchor);
        let [top, right, bottom, left] = spec.margin;
        layer.set_margin(top, right, bottom, left);
        layer.set_size(wanted.0, wanted.1);
        // The reserved strip stays the membrane's, however tall the surface
        // grows while a cell is open.
        match spec.reserve_strip {
            _ if s.asleep => layer.set_exclusive_zone(0),
            // A strip its `reserve` asks for comes first: a membrane over
            // every zone while it keeps none keeps its own when it has one.
            _ if s.reserve_wish.is_some_and(|w| w.is_some()) => layer.set_exclusive_zone(s.reserve_wish.flatten().unwrap_or(0)),
            _ if spec.above_zones => layer.set_exclusive_zone(-1),
            _ if s.reserve_wish.is_some() => layer.set_exclusive_zone(0),
            Some(strip) => layer.set_exclusive_zone(if spec.reserve { strip } else { 0 }),
            None if !s.tall => layer.set_exclusive_zone(if spec.reserve { wanted.1 as i32 } else { 0 }),
            None => {}
        }
        s.keyboard = s.scene.borrow().catcher().is_some();
        layer.set_keyboard_interactivity(if s.keyboard {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        layer.commit();
        s.dirty = true;
    }

    /// What each surface's description asks for now, by target (the order
    /// [`App::apply`] returned): a surface that goes to sleep or wakes, or
    /// keeps another strip, is placed again.
    pub fn apply_wishes(&mut self, wishes: &[Wish]) {
        for (i, wish) in wishes.iter().enumerate() {
            let Some(s) = self.surfaces.get_mut(i) else { break };
            let asleep = !wish.present;
            if s.asleep == asleep && s.reserve_wish == wish.reserve {
                continue;
            }
            log::debug!("surface {}: {}", s.id, if asleep { "asleep" } else { "awake" });
            s.asleep = asleep;
            s.reserve_wish = wish.reserve;
            self.place(i);
        }
    }

    /// The surfaces whose area changed, once, each as the target to build
    /// again (its index among the targets, and the target).
    pub fn take_areas_changed(&mut self) -> Vec<(usize, Target)> {
        let ids = std::mem::take(&mut self.areas_changed);
        let mut out = Vec::new();
        for id in ids {
            let Some(i) = self.surfaces.iter().position(|s| s.id == id) else { continue };
            let s = &mut self.surfaces[i];
            s.built = s.area.or(s.output_size);
            out.push((i, (s.spec, s.output.clone(), s.scene.clone(), s.built)));
        }
        out
    }

    /// Whether the outputs changed under an interface that spans them all,
    /// once: its trees are then evaluated and mounted again.
    pub fn take_outputs_changed(&mut self) -> bool {
        std::mem::replace(&mut self.outputs_changed, false)
    }

    /// After every batch of events: each surface whose scene changed is laid
    /// out and asks for a frame. This is the only place a frame is asked for,
    /// apart from motion in flight.
    pub fn after_events(&mut self) {
        self.flush_drag();
        self.sync_idle();
        for i in 0..self.surfaces.len() {
            let changed = self.surfaces[i].scene.borrow_mut().take_dirty();
            let s = &self.surfaces[i];
            if !(changed || s.dirty) || !s.configured {
                continue;
            }
            let wanted = self.measure(i);
            if wanted != self.surfaces[i].wanted {
                // A new size waits for the compositor's configure, which draws.
                self.place(i);
                continue;
            }
            self.surfaces[i].dirty = true;
            if !self.surfaces[i].frame_pending {
                self.draw(i);
            }
        }
    }

    /// What the interface's other surfaces on this one's output take the
    /// pointer for. A catching surface leaves those to them, so a press on
    /// another membrane's cell reaches that cell rather than closing this
    /// one: the two regions never overlap, whatever the stacking. Only full
    /// surfaces are counted, since they alone share the output's origin.
    fn holes_for(&self, i: usize) -> Vec<[i32; 4]> {
        let s = &self.surfaces[i];
        let mut holes = Vec::new();
        for (j, other) in self.surfaces.iter().enumerate() {
            let full = self.specs.get(other.spec).is_some_and(|spec| spec.full);
            if j == i || !full || !other.configured || other.output != s.output {
                continue;
            }
            for r in other.scene.borrow().input_rects() {
                holes.push([r[0].floor() as i32, r[1].floor() as i32, r[2].ceil() as i32, r[3].ceil() as i32]);
            }
        }
        holes
    }

    fn draw(&mut self, i: usize) {
        let spans = self.spans(i);
        let catching = self.surfaces[i].scene.borrow().catcher().is_some();
        let holes = if catching { self.holes_for(i) } else { Vec::new() };
        let mut inputs_changed = false;
        let display = NonNull::new(self.conn.backend().display_ptr() as *mut c_void).unwrap();
        let (Some(renderer), Some(handle)) = (&self.renderer, self.handle.clone()) else { return };
        let compositor = &self.compositor;
        let qh = &self.qh;
        let s = &mut self.surfaces[i];
        let started = Instant::now();
        // A lock surface's first frame waits for its pictures, a while: the
        // frame is laid out and listed, which asks for them, and only shown
        // once nothing is still decoding.
        if matches!(s.role, Role::Lock(_)) && s.frames == 0 {
            let since = *s.held.get_or_insert_with(Instant::now);
            if since.elapsed() < LOCK_PICTURES {
                // Advanced as a frame would be: that is where finished
                // decodes are taken in.
                let _ = s.scene.borrow_mut().advance(Instant::now(), s.scale as f32);
                s.scene.borrow_mut().layout(spans.then_some(s.logical.0 as f32), None);
                let _ = s.scene.borrow_mut().draw_list(s.scale as f32);
                if let Some(wait) = s.scene.borrow().images_pending() {
                    let id = s.id;
                    if let Some(old) = s.wake.take() {
                        handle.remove(old);
                    }
                    s.wake = handle
                        .insert_source(Timer::from_duration(wait), move |_, _, app| {
                            if let Some(i) = app.surfaces.iter().position(|s| s.id == id) {
                                app.surfaces[i].wake = None;
                                app.draw(i);
                            }
                            TimeoutAction::Drop
                        })
                        .ok();
                    return;
                }
            }
        }
        let surface = s.role.wl_surface().clone();
        if !s.target_made {
            let wl = NonNull::new(surface.id().as_ptr() as *mut c_void).unwrap();
            // Safety: the display lives as long as the connection, the surface
            // as long as its layer surface, and the target is destroyed
            // before the surface is.
            unsafe { renderer.create_target(s.id, display, wl) };
            s.target_made = true;
        }

        let width = (s.logical.0 as f64 * s.scale).round() as u32;
        let height = (s.logical.1 as f64 * s.scale).round() as u32;
        match &s.viewport {
            Some(viewport) => viewport.set_destination(s.logical.0 as i32, s.logical.1 as i32),
            None => surface.set_buffer_scale(s.scale.ceil() as i32),
        }
        let motion = s.scene.borrow_mut().advance(Instant::now(), s.scale as f32);
        s.scene.borrow_mut().layout(spans.then_some(s.logical.0 as f32), None);
        let list = s.scene.borrow_mut().draw_list(s.scale as f32);
        let animating = motion.moving;
        // An image asked for by this very frame is decoding now, and the
        // beat that looks for it again has to be set here: `advance` ran
        // before the frame asked. Without it a picture that nothing else
        // redrew stayed undrawn (a wallpaper's next picture, on a still
        // desktop).
        let mut motion = motion;
        if motion.wake.is_none() {
            motion.wake = s.scene.borrow().images_pending();
        }

        // The blur region follows the shapes, and is part of this commit.
        let mut shapes = Vec::new();
        for b in s.scene.borrow().blur_shapes() {
            match b {
                Blur::Rounded(b) => {
                    // Far beyond any screen is as good as the screen's edge,
                    // and keeps the sums below from passing i32.
                    let [x, y, w, h, r] = b.map(|v| v.clamp(-1.0e6, 1.0e6).round() as i32);
                    rounded(&mut shapes, x, y, w, h, r);
                }
                Blur::Outline(points) => outline(&mut shapes, points, s.logical.1 as i32),
            }
        }
        let shapes = if s.asleep { Vec::new() } else { within(shapes, s.logical) };
        if shapes != s.blurred {
            if let (Some(effect), Ok(region)) = (&s.effect, Region::new(compositor)) {
                for &[x, y, w, h] in &shapes {
                    region.add(x, y, w, h);
                }
                effect.set_blur_region(Some(region.wl_region()));
            }
            s.blurred = shapes;
        }
        // The input region: only what takes the pointer, or the whole surface
        // less the holes while a cell is catching. The rest lets the pointer
        // through.
        let inputs: Vec<[i32; 4]> = if catching {
            vec![[0, 0, s.logical.0 as i32, s.logical.1 as i32]]
        } else {
            s.scene
                .borrow()
                .input_rects()
                .iter()
                .map(|r| [r[0].floor() as i32, r[1].floor() as i32, r[2].ceil() as i32, r[3].ceil() as i32])
                .collect()
        };
        let inputs = if s.asleep { Vec::new() } else { within(inputs, s.logical) };
        if s.inputs.as_ref() != Some(&inputs) || s.holes != holes {
            if let Ok(region) = Region::new(compositor) {
                for &[x, y, w, h] in &inputs {
                    region.add(x, y, w, h);
                }
                for &[x, y, w, h] in &holes {
                    region.subtract(x, y, w, h);
                }
                surface.set_input_region(Some(region.wl_region()));
            }
            inputs_changed = s.inputs.as_ref() != Some(&inputs);
            s.inputs = Some(inputs);
            s.holes = holes;
        }
        // The keyboard while a cell is catching, with this frame's commit: a
        // full surface never changes size, so it is never placed again.
        if s.keyboard != catching {
            if let Some(layer) = s.role.layer() {
                layer.set_keyboard_interactivity(if catching {
                    KeyboardInteractivity::Exclusive
                } else {
                    KeyboardInteractivity::None
                });
            }
            s.keyboard = catching;
        }
        // The compositor's shortcuts stand aside while a catching surface's
        // node asks it, and come back as soon as it no longer does.
        let inhibit = catching && s.scene.borrow().inhibits_shortcuts();
        if inhibit != s.inhibitor.is_some() {
            if let Some(inhibitor) = s.inhibitor.take() {
                inhibitor.destroy();
            } else if let (Some(manager), Some(seat)) = (&self.shortcuts_inhibit, &self.seat) {
                s.inhibitor = Some(manager.inhibit_shortcuts(&surface, seat, qh, Ignore));
            }
        }
        // The scene's own record of this change is consumed by this frame.
        s.scene.borrow_mut().take_dirty();

        // Asked for with this frame's commit, so the next change waits for
        // the compositor instead of racing it.
        surface.frame(qh, FrameCallbackData(surface.clone()));
        s.frame_pending = true;
        renderer.draw(s.id, width, height, list);
        let built = started.elapsed();
        log::trace!("build {} us", built.as_micros());
        // A frame the eye can catch late: said, with what it held, so a
        // stall reported afterwards can be read back.
        if built > Duration::from_millis(20) {
            log::warn!("surface {} frame {}: built in {} ms", s.id, s.frames + 1, built.as_millis());
        }
        s.dirty = animating;
        s.frames += 1;
        // A minute's account at debug level: how often the surface drew and
        // what building its frames cost, so a slow drift can be read back.
        s.tally.0 += 1;
        s.tally.1 += built;
        if s.tally.2.elapsed() >= Duration::from_secs(60) {
            let (n, total, since) = s.tally;
            log::debug!(
                "surface {}: {} frames in {} s, build {:.2} ms each, {} animating",
                s.id,
                n,
                since.elapsed().as_secs(),
                total.as_secs_f64() * 1000.0 / n.max(1) as f64,
                animating
            );
            s.tally = (0, Duration::ZERO, Instant::now());
        }
        if s.access_checked.is_none_or(|t| t.elapsed() >= Duration::from_millis(250)) {
            s.access_checked = Some(Instant::now());
            let title = match &s.output {
                Some(o) => format!("Genoma on {o}"),
                None => "Genoma".to_owned(),
            };
            let nodes = s.scene.borrow().accessible();
            s.access.update(&title, nodes);
        }
        log::debug!(
            "surface {} frame {} at {:.3} s: {}×{} px, scale {}{}",
            s.id,
            s.frames,
            self.started.elapsed().as_secs_f64(),
            width,
            height,
            s.scale,
            if animating { ", animating" } else { "" }
        );
        // Between beats nothing moves: no frames, only a timer for the next.
        if let (false, Some(wait)) = (animating, motion.wake) {
            if let Some(old) = s.wake.take() {
                handle.remove(old);
            }
            let id = s.id;
            s.wake = handle
                .insert_source(Timer::from_duration(wait), move |_, _, app| {
                    if let Some(i) = app.surfaces.iter().position(|s| s.id == id) {
                        app.surfaces[i].wake = None;
                        app.surfaces[i].dirty = true;
                        if !app.surfaces[i].frame_pending {
                            app.draw(i);
                        }
                    }
                    TimeoutAction::Drop
                })
                .ok();
        }

        // Images drawn for the first time: told, once each, after the frame
        // that shows them (a wallpaper fades in only once it is there).
        let ready = self.surfaces[i].scene.borrow_mut().take_ready();
        for node in ready {
            self.deliver(i, node, Input::Ready);
        }

        // What this surface takes moved, so a surface catching on the same
        // output cuts its holes again.
        if inputs_changed {
            let output = self.surfaces[i].output.clone();
            for j in 0..self.surfaces.len() {
                let s = &self.surfaces[j];
                if j != i && s.configured && s.output == output && s.scene.borrow().catcher().is_some() {
                    self.surfaces[j].dirty = true;
                    if !self.surfaces[j].frame_pending {
                        self.draw(j);
                    }
                }
            }
        }
    }

    fn by_wl(&self, wl: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.role.wl_surface() == wl)
    }

    fn flush_drag(&mut self) {
        if let Some((i, node, x, y)) = self.drag.take() {
            self.deliver(i, node, Input::Drag(x, y));
        }
    }

    fn deliver(&mut self, i: usize, node: NodeId, input: Input) {
        if let Some(f) = self.input.as_mut() {
            f(i, node, input);
        }
    }

    fn key(&mut self, event: &KeyEvent) {
        // The keys go to the cell that is catching, on whichever output.
        let Some((i, catcher)) = self
            .surfaces
            .iter()
            .enumerate()
            .find_map(|(i, s)| s.scene.borrow().catcher().map(|c| (i, c)))
        else {
            return;
        };
        let name = event.keysym.name().unwrap_or_default();
        let name = name.strip_prefix("XK_").unwrap_or(name).to_owned();
        // The text the key types, as the keymap and its layout make it; none
        // while Ctrl or Alt is held, when the key is a command.
        let m = self.modifiers;
        let text = event.utf8.clone().filter(|t| !t.chars().any(char::is_control) && !m.ctrl && !m.alt);
        let key = genoma_script::Key { name, text, ctrl: m.ctrl, shift: m.shift, alt: m.alt, logo: m.logo, code: event.raw_code + 8 };
        self.deliver(i, catcher, Input::Key(key));
    }
}

/// A rounded rectangle as a region: the middle band, and one row per logical
/// unit in each corner, inset along the circle. As fine as the region can be
/// in surface-local units.
fn rounded(region: &mut Vec<[i32; 4]>, x: i32, y: i32, w: i32, h: i32, r: i32) {
    let r = r.min(w / 2).min(h / 2).max(0);
    region.push([x, y + r, w, h - 2 * r]);
    for i in 0..r {
        let d = r as f32 - i as f32 - 0.5;
        let inset = (r as f32 - (r as f32 * r as f32 - d * d).max(0.0).sqrt()).round() as i32;
        region.push([x + inset, y + i, w - 2 * inset, 1]);
        region.push([x + inset, y + h - 1 - i, w - 2 * inset, 1]);
    }
}

/// Rectangles cut to the surface, the empty ones left out. A region is the
/// compositor's to add up, and niri 26.04 aborts when a rectangle's far edge
/// passes i32 (`utils/region.rs`, the blur region): whatever a script did,
/// nothing outside the surface is sent.
fn within(rects: Vec<[i32; 4]>, logical: (u32, u32)) -> Vec<[i32; 4]> {
    let (sw, sh) = (logical.0 as i64, logical.1 as i64);
    rects
        .into_iter()
        .filter_map(|[x, y, w, h]| {
            let (x0, y0) = ((x as i64).clamp(0, sw), (y as i64).clamp(0, sh));
            let (x1, y1) = ((x as i64 + w as i64).clamp(0, sw), (y as i64 + h as i64).clamp(0, sh));
            (x1 > x0 && y1 > y0).then(|| [x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32])
        })
        .collect()
}

/// A convex outline as rows one logical unit tall: each row spans where its
/// middle crosses the outline's edges.
/// Only the rows on the surface, `0..height`, are walked.
fn outline(region: &mut Vec<[i32; 4]>, points: &[[f32; 2]], height: i32) {
    let top = points.iter().map(|p| p[1]).fold(f32::MAX, f32::min).round().max(0.0) as i32;
    let bottom = (points.iter().map(|p| p[1]).fold(f32::MIN, f32::max).round() as i32).min(height);
    for row in top..bottom {
        let y = row as f32 + 0.5;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for (i, a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            if (a[1] <= y) != (b[1] <= y) {
                let x = a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                lo = lo.min(x);
                hi = hi.max(x);
            }
        }
        let (lo, hi) = (lo.round() as i32, hi.round() as i32);
        if hi > lo {
            region.push([lo, row, hi - lo, 1]);
        }
    }
}

/// The PCI address of a DRM device, from its `dev_t`: the device directory
/// sysfs keeps for every character device ends in it (`0000:03:00.0`).
fn pci_bus_id(dev: u64) -> Option<String> {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    let path = PathBuf::from(format!("/sys/dev/char/{major}:{minor}/device"));
    let pci = std::fs::canonicalize(path).ok()?.file_name()?.to_str()?.to_owned();
    pci.contains(':').then_some(pci)
}

/// Objects whose events Genoma does not need (managers, viewports).
struct Ignore;

impl<I: Proxy> Dispatch2<I, App> for Ignore {
    fn event(&self, _: &mut App, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<App>) {}
}

/// An idle notification, by the name it was asked for under: idle once the
/// compositor says so, back on input.
struct IdleData(String);

impl Dispatch2<ExtIdleNotificationV1, App> for IdleData {
    fn event(&self, app: &mut App, _: &ExtIdleNotificationV1, event: ext_idle_notification_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        let Some(idle) = &app.idle else { return };
        match event {
            ext_idle_notification_v1::Event::Idled => idle.set_idle(&self.0, true),
            ext_idle_notification_v1::Event::Resumed => idle.set_idle(&self.0, false),
            _ => {}
        }
    }
}

/// A surface's fractional scale object, carrying the surface's id.
struct ScaleData(SurfaceId);

impl Dispatch2<WpFractionalScaleV1, App> for ScaleData {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if let Some(s) = app.surfaces.iter_mut().find(|s| s.id == self.0) {
                if scale != s.scale {
                    s.scale = scale;
                    s.dirty = true;
                }
            }
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, factor: i32) {
        // Only without fractional scale, which already said it more exactly.
        if let Some(i) = self.by_wl(wl) {
            let s = &mut self.surfaces[i];
            if s.viewport.is_none() && factor as f64 != s.scale {
                s.scale = factor as f64;
                s.dirty = true;
            }
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.by_wl(wl) {
            self.surfaces[i].frame_pending = false;
            if self.surfaces[i].dirty {
                self.draw(i);
            }
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, wl: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        let size = self
            .outputs
            .info(output)
            .and_then(|i| i.logical_size)
            .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
        if let Some(i) = self.by_wl(wl) {
            self.surfaces[i].output_size = size;
        }
    }

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor took the surface away (its output went): drop it.
        if let Some(i) = self.by_wl(layer.wl_surface()) {
            let s = self.surfaces.remove(i);
            self.drag = None;
            if let Some(renderer) = &self.renderer {
                renderer.destroy_target(s.id);
            }
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(i) = self.by_wl(layer.wl_surface()) else { return };
        let full = self.specs.get(self.surfaces[i].spec).is_some_and(|spec| spec.full);
        let s = &mut self.surfaces[i];
        let (w, h) = configure.new_size;
        s.logical = if w > 0 && h > 0 { (w, h) } else { s.wanted };
        // A full surface's tree is built for its output, but the compositor
        // keeps it clear of other surfaces' zones: a bottom membrane under
        // another shell's top bar got 56 px less, and its band was laid out
        // below the surface's end, out of sight. Built again for the size
        // given; the next configure gives the same and nothing follows.
        // Asleep, it is one unit square: nothing to build for.
        if full && !s.asleep && w > 0 && h > 0 && s.built.is_some() && s.built != Some((w, h)) {
            s.area = Some((w, h));
            if !self.areas_changed.contains(&s.id) {
                self.areas_changed.push(s.id);
            }
        }
        s.configured = true;
        // A configure must be answered with a buffer, frame callback or not.
        s.frame_pending = false;
        self.draw(i);
    }
}

impl SessionLockHandler for App {
    fn locked(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        log::info!("session locked");
        if let Some(lock) = &self.lock {
            lock.locked.set(true);
        }
    }

    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        // Refused, or ended by the compositor: the lock surfaces are inert
        // and go; the interface hears it and decides what follows.
        log::warn!("session lock finished by the compositor");
        self.session_lock = None;
        self.drop_lock_surfaces();
        if let Some(lock) = &self.lock {
            lock.locked.set(false);
            lock.finished.set(true);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.by_wl(surface.wl_surface()) else { return };
        let s = &mut self.surfaces[i];
        let (w, h) = configure.new_size;
        s.logical = (w.max(1), h.max(1));
        // Built for the output's size, as the compositor gives it: told
        // otherwise, built again.
        if s.built.is_some() && s.built != Some(s.logical) {
            s.area = Some(s.logical);
            self.outputs_changed = true;
        }
        s.wanted = s.logical;
        s.configured = true;
        s.frame_pending = false;
        self.draw(i);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(qh, &seat).ok();
            if let (Some(pointer), Some(shapes)) = (&self.pointer, &self.cursor_shapes) {
                self.cursor_device = Some(shapes.get_shape_device(pointer, qh));
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let _ = qh;
            self.take_keyboard(&seat);
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(device) = self.cursor_device.take() {
                device.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key(&event);
    }

    // Repeats arrive through the callback the keyboard was taken with.
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, modifiers: Modifiers, _: RawModifiers, _: u32) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use smithay_client_toolkit::seat::pointer::PointerEventKind as K;
        for event in events {
            log::trace!("pointer {:?} at {:?}", event.kind, event.position);
            if !matches!(event.kind, K::Motion { .. }) {
                self.flush_drag();
            }
            let Some(i) = self.by_wl(&event.surface) else { continue };
            let scene = self.surfaces[i].scene.clone();
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            let hit = match event.kind {
                K::Leave { .. } => None,
                _ => scene.borrow().hit_test(x, y),
            };
            if hit != self.surfaces[i].hovered {
                if let Some(old) = self.surfaces[i].hovered.take() {
                    self.deliver(i, old, Input::Hover(false));
                }
                if let Some(new) = hit {
                    self.deliver(i, new, Input::Hover(true));
                }
                self.surfaces[i].hovered = hit;
            }
            if let K::Enter { serial } = event.kind {
                self.cursor_serial = serial;
                self.cursor_shape = None;
            }
            // Where it is, to what it is over: on entering too, since a
            // surface that starts taking input under a pointer that has not
            // moved is told where that pointer is only then (summoning a
            // cell at the pointer).
            if matches!(event.kind, K::Enter { .. } | K::Motion { .. }) {
                if let Some(node) = hit {
                    self.deliver(i, node, Input::Move(x, y));
                }
            }
            if !matches!(event.kind, K::Leave { .. }) {
                let wanted = hit.map_or(genoma_scene::CURSORS[0], |h| scene.borrow().cursor(h));
                self.set_cursor(wanted);
            }
            match event.kind {
                K::Motion { .. } => {
                    if let Some(node) = self.surfaces[i].pressed {
                        let (ox, oy) = self.surfaces[i].pressed_at;
                        self.drag = Some((i, node, x - ox, y - oy));
                    }
                }
                K::Press { button, .. } => {
                    let catcher = scene.borrow().catcher();
                    if let Some(c) = catcher {
                        if hit.is_none_or(|h| !scene.borrow().within(h, c)) {
                            self.deliver(i, c, Input::Outside);
                        }
                    }
                    self.surfaces[i].pressed = hit;
                    if let Some(node) = hit {
                        let (lx, ly) = scene.borrow().local(node, x, y);
                        self.surfaces[i].pressed_at = (x - lx, y - ly);
                        self.deliver(i, node, Input::Press(button, lx, ly));
                    }
                }
                K::Release { button, .. } => {
                    // The node pressed hears its release wherever the pointer
                    // is let go: a drag that ends over another node ends for
                    // the one that began it, as a pointer grab does. It is a
                    // click only let go over the node pressed.
                    let pressed = self.surfaces[i].pressed.take();
                    if let Some(node) = pressed.or(hit) {
                        let (lx, ly) = scene.borrow().local(node, x, y);
                        self.deliver(i, node, Input::Release(button, lx, ly));
                        if pressed.is_some() && pressed == hit {
                            self.deliver(i, node, Input::Click(button, lx, ly));
                        }
                    }
                }
                K::Axis { horizontal, vertical, source, .. } => {
                    let Some(node) = hit else { continue };
                    // A touchpad stroke is followed, and remembered: when the
                    // fingers lift while moving, the content carries on at
                    // their speed over Bioma's Timing.fling, 80 ms.
                    if source == Some(wl_pointer::AxisSource::Finger) {
                        let now = Instant::now();
                        if horizontal.stop || vertical.stop {
                            let window = Duration::from_millis(80);
                            let recent: Vec<(Instant, f64, f64)> = self.surfaces[i]
                                .strokes
                                .iter()
                                .filter(|s| now.duration_since(s.0) <= window)
                                .copied()
                                .collect();
                            let (dx, dy) = recent.iter().fold((0.0, 0.0), |a, s| (a.0 + s.1, a.1 + s.2));
                            self.surfaces[i].strokes.clear();
                            // Fingers that stopped before lifting leave the
                            // content where it is.
                            if !recent.is_empty() && (dx != 0.0 || dy != 0.0) {
                                let secs = window.as_secs_f64();
                                self.deliver(i, node, Input::Fling(dx / secs, dy / secs));
                            }
                            continue;
                        }
                        let strokes = &mut self.surfaces[i].strokes;
                        strokes.push((now, horizontal.absolute, vertical.absolute));
                        strokes.retain(|s| now.duration_since(s.0) <= Duration::from_millis(200));
                    }
                    // A wheel speaks in steps of 120; a touchpad in logical
                    // distance, and only when it speaks at all.
                    let step = |a: &AxisScroll| if a.value120 != 0 { a.value120 as f64 / 120.0 } else { a.absolute };
                    let (dx, dy) = (step(&horizontal), step(&vertical));
                    if dx != 0.0 || dy != 0.0 {
                        self.deliver(i, node, Input::Wheel(dx, dy));
                    }
                }
                K::Leave { .. } => self.surfaces[i].pressed = None,
                _ => {}
            }
        }
    }
}

impl App {
    /// The pointer's shape, set only when it changes: the compositor draws
    /// it from the user's cursor theme.
    fn set_cursor(&mut self, name: &'static str) {
        if self.cursor_shape == Some(name) {
            return;
        }
        let Some(device) = &self.cursor_device else { return };
        let shape = match name {
            "pointer" => Shape::Pointer,
            "crosshair" => Shape::Crosshair,
            "text" => Shape::Text,
            "grab" => Shape::Grab,
            "grabbing" => Shape::Grabbing,
            "not-allowed" => Shape::NotAllowed,
            _ => Shape::Default,
        };
        device.set_shape(self.cursor_serial, shape);
        self.cursor_shape = Some(name);
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if self.specs.iter().any(|s| s.all_outputs || s.outputs.is_some() || s.layer == "lock") {
            self.outputs_changed = true;
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if self.specs.iter().any(|s| s.all_outputs || s.outputs.is_some() || s.layer == "lock") {
            self.outputs_changed = true;
        }
    }
}

impl DmabufHandler for App {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        proxy: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        feedback: DmabufFeedback,
    ) {
        self.main_device = Some(feedback.main_device() as u64);
        self.feedback_done = true;
        proxy.destroy();
    }

    fn created(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, _: wl_buffer::WlBuffer) {}

    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1) {}

    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_buffer::WlBuffer) {}
}

impl ForeignToplevelListHandler for App {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        self.toplevel_list.as_mut().expect("bound before its events")
    }

    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {
        self.publish_toplevels();
    }

    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {
        self.publish_toplevels();
    }

    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) {
        self.publish_toplevels();
    }
}

impl BackgroundEffectHandler for App {
    fn background_effect_state(&mut self) -> &mut BackgroundEffectState {
        &mut self.effects
    }

    fn update_capabilities(&mut self) {}
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
