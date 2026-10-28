//! Milestone 0 (PRD §11): one layer surface on niri, drawn by one wgpu device
//! on the compositor's GPU, at the output's fractional scale, and nothing at
//! all while nothing changes.
//!
//! The surface is a single capsule with Bioma's lit border. Hovering it lights
//! the rim in the primary colour, so a frame can be caused on purpose and the
//! log can show that none happens otherwise.

use std::{ffi::c_void, path::PathBuf, ptr::NonNull, time::Instant};

use genoma_render::{Gpu, Rect, Target};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    dispatch2::Dispatch2,
    dmabuf::{DmabufFeedback, DmabufHandler, DmabufState},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::EventLoop,
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_buffer, wl_output, wl_pointer, wl_seat, wl_surface},
        },
        protocols::wp::{
            fractional_scale::v1::client::{
                wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                wp_fractional_scale_v1::{self, WpFractionalScaleV1},
            },
            linux_dmabuf::zv1::client::{
                zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1,
            },
            viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

/// The surface, in logical units: a 40-unit capsule (Bioma's cell height)
/// with room around it.
const SURFACE: (u32, u32) = (168, 48);
const CAPSULE_INSET: f64 = 4.0;
/// Below a Bioma membrane, while the two run side by side.
const MARGIN_TOP: i32 = 72;

fn rgb(hex: u32, alpha: f32) -> [f32; 4] {
    let channel = |shift: u32| ((hex >> shift) & 0xff) as f32 / 255.0;
    [channel(16), channel(8), channel(0), alpha]
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("GENOMA_LOG", "info")).init();

    let conn = Connection::connect_to_env().expect("no Wayland compositor");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry");
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell");
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, Ignore).ok();
    let fractional: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, Ignore).ok();

    let mut app = App {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        dmabuf: DmabufState::new(&globals, &qh),
        main_device: None,
        feedback_done: false,
        gpu: None,
        target: None,
        layer: None,
        viewport: None,
        conn: conn.clone(),
        pointer: None,
        logical: SURFACE,
        scale: 1.0,
        configured: false,
        hovered: false,
        dirty: false,
        frame_pending: false,
        frames: 0,
        started: Instant::now(),
    };

    // The compositor's main device, before any GPU is opened: rendering on
    // another one copies every frame between the two.
    match app.dmabuf.get_default_feedback(&qh) {
        Ok(_feedback) => {
            while !app.feedback_done {
                queue.blocking_dispatch(&mut app).expect("dispatch");
            }
        }
        Err(_) => log::warn!("linux-dmabuf below version 4: no main device to match"),
    }
    let pci = app.main_device.and_then(pci_bus_id);
    log::info!("compositor's main device: {}", pci.as_deref().unwrap_or("unknown"));
    let gpu = Gpu::new(pci.as_deref()).expect("gpu");
    if pci.is_some_and(|id| !gpu.adapter_info().device_pci_bus_id.eq_ignore_ascii_case(&id)) {
        log::warn!("drawing on a different GPU from the compositor's");
    }
    app.gpu = Some(gpu);

    let surface = compositor.create_surface(&qh);
    if let Some(fractional) = &fractional {
        fractional.get_fractional_scale(&surface, &qh, ScaleData);
    }
    app.viewport = viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, Ignore));

    // GENOMA_OUTPUT names the output (niri's connector name); without it the
    // compositor chooses.
    let output = std::env::var("GENOMA_OUTPUT").ok().and_then(|name| {
        app.outputs.outputs().find(|o| {
            app.outputs.info(o).and_then(|info| info.name).as_deref() == Some(name.as_str())
        })
    });
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Top,
        Some("genoma"),
        output.as_ref(),
    );
    layer.set_anchor(Anchor::TOP);
    layer.set_margin(MARGIN_TOP, 0, 0, 0);
    layer.set_size(SURFACE.0, SURFACE.1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit();
    app.layer = Some(layer);

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    WaylandSource::new(conn, queue).insert(event_loop.handle()).expect("wayland source");
    // No timeout: the loop sleeps until the compositor or a service says
    // something. Nothing here wakes it on a schedule.
    event_loop.run(None, &mut app, |_| {}).expect("event loop");
}

struct App {
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    dmabuf: DmabufState,
    main_device: Option<u64>,
    feedback_done: bool,

    gpu: Option<Gpu>,
    target: Option<Target>,
    layer: Option<LayerSurface>,
    viewport: Option<WpViewport>,
    conn: Connection,
    pointer: Option<wl_pointer::WlPointer>,

    logical: (u32, u32),
    scale: f64,
    configured: bool,
    hovered: bool,

    /// Something changed since the last frame.
    dirty: bool,
    /// A frame callback is outstanding: the next draw waits for it.
    frame_pending: bool,
    frames: u64,
    started: Instant,
}

impl App {
    /// A value changed. Draw now if the compositor is not still busy with the
    /// last frame; otherwise the frame callback will.
    fn invalidate(&mut self, qh: &QueueHandle<App>, why: &str) {
        self.dirty = true;
        log::debug!("invalidated: {why}");
        if self.configured && !self.frame_pending {
            self.draw(qh);
        }
    }

    fn draw(&mut self, qh: &QueueHandle<App>) {
        let (Some(layer), Some(gpu)) = (&self.layer, &mut self.gpu) else { return };
        let surface = layer.wl_surface();

        if self.target.is_none() {
            let display = NonNull::new(self.conn.backend().display_ptr() as *mut c_void);
            let wl = NonNull::new(surface.id().as_ptr() as *mut c_void);
            // Safety: the display lives as long as the connection, and the
            // surface as long as the layer surface; both outlive the target.
            self.target = Some(unsafe { gpu.target(display.unwrap(), wl.unwrap()) }.expect("target"));
        }

        let width = (self.logical.0 as f64 * self.scale).round() as u32;
        let height = (self.logical.1 as f64 * self.scale).round() as u32;
        match &self.viewport {
            Some(viewport) => viewport.set_destination(self.logical.0 as i32, self.logical.1 as i32),
            None => surface.set_buffer_scale(self.scale.ceil() as i32),
        }

        // Bioma's rim: 1 logical unit, rounded to the physical pixel, never
        // under 2 physical pixels (IMPLEMENTATION.md, Metrics.rim()).
        let rim = (self.scale.round() as f32).max(2.0);
        let inset = (CAPSULE_INSET * self.scale) as f32;
        let size = [width as f32 - 2.0 * inset, height as f32 - 2.0 * inset];
        let capsule = Rect {
            origin: [inset, inset],
            size,
            radius: size[1] / 2.0,
            rim,
            _pad: [0.0; 2],
            fill: rgb(0x232a1e, 0.72),
            rim_top: rgb(if self.hovered { 0xb9de6b } else { 0x8ba86e }, 1.0),
            rim_bottom: rgb(0x4f6340, 1.0),
        };

        // The callback is asked for with this frame's commit, so the next
        // change waits for the compositor instead of racing it.
        surface.frame(qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
        gpu.draw(self.target.as_mut().unwrap(), width, height, &[capsule]);
        self.dirty = false;
        self.frames += 1;
        log::info!(
            "frame {} at {:.1} s: {}×{} px, scale {}, hovered {}",
            self.frames,
            self.started.elapsed().as_secs_f64(),
            width,
            height,
            self.scale,
            self.hovered
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
        qh: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let scale = scale as f64 / 120.0;
            if scale != app.scale {
                app.scale = scale;
                app.invalidate(qh, "scale");
            }
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        // Only without fractional scale, which already said it more exactly.
        if self.viewport.is_none() && factor as f64 != self.scale {
            self.scale = factor as f64;
            self.invalidate(qh, "integer scale");
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.dirty {
            self.draw(qh);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        std::process::exit(0);
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if w > 0 && h > 0 {
            self.logical = (w, h);
        }
        self.configured = true;
        // A configure must be answered with a buffer, frame callback or not.
        self.frame_pending = false;
        self.invalidate(qh, "configure");
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let hovered = match event.kind {
                PointerEventKind::Enter { .. } => true,
                PointerEventKind::Leave { .. } => false,
                _ => continue,
            };
            if hovered != self.hovered {
                self.hovered = hovered;
                self.invalidate(qh, "hover");
            }
        }
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

    fn created(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _: wl_buffer::WlBuffer,
    ) {
    }

    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
    }

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
