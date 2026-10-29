//! The surfaces on the compositor: one layer surface per output the
//! interface asks for, each at its output's fractional scale, each drawn only
//! when its own scene says something changed.

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
use genoma_script::{Input, SurfaceSpec, Target};
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
            },
            wp::{
                fractional_scale::v1::client::{
                    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
                    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
                },
                linux_dmabuf::zv1::client::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1},
                viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
            },
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{AxisScroll, PointerEvent, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};

/// One layer surface and everything about it that is its own.
struct Surface {
    id: SurfaceId,
    /// Which of the interface's surfaces it is (its place in the list).
    spec: usize,
    /// The output it was made for, by name; `None` for the compositor's
    /// choice.
    output: Option<String>,
    scene: Rc<RefCell<Scene>>,
    layer: LayerSurface,
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
    /// The blur and input regions last sent, as rectangles, so each is set
    /// only when it changes; the inputs `None` until the first is sent,
    /// because Wayland's default is the whole surface.
    blurred: Vec<[i32; 4]>,
    inputs: Option<Vec<[i32; 4]>>,
    /// What is cut out of a catching surface's input: the other surfaces'
    /// inputs on its output, as last sent.
    holes: Vec<[i32; 4]>,
    /// Whether the surface last asked for the keyboard.
    keyboard: bool,
    hovered: Option<NodeId>,
    pressed: Option<NodeId>,
    /// The touchpad's recent movement: when, and how far, for the fling.
    strokes: Vec<(Instant, f64, f64)>,
    /// The surface covers its output: a cell is open, or closing.
    tall: bool,
    /// The output's logical size.
    output_size: Option<(u32, u32)>,
    /// The timer that wakes the surface for the next beat, if one is set.
    wake: Option<RegistrationToken>,
    /// The surface as assistive technology sees it, checked at most every
    /// quarter second: motion never changes it, so it is not looked at per
    /// frame.
    access: crate::access::Access,
    access_checked: Option<Instant>,
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
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// A seat whose keyboard arrived before the loop did: the keyboard is
    /// taken with key repeat, which needs the loop.
    keyboard_seat: Option<wl_seat::WlSeat>,
    /// The modifiers held now, for the keys a field reads them with.
    modifiers: Modifiers,
    /// Where pointer and keyboard input goes: the script's dispatcher, by
    /// the index of the surface's tree.
    input: Option<Box<dyn FnMut(usize, NodeId, Input)>>,
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
            keyboard: None,
            keyboard_seat: None,
            modifiers: Modifiers::default(),
            input: None,
            outputs_changed: false,
            started: Instant::now(),
            toplevel_list: None,
            toplevels: None,
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
        self.surfaces.iter().map(|s| (s.spec, s.output.clone(), s.scene.clone(), s.output_size)).collect()
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
        let layer = match spec.layer.as_str() {
            "background" => Layer::Background,
            "bottom" => Layer::Bottom,
            "overlay" => Layer::Overlay,
            _ => Layer::Top,
        };
        let layer = self.layer_shell.create_layer_surface(&qh, wl, layer, Some(spec.namespace.clone()), output.as_ref());
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        self.surfaces.push(Surface {
            id,
            spec: spec_index,
            output: output_name,
            scene: Rc::new(RefCell::new(Scene::new(fonts))),
            layer,
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
            blurred: Vec::new(),
            inputs: None,
            holes: Vec::new(),
            keyboard: false,
            hovered: None,
            pressed: None,
            strokes: Vec::new(),
            tall: false,
            output_size,
            wake: None,
            access: crate::access::Access::new(),
            access_checked: None,
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
        let s = &mut self.surfaces[i];
        if full {
            if let Some((ow, oh)) = s.output_size {
                return (if spans { 0 } else { ow }, oh);
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
        s.layer.set_anchor(anchor);
        let [top, right, bottom, left] = spec.margin;
        s.layer.set_margin(top, right, bottom, left);
        s.layer.set_size(wanted.0, wanted.1);
        // The reserved strip stays the membrane's, however tall the surface
        // grows while a cell is open.
        match spec.reserve_strip {
            Some(strip) => s.layer.set_exclusive_zone(if spec.reserve { strip } else { 0 }),
            None if !s.tall => s.layer.set_exclusive_zone(if spec.reserve { wanted.1 as i32 } else { 0 }),
            None => {}
        }
        s.keyboard = s.scene.borrow().catcher().is_some();
        s.layer.set_keyboard_interactivity(if s.keyboard {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        s.layer.commit();
        s.dirty = true;
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
        let surface = s.layer.wl_surface().clone();
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

        // The blur region follows the shapes, and is part of this commit.
        let mut shapes = Vec::new();
        for b in s.scene.borrow().blur_shapes() {
            match b {
                Blur::Rounded(b) => {
                    let [x, y, w, h, r] = b.map(|v| v.round() as i32);
                    rounded(&mut shapes, x, y, w, h, r);
                }
                Blur::Outline(points) => outline(&mut shapes, points),
            }
        }
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
            s.layer.set_keyboard_interactivity(if catching {
                KeyboardInteractivity::Exclusive
            } else {
                KeyboardInteractivity::None
            });
            s.keyboard = catching;
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
        self.surfaces.iter().position(|s| s.layer.wl_surface() == wl)
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
        let key = genoma_script::Key { name, text, ctrl: m.ctrl, shift: m.shift, alt: m.alt };
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

/// A convex outline as rows one logical unit tall: each row spans where its
/// middle crosses the outline's edges.
fn outline(region: &mut Vec<[i32; 4]>, points: &[[f32; 2]]) {
    let top = points.iter().map(|p| p[1]).fold(f32::MAX, f32::min).round() as i32;
    let bottom = points.iter().map(|p| p[1]).fold(f32::MIN, f32::max).round() as i32;
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
            if let Some(renderer) = &self.renderer {
                renderer.destroy_target(s.id);
            }
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(i) = self.by_wl(layer.wl_surface()) else { return };
        let s = &mut self.surfaces[i];
        let (w, h) = configure.new_size;
        s.logical = if w > 0 && h > 0 { (w, h) } else { s.wanted };
        s.configured = true;
        // A configure must be answered with a buffer, frame callback or not.
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
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let _ = qh;
            self.take_keyboard(&seat);
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
            match event.kind {
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
                        self.deliver(i, node, Input::Press(button, lx, ly));
                    }
                }
                K::Release { button, .. } => {
                    if let Some(node) = hit {
                        let (lx, ly) = scene.borrow().local(node, x, y);
                        self.deliver(i, node, Input::Release(button, lx, ly));
                        if self.surfaces[i].pressed == Some(node) {
                            self.deliver(i, node, Input::Click(button, lx, ly));
                        }
                    }
                    self.surfaces[i].pressed = None;
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

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if self.specs.iter().any(|s| s.all_outputs || s.outputs.is_some()) {
            self.outputs_changed = true;
        }
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if self.specs.iter().any(|s| s.all_outputs || s.outputs.is_some()) {
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
