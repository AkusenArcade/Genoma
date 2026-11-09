//! The clipboard (PRD §6.1): `ext-data-control-v1`, the selection as a
//! clipboard manager sees it. Every copy in the session is announced with
//! the types it is offered in and, when one of them is text, the text
//! itself; the interface can put text or a file's content on the clipboard
//! and keep it there after the program that copied it is gone, and save
//! what is on it to a file (a history of images, say) without `wl-paste`.
//!
//! The protocol lives on the main thread's Wayland connection, so the
//! runtime binds it (only once an interface asks) and sets these; the
//! interface only asks, through `requests`, which the runtime takes on its
//! next turn.

use std::{cell::RefCell, path::PathBuf};

use genoma_core::reactive::Signal;

/// The types a text is offered in, the most precise first: what browsers,
/// toolkits and X11 programs through Xwayland each look for.
pub const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];

/// The most text read from one copy. A selection larger than this is
/// still announced, with its types, but `text` stays empty: it is somebody
/// copying a file's worth, and `save` is the way to keep it.
pub const TEXT_LIMIT: usize = 1 << 20;

/// What the interface asks of the clipboard.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Own the selection with these bytes, offered under these types.
    Set { types: Vec<String>, data: Vec<u8> },
    /// Own the selection with nothing on it.
    Clear,
    /// Write what the selection holds in this type to this file.
    Save { mime: String, path: PathBuf },
}

pub struct Clipboard {
    /// Whether the compositor offers the protocol at all.
    pub available: Signal<bool>,
    /// The types the current selection is offered in; empty when nothing
    /// is on the clipboard.
    pub types: Signal<Vec<String>>,
    /// The current selection as text, when it is offered as text and fits
    /// `TEXT_LIMIT`; otherwise empty.
    pub text: Signal<String>,
    /// Counts selections: it moves on every copy, the same text copied
    /// twice included, so a history can tell one copy from the next.
    pub changed: Signal<u64>,
    /// What the interface asked since the runtime last looked.
    pub requests: RefCell<Vec<Request>>,
}

impl Clipboard {
    pub fn ask(&self, request: Request) {
        self.requests.borrow_mut().push(request);
    }

    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }

    /// A new selection, as the compositor announced it.
    pub fn announce(&self, types: Vec<String>) {
        self.types.set(types);
        self.text.set(String::new());
        self.changed.set(self.changed.get_untracked().unwrap_or(0) + 1);
    }

    /// The first text type among `types`, in the order of `TEXT_TYPES`.
    pub fn text_type(types: &[String]) -> Option<&'static str> {
        TEXT_TYPES.iter().copied().find(|t| types.iter().any(|o| o == t))
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Clipboard {
            available: Signal::new(false),
            types: Signal::new(Vec::new()),
            text: Signal::new(String::new()),
            changed: Signal::new(0),
            requests: RefCell::new(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clipboard;

    #[test]
    fn the_most_precise_text_type_is_read() {
        let offered = vec!["image/png".to_string(), "STRING".to_string(), "text/plain;charset=utf-8".to_string()];
        assert_eq!(Clipboard::text_type(&offered), Some("text/plain;charset=utf-8"));
        assert_eq!(Clipboard::text_type(&["image/png".to_string()]), None);
    }
}
