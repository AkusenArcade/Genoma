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
    /// The texts copied, the newest first, each once (copied again, it
    /// moves to the front), at most `history_limit` of them. Kept in
    /// memory only; a copy a password manager marks secret is left out.
    pub history: Signal<Vec<Copied>>,
    pub history_limit: std::cell::Cell<usize>,
}

/// One text in the history.
#[derive(Clone, Debug, PartialEq)]
pub struct Copied {
    pub text: String,
    /// Seconds since the epoch.
    pub at: u64,
}

/// The type password managers (KeePassXC, KDE's) add to a copy that must
/// not be remembered.
pub const SECRET_HINT: &str = "x-kde-passwordManagerHint";

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

    /// The current selection's text, read: set, and kept in the history
    /// unless it is empty or marked secret.
    pub fn read(&self, text: String) {
        let secret = self.types.get_untracked().unwrap_or_default().iter().any(|t| t == SECRET_HINT);
        if !text.trim().is_empty() && !secret {
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let mut list = self.history.get_untracked().unwrap_or_default();
            list.retain(|c| c.text != text);
            list.insert(0, Copied { text: text.clone(), at });
            list.truncate(self.history_limit.get().max(1));
            self.history.set(list);
        }
        self.text.set(text);
    }

    /// Takes the entry at `index` (from 0) out of the history.
    pub fn forget(&self, index: usize) {
        let mut list = self.history.get_untracked().unwrap_or_default();
        if index < list.len() {
            list.remove(index);
            self.history.set(list);
        }
    }

    pub fn clear_history(&self) {
        self.history.set(Vec::new());
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
            history: Signal::new(Vec::new()),
            history_limit: std::cell::Cell::new(50),
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

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn the_history_keeps_each_text_once_newest_first_and_no_secrets() {
        let c = Clipboard::default();
        c.history_limit.set(3);
        for t in ["one", "two", "one", "three", "four"] {
            c.announce(vec!["text/plain".into()]);
            c.read(t.into());
        }
        let texts: Vec<String> = c.history.get_untracked().unwrap().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["four", "three", "one"]);
        c.announce(vec!["text/plain".into(), SECRET_HINT.into()]);
        c.read("hunter2".into());
        assert_eq!(c.text.get_untracked().unwrap(), "hunter2", "on the clipboard still");
        assert!(!c.history.get_untracked().unwrap().iter().any(|e| e.text == "hunter2"), "not remembered");
        c.forget(0);
        assert_eq!(c.history.get_untracked().unwrap()[0].text, "three");
    }
}
