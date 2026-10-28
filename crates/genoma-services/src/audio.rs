//! Audio (PRD §9.1): PipeWire's nodes, the default sink and source, and the
//! links between nodes, as Bioma's `services/Audio.qml` and
//! `services/Privacy.qml` read them from `Quickshell.Services.Pipewire`;
//! volume, mute and the default set back.
//!
//! PipeWire brings its own loop, so this service has its own thread (PRD §4):
//! it binds every node, link and the "default" metadata the registry
//! announces, and posts each change into the main loop. Commands go the
//! other way on PipeWire's channel. Volumes are on `wpctl`'s scale, the cube
//! root of what PipeWire stores per channel, which is what a person sees.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use calloop::channel::Sender;
use genoma_core::reactive::{Signal, batch};
use pipewire as pw;
use pw::{
    spa::{
        self,
        param::ParamType,
        pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray, deserialize::PodDeserializer, serialize::PodSerializer},
    },
    types::ObjectType,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub id: u32,
    /// `node.name`: what the default is set by.
    pub name: String,
    pub description: String,
    pub nickname: String,
    pub media_class: String,
    /// "sink", "source", "output-stream", "input-stream", "video-source",
    /// "video-stream" or "other".
    pub kind: &'static str,
    /// Every property PipeWire gives the node (`application.name`,
    /// `device.form-factor`, `media.name`…).
    pub properties: Vec<(String, String)>,
    pub has_volume: bool,
    /// 0 to 1 and above, on `wpctl`'s scale: the channels' average.
    pub volume: f64,
    pub channels: usize,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub id: u32,
    /// From the output node to the input node.
    pub output: u32,
    pub input: u32,
    pub active: bool,
}

pub enum Update {
    Node(Box<Node>),
    NodeGone(u32),
    Link(Link),
    LinkGone(u32),
    /// The default sink's and source's `node.name`.
    Defaults { sink: Option<String>, source: Option<String> },
}

pub enum Command {
    Volume(u32, f64),
    Mute(u32, bool),
    /// Sets the default sink (`true`) or source by `node.name`.
    Default(bool, String),
}

pub struct Audio {
    /// By id.
    pub nodes: Signal<Vec<Node>>,
    pub links: Signal<Vec<Link>>,
    pub default_sink: Signal<String>,
    pub default_source: Signal<String>,
    commands: RefCell<Option<pw::channel::Sender<Command>>>,
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            nodes: Signal::new(Vec::new()),
            links: Signal::new(Vec::new()),
            default_sink: Signal::new(String::new()),
            default_source: Signal::new(String::new()),
            commands: RefCell::new(None),
        }
    }
}

fn kind_of(class: &str) -> &'static str {
    match class {
        c if c.starts_with("Audio/Sink") => "sink",
        c if c.starts_with("Audio/Source") => "source",
        c if c.starts_with("Stream/Output/Audio") => "output-stream",
        c if c.starts_with("Stream/Input/Audio") => "input-stream",
        c if c.starts_with("Video/Source") => "video-source",
        c if c.starts_with("Stream/Input/Video") => "video-stream",
        _ => "other",
    }
}

/// `{"name":"alsa_output…"}`, the form the default metadata keeps.
fn default_name(value: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(value?).ok()?;
    v.get("name")?.as_str().map(str::to_owned)
}

/// Volume and mute from a `Props` parameter.
fn read_props(pod: &Pod, node: &mut Node) {
    let Ok((_, Value::Object(object))) = PodDeserializer::deserialize_any_from(pod.as_bytes()) else { return };
    for p in object.properties {
        match (p.key, p.value) {
            (spa::sys::SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(v))) if !v.is_empty() => {
                node.has_volume = true;
                node.channels = v.len();
                node.volume = v.iter().map(|c| (c.max(0.0) as f64).cbrt()).sum::<f64>() / v.len() as f64;
            }
            (spa::sys::SPA_PROP_mute, Value::Bool(m)) => node.muted = m,
            _ => {}
        }
    }
}

fn props_pod(properties: Vec<Property>) -> Option<Vec<u8>> {
    PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Props,
            id: spa::sys::SPA_PARAM_Props,
            properties,
        }),
    )
    .ok()
    .map(|(cursor, _)| cursor.into_inner())
}

/// What the PipeWire thread keeps of each object: the proxy, its listener,
/// and for a node what it last posted.
struct Bound {
    nodes: HashMap<u32, (pw::node::Node, pw::node::NodeListener, Node)>,
    links: HashMap<u32, (pw::link::Link, pw::link::LinkListener)>,
    metadata: Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>,
    defaults: (Option<String>, Option<String>),
}

fn run(post: Sender<Update>, commands: pw::channel::Receiver<Command>) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let bound = Rc::new(RefCell::new(Bound {
        nodes: HashMap::new(),
        links: HashMap::new(),
        metadata: None,
        defaults: (None, None),
    }));

    // Commands from the main thread.
    let b = bound.clone();
    let _commands = commands.attach(mainloop.loop_(), move |command| {
        let b = b.borrow();
        match command {
            Command::Volume(id, volume) => {
                let Some((proxy, _, node)) = b.nodes.get(&id) else { return };
                let linear = volume.max(0.0).powi(3) as f32;
                let channels = node.channels.max(1);
                let pod = props_pod(vec![Property {
                    key: spa::sys::SPA_PROP_channelVolumes,
                    flags: PropertyFlags::empty(),
                    value: Value::ValueArray(ValueArray::Float(vec![linear; channels])),
                }]);
                if let Some(pod) = pod.as_deref().and_then(Pod::from_bytes) {
                    proxy.set_param(ParamType::Props, 0, pod);
                }
            }
            Command::Mute(id, muted) => {
                let Some((proxy, _, _)) = b.nodes.get(&id) else { return };
                let pod = props_pod(vec![Property {
                    key: spa::sys::SPA_PROP_mute,
                    flags: PropertyFlags::empty(),
                    value: Value::Bool(muted),
                }]);
                if let Some(pod) = pod.as_deref().and_then(Pod::from_bytes) {
                    proxy.set_param(ParamType::Props, 0, pod);
                }
            }
            Command::Default(sink, name) => {
                let Some((metadata, _)) = &b.metadata else { return };
                // What a person chose, which WirePlumber keeps and follows:
                // the `configured` key, not the one it derives.
                let key = if sink { "default.configured.audio.sink" } else { "default.configured.audio.source" };
                let value = serde_json::json!({ "name": name }).to_string();
                metadata.set_property(0, key, Some("Spa:String:JSON"), Some(&value));
            }
        }
    });

    let b = bound.clone();
    let p = post.clone();
    let r = registry.clone();
    let _registry = registry
        .add_listener_local()
        .global(move |global| match &global.type_ {
            ObjectType::Node => {
                let Ok(proxy) = r.bind::<pw::node::Node, _>(global) else { return };
                let id = global.id;
                let (b1, p1, b2, p2) = (b.clone(), p.clone(), b.clone(), p.clone());
                let listener = proxy
                    .add_listener_local()
                    .info(move |info| {
                        let mut b = b1.borrow_mut();
                        let Some((_, _, node)) = b.nodes.get_mut(&id) else { return };
                        // Only when they changed: an event for the parameters
                        // alone carries an empty dictionary.
                        let props = info.props().filter(|_| info.change_mask().contains(pw::node::NodeChangeMask::PROPS));
                        if let Some(props) = props {
                            node.properties = props.iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
                            let get = |k: &str| props.get(k).unwrap_or_default().to_owned();
                            node.name = get("node.name");
                            node.description = get("node.description");
                            node.nickname = get("node.nick");
                            node.media_class = get("media.class");
                            node.kind = kind_of(&node.media_class);
                        }
                        let _ = p1.send(Update::Node(Box::new(node.clone())));
                    })
                    .param(move |_, kind, _, _, pod| {
                        if kind != ParamType::Props {
                            return;
                        }
                        let Some(pod) = pod else { return };
                        let mut b = b2.borrow_mut();
                        let Some((_, _, node)) = b.nodes.get_mut(&id) else { return };
                        read_props(pod, node);
                        let _ = p2.send(Update::Node(Box::new(node.clone())));
                    })
                    .register();
                proxy.subscribe_params(&[ParamType::Props]);
                let node = Node { id, kind: "other", ..Node::default() };
                b.borrow_mut().nodes.insert(id, (proxy, listener, node));
            }
            ObjectType::Link => {
                let Ok(proxy) = r.bind::<pw::link::Link, _>(global) else { return };
                let id = global.id;
                let p1 = p.clone();
                let listener = proxy
                    .add_listener_local()
                    .info(move |info| {
                        let active = matches!(info.state(), pw::link::LinkState::Active);
                        let _ = p1.send(Update::Link(Link {
                            id,
                            output: info.output_node_id(),
                            input: info.input_node_id(),
                            active,
                        }));
                    })
                    .register();
                b.borrow_mut().links.insert(id, (proxy, listener));
            }
            ObjectType::Metadata => {
                let named_default = global.props.as_ref().and_then(|p| p.get("metadata.name")) == Some("default");
                if !named_default {
                    return;
                }
                let Ok(proxy) = r.bind::<pw::metadata::Metadata, _>(global) else { return };
                let (b1, p1) = (b.clone(), p.clone());
                let listener = proxy
                    .add_listener_local()
                    .property(move |_, key, _, value| {
                        let mut b = b1.borrow_mut();
                        match key {
                            Some("default.audio.sink") => b.defaults.0 = default_name(value),
                            Some("default.audio.source") => b.defaults.1 = default_name(value),
                            _ => return 0,
                        }
                        let (sink, source) = b.defaults.clone();
                        let _ = p1.send(Update::Defaults { sink, source });
                        0
                    })
                    .register();
                b.borrow_mut().metadata = Some((proxy, listener));
            }
            _ => {}
        })
        .global_remove({
            let (b, p) = (bound.clone(), post.clone());
            move |id| {
                let mut b = b.borrow_mut();
                if b.nodes.remove(&id).is_some() {
                    let _ = p.send(Update::NodeGone(id));
                } else if b.links.remove(&id).is_some() {
                    let _ = p.send(Update::LinkGone(id));
                }
            }
        })
        .register();

    mainloop.run();
    Ok(())
}

impl Audio {
    pub fn apply(&self, update: Update) {
        batch(|| match update {
            Update::Node(node) => {
                // A node is posted before its properties arrive: listed only
                // once it has a class.
                if node.media_class.is_empty() {
                    return;
                }
                let mut nodes = self.nodes.get_untracked().unwrap_or_default();
                match nodes.binary_search_by_key(&node.id, |n| n.id) {
                    Ok(i) => nodes[i] = *node,
                    Err(i) => nodes.insert(i, *node),
                }
                self.nodes.set(nodes);
            }
            Update::NodeGone(id) => {
                let mut nodes = self.nodes.get_untracked().unwrap_or_default();
                nodes.retain(|n| n.id != id);
                self.nodes.set(nodes);
            }
            Update::Link(link) => {
                let mut links = self.links.get_untracked().unwrap_or_default();
                match links.iter_mut().find(|l| l.id == link.id) {
                    Some(l) => *l = link,
                    None => links.push(link),
                }
                self.links.set(links);
            }
            Update::LinkGone(id) => {
                let mut links = self.links.get_untracked().unwrap_or_default();
                links.retain(|l| l.id != id);
                self.links.set(links);
            }
            Update::Defaults { sink, source } => {
                self.default_sink.set(sink.unwrap_or_default());
                self.default_source.set(source.unwrap_or_default());
            }
        });
    }

    /// Starts the PipeWire thread; its updates arrive through `post`.
    pub fn start(&self, post: Sender<Update>) {
        let (sender, receiver) = pw::channel::channel::<Command>();
        let started = std::thread::Builder::new().name("genoma-pipewire".into()).spawn(move || {
            if let Err(e) = run(post, receiver) {
                log::warn!("audio: {e}");
            }
        });
        match started {
            Ok(_) => *self.commands.borrow_mut() = Some(sender),
            Err(e) => log::warn!("audio: no thread: {e}"),
        }
    }

    fn send(&self, command: Command) {
        match self.commands.borrow().as_ref() {
            Some(s) => {
                let _ = s.send(command);
            }
            None => log::warn!("audio: a command before the service started"),
        }
    }

    pub fn set_volume(&self, id: u32, volume: f64) {
        self.send(Command::Volume(id, volume));
    }

    pub fn set_mute(&self, id: u32, muted: bool) {
        self.send(Command::Mute(id, muted));
    }

    /// Makes node `id` the default sink or source, by its kind.
    pub fn set_default(&self, id: u32) {
        let nodes = self.nodes.get_untracked().unwrap_or_default();
        let Some(node) = nodes.iter().find(|n| n.id == id) else { return };
        match node.kind {
            "sink" => self.send(Command::Default(true, node.name.clone())),
            "source" => self.send(Command::Default(false, node.name.clone())),
            _ => log::warn!("audio: node {id} is neither a sink nor a source"),
        }
    }
}
