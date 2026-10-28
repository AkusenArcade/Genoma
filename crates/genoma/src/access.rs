//! What a screen reader sees (PRD §12, question 5): each surface is an
//! AT-SPI window through AccessKit, holding the nodes that carry a role or a
//! text, with their names and bounds.
//!
//! Nothing is built while no assistive technology is listening: the adapter
//! asks for a tree only once one is.

use std::sync::{Arc, Mutex};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Rect,
    Role, TreeId, TreeInfo, TreeUpdate,
};
use genoma_scene::{Accessible, Role as SceneRole};

pub struct Access {
    adapter: accesskit_unix::Adapter,
    /// The last tree built, for an assistive technology that arrives later.
    latest: Arc<Mutex<Option<TreeUpdate>>>,
    last: Vec<Accessible>,
}

struct Initial(Arc<Mutex<Option<TreeUpdate>>>);

impl ActivationHandler for Initial {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.lock().unwrap().clone()
    }
}

/// Actions (a screen reader pressing a button) come with the AT-SPI work of
/// a later milestone; for now they are only logged.
struct Actions;

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        log::debug!("accessibility action {:?} on {:?}", request.action, request.target_node);
    }
}

struct Quiet;

impl DeactivationHandler for Quiet {
    fn deactivate_accessibility(&mut self) {}
}

fn role(r: SceneRole) -> Role {
    match r {
        SceneRole::None | SceneRole::Group => Role::Group,
        SceneRole::Label => Role::Label,
        SceneRole::Button => Role::Button,
        SceneRole::Slider => Role::Slider,
        SceneRole::Timer => Role::Timer,
        SceneRole::Status => Role::Status,
    }
}

impl Access {
    pub fn new() -> Access {
        let latest = Arc::new(Mutex::new(None));
        let adapter = accesskit_unix::Adapter::new(Initial(latest.clone()), Actions, Quiet);
        Access { adapter, latest, last: Vec::new() }
    }

    /// Publishes the tree if it differs from the last one published.
    pub fn update(&mut self, title: &str, nodes: Vec<Accessible>) {
        if nodes == self.last || nodes.is_empty() {
            return;
        }
        let root = NodeId(nodes[0].id);
        let mut out = Vec::with_capacity(nodes.len());
        for (i, a) in nodes.iter().enumerate() {
            let mut node = Node::new(if i == 0 { Role::Window } else { role(a.role) });
            if i == 0 {
                node.set_label(title.to_owned());
            } else if let Some(name) = &a.name {
                node.set_label(name.clone());
                // A label's text is its value too: that is what a screen
                // reader reads out of a text, as AccessKit models it.
                if a.role == SceneRole::Label {
                    node.set_value(name.clone());
                }
            }
            let [x, y, w, h] = a.bounds;
            node.set_bounds(Rect { x0: x as f64, y0: y as f64, x1: (x + w) as f64, y1: (y + h) as f64 });
            node.set_children(a.children.iter().map(|c| NodeId(*c)).collect::<Vec<_>>());
            out.push((NodeId(a.id), node));
        }
        let update = TreeUpdate { nodes: out, tree: Some(TreeInfo::new(root)), tree_id: TreeId::ROOT, focus: root };
        *self.latest.lock().unwrap() = Some(update.clone());
        self.adapter.update_if_active(|| update);
        self.last = nodes;
    }
}
