//! The surface on the compositor: a layer surface at the output's fractional
//! scale, drawn only when its scene says something changed.

use std::{cell::RefCell, ffi::c_void, path::PathBuf, ptr::NonNull, rc::Rc, time::Instant};

use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken, timer::{TimeoutAction, Timer}};

use genoma_render::{Renderer, SurfaceId};
use genoma_scene::Scene;
use genoma_script::SurfaceSpec;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    dispatch2::Dispatch2,
    dmabuf::{DmabufFeedback, DmabufHandler, DmabufState},
    output::{OutputHandler, OutputState},
    reexports::{
        client::{
            Connection, EventQueue, Proxy, QueueHandle,
            globals::{GlobalList, registry_queue_init},
            protocol::{wl_buffer, wl_output, wl_pointer, wl_seat, wl_surface},
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1},
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

const SURFACE: SurfaceId = 1;

pub struct App {
    conn: Connection,
    qh: QueueHandle<App>,
    globals: GlobalList,
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    dmabuf: DmabufState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    viewporter: Option<WpViewporter>,
    fractional: Option<WpFractionalScaleManagerV1>,
    main_device: Option<u64>,
    feedback_done: bool,

    renderer: Option<Renderer>,
    scene: Option<Rc<RefCell<Scene>>>,
    layer: Option<LayerSurface>,
    viewport: Option<WpViewport>,
    target_made: bool,
    pointer: Option<wl_pointer::WlPointer>,
    spec: Option<SurfaceSpec>,
    handle: Option<LoopHandle<'static, App>>,
    /// The timer that wakes the surface for the next beat, if one is set.
    wake: Option<RegistrationToken>,

    /// The surface's size in logical units, as last configured.
    logical: (u32, u32),
    /// What the content asks for; differs from `logical` until a configure.
    wanted: (u32, u32),
    scale: f64,
    configured: bool,
    dirty: bool,
    frame_pending: bool,
    frames: u64,
    started: Instant,
}

impl App {
    pub fn connect() -> (App, EventQueue<App>) {
        let conn = Connection::connect_to_env().expect("no Wayland compositor");
        let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry");
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
        let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell");
        let viewporter = globals.bind(&qh, 1..=1, Ignore).ok();
        let fractional = globals.bind(&qh, 1..=1, Ignore).ok();
        let mut app = App {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            dmabuf: DmabufState::new(&globals, &qh),
            compositor,
            layer_shell,
            viewporter,
            fractional,
            conn: conn.clone(),
            qh: qh.clone(),
            globals,
            main_device: None,
            feedback_done: false,
            renderer: None,
            scene: None,
            layer: None,
            viewport: None,
            target_made: false,
            pointer: None,
            spec: None,
            handle: None,
            wake: None,
            logical: (1, 1),
            wanted: (1, 1),
            scale: 1.0,
            configured: false,
            dirty: false,
            frame_pending: false,
            frames: 0,
            started: Instant::now(),
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
        let _ = &app.globals;
        (app, queue)
    }

    pub fn connection(&self) -> Connection {
        self.conn.clone()
    }

    pub fn main_device_pci(&self) -> Option<String> {
        self.main_device.and_then(pci_bus_id)
    }

    pub fn open(
        &mut self,
        renderer: Renderer,
        scene: Rc<RefCell<Scene>>,
        spec: SurfaceSpec,
        qh: &QueueHandle<App>,
        handle: LoopHandle<'static, App>,
    ) {
        self.handle = Some(handle);
        self.spec = Some(spec.clone());
        self.wanted = self.measure(&scene);
        self.renderer = Some(renderer);
        self.scene = Some(scene);

        let surface = self.compositor.create_surface(qh);
        if let Some(fractional) = &self.fractional {
            fractional.get_fractional_scale(&surface, qh, ScaleData);
        }
        self.viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, Ignore));

        // GENOMA_OUTPUT overrides the interface's output, for testing.
        let output_name = std::env::var("GENOMA_OUTPUT").ok().or(spec.output.clone());
        let output = output_name.and_then(|name| {
            self.outputs.outputs().find(|o| {
                self.outputs.info(o).and_then(|info| info.name).as_deref() == Some(name.as_str())
            })
        });
        let layer = match spec.layer.as_str() {
            "background" => Layer::Background,
            "bottom" => Layer::Bottom,
            "overlay" => Layer::Overlay,
            _ => Layer::Top,
        };
        let layer =
            self.layer_shell.create_layer_surface(qh, surface, layer, Some("genoma"), output.as_ref());
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        self.layer = Some(layer);
        self.place(&spec);
    }

    /// A surface anchored to both left and right spans its edge, as a
    /// membrane does: the compositor gives it the width and the content lays
    /// out across it.
    fn spans(&self) -> bool {
        self.spec.as_ref().is_some_and(|s| {
            s.anchor.iter().any(|a| a == "left") && s.anchor.iter().any(|a| a == "right")
        })
    }

    /// The size to ask the compositor for: the content's, with 0 for a
    /// spanned width, which the compositor fills.
    fn measure(&self, scene: &Rc<RefCell<Scene>>) -> (u32, u32) {
        let spans = self.spans();
        let width = (spans && self.configured).then_some(self.logical.0 as f32);
        let size = scene.borrow_mut().layout(width, None);
        let w = if spans { 0 } else { size.0.ceil().max(1.0) as u32 };
        (w, size.1.ceil().max(1.0) as u32)
    }

    fn place(&mut self, spec: &SurfaceSpec) {
        let Some(layer) = &self.layer else { return };
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
        layer.set_size(self.wanted.0, self.wanted.1);
        layer.set_exclusive_zone(if spec.reserve { self.wanted.1 as i32 } else { 0 });
        layer.commit();
    }

    /// A reload's surface description. The output and the layer are fixed
    /// when the surface is made; the rest moves in place.
    pub fn respec(&mut self, spec: SurfaceSpec) {
        if let Some(old) = &self.spec {
            if old.output != spec.output || old.layer != spec.layer {
                log::warn!("a new output or layer takes effect at the next start");
            }
        }
        self.spec = Some(spec.clone());
        self.place(&spec);
        self.dirty = true;
    }

    /// After every batch of events: if the scene changed, lay it out and ask
    /// for a frame. This is the only place a frame is asked for, apart from a
    /// transition in flight.
    pub fn after_events(&mut self) {
        let Some(scene) = self.scene.clone() else { return };
        let changed = scene.borrow_mut().take_dirty();
        if !(changed || self.dirty) || !self.configured {
            return;
        }
        let wanted = self.measure(&scene);
        if wanted != self.wanted {
            // A new size waits for the compositor's configure, which draws.
            self.wanted = wanted;
            if let (Some(layer), Some(spec)) = (&self.layer, &self.spec) {
                layer.set_size(wanted.0, wanted.1);
                layer.set_exclusive_zone(if spec.reserve { wanted.1 as i32 } else { 0 });
                layer.commit();
            }
            return;
        }
        self.dirty = true;
        if !self.frame_pending {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let (Some(layer), Some(renderer), Some(scene)) = (&self.layer, &self.renderer, &self.scene) else {
            return;
        };
        let started = Instant::now();
        let surface = layer.wl_surface();
        if !self.target_made {
            let display = NonNull::new(self.conn.backend().display_ptr() as *mut c_void).unwrap();
            let wl = NonNull::new(surface.id().as_ptr() as *mut c_void).unwrap();
            // Safety: the display lives as long as the connection, the surface
            // as long as the layer surface; both outlive the target.
            unsafe { renderer.create_target(SURFACE, display, wl) };
            self.target_made = true;
        }

        let width = (self.logical.0 as f64 * self.scale).round() as u32;
        let height = (self.logical.1 as f64 * self.scale).round() as u32;
        match &self.viewport {
            Some(viewport) => viewport.set_destination(self.logical.0 as i32, self.logical.1 as i32),
            None => surface.set_buffer_scale(self.scale.ceil() as i32),
        }
        let motion = scene.borrow_mut().advance(Instant::now(), self.scale as f32);
        let span = self.spans().then_some(self.logical.0 as f32);
        scene.borrow_mut().layout(span, None);
        let list = scene.borrow_mut().draw_list(self.scale as f32);
        let animating = motion.moving;
        // The scene's own record of this change is consumed by this frame.
        scene.borrow_mut().take_dirty();

        // Asked for with this frame's commit, so the next change waits for
        // the compositor instead of racing it.
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
        renderer.draw(SURFACE, width, height, list);
        log::trace!("build {} us", started.elapsed().as_micros());
        self.dirty = animating;
        self.frames += 1;
        // Between beats nothing moves: no frames, only a timer for the next.
        if let (false, Some(wait), Some(handle)) = (animating, motion.wake, self.handle.clone()) {
            if let Some(old) = self.wake.take() {
                handle.remove(old);
            }
            self.wake = handle
                .insert_source(Timer::from_duration(wait), |_, _, app| {
                    app.wake = None;
                    app.dirty = true;
                    if !app.frame_pending {
                        app.draw();
                    }
                    TimeoutAction::Drop
                })
                .ok();
        }
        log::debug!(
            "frame {} at {:.3} s: {}×{} px, scale {}{}",
            self.frames,
            self.started.elapsed().as_secs_f64(),
            width,
            height,
            self.scale,
            if animating { ", animating" } else { "" }
        );
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

struct ScaleData;

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
            if scale != app.scale {
                app.scale = scale;
                app.dirty = true;
            }
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        // Only without fractional scale, which already said it more exactly.
        if self.viewport.is_none() && factor as f64 != self.scale {
            self.scale = factor as f64;
            self.dirty = true;
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.draw();
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        std::process::exit(0);
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let (w, h) = configure.new_size;
        self.logical = if w > 0 && h > 0 { (w, h) } else { self.wanted };
        self.configured = true;
        // A configure must be answered with a buffer, frame callback or not.
        self.frame_pending = false;
        self.draw();
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
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, _: &[PointerEvent]) {
        // Input reaches the interface in M3.
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
