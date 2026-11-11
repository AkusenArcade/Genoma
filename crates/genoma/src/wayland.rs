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
    registry::SimpleGlobal,
    reexports::protocols::{
        wp::text_input::zv3::client::{
            zwp_text_input_manager_v3::ZwpTextInputManagerV3,
            zwp_text_input_v3::{self, ZwpTextInputV3},
        },
        xdg::shell::client::xdg_wm_base::XdgWmBase,
    },
};
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
            Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            generic::Generic,
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
                data_control::v1::client::{
                    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
                    ext_data_control_manager_v1::ExtDataControlManagerV1,
                    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
                    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
                },
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
                idle_inhibit::zv1::client::{
                    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
                },
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
        xdg::{
            XdgPositioner, XdgShell, XdgSurface,
            popup::{Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
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
    /// Beside a node of a layer surface, over everything, while its
    /// description is present: `None` while closed. Each opening is a new
    /// Wayland surface, the scene and its tree staying.
    Popup(Option<PopupRole>),
    /// A window the compositor manages, while its description is present:
    /// `None` while closed, the scene and its tree staying, as a popup's.
    Window(Option<WindowRole>),
}

/// An open window's own Wayland objects, and what its last configure said.
struct WindowRole {
    window: Window,
    fraction: Option<WpFractionalScaleV1>,
    title: String,
    maximized: bool,
}

/// What a popup opens from: a layer surface, or a window.
#[derive(Clone)]
enum PopupParent {
    Layer(LayerSurface),
    Window(Window),
}

/// An open popup's own Wayland objects.
struct PopupRole {
    popup: Popup,
    fraction: Option<WpFractionalScaleV1>,
    /// The layer surface it opened from, and the anchor's rectangle there
    /// as the positioner was last given it.
    parent: SurfaceId,
    anchor: [i32; 4],
}

impl Role {
    fn wl_surface(&self) -> Option<&wl_surface::WlSurface> {
        match self {
            Role::Layer(layer) => Some(layer.wl_surface()),
            Role::Lock(lock) => Some(lock.wl_surface()),
            Role::Popup(open) => open.as_ref().map(|p| p.popup.wl_surface()),
            Role::Window(open) => open.as_ref().map(|w| w.window.wl_surface()),
        }
    }

    fn layer(&self) -> Option<&LayerSurface> {
        match self {
            Role::Layer(layer) => Some(layer),
            Role::Lock(_) | Role::Popup(_) | Role::Window(_) => None,
        }
    }

    fn is_popup(&self) -> bool {
        matches!(self, Role::Popup(_))
    }

    fn is_window(&self) -> bool {
        matches!(self, Role::Window(_))
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
    /// The last frame's time on the main thread, for the overlay.
    built_ms: f32,
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
    /// The session kept awake while this surface is seen.
    idle_inhibitor: Option<ZwpIdleInhibitorV1>,
    /// Whether the surface last asked for the keyboard.
    keyboard: bool,
    /// What the pointer is within: the node hit, then each ancestor, as
    /// they were when it arrived (a node removed since keeps its place).
    hovered: Vec<NodeId>,
    /// The button the keyboard's focus is on, if any.
    focused: Option<NodeId>,
    /// The node the press landed on, and where that node was then: a drag
    /// is told relative to it. A node carried by its own drag moves before
    /// the frame lays it out again, and several motions arrive in one frame;
    /// measured from where it is now, each motion counted the ones before it
    /// again, and a carried organism ran away from the pointer faster and
    /// faster, until its blur region overflowed niri's i32 (an abort).
    pressed: Option<NodeId>,
    pressed_at: (f32, f32),
    /// Where the pointer was pressed, surface coordinates: a drag starts
    /// once it has gone a few units from there.
    press_point: (f32, f32),
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
    /// A popup the compositor closed: it stays closed until its
    /// description lets it go and asks for it again.
    dismissed: bool,
    /// A popup's anchor as its function gives it now.
    anchor_wish: Option<String>,
    /// A window's title as its function gives it now.
    title_wish: Option<String>,
    /// When a window's title bar was last pressed: twice soon maximizes.
    title_pressed: Option<Instant>,
    /// A field has the keys while nothing catches: the surface takes the
    /// keyboard when pressed (on demand).
    typing: bool,
}

impl Surface {
    /// A surface as it is made: nothing configured, drawn or pressed yet.
    #[allow(clippy::too_many_arguments)]
    fn blank(
        id: SurfaceId,
        spec: usize,
        output: Option<String>,
        scene: Rc<RefCell<Scene>>,
        role: Role,
        viewport: Option<WpViewport>,
        effect: Option<ExtBackgroundEffectSurfaceV1>,
        output_size: Option<(u32, u32)>,
    ) -> Surface {
        Surface {
            id,
            spec,
            output,
            scene,
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
            built_ms: 0.0,
            blurred: Vec::new(),
            inputs: None,
            holes: Vec::new(),
            inhibitor: None,
            idle_inhibitor: None,
            keyboard: false,
            hovered: Vec::new(),
            focused: None,
            pressed: None,
            pressed_at: (0.0, 0.0),
            press_point: (0.0, 0.0),
            asleep: false,
            reserve_wish: None,
            dismissed: false,
            anchor_wish: None,
            title_wish: None,
            title_pressed: None,
            typing: false,
            strokes: Vec::new(),
            tall: false,
            output_size,
            area: None,
            built: None,
            wake: None,
            access: crate::access::Access::new(),
            access_checked: None,
            held: None,
        }
    }
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
    /// The profiling overlay (`genoma ipc genoma profile on`, or
    /// GENOMA_OVERLAY=1): each surface's frame times, what it drew again,
    /// and the textures kept, over every surface.
    overlay: bool,
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
    /// `idle-inhibit`: the session stays awake while a node asks it.
    idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    /// Drag and drop (wl_data_device): the manager, the seat's device, a drag
    /// over one of our surfaces now, and our own drag's source with what it
    /// gives by type.
    dnd_manager: Option<smithay_client_toolkit::data_device_manager::DataDeviceManagerState>,
    dnd_device: Option<smithay_client_toolkit::data_device_manager::data_device::DataDevice>,
    dnd: Option<DragOver>,
    drag_source: Option<(smithay_client_toolkit::data_device_manager::data_source::DragSource, Offered)>,
    /// xdg-activation: tokens for the programs the interface starts.
    activation: Option<smithay_client_toolkit::activation::ActivationState>,
    activation_requests: Option<genoma_script::ActivationQueue>,
    tokens: Vec<(u64, Option<String>)>,
    /// The surface of the last press or key: what a token is asked from.
    input_surface: Option<wl_surface::WlSurface>,
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
    focus: Option<genoma_script::FocusQueue>,
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
    /// `@genoma/compositor` through ext-workspace and wlr-foreign-toplevel,
    /// where no compositor IPC is known.
    wm: Option<wm::Wm>,
    /// `@genoma/displays` through wlr-output-management.
    dm: Option<displays::Dm>,
    /// `@genoma/gamma` through wlr-gamma-control.
    gm: Option<gamma::Gm>,
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
    /// `ext-data-control`, bound once an interface requires
    /// `@genoma/clipboard`: the manager, the seat's device, the offer on the
    /// clipboard now, and the service they fill.
    data_control: Option<ExtDataControlManagerV1>,
    data_device: Option<ExtDataControlDeviceV1>,
    clip_offer: Option<ExtDataControlOfferV1>,
    clipboard: Option<std::rc::Rc<genoma_services::clipboard::Clipboard>>,
    /// The clipboard service, started by the first field that copies or
    /// pastes when the interface has not required it.
    clipboard_service: Option<std::rc::Rc<genoma_services::clipboard::Clipboard>>,
    /// `xdg-shell`, for popups and windows (with the decoration manager,
    /// where the compositor has one).
    xdg_shell: Option<XdgShell>,
    /// The serial of the last press or key on one of the shell's surfaces:
    /// a popup takes the keyboard with it, and only in answer to one.
    input_serial: u32,
    /// The surface the keyboard is on, as the compositor says.
    keyboard_on: Option<wl_surface::WlSurface>,
    /// Popups the compositor closed, by description and output, for the
    /// interface to hear.
    dismissed: Vec<(usize, Option<String>)>,
    /// `text-input-v3`: an input method composing into the field that has
    /// the keys (CJK, emoji, dictation).
    text_input_manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    /// The surface the text input entered, whether it is enabled, the
    /// state last told to it, and what the input method sent before its
    /// next `done`.
    text_input_on: Option<wl_surface::WlSurface>,
    text_input_enabled: bool,
    text_input_told: Option<(String, usize, usize, [i32; 4])>,
    text_input_pending: TextInputPending,
    /// The last change came from the input method (said with the next state).
    text_input_cause: bool,
}

/// What an input method sends, applied all at once on its `done`.
#[derive(Default)]
struct TextInputPending {
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
    delete: Option<(u32, u32)>,
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
            overlay: std::env::var_os("GENOMA_OVERLAY").is_some(),
            specs: Vec::new(),
            surfaces: Vec::new(),
            next_id: 1,
            handle: None,
            pointer: None,
            cursor_shapes: CursorShapeManager::bind(&globals, &qh).ok(),
            shortcuts_inhibit: globals.bind(&qh, 1..=1, Ignore).ok(),
            idle_inhibit: globals.bind(&qh, 1..=1, Ignore).ok(),
            seat: None,
            cursor_device: None,
            cursor_serial: 0,
            cursor_shape: None,
            keyboard: None,
            keyboard_seat: None,
            modifiers: Modifiers::default(),
            input: None,
            focus: None,
            drag: None,
            areas_changed: Vec::new(),
            outputs_changed: false,
            started: Instant::now(),
            toplevel_list: None,
            toplevels: None,
            wm: None,
            dm: None,
            gm: None,
            idle_notifier: None,
            idle: None,
            idle_notifications: std::collections::HashMap::new(),
            lock_state: None,
            lock: None,
            session_lock: None,
            data_control: None,
            data_device: None,
            clip_offer: None,
            clipboard: None,
            clipboard_service: None,
            xdg_shell: None,
            input_serial: 0,
            keyboard_on: None,
            activation: None,
            activation_requests: None,
            dnd_manager: None,
            dnd_device: None,
            dnd: None,
            drag_source: None,
            tokens: Vec::new(),
            input_surface: None,
            dismissed: Vec::new(),
            text_input_manager: None,
            text_input: None,
            text_input_on: None,
            text_input_enabled: false,
            text_input_told: None,
            text_input_pending: TextInputPending::default(),
            text_input_cause: false,
            globals,
        };
        app.xdg_shell = XdgShell::bind(&app.globals, &app.qh).ok();
        app.activation = smithay_client_toolkit::activation::ActivationState::bind(&app.globals, &app.qh).ok();
        app.dnd_manager = smithay_client_toolkit::data_device_manager::DataDeviceManagerState::bind(&app.globals, &app.qh).ok();
        app.text_input_manager = app.globals.bind(&app.qh, 1..=1, Ignore).ok();
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

    /// Binds `ext-data-control` and follows the selection from now on:
    /// called once an interface requires the clipboard. A compositor
    /// without the protocol leaves it empty and `available` false.
    pub fn start_clipboard(&mut self, clipboard: std::rc::Rc<genoma_services::clipboard::Clipboard>) {
        if self.clipboard.is_some() {
            return;
        }
        self.data_control = self.globals.bind(&self.qh, 1..=1, Ignore).ok();
        clipboard.available.set(self.data_control.is_some());
        self.clipboard = Some(clipboard);
        self.make_data_device();
    }

    /// The service a field copies and pastes through.
    pub fn set_clipboard_service(&mut self, clipboard: std::rc::Rc<genoma_services::clipboard::Clipboard>) {
        self.clipboard_service = Some(clipboard);
    }

    /// The clipboard followed from now on, for a field: started early, at
    /// its first press or key, its text is known by the time it pastes.
    fn ensure_clipboard(&mut self) {
        if let (None, Some(service)) = (&self.clipboard, self.clipboard_service.clone()) {
            self.start_clipboard(service);
        }
    }

    /// The seat's device, once there is a seat: the compositor then says
    /// what is on the clipboard straight away.
    fn make_data_device(&mut self) {
        if self.data_device.is_some() {
            return;
        }
        let (Some(manager), Some(seat)) = (&self.data_control, self.seats.seats().next()) else { return };
        self.data_device = Some(manager.get_data_device(&seat, &self.qh, DeviceData));
    }

    /// What the interface asked of the clipboard since the last turn.
    pub fn sync_clipboard(&mut self) {
        let Some(clipboard) = self.clipboard.clone() else { return };
        self.make_data_device();
        for request in clipboard.take_requests() {
            match request {
                genoma_services::clipboard::Request::Set { types, data } => self.own_selection(types, data),
                genoma_services::clipboard::Request::Clear => {
                    if let Some(device) = &self.data_device {
                        device.set_selection(None);
                    }
                }
                genoma_services::clipboard::Request::Save { mime, path } => self.save_selection(&mime, &path),
            }
        }
        let _ = self.conn.flush();
    }

    /// The shell's own copy: a source that hands the bytes to whoever
    /// pastes, until somebody else copies.
    fn own_selection(&mut self, types: Vec<String>, data: Vec<u8>) {
        let (Some(manager), Some(device)) = (&self.data_control, &self.data_device) else {
            log::warn!("clipboard: nothing to copy with (no data-control or no seat)");
            return;
        };
        let source = manager.create_data_source(&self.qh, SourceData(std::sync::Arc::new(data)));
        for t in types {
            source.offer(t);
        }
        device.set_selection(Some(&source));
    }

    /// The current copy, in this type, written by its owner into `path`.
    fn save_selection(&mut self, mime: &str, path: &std::path::Path) {
        let Some(offer) = &self.clip_offer else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::File::create(path) {
            Ok(file) => {
                use std::os::fd::AsFd;
                offer.receive(mime.to_owned(), file.as_fd());
                // The descriptor travels with the request: sent, it can close here.
                let _ = self.conn.flush();
            }
            Err(e) => log::warn!("clipboard: {}: {e}", path.display()),
        }
    }

    /// A new selection: its types announced now, its text read through a
    /// pipe on the loop, so a large or slow owner never holds the shell.
    fn selection(&mut self, offer: Option<ExtDataControlOfferV1>) {
        if let Some(old) = self.clip_offer.take() {
            old.destroy();
        }
        let Some(clipboard) = self.clipboard.clone() else {
            if let Some(offer) = offer {
                offer.destroy();
            }
            return;
        };
        let Some(offer) = offer else {
            clipboard.announce(Vec::new());
            return;
        };
        let types = offer.data::<OfferData>().map(|d| d.0.lock().unwrap().clone()).unwrap_or_default();
        clipboard.announce(types.clone());
        if let Some(mime) = genoma_services::clipboard::Clipboard::text_type(&types) {
            self.read_text(&offer, mime, clipboard.changed.get_untracked().unwrap_or(0));
        }
        self.clip_offer = Some(offer);
    }

    fn read_text(&mut self, offer: &ExtDataControlOfferV1, mime: &str, serial: u64) {
        use std::os::fd::AsFd;
        let Some(handle) = self.handle.clone() else { return };
        let (read, write) = match rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) {
            Ok(ends) => ends,
            Err(e) => {
                log::warn!("clipboard: pipe: {e}");
                return;
            }
        };
        offer.receive(mime.to_owned(), write.as_fd());
        let _ = self.conn.flush();
        drop(write);
        // Only the reading end waits for nothing; the writer's stays as the
        // owner expects it.
        if let Err(e) = rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK) {
            log::warn!("clipboard: {e}");
            return;
        }
        let mut text = Vec::new();
        let mut too_long = false;
        let source = Generic::new(read, Interest::READ, Mode::Level);
        let inserted = handle.insert_source(source, move |_, fd, app: &mut App| {
            let mut chunk = [0u8; 16384];
            loop {
                match rustix::io::read(&**fd, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        // Past the limit the rest is drained, not kept: an
                        // owner whose pipe closes early may take it badly.
                        if !too_long && text.len() + n <= genoma_services::clipboard::TEXT_LIMIT {
                            text.extend_from_slice(&chunk[..n]);
                        } else {
                            too_long = true;
                            text.clear();
                        }
                    }
                    Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => break,
                }
            }
            if let Some(clipboard) = &app.clipboard {
                // Only if nobody has copied since.
                if !too_long && clipboard.changed.get_untracked() == Some(serial) {
                    clipboard.read(String::from_utf8_lossy(&text).into_owned());
                }
            }
            Ok(PostAction::Remove)
        });
        if let Err(e) = inserted {
            log::warn!("clipboard: {e}");
        }
    }

    /// Each popup as its description wants it now: opened once what it
    /// opens from is on screen, closed when let go, moved when its anchor
    /// moved or its content changed size.
    fn sync_popups(&mut self) {
        for i in 0..self.surfaces.len() {
            if self.surfaces[i].role.is_window() {
                self.sync_window(i);
                continue;
            }
            if !self.surfaces[i].role.is_popup() {
                continue;
            }
            let s = &self.surfaces[i];
            let wanted = !s.asleep && !s.dismissed;
            match (&s.role, wanted) {
                (Role::Popup(None), true) => self.open_popup(i),
                (Role::Popup(Some(_)), false) => self.close_popup(i),
                (Role::Popup(Some(p)), true) => {
                    let (parent, anchor) = (p.parent, p.anchor);
                    if self.anchor_of(i).is_some_and(|(j, _, rect)| self.surfaces[j].id == parent && rect != anchor) {
                        self.reposition(i);
                    }
                }
                _ => {}
            }
        }
    }

    /// Where popup `i` opens from: the layer surface on its output (any,
    /// for a popup with none), or the window, holding the node its anchor
    /// names, as last drawn, and that node's rectangle there.
    fn anchor_of(&self, i: usize) -> Option<(usize, PopupParent, [i32; 4])> {
        let spec = self.specs.get(self.surfaces[i].spec)?.popup.as_ref()?;
        let id = spec.anchor.as_ref().or(self.surfaces[i].anchor_wish.as_ref())?;
        let output = &self.surfaces[i].output;
        self.surfaces.iter().enumerate().find_map(|(j, p)| {
            let parent = match &p.role {
                Role::Layer(layer) => PopupParent::Layer(layer.clone()),
                Role::Window(Some(open)) => PopupParent::Window(open.window.clone()),
                _ => return None,
            };
            if j == i || !p.configured || p.asleep || p.frames == 0 || (output.is_some() && &p.output != output) {
                return None;
            }
            let scene = p.scene.borrow();
            let [x, y, w, h] = scene.extent(scene.find(id)?)?;
            Some((j, parent, [x.floor() as i32, y.floor() as i32, w.ceil().max(1.0) as i32, h.ceil().max(1.0) as i32]))
        })
    }

    /// The positioner for popup `i` at `anchor`: opened on its side of the
    /// node, aligned along it, the gap kept as part of the anchor so that a
    /// popup turned to the other side keeps it too; turned when it would
    /// leave the output, slid along when it still would.
    fn positioner(&self, i: usize, anchor: [i32; 4]) -> Option<XdgPositioner> {
        use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{
            Anchor as A, ConstraintAdjustment as C, Gravity as G,
        };
        let spec = self.specs.get(self.surfaces[i].spec)?.popup.as_ref()?;
        let base = SimpleGlobal::<XdgWmBase, 6>::from_bound(self.xdg_shell.as_ref()?.xdg_wm_base().clone());
        let positioner = XdgPositioner::new(&base).ok()?;
        let (w, h) = self.surfaces[i].wanted;
        positioner.set_size(w.max(1) as i32, h.max(1) as i32);
        let [mut x, mut y, mut aw, mut ah] = anchor;
        let gap = spec.gap.max(0);
        let vertical = matches!(spec.side.as_str(), "bottom" | "top");
        if vertical {
            y -= gap;
            ah += 2 * gap;
        } else {
            x -= gap;
            aw += 2 * gap;
        }
        positioner.set_anchor_rect(x, y, aw, ah);
        let (a, g) = match (spec.side.as_str(), spec.align.as_str()) {
            ("bottom", "start") => (A::BottomLeft, G::BottomRight),
            ("bottom", "end") => (A::BottomRight, G::BottomLeft),
            ("bottom", _) => (A::Bottom, G::Bottom),
            ("top", "start") => (A::TopLeft, G::TopRight),
            ("top", "end") => (A::TopRight, G::TopLeft),
            ("top", _) => (A::Top, G::Top),
            ("right", "start") => (A::TopRight, G::BottomRight),
            ("right", "end") => (A::BottomRight, G::TopRight),
            ("right", _) => (A::Right, G::Right),
            ("left", "start") => (A::TopLeft, G::BottomLeft),
            ("left", "end") => (A::BottomLeft, G::TopLeft),
            (_, _) => (A::Left, G::Left),
        };
        positioner.set_anchor(a);
        positioner.set_gravity(g);
        positioner.set_constraint_adjustment(if vertical {
            C::FlipY | C::SlideX | C::SlideY
        } else {
            C::FlipX | C::SlideX | C::SlideY
        });
        if positioner.version() >= 3 {
            positioner.set_reactive();
        }
        Some(positioner)
    }

    fn open_popup(&mut self, i: usize) {
        let Some(spec) = self.specs.get(self.surfaces[i].spec).and_then(|s| s.popup.clone()) else { return };
        let Some(base) = self.xdg_shell.as_ref().map(|s| SimpleGlobal::<XdgWmBase, 5>::from_bound(s.xdg_wm_base().clone())) else {
            log::warn!("g.popup: the compositor offers no xdg-shell");
            self.surfaces[i].dismissed = true;
            return;
        };
        // Until what it opens from is drawn: tried again after the next
        // batch of events.
        let Some((j, parent, anchor)) = self.anchor_of(i) else { return };
        self.surfaces[i].wanted = self.measure(i);
        let Some(positioner) = self.positioner(i, anchor) else { return };
        let qh = self.qh.clone();
        let id = self.surfaces[i].id;
        let wl = self.compositor.create_surface(&qh);
        let fraction = self.fractional.as_ref().map(|f| f.get_fractional_scale(&wl, &qh, ScaleData(id)));
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl, &qh, Ignore));
        let effect = self.effects.get_background_effect(&wl, &qh).ok();
        // A window's popup is its xdg surface's child; a layer surface's
        // is given its parent once made.
        let xdg_parent = match &parent {
            PopupParent::Window(window) => Some(window.xdg_surface().clone()),
            PopupParent::Layer(_) => None,
        };
        let popup = match Popup::from_surface(xdg_parent.as_ref(), &positioner, &qh, wl, &base) {
            Ok(popup) => popup,
            Err(e) => {
                log::warn!("g.popup: {e}");
                self.surfaces[i].dismissed = true;
                return;
            }
        };
        // The keyboard, and closing on a press elsewhere, only in answer to
        // a press or a key on the shell: a popup opened by anything else
        // (a timer, a call from outside) is shown without a grab. The
        // compositor gives a grabbing popup the keyboard only if what it
        // opened from could take it: the layer surface may, while it is open.
        let seat = self.seat.clone().or_else(|| self.seats.seats().next());
        let grab = spec.grab && self.input_serial != 0 && seat.is_some();
        // A window takes the keyboard by the compositor's focus, as it is.
        if let PopupParent::Layer(layer) = &parent {
            if grab && !self.surfaces[j].keyboard && !self.surfaces[j].typing {
                layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                layer.commit();
            }
            layer.get_popup(popup.xdg_popup());
        }
        if let (true, Some(seat)) = (grab, seat) {
            popup.xdg_popup().grab(&seat, self.input_serial);
        }
        popup.wl_surface().commit();
        let scale = self.surfaces[j].scale;
        let parent = self.surfaces[j].id;
        let s = &mut self.surfaces[i];
        s.role = Role::Popup(Some(PopupRole { popup, fraction, parent, anchor }));
        s.viewport = viewport;
        s.effect = effect;
        s.scale = scale;
        s.configured = false;
        s.target_made = false;
        s.frame_pending = false;
        s.frames = 0;
        s.blurred.clear();
        s.inputs = None;
        s.holes.clear();
        log::debug!("popup {id}: open beside {anchor:?} on surface {parent}");
    }

    /// Closes popup `i`: its renderer target first, then its Wayland
    /// objects, its scene and tree staying for the next opening.
    fn close_popup(&mut self, i: usize) {
        let s = &mut self.surfaces[i];
        let Role::Popup(Some(open)) = std::mem::replace(&mut s.role, Role::Popup(None)) else { return };
        if s.target_made {
            if let Some(renderer) = &self.renderer {
                renderer.destroy_target_now(s.id);
            }
            s.target_made = false;
        }
        if let Some(viewport) = s.viewport.take() {
            viewport.destroy();
        }
        if let Some(effect) = s.effect.take() {
            effect.destroy();
        }
        if let Some(inhibitor) = s.inhibitor.take() {
            inhibitor.destroy();
        }
        if let Some(inhibitor) = s.idle_inhibitor.take() {
            inhibitor.destroy();
        }
        if let Some(fraction) = open.fraction {
            fraction.destroy();
        }
        if let (Some(wake), Some(handle)) = (s.wake.take(), &self.handle) {
            handle.remove(wake);
        }
        if self.keyboard_on.as_ref() == Some(open.popup.wl_surface()) {
            self.keyboard_on = None;
        }
        // What it opened from takes the keyboard again only if it catches,
        // and says so before the popup goes: the compositor would otherwise
        // hand it the keyboard the popup leaves, away from the window that
        // had it before.
        let parent = self.surfaces.iter().find(|p| p.id == open.parent);
        if let Some(layer) = parent.filter(|p| !p.keyboard && !p.typing).and_then(|p| p.role.layer()) {
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
        }
        // The popup, its xdg surface, then its surface.
        drop(open.popup);
        let s = &mut self.surfaces[i];
        s.configured = false;
        s.frame_pending = false;
        s.pressed = None;
        let hovered = std::mem::take(&mut s.hovered);
        let focused = s.focused.take();
        let id = s.id;
        if self.drag.is_some_and(|d| d.0 == i) {
            self.drag = None;
        }
        // What the pointer was over is left, as on a leave.
        for node in hovered {
            self.deliver(i, node, Input::Hover(false));
        }
        if let Some(node) = focused {
            self.deliver(i, node, Input::Focus(false));
        }
        let _ = self.conn.flush();
        log::debug!("popup {id}: closed");
    }

    /// Popup `i` placed again (its anchor moved, its content changed
    /// size): moved by the compositor where it can, opened again where not.
    fn reposition(&mut self, i: usize) {
        let Role::Popup(Some(open)) = &self.surfaces[i].role else { return };
        let parent = open.parent;
        let Some((j, _, anchor)) = self.anchor_of(i) else { return };
        if self.surfaces[j].id != parent {
            self.close_popup(i);
            return;
        }
        let Some(positioner) = self.positioner(i, anchor) else { return };
        let Role::Popup(Some(open)) = &mut self.surfaces[i].role else { return };
        if open.popup.xdg_popup().version() >= 3 {
            open.anchor = anchor;
            open.popup.reposition(&positioner, 0);
            open.popup.wl_surface().commit();
        } else {
            // Opened again, at once, where it is now: without the grab.
            self.close_popup(i);
            self.open_popup(i);
        }
    }

    /// Window `i` as its description wants it now: opened, closed, or its
    /// title changed.
    fn sync_window(&mut self, i: usize) {
        let s = &self.surfaces[i];
        let wanted = !s.asleep && !s.dismissed;
        match (&s.role, wanted) {
            (Role::Window(None), true) => self.open_window(i),
            (Role::Window(Some(_)), false) => self.close_window(i),
            (Role::Window(Some(open)), true) => {
                let title = self.title_of(i);
                if open.title != title {
                    open.window.set_title(title.clone());
                    if let Role::Window(Some(open)) = &mut self.surfaces[i].role {
                        open.title = title;
                    }
                }
            }
            _ => {}
        }
    }

    /// A window's title: its function's, else its description's.
    fn title_of(&self, i: usize) -> String {
        let s = &self.surfaces[i];
        s.title_wish
            .clone()
            .or_else(|| self.specs.get(s.spec).and_then(|spec| spec.window.as_ref()).and_then(|w| w.title.clone()))
            .unwrap_or_default()
    }

    fn open_window(&mut self, i: usize) {
        let Some(spec) = self.specs.get(self.surfaces[i].spec).and_then(|s| s.window.clone()) else { return };
        let Some(shell) = &self.xdg_shell else {
            log::warn!("g.window: the compositor offers no xdg-shell");
            self.surfaces[i].dismissed = true;
            return;
        };
        let qh = self.qh.clone();
        let id = self.surfaces[i].id;
        let wl = self.compositor.create_surface(&qh);
        let fraction = self.fractional.as_ref().map(|f| f.get_fractional_scale(&wl, &qh, ScaleData(id)));
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl, &qh, Ignore));
        let effect = self.effects.get_background_effect(&wl, &qh).ok();
        // Decorated by the compositor where it decorates; where it does not
        // (niri draws its border and focus ring only), the interface's own
        // title bar is what moves it (`move_window`).
        let window = shell.create_window(wl, WindowDecorations::RequestServer, &qh);
        let title = self.title_of(i);
        window.set_title(title.clone());
        window.set_app_id(spec.app_id.clone());
        window.set_min_size(spec.min);
        window.set_max_size(spec.max);
        // The first commit carries no buffer: the compositor answers it with
        // the first configure, and the size it gives.
        window.commit();
        let s = &mut self.surfaces[i];
        s.role = Role::Window(Some(WindowRole { window, fraction, title, maximized: false }));
        s.viewport = viewport;
        s.effect = effect;
        s.logical = spec.size;
        s.wanted = spec.size;
        s.configured = false;
        s.target_made = false;
        s.frame_pending = false;
        s.frames = 0;
        s.blurred.clear();
        s.inputs = None;
        s.holes.clear();
        log::debug!("window {id}: open, {}×{} asked", spec.size.0, spec.size.1);
    }

    /// Closes window `i`: its renderer target first, then its Wayland
    /// objects, its scene and tree staying for the next opening.
    fn close_window(&mut self, i: usize) {
        // Its popups first: a parent goes after its children.
        let id = self.surfaces[i].id;
        for j in 0..self.surfaces.len() {
            if matches!(&self.surfaces[j].role, Role::Popup(Some(p)) if p.parent == id) {
                self.close_popup(j);
            }
        }
        let s = &mut self.surfaces[i];
        let Role::Window(Some(open)) = std::mem::replace(&mut s.role, Role::Window(None)) else { return };
        if s.target_made {
            if let Some(renderer) = &self.renderer {
                renderer.destroy_target_now(s.id);
            }
            s.target_made = false;
        }
        if let Some(viewport) = s.viewport.take() {
            viewport.destroy();
        }
        if let Some(effect) = s.effect.take() {
            effect.destroy();
        }
        if let Some(inhibitor) = s.inhibitor.take() {
            inhibitor.destroy();
        }
        if let Some(inhibitor) = s.idle_inhibitor.take() {
            inhibitor.destroy();
        }
        if let Some(fraction) = open.fraction {
            fraction.destroy();
        }
        if let (Some(wake), Some(handle)) = (s.wake.take(), &self.handle) {
            handle.remove(wake);
        }
        if self.keyboard_on.as_ref() == Some(open.window.wl_surface()) {
            self.keyboard_on = None;
        }
        // The toplevel, its xdg surface, then its surface.
        drop(open.window);
        let s = &mut self.surfaces[i];
        s.configured = false;
        s.frame_pending = false;
        s.pressed = None;
        let hovered = std::mem::take(&mut s.hovered);
        let focused = s.focused.take();
        let id = s.id;
        if self.drag.is_some_and(|d| d.0 == i) {
            self.drag = None;
        }
        for node in hovered {
            self.deliver(i, node, Input::Hover(false));
        }
        if let Some(node) = focused {
            self.deliver(i, node, Input::Focus(false));
        }
        let _ = self.conn.flush();
        log::debug!("window {id}: closed");
    }

    /// A press on a `move_window` node of window `i`: the compositor moves
    /// the window with the pointer; a second press soon after maximizes it,
    /// or gives back its size.
    fn press_title(&mut self, i: usize, serial: u32) -> bool {
        let Role::Window(Some(open)) = &self.surfaces[i].role else { return false };
        let Some(seat) = self.seat.clone().or_else(|| self.seats.seats().next()) else { return false };
        let now = Instant::now();
        let twice = self.surfaces[i].title_pressed.is_some_and(|t| now.duration_since(t) < Duration::from_millis(400));
        if twice {
            if open.maximized { open.window.unset_maximized() } else { open.window.set_maximized() }
            self.surfaces[i].title_pressed = None;
        } else {
            open.window.move_(&seat, serial);
            self.surfaces[i].title_pressed = Some(now);
        }
        true
    }

    /// The popups the compositor closed since the last look, by description
    /// and output.
    pub fn take_dismissed(&mut self) -> Vec<(usize, Option<String>)> {
        std::mem::take(&mut self.dismissed)
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

    /// A press on a node that gives something when dragged, moved six units
    /// from where it was pressed: the compositor carries it from here, to
    /// another program or to one of ours. True when it started.
    fn start_drag(&mut self, i: usize, node: NodeId, x: f32, y: f32) -> bool {
        let (px, py) = self.surfaces[i].press_point;
        if (x - px).hypot(y - py) < 6.0 {
            return false;
        }
        let Some((files, text)) = self.surfaces[i].scene.borrow().drag_data(node) else { return false };
        let (Some(manager), Some(device), Some(origin)) =
            (&self.dnd_manager, &self.dnd_device, self.surfaces[i].role.wl_surface().cloned())
        else {
            return false;
        };
        let mut offered: Vec<(String, Vec<u8>)> = Vec::new();
        if !files.is_empty() {
            let list: String = files.iter().map(|f| format!("{}\r\n", file_uri(f))).collect();
            offered.push(("text/uri-list".into(), list.into_bytes()));
        }
        let plain = text.unwrap_or_else(|| files.join("\n"));
        for mime in ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"] {
            offered.push((mime.into(), plain.clone().into_bytes()));
        }
        let source = manager.create_drag_and_drop_source(
            &self.qh,
            offered.iter().map(|(m, _)| m.clone()),
            smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction::Copy,
        );
        source.start_drag(device, &origin, None, self.input_serial);
        let _ = self.conn.flush();
        self.drag_source = Some((source, offered));
        // The compositor has the pointer now: the press ends here, without
        // a click.
        self.surfaces[i].pressed = None;
        let [_, _, w, h] = self.surfaces[i].scene.borrow().extent(node).unwrap_or([0.0; 4]);
        self.deliver(i, node, Input::Release(0x110, w / 2.0, h / 2.0));
        log::debug!("surface {}: drag started", self.surfaces[i].id);
        true
    }

    /// Something dragged is over surface `i` at (x, y): the drop target
    /// under it told, and the offer accepted only over one.
    fn drag_over(&mut self, i: usize, x: f32, y: f32) {
        let target = self.surfaces[i].scene.borrow().drop_target_at(x, y);
        let before = self.dnd.as_ref().filter(|d| d.surface == i).and_then(|d| d.target);
        if before != target {
            if let Some(old) = before {
                self.deliver(i, old, Input::DropHover(false));
            }
            if let Some(new) = target {
                self.deliver(i, new, Input::DropHover(true));
            }
        }
        self.dnd = Some(DragOver { surface: i, target, x, y });
        let Some(offer) = self.dnd_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let mime = target.and_then(|_| offer.with_mime_types(preferred_mime));
        offer.accept_mime_type(offer.serial, mime);
        use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
        if target.is_some() {
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        } else {
            offer.set_actions(DndAction::empty(), DndAction::empty());
        }
    }

    /// The drag left, or was dropped: the target told it is no longer over it.
    fn drag_gone(&mut self) {
        if let Some(DragOver { surface, target: Some(node), .. }) = self.dnd.take()
            && surface < self.surfaces.len()
        {
            self.deliver(surface, node, Input::DropHover(false));
        }
    }

    /// Where `g.activation_token` leaves its requests.
    pub fn on_activation(&mut self, queue: genoma_script::ActivationQueue) {
        self.activation_requests = Some(queue);
    }

    /// The tokens asked for since the last look, sent to the compositor;
    /// with no xdg-activation, answered at once with none.
    fn serve_activation(&mut self) {
        let Some(queue) = &self.activation_requests else { return };
        let asked: Vec<(u64, Option<String>)> = std::mem::take(&mut *queue.borrow_mut());
        for (id, app_id) in asked {
            let Some(activation) = &self.activation else {
                self.tokens.push((id, None));
                continue;
            };
            let seat = self.seat.clone().or_else(|| self.seats.seats().next());
            activation.request_token(
                &self.qh,
                smithay_client_toolkit::activation::RequestData {
                    app_id,
                    seat_and_serial: seat.filter(|_| self.input_serial != 0).map(|s| (s, self.input_serial)),
                    surface: self.input_surface.clone(),
                    udata: id,
                },
            );
        }
    }

    /// The tokens the compositor gave since the last look, by request.
    pub fn take_tokens(&mut self) -> Vec<(u64, Option<String>)> {
        std::mem::take(&mut self.tokens)
    }

    /// Where `g.focus_*` leaves its requests.
    pub fn on_focus(&mut self, queue: genoma_script::FocusQueue) {
        self.focus = Some(queue);
    }

    /// Does what the key just handled asked of the keyboard's focus, on the
    /// surface it went to, inside the cell that is catching.
    fn serve_focus(&mut self, i: usize, catcher: NodeId) {
        use genoma_script::FocusRequest;
        let Some((queue, held)) = self.focus.clone() else { return };
        // A focus left on something gone, or outside the open cell, is let
        // go without a word: there is nobody to tell.
        if let Some(f) = self.surfaces[i].focused {
            let scene = self.surfaces[i].scene.borrow();
            if !scene.within(f, catcher) {
                drop(scene);
                self.surfaces[i].focused = None;
            }
        }
        let requests: Vec<FocusRequest> = std::mem::take(&mut *queue.borrow_mut());
        for request in requests {
            match request {
                FocusRequest::Move(step) => self.focus_step(i, catcher, step),
                FocusRequest::Press => {
                    if let Some(node) = self.surfaces[i].focused {
                        let [_, _, w, h] = self.surfaces[i].scene.borrow().extent(node).unwrap_or([0.0; 4]);
                        self.deliver(i, node, Input::Click(272, w / 2.0, h / 2.0));
                    }
                }
                FocusRequest::Clear => {
                    if let Some(old) = self.surfaces[i].focused.take() {
                        self.deliver(i, old, Input::Focus(false));
                    }
                }
            }
        }
        held.set(self.surfaces[i].focused.is_some());
    }

    /// The keyboard's focus `step` places on among what it can reach in
    /// `within`, round from the end to the start; told to the node left and
    /// the node reached, and to the wells around it (FocusWithin) so they
    /// can bring it into view.
    fn focus_step(&mut self, i: usize, within: NodeId, step: i32) {
        let list = self.surfaces[i].scene.borrow().focusables(within);
        if list.is_empty() {
            return;
        }
        let now = self.surfaces[i].focused.and_then(|f| list.iter().position(|&n| n == f));
        let next = match now {
            Some(at) => (at as i64 + step as i64).rem_euclid(list.len() as i64) as usize,
            None if step < 0 => list.len() - 1,
            None => 0,
        };
        let node = list[next];
        if let Some(old) = self.surfaces[i].focused.take() {
            self.deliver(i, old, Input::Focus(false));
        }
        self.surfaces[i].focused = Some(node);
        self.deliver(i, node, Input::Focus(true));
        let extent = self.surfaces[i].scene.borrow().extent(node);
        if let Some([x, y, w, h]) = extent {
            self.deliver(i, node, Input::FocusWithin(x, y, w, h));
        }
        self.reveal_focused(i, node);
    }

    /// A row of a `g.list` that took the keyboard's focus is scrolled into
    /// view, the least way: the list mounts a few rows past its view, so
    /// the next row is always there for the next step, and the focus walks
    /// the whole list rather than only the rows mounted when it began.
    fn reveal_focused(&mut self, i: usize, node: NodeId) {
        let (list, delta) = {
            let scene = self.surfaces[i].scene.borrow();
            let Some(list) = scene.list_around(node) else { return };
            let (Some([_, y, _, h]), Some([_, ly, _, lh])) = (scene.extent(node), scene.extent(list)) else { return };
            let delta = if y < ly {
                y - ly
            } else if y + h > ly + lh {
                (y + h) - (ly + lh)
            } else {
                return;
            };
            (list, delta)
        };
        self.deliver(i, list, Input::Wheel(0.0, delta as f64, false));
    }

    /// Where the keyboard walks on surface `i`, if it does: the `navigate`
    /// node around the focused one, else around what catches, else the
    /// first one in the surface.
    fn walk_scope(&self, i: usize) -> Option<NodeId> {
        let scene = self.surfaces[i].scene.borrow();
        let from = self.surfaces[i].focused.or_else(|| scene.catcher());
        from.and_then(|n| scene.navigate_scope(n)).or_else(|| scene.first_navigate())
    }

    /// A key inside a `navigate` node: the walk takes Tab, the arrows and
    /// the press keys; the rest goes to the focused node and on up.
    fn walk_key(&mut self, i: usize, scope: NodeId, event: &KeyEvent) {
        let key = self.key_of(event);
        let focused = {
            let scene = self.surfaces[i].scene.borrow();
            self.surfaces[i].focused.filter(|f| scene.within(*f, scope))
        };
        let slider = focused.is_some_and(|f| self.surfaces[i].scene.borrow().is_slider(f));
        match key.name.as_str() {
            "Tab" | "ISO_Left_Tab" => {
                let back = key.shift || key.name == "ISO_Left_Tab";
                self.focus_step(i, scope, if back { -1 } else { 1 });
            }
            "Left" | "Right" if slider => {
                if let Some(f) = focused {
                    self.deliver(i, f, Input::Key(key));
                }
            }
            "Down" | "Right" => self.focus_step(i, scope, 1),
            "Up" | "Left" => self.focus_step(i, scope, -1),
            "Return" | "KP_Enter" | "space" if focused.is_some() => {
                if let Some(f) = focused {
                    let [_, _, w, h] = self.surfaces[i].scene.borrow().extent(f).unwrap_or([0.0; 4]);
                    self.deliver(i, f, Input::Click(272, w / 2.0, h / 2.0));
                }
            }
            _ => {
                // Escape lets the focus go, and is still heard.
                if key.name == "Escape"
                    && let Some(f) = self.surfaces[i].focused.take()
                {
                    self.deliver(i, f, Input::Focus(false));
                }
                self.deliver(i, focused.unwrap_or(scope), Input::Key(key));
            }
        }
        self.serve_focus(i, scope);
        let ring = self.surfaces[i].focused;
        self.surfaces[i].scene.borrow_mut().set_focus_ring(ring);
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
        if let (Some(manager), None) = (&self.text_input_manager, &self.text_input) {
            self.text_input = Some(manager.get_text_input(seat, &qh, TextInputData));
        }
    }

    /// The text input as the field with the keys wants it: enabled on the
    /// surface it entered while a field there has the keys, told its text,
    /// caret and where the caret is drawn whenever they change.
    fn sync_text_input(&mut self) {
        use zwp_text_input_v3::{ChangeCause, ContentHint, ContentPurpose};
        let Some(ti) = self.text_input.clone() else { return };
        let target = self
            .text_input_on
            .as_ref()
            .and_then(|wl| self.by_wl(wl))
            .and_then(|i| self.surfaces[i].scene.borrow().focused_field().map(|f| (i, f)));
        match target {
            Some((i, f)) => {
                let scene = self.surfaces[i].scene.borrow();
                let Some((text, caret, anchor)) = scene.field_surrounding(f) else { return };
                let r = scene.field_caret_rect(f).unwrap_or([0.0, 0.0, 1.0, 1.0]);
                let rect = [r[0].round() as i32, r[1].round() as i32, r[2].ceil().max(1.0) as i32, r[3].ceil().max(1.0) as i32];
                let secret = scene.field_is_secret(f);
                drop(scene);
                let fresh = !self.text_input_enabled;
                let state = (text, caret, anchor, rect);
                if !fresh && self.text_input_told.as_ref() == Some(&state) {
                    return;
                }
                if fresh {
                    ti.enable();
                    self.text_input_enabled = true;
                }
                let (text, caret, anchor, rect) = &state;
                // At most 4000 bytes: the stretch around the caret.
                let (from, to) = surrounding_window(text, (*caret).min(*anchor), (*caret).max(*anchor));
                let inside = |i: usize| (i.clamp(from, to) - from) as i32;
                ti.set_surrounding_text(text[from..to].to_owned(), inside(*caret), inside(*anchor));
                ti.set_text_change_cause(if self.text_input_cause { ChangeCause::InputMethod } else { ChangeCause::Other });
                if secret {
                    ti.set_content_type(ContentHint::SensitiveData | ContentHint::HiddenText, ContentPurpose::Password);
                } else {
                    ti.set_content_type(ContentHint::None, ContentPurpose::Normal);
                }
                ti.set_cursor_rectangle(rect[0], rect[1], rect[2], rect[3]);
                ti.commit();
                self.text_input_cause = false;
                self.text_input_told = Some(state);
            }
            None if self.text_input_enabled => {
                ti.disable();
                ti.commit();
                self.text_input_enabled = false;
                self.text_input_told = None;
            }
            None => {}
        }
    }

    /// The input method's `done`: what it sent since, applied to the field
    /// that has the keys in the protocol's order — the composition taken
    /// out, the text around the caret deleted, the committed text put in,
    /// then the new composition shown.
    fn text_input_done(&mut self) {
        let pending = std::mem::take(&mut self.text_input_pending);
        let Some((i, f)) = self
            .text_input_on
            .as_ref()
            .and_then(|wl| self.by_wl(wl))
            .and_then(|i| self.surfaces[i].scene.borrow().focused_field().map(|f| (i, f)))
        else {
            return;
        };
        let scene = self.surfaces[i].scene.clone();
        let mut changed = false;
        scene.borrow_mut().field_preedit(f, None);
        if let Some((before, after)) = pending.delete {
            changed |= scene.borrow_mut().field_delete_surrounding(f, before as usize, after as usize);
        }
        if let Some(text) = pending.commit {
            changed |= scene.borrow_mut().field_insert(f, &text);
        }
        if let Some((text, begin, _)) = pending.preedit {
            let cursor = usize::try_from(begin).ok();
            scene.borrow_mut().field_preedit(f, Some((text, cursor)));
        }
        if changed {
            self.text_input_cause = true;
            let text = scene.borrow().field_text(f).unwrap_or_default();
            self.deliver(i, f, Input::Change(text));
        }
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
            old.get(i).is_none_or(|o| {
                specs.get(i).is_none_or(|n| o.layer != n.layer || o.namespace != n.namespace || o.popup.is_some() != n.popup.is_some())
            })
        };
        let old = std::mem::take(&mut self.specs);
        let gone: Vec<SurfaceId> = self
            .surfaces
            .iter()
            .filter(|s| !wanted.contains(&(s.spec, s.output.clone())) || changed(s.spec, &old))
            .map(|s| s.id)
            .collect();
        // A popup goes before the surface it opened from: it opens again,
        // on what is made now, if it is still asked for.
        for i in 0..self.surfaces.len() {
            let parent = match &self.surfaces[i].role {
                Role::Popup(Some(p)) => p.parent,
                Role::Window(Some(_)) => {
                    if gone.contains(&self.surfaces[i].id) {
                        self.close_window(i);
                    }
                    continue;
                }
                _ => continue,
            };
            if gone.contains(&parent) || gone.contains(&self.surfaces[i].id) {
                self.close_popup(i);
            }
        }
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
        let output = output_name.as_ref().and_then(|name| {
            self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name.as_str()))
        });
        let output_size = output
            .as_ref()
            .and_then(|o| self.outputs.info(o))
            .and_then(|i| i.logical_size)
            .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
        // A popup, or a window, has no Wayland surface until it opens.
        if spec.popup.is_some() || spec.window.is_some() {
            let scene = Rc::new(RefCell::new(Scene::new(fonts)));
            let role = if spec.window.is_some() { Role::Window(None) } else { Role::Popup(None) };
            self.surfaces.push(Surface::blank(id, spec_index, output_name, scene, role, None, None, output_size));
            return;
        }
        let wl = self.compositor.create_surface(&qh);
        if let Some(fractional) = &self.fractional {
            fractional.get_fractional_scale(&wl, &qh, ScaleData(id));
        }
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl, &qh, Ignore));
        let effect = self.effects.get_background_effect(&wl, &qh).ok();
        if effect.is_none() {
            log::warn!("the compositor offers no ext-background-effect: no blur");
        }
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
        let scene = Rc::new(RefCell::new(Scene::new(fonts)));
        self.surfaces.push(Surface::blank(id, spec_index, output_name, scene, role, viewport, effect, output_size));
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
        // A window is the size the compositor gave it, its content laid out
        // across it.
        if self.surfaces[i].role.is_window() {
            return self.surfaces[i].logical;
        }
        // A popup is its content's size, whatever it catches.
        if self.surfaces[i].role.is_popup() {
            let size = self.surfaces[i].scene.borrow_mut().layout(None, None);
            return (size.0.ceil().max(1.0) as u32, size.1.ceil().max(1.0) as u32);
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
        if s.role.is_popup() {
            s.dirty = true;
            self.reposition(i);
            return;
        }
        if s.role.is_window() {
            s.dirty = true;
            return;
        }
        let s = &mut self.surfaces[i];
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
        let mode = keyboard_mode(&spec.keyboard, s.scene.borrow().catcher().is_some(), &s.scene.borrow());
        s.keyboard = mode == KeyboardInteractivity::Exclusive;
        s.typing = mode == KeyboardInteractivity::OnDemand;
        layer.set_keyboard_interactivity(mode);
        layer.commit();
        s.dirty = true;
    }

    /// What each surface's description asks for now, by target (the order
    /// [`App::apply`] returned): a surface that goes to sleep or wakes, or
    /// keeps another strip, is placed again.
    /// The inspector's calls (`genoma ipc genoma …`):
    ///
    /// - `tree`: every surface and every node in it, as JSON;
    /// - `highlight <surface> <node>`: that node outlined, `highlight none`
    ///   for none.
    pub fn inspect(&mut self, words: &[String]) -> Result<String, String> {
        match words {
            [what] if what == "tree" => {
                let surfaces: Vec<serde_json::Value> = self
                    .surfaces
                    .iter()
                    .map(|s| {
                        let spec = self.specs.get(s.spec);
                        let nodes: Vec<serde_json::Value> = s
                            .scene
                            .borrow()
                            .inspect()
                            .into_iter()
                            .map(|n| {
                                serde_json::json!({
                                    "node": n.node,
                                    "parent": n.parent,
                                    "depth": n.depth,
                                    "kind": n.kind,
                                    "role": n.role,
                                    "name": n.name,
                                    "id": n.id,
                                    "text": n.text,
                                    "bounds": n.bounds,
                                    "present": n.present,
                                    "props": n.props.into_iter().map(|(k, v)| serde_json::json!([k, v])).collect::<Vec<_>>(),
                                })
                            })
                            .collect();
                        serde_json::json!({
                            "surface": s.id,
                            "namespace": spec.map(|p| p.namespace.clone()),
                            "output": s.output,
                            "size": [s.logical.0, s.logical.1],
                            "moving": s.scene.borrow().moving_report(),
                            "kind": match &s.role { Role::Popup(_) => "popup", Role::Lock(_) => "lock", Role::Window(_) => "window", _ => "layer" },
                            "asleep": s.asleep,
                            "nodes": nodes,
                        })
                    })
                    .collect();
                Ok(serde_json::Value::Array(surfaces).to_string())
            }
            [what, none] if what == "highlight" && none == "none" => {
                for i in 0..self.surfaces.len() {
                    self.surfaces[i].scene.borrow_mut().set_highlight(None);
                }
                Ok(String::new())
            }
            [what, surface, node] if what == "highlight" => {
                let surface: u32 = surface.parse().map_err(|_| format!("no surface {surface}"))?;
                let node: u64 = node.parse().map_err(|_| format!("no node {node}"))?;
                for i in 0..self.surfaces.len() {
                    let mark = (self.surfaces[i].id == surface).then_some(node);
                    self.surfaces[i].scene.borrow_mut().set_highlight(mark);
                }
                Ok(String::new())
            }
            [what, on] if what == "profile" && (on == "on" || on == "off") => {
                self.overlay = on == "on";
                for s in &self.surfaces {
                    s.scene.borrow_mut().touch();
                }
                Ok(on.clone())
            }
            _ => Err("genoma: tree, highlight <surface> <node>, highlight none, profile on|off".into()),
        }
    }

    pub fn apply_wishes(&mut self, wishes: &[Wish]) {
        for (i, wish) in wishes.iter().enumerate() {
            let Some(s) = self.surfaces.get_mut(i) else { break };
            let asleep = !wish.present;
            // A popup's anchor moves it at the next batch (sync_popups).
            s.anchor_wish = wish.anchor.clone();
            // A window's title is set at the next batch (sync_window).
            s.title_wish = wish.title.clone();
            if s.asleep == asleep && s.reserve_wish == wish.reserve {
                continue;
            }
            log::debug!("surface {}: {}", s.id, if asleep { "asleep" } else { "awake" });
            if asleep {
                s.dismissed = false;
            }
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
        self.serve_activation();
        self.flush_drag();
        self.sync_idle();
        self.sync_popups();
        self.sync_text_input();
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
    /// surfaces are counted, since they alone share the output's origin,
    /// and only those on its layer or above: one on a lower layer is under
    /// it by the compositor's own order, so a surface over everything (a
    /// selection rectangle on the overlay) takes the pointer everywhere
    /// rather than handing it to an open panel beneath.
    fn holes_for(&self, i: usize) -> Vec<[i32; 4]> {
        let s = &self.surfaces[i];
        let rank = self.specs.get(s.spec).map_or(layer_rank("top"), |spec| layer_rank(&spec.layer));
        let mut holes = Vec::new();
        for (j, other) in self.surfaces.iter().enumerate() {
            let Some(spec) = self.specs.get(other.spec) else { continue };
            if j == i || !spec.full || layer_rank(&spec.layer) < rank || !other.configured || other.output != s.output {
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
        // A window's content is laid out across the size it was given.
        let window = self.surfaces[i].role.is_window();
        let wish = self.specs.get(self.surfaces[i].spec).map(|s| s.keyboard.clone()).unwrap_or_default();
        // Asleep, nothing of it catches. A full surface keeps its output's
        // size and goes on drawing (what is closing finishes leaving), but
        // must not keep the pointer or the keys.
        let catching = !self.surfaces[i].asleep && self.surfaces[i].scene.borrow().catcher().is_some();
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
                s.scene.borrow_mut().layout(if window { Some(s.logical.0 as f32) } else { spans.then_some(s.logical.0 as f32) }, window.then_some(s.logical.1 as f32));
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
        let Some(surface) = s.role.wl_surface().cloned() else { return };
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
        s.scene.borrow_mut().layout(if window { Some(s.logical.0 as f32) } else { spans.then_some(s.logical.0 as f32) }, window.then_some(s.logical.1 as f32));
        let mut list = s.scene.borrow_mut().draw_list(s.scale as f32);
        if self.overlay
            && let Some(fonts) = &self.fonts
        {
            let stats = renderer.stats();
            let mine = stats.surfaces.get(&s.id).cloned().unwrap_or_default();
            let name = self.specs.get(s.spec).map_or("?", |spec| spec.namespace.as_str());
            let kind = match &s.role {
                Role::Popup(_) => "popup",
                Role::Window(_) => "window",
                Role::Lock(_) => "lock",
                Role::Layer(_) => "layer",
            };
            let damage = match mine.damage {
                None => "nothing".to_owned(),
                Some([_, _, w, h]) if w == width && h == height => "all".to_owned(),
                Some([_, _, w, h]) => format!("{:.0}%", 100.0 * (w * h) as f32 / (width * height).max(1) as f32),
            };
            let header = format!(
                "genoma: {} textures, {:.1} MB",
                stats.textures,
                stats.texture_bytes as f64 / 1048576.0
            );
            // Short enough for a narrow surface: the wait only when it
            // counts.
            let wait = if mine.wait_ms >= 1.0 { format!(" wait {:.0}", mine.wait_ms) } else { String::new() };
            let line = format!(
                "{} {kind} {name} {width}×{height} {:.0}fps main {:.1} gpu {:.1}{wait} ms {damage}{}",
                s.id,
                mine.fps,
                s.built_ms,
                mine.render_ms,
                if mine.canvas { " canvas" } else { "" }
            );
            draw_overlay(&mut list, fonts, s.scale as f32, i + 1, &header, &line, mine.damage);
        }
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
        // full surface never changes size, so it is never placed again. A
        // field with the keys and nothing catching takes it when pressed.
        let mode = if s.asleep { KeyboardInteractivity::None } else { keyboard_mode(&wish, catching, &s.scene.borrow()) };
        let (exclusive, typing) = (mode == KeyboardInteractivity::Exclusive, mode == KeyboardInteractivity::OnDemand);
        if s.keyboard != exclusive || s.typing != typing {
            if let Some(layer) = s.role.layer() {
                layer.set_keyboard_interactivity(mode);
            }
            s.keyboard = exclusive;
            s.typing = typing;
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
        // The session stays awake while a node asking it is seen; the
        // compositor adds whether it sees the surface.
        let awake = !s.asleep && s.scene.borrow().inhibits_idle();
        if awake != s.idle_inhibitor.is_some() {
            if let Some(inhibitor) = s.idle_inhibitor.take() {
                inhibitor.destroy();
            } else if let Some(manager) = &self.idle_inhibit {
                s.idle_inhibitor = Some(manager.create_inhibitor(&surface, qh, Ignore));
            }
            log::debug!("surface {}: idle {}", s.id, if awake { "inhibited" } else { "allowed" });
        }
        // The scene's own record of this change is consumed by this frame.
        s.scene.borrow_mut().take_dirty();

        // Asked for with this frame's commit, so the next change waits for
        // the compositor instead of racing it.
        surface.frame(qh, FrameCallbackData(surface.clone()));
        s.frame_pending = true;
        renderer.draw(s.id, width, height, list);
        let built = started.elapsed();
        s.built_ms = built.as_secs_f32() * 1000.0;
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
        // Sizes the layout changed: told after the frame, so content that
        // answers by moving (a scroll brought back within reach) moves in
        // the next one.
        let sized = self.surfaces[i].scene.borrow_mut().take_sized();
        for (node, w, h) in sized {
            self.deliver(i, node, Input::Size(w, h));
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
        self.surfaces.iter().position(|s| s.role.wl_surface() == Some(wl))
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
        let on = self.keyboard_on.as_ref().and_then(|wl| self.by_wl(wl));
        // A field with the keys takes them first: on the surface the
        // keyboard is on, else wherever one has them.
        let field = on
            .and_then(|i| self.surfaces[i].scene.borrow().focused_field().map(|f| (i, f)))
            .or_else(|| self.surfaces.iter().enumerate().find_map(|(i, s)| s.scene.borrow().focused_field().map(|f| (i, f))));
        if field.is_some_and(|(i, f)| self.field_key(i, f, event)) {
            return;
        }
        // Inside a `navigate` node, on the surface with the keys, the
        // keyboard walks.
        if let Some(i) = on
            && let Some(scope) = self.walk_scope(i)
        {
            self.walk_key(i, scope, event);
            return;
        }
        // The keys go to what is catching on the surface the keyboard is on
        // (an open popup), else to the cell catching on whichever output.
        let held = on.and_then(|i| self.surfaces[i].scene.borrow().catcher().map(|c| (i, c)));
        let Some((i, catcher)) = held.or_else(|| {
            self.surfaces.iter().enumerate().find_map(|(i, s)| s.scene.borrow().catcher().map(|c| (i, c)))
        }) else {
            // Nothing catching: a key the field let pass goes to it, and on
            // to whoever above it listens.
            if let Some((i, f)) = field {
                let key = self.key_of(event);
                self.deliver(i, f, Input::Key(key));
            }
            return;
        };
        log::trace!("key {:?} to surface {} node {catcher:?}", event.keysym, self.surfaces[i].id);
        let key = self.key_of(event);
        self.deliver(i, catcher, Input::Key(key));
        self.serve_focus(i, catcher);
    }
}

impl App {
    fn key_of(&self, event: &KeyEvent) -> genoma_script::Key {
        let name = event.keysym.name().unwrap_or_default();
        let name = name.strip_prefix("XK_").unwrap_or(name).to_owned();
        // The text the key types, as the keymap and its layout make it; none
        // while Ctrl or Alt is held, when the key is a command.
        let m = self.modifiers;
        let text = event.utf8.clone().filter(|t| !t.chars().any(char::is_control) && !m.ctrl && !m.alt);
        genoma_script::Key { name, text, ctrl: m.ctrl, shift: m.shift, alt: m.alt, logo: m.logo, code: event.raw_code + 8 }
    }

    /// A key for field `f` of surface `i`: whether the field took it. What
    /// it changed is told to the interface, what it copied goes on the
    /// clipboard, and Ctrl+V brings the clipboard's text in.
    fn field_key(&mut self, i: usize, f: NodeId, event: &KeyEvent) -> bool {
        self.ensure_clipboard();
        let key = self.key_of(event);
        let scene = self.surfaces[i].scene.clone();
        let Some(out) = scene.borrow_mut().field_key(f, &key.name, key.text.as_deref(), key.ctrl, key.shift) else { return false };
        if !out.used {
            return false;
        }
        if let Some(text) = out.copy {
            self.own_selection(genoma_services::clipboard::TEXT_TYPES.iter().map(|t| t.to_string()).collect(), text.into_bytes());
            let _ = self.conn.flush();
        }
        let mut changed = out.changed;
        if out.paste {
            let text = self.clipboard.as_ref().and_then(|c| c.text.get_untracked()).unwrap_or_default();
            if !text.is_empty() {
                changed |= scene.borrow_mut().field_insert(f, &text);
            }
        }
        let text = scene.borrow().field_text(f).unwrap_or_default();
        if changed {
            self.deliver(i, f, Input::Change(text.clone()));
        }
        if out.submit {
            self.deliver(i, f, Input::Submit(text));
        }
        true
    }
}

/// At most 4000 bytes of `text` around `from..to`, on character boundaries:
/// what the text input protocol may carry.
fn surrounding_window(text: &str, from: usize, to: usize) -> (usize, usize) {
    const MAX: usize = 4000;
    if text.len() <= MAX {
        return (0, text.len());
    }
    let mut a = from.saturating_sub(MAX.saturating_sub(to - from) / 2);
    let mut b = (a + MAX).min(text.len());
    a = a.min(b.saturating_sub(MAX));
    while !text.is_char_boundary(a) {
        a += 1;
    }
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    (a, b)
}

/// The text input's events.
struct TextInputData;

impl Dispatch2<ZwpTextInputV3, App> for TextInputData {
    fn event(&self, app: &mut App, _: &ZwpTextInputV3, event: zwp_text_input_v3::Event, _: &Connection, _: &QueueHandle<App>) {
        use zwp_text_input_v3::Event as E;
        match event {
            E::Enter { surface } => {
                app.text_input_on = Some(surface);
                app.text_input_enabled = false;
                app.text_input_told = None;
            }
            // Left, it is disabled by the compositor: a composition in
            // flight is dropped.
            E::Leave { surface } if app.text_input_on.as_ref() == Some(&surface) => {
                if let Some(i) = app.by_wl(&surface) {
                    let scene = app.surfaces[i].scene.clone();
                    // Read first: the borrow would last through the block.
                    let field = scene.borrow().focused_field();
                    if let Some(f) = field {
                        scene.borrow_mut().field_preedit(f, None);
                    }
                }
                app.text_input_on = None;
                app.text_input_enabled = false;
                app.text_input_told = None;
            }
            E::PreeditString { text, cursor_begin, cursor_end } => {
                app.text_input_pending.preedit = text.map(|t| (t, cursor_begin, cursor_end));
            }
            E::CommitString { text } => app.text_input_pending.commit = text,
            E::DeleteSurroundingText { before_length, after_length } => {
                app.text_input_pending.delete = Some((before_length, after_length));
            }
            E::Done { .. } => app.text_input_done(),
            _ => {}
        }
    }
}

/// When a layer surface takes the keyboard: always while a node catches or
/// its description says so; when pressed while a field has the keys or its
/// description asks for it; otherwise never.
fn keyboard_mode(wish: &str, catching: bool, scene: &Scene) -> KeyboardInteractivity {
    match wish {
        _ if catching => KeyboardInteractivity::Exclusive,
        "exclusive" => KeyboardInteractivity::Exclusive,
        "on_demand" => KeyboardInteractivity::OnDemand,
        _ if scene.focused_field().is_some() => KeyboardInteractivity::OnDemand,
        _ => KeyboardInteractivity::None,
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

/// A node and each of its ancestors, innermost first: what the pointer is
/// within when it is over `node`.
fn hover_chain(scene: &Scene, node: NodeId) -> Vec<NodeId> {
    let mut chain = vec![node];
    while let Some(up) = scene.parent(*chain.last().unwrap()) {
        chain.push(up);
    }
    chain
}

/// Where a layer sits in the compositor's stacking, lowest first. An
/// unknown name is Top, as the surface is made on it.
fn layer_rank(layer: &str) -> u8 {
    match layer {
        "background" => 0,
        "bottom" => 1,
        "overlay" => 3,
        _ => 2,
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

/// A clipboard offer's types, as the compositor lists them before it says
/// which offer the selection is.
#[derive(Default)]
struct OfferData(std::sync::Mutex<Vec<String>>);

impl Dispatch2<ExtDataControlOfferV1, App> for OfferData {
    fn event(&self, _: &mut App, _: &ExtDataControlOfferV1, event: ext_data_control_offer_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        // Once each: a program may announce a type twice (wl-copy does with
        // text from stdin).
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            let mut types = self.0.lock().unwrap();
            if !types.contains(&mime_type) {
                types.push(mime_type);
            }
        }
    }
}

/// The seat's clipboard device.
struct DeviceData;

impl Dispatch2<ExtDataControlDeviceV1, App> for DeviceData {
    fn event(&self, app: &mut App, device: &ExtDataControlDeviceV1, event: ext_data_control_device_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        match event {
            // Its types arrive on the offer itself.
            ext_data_control_device_v1::Event::DataOffer { .. } => {}
            ext_data_control_device_v1::Event::Selection { id } => app.selection(id),
            // The primary selection (middle click) is not followed.
            ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            ext_data_control_device_v1::Event::Finished => {
                device.destroy();
                app.data_device = None;
            }
            _ => {}
        }
    }

    smithay_client_toolkit::reexports::client::event_created_child!(App, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, OfferData::default())
    ]);
}

/// The shell's own copy: its bytes, handed to every program that pastes.
struct SourceData(std::sync::Arc<Vec<u8>>);

impl Dispatch2<ExtDataControlSourceV1, App> for SourceData {
    fn event(&self, _: &mut App, source: &ExtDataControlSourceV1, event: ext_data_control_source_v1::Event, _: &Connection, _: &QueueHandle<App>) {
        match event {
            // Written apart from the loop: the reader may be the shell itself,
            // reading the text of its own copy on this very loop.
            ext_data_control_source_v1::Event::Send { fd, .. } => {
                let data = self.0.clone();
                let _ = std::thread::Builder::new().name("genoma-clipboard".into()).spawn(move || {
                    use std::io::Write;
                    let _ = std::fs::File::from(fd).write_all(&data);
                });
            }
            ext_data_control_source_v1::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
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
            let id = self.surfaces[i].id;
            for j in 0..self.surfaces.len() {
                if matches!(&self.surfaces[j].role, Role::Popup(Some(p)) if p.parent == id) {
                    self.close_popup(j);
                }
            }
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
        // given; the next configure gives the same and nothing follows. A
        // tree built before its size was known (a surface naming no
        // output) is built again once it is, asleep or not: anchored to
        // every edge, an asleep surface is still the output's size. A
        // unit square or line is an asleep surface's: nothing to build for.
        if full && w > 1 && h > 1 && s.built != Some((w, h)) {
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

impl PopupHandler for App {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup, config: PopupConfigure) {
        let Some(i) = self.by_wl(popup.wl_surface()) else { return };
        let s = &mut self.surfaces[i];
        s.logical = (config.width.max(1) as u32, config.height.max(1) as u32);
        s.configured = true;
        // A configure must be answered with a buffer, frame callback or not.
        s.frame_pending = false;
        self.draw(i);
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        let Some(i) = self.by_wl(popup.wl_surface()) else { return };
        self.close_popup(i);
        let s = &mut self.surfaces[i];
        s.dismissed = true;
        self.dismissed.push((s.spec, s.output.clone()));
    }
}

/// What our own drag gives, by type: its bytes in each.
type Offered = Vec<(String, Vec<u8>)>;

/// A drag over one of our surfaces: which, the drop target under it, where.
#[derive(Clone, Copy)]
struct DragOver {
    surface: usize,
    target: Option<NodeId>,
    x: f32,
    y: f32,
}

/// The type to read of what is offered: the files first, then text.
fn preferred_mime(mimes: &[String]) -> Option<String> {
    ["text/uri-list", "text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
        .iter()
        .find(|m| mimes.iter().any(|x| x == *m))
        .map(|m| (*m).to_owned())
}

/// A path as a `file://` URI, what is not plain percent-encoded.
fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What a `text/uri-list` holds: its URIs, and the files among them as
/// paths.
fn uri_list(text: &str) -> (Vec<String>, Vec<String>) {
    let uris: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect();
    let files = uris
        .iter()
        .filter_map(|u| u.strip_prefix("file://"))
        // file://host/path: the host, usually empty or localhost, goes.
        .map(|rest| if rest.starts_with('/') { rest } else { rest.find('/').map_or(rest, |at| &rest[at..]) })
        .map(|p| {
            let bytes = p.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut k = 0;
            while k < bytes.len() {
                if bytes[k] == b'%'
                    && k + 2 < bytes.len()
                    && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[k + 1..k + 3]).unwrap_or(""), 16)
                {
                    out.push(v);
                    k += 3;
                } else {
                    out.push(bytes[k]);
                    k += 1;
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        })
        .collect();
    (uris, files)
}

impl smithay_client_toolkit::data_device_manager::data_device::DataDeviceHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64, surface: &wl_surface::WlSurface) {
        self.drag_gone();
        if let Some(i) = self.by_wl(surface) {
            self.drag_over(i, x as f32, y as f32);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice) {
        self.drag_gone();
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64) {
        if let Some(d) = self.dnd {
            self.drag_over(d.surface, x as f32, y as f32);
        }
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice) {}

    /// Dropped: what is offered read in the type preferred, without
    /// blocking, and given to the target's on_drop; the offer finished.
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice) {
        let Some(over) = self.dnd else { return };
        let Some(offer) = self.dnd_device.as_ref().and_then(|d| d.data().drag_offer()) else { return };
        let (Some(target), Some(mime)) = (over.target, offer.with_mime_types(preferred_mime)) else {
            offer.finish();
            offer.destroy();
            self.drag_gone();
            return;
        };
        let pipe = match offer.receive(mime.clone()) {
            Ok(pipe) => pipe,
            Err(e) => {
                log::warn!("drop: {e}");
                self.drag_gone();
                return;
            }
        };
        let _ = self.conn.flush();
        let Some(handle) = self.handle.clone() else { return };
        use std::os::fd::AsFd;
        if let Err(e) = rustix::fs::fcntl_setfl(pipe.as_fd(), rustix::fs::OFlags::NONBLOCK) {
            log::warn!("drop: {e}");
            return;
        }
        let surface_id = self.surfaces[over.surface].id;
        let mut read = Vec::new();
        let inserted = handle.insert_source(Generic::new(pipe, Interest::READ, Mode::Level), move |_, fd, app: &mut App| {
            let mut chunk = [0u8; 16384];
            loop {
                match rustix::io::read(fd.as_fd(), &mut chunk) {
                    Ok(0) => break,
                    Ok(n) if read.len() < 16 << 20 => read.extend_from_slice(&chunk[..n]),
                    Ok(_) => {}
                    Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&read).into_owned();
            let mut dropped = genoma_script::Dropped { x: over.x, y: over.y, ..Default::default() };
            if mime == "text/uri-list" {
                (dropped.uris, dropped.files) = uri_list(&text);
            } else {
                dropped.text = Some(text);
            }
            if let Some(i) = app.surfaces.iter().position(|s| s.id == surface_id) {
                log::debug!("surface {surface_id}: dropped {} files", dropped.files.len());
                app.deliver(i, target, Input::Drop(dropped));
            }
            if let Some(offer) = app.dnd_device.as_ref().and_then(|d| d.data().drag_offer()) {
                offer.finish();
                offer.destroy();
            }
            app.drag_gone();
            Ok(PostAction::Remove)
        });
        if let Err(e) = inserted {
            log::warn!("drop: {e}");
        }
    }
}

impl smithay_client_toolkit::data_device_manager::data_offer::DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut smithay_client_toolkit::data_device_manager::data_offer::DragOffer, _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction) {}

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut smithay_client_toolkit::data_device_manager::data_offer::DragOffer, _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction) {}
}

impl smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource, mime: Option<String>) {
        log::debug!("drag: the target accepts {mime:?}");
    }

    /// The program it was dropped on asks for it in one of the types offered.
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource,
        mime: String,
        mut fd: smithay_client_toolkit::data_device_manager::WritePipe,
    ) {
        use std::io::Write;
        log::debug!("drag: asked for {mime}");
        if let Some((_, offered)) = &self.drag_source
            && let Some((_, data)) = offered.iter().find(|(m, _)| *m == mime)
            && let Err(e) = fd.write_all(data)
        {
            log::warn!("drag: {e}");
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource) {
        log::debug!("drag: cancelled");
        self.drag_source = None;
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource) {
        log::debug!("drag: finished");
        self.drag_source = None;
    }

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource, _: smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction) {}
}

impl smithay_client_toolkit::activation::ActivationHandler for App {
    type RequestUdata = u64;

    fn new_token(&mut self, token: String, data: &smithay_client_toolkit::activation::RequestData<u64>) {
        self.tokens.push((data.udata, Some(token)));
    }
}

impl WindowHandler for App {
    /// The compositor asks to close a window (its close key, its title
    /// bar's button): one with no `on_close` closes; one with an
    /// `on_close` is told, and stays until its `present` lets it go.
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        let Some(i) = self.by_wl(window.wl_surface()) else { return };
        let spec = self.surfaces[i].spec;
        if self.specs.get(spec).and_then(|s| s.window.as_ref()).is_none_or(|w| w.closes_itself) {
            self.close_window(i);
            self.surfaces[i].dismissed = true;
        }
        let output = self.surfaces[i].output.clone();
        self.dismissed.push((spec, output));
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, configure: WindowConfigure, _: u32) {
        let Some(i) = self.by_wl(window.wl_surface()) else { return };
        let s = &mut self.surfaces[i];
        // No size is the window's own choice: the one it has, at first
        // the one its description asks for.
        let (w, h) = configure.new_size;
        s.logical = (w.map_or(s.logical.0, |v| v.get()), h.map_or(s.logical.1, |v| v.get()));
        s.wanted = s.logical;
        if let Role::Window(Some(open)) = &mut s.role {
            open.maximized = configure.is_maximized();
        }
        s.configured = true;
        s.dirty = true;
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
            if self.dnd_device.is_none()
                && let Some(manager) = &self.dnd_manager
            {
                self.dnd_device = Some(manager.get_data_device(qh, &seat));
            }
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
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, wl: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {
        self.keyboard_on = Some(wl.clone());
        log::debug!("keyboard on surface {:?}", self.by_wl(wl).map(|i| self.surfaces[i].id));
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, wl: &wl_surface::WlSurface, _: u32) {
        if self.keyboard_on.as_ref() == Some(wl) {
            self.keyboard_on = None;
        }
        log::debug!("keyboard off surface {:?}", self.by_wl(wl).map(|i| self.surfaces[i].id));
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.input_serial = serial;
        self.input_surface = self.keyboard_on.clone();
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
            // Hover is being within a node: over a child, the pointer is
            // still over each of its ancestors. Only the nodes it left are
            // told so, innermost first, then those it entered, outermost
            // first; a node it stays within hears nothing.
            if hit != self.surfaces[i].hovered.first().copied() {
                let now = hit.map_or_else(Vec::new, |n| hover_chain(&scene.borrow(), n));
                let was = std::mem::take(&mut self.surfaces[i].hovered);
                for old in was.iter().filter(|n| !now.contains(n)) {
                    self.deliver(i, *old, Input::Hover(false));
                }
                for new in now.iter().rev().filter(|n| !was.contains(n)) {
                    self.deliver(i, *new, Input::Hover(true));
                }
                self.surfaces[i].hovered = now;
            }
            if let K::Enter { serial } = event.kind {
                self.cursor_serial = serial;
                self.cursor_shape = None;
            }
            // Where it is, for shaders that read it.
            let held = match event.kind {
                K::Press { .. } => Some(true),
                K::Release { .. } => Some(false),
                _ => None,
            };
            let now = match event.kind {
                K::Leave { .. } => None,
                _ => {
                    let was = scene.borrow().pointer_held();
                    Some((x, y, held.unwrap_or(was)))
                }
            };
            scene.borrow_mut().set_pointer(now);
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
                    if let Some(node) = self.surfaces[i].pressed
                        && self.start_drag(i, node, x, y)
                    {
                        continue;
                    }
                    if let Some(node) = self.surfaces[i].pressed {
                        let (ox, oy) = self.surfaces[i].pressed_at;
                        self.drag = Some((i, node, x - ox, y - oy));
                        if scene.borrow().is_field(node) {
                            scene.borrow_mut().field_drag(node, x - ox);
                        }
                    }
                }
                K::Press { button, serial, .. } => {
                    self.input_serial = serial;
                    self.input_surface = self.surfaces[i].role.wl_surface().cloned();
                    // The pointer takes over from the keyboard's walk: its
                    // ring goes.
                    if self.walk_scope(i).is_some()
                        && let Some(f) = self.surfaces[i].focused.take()
                    {
                        self.deliver(i, f, Input::Focus(false));
                        scene.borrow_mut().set_focus_ring(None);
                    }
                    let catcher = scene.borrow().catcher();
                    if let Some(c) = catcher {
                        if hit.is_none_or(|h| !scene.borrow().within(h, c)) {
                            self.deliver(i, c, Input::Outside);
                        }
                    }
                    // A title bar's press is the compositor's: it moves the
                    // window, and nothing is pressed here.
                    if button == 0x110
                        && let Some(node) = hit
                        && scene.borrow().moves_window(node)
                        && self.press_title(i, serial)
                    {
                        continue;
                    }
                    self.surfaces[i].pressed = hit;
                    self.surfaces[i].press_point = (x, y);
                    if let Some(node) = hit {
                        let (lx, ly) = scene.borrow().local(node, x, y);
                        self.surfaces[i].pressed_at = (x - lx, y - ly);
                        // A field places its caret before the interface
                        // hears the press (which may give it the keys).
                        if button == 0x110 && scene.borrow().is_field(node) {
                            self.ensure_clipboard();
                            scene.borrow_mut().field_press(node, lx, self.modifiers.shift);
                        }
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
                        let steps = horizontal.value120 != 0 || vertical.value120 != 0;
                        self.deliver(i, node, Input::Wheel(dx, dy, steps));
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

mod displays;
mod gamma;
mod wm;
smithay_client_toolkit::delegate_dispatch2!(App);

#[cfg(test)]
mod tests {
    use super::surrounding_window;

    #[test]
    fn a_dragged_file_is_a_uri_and_a_dropped_uri_a_file() {
        let uri = super::file_uri("/home/a/My Pictures/città.png");
        assert_eq!(uri, "file:///home/a/My%20Pictures/citt%C3%A0.png");
        let (uris, files) = super::uri_list(&format!("# a comment\r\n{uri}\r\nfile://localhost/tmp/x\r\nhttps://example.org/\r\n"));
        assert_eq!(uris.len(), 3);
        assert_eq!(files, vec!["/home/a/My Pictures/città.png".to_owned(), "/tmp/x".to_owned()]);
        assert_eq!(super::preferred_mime(&["text/plain".into(), "text/uri-list".into()]).as_deref(), Some("text/uri-list"));
        assert_eq!(super::preferred_mime(&["image/png".into()]), None);
    }

    #[test]
    fn layers_rank_in_the_compositors_order() {
        use super::layer_rank;
        assert!(layer_rank("background") < layer_rank("bottom"));
        assert!(layer_rank("bottom") < layer_rank("top"));
        assert!(layer_rank("top") < layer_rank("overlay"));
        // A surface made with no layer, or one Genoma does not know, is on Top.
        assert_eq!(layer_rank(""), layer_rank("top"));
        assert_eq!(layer_rank("lock"), layer_rank("top"));
    }

    #[test]
    fn surrounding_text_is_at_most_4000_bytes_around_the_caret() {
        assert_eq!(surrounding_window("short", 2, 2), (0, 5));
        let long = "é".repeat(5000); // 10000 bytes
        let (a, b) = surrounding_window(&long, 6000, 6000);
        assert!(b - a <= 4000 && a <= 6000 && 6000 <= b, "{a}..{b}");
        assert!(long.is_char_boundary(a) && long.is_char_boundary(b));
        let (a, b) = surrounding_window(&long, 10000, 10000);
        assert_eq!(b, 10000);
        assert!(b - a <= 4000);
    }
}

/// The profiling overlay over one surface's list: a line for it at `row`
/// (each surface its own, so those that cover an output make a table), the
/// header above them all, and an outline round what its last frame drew
/// again, physical pixels.
fn draw_overlay(list: &mut genoma_render::DrawList, fonts: &Fonts, scale: f32, row: usize, header: &str, line: &str, damage: Option<[u32; 4]>) {
    use genoma_render::cosmic_text as ct;
    use genoma_render::Rect;
    let rect = |origin: [f32; 2], size: [f32; 2], fill: [f32; 4], rim: f32, edge: [f32; 4]| Rect {
        origin,
        size,
        radius: 3.0 * scale,
        rim,
        level: 1.0,
        cut: 0.0,
        fill,
        rim_top: edge,
        rim_bottom: edge,
        wave: [0.0; 4],
        arc: Rect::WHOLE,
        fill_to: [0.0; 4],
        clip: Rect::NO_CLIP,
        path: [0.0; 4],
        path_end: [0.0; 4],
        fade: [0.0; 4],
        bud: [0.0; 4],
        bud_shape: [0.0; 4],
        stops: Rect::NO_STOPS,
        shapes: Rect::NO_SHAPES,
        transform: Rect::IDENTITY,
    };
    if let Some([x, y, w, h]) = damage {
        list.push_rect(rect([x as f32, y as f32], [w as f32, h as f32], [0.0; 4], 2.0 * scale, [1.0, 0.0, 0.8, 0.9]));
    }
    let mut fonts = fonts.lock().unwrap();
    // The header once, by the first surface.
    let lines: &[(usize, &str)] = if row == 1 { &[(0, header), (row, line)] } else { &[(row, line)] };
    for &(at, text) in lines {
        let mut buffer = ct::Buffer::new(&mut fonts, ct::Metrics::new(12.0, 16.0));
        buffer.set_size(None, None);
        buffer.set_text(text, &ct::Attrs::new().family(ct::Family::Monospace), ct::Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut fonts, false);
        let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        let origin = [8.0 * scale, (8.0 + at as f32 * 18.0) * scale];
        let size = [(width + 8.0) * scale, 17.0 * scale];
        list.push_rect(rect([origin[0] - 4.0 * scale, origin[1] - 1.0 * scale], size, [0.05, 0.05, 0.08, 0.82], 0.0, [0.0; 4]));
        list.push_text(
            genoma_render::Text { buffer: std::sync::Arc::new(buffer), origin, scale, color: [0.9, 0.95, 1.0, 1.0], clip: Rect::NO_CLIP },
            size,
        );
    }
}

