//! What a field (`g.field`) knows apart from how it is drawn: its text,
//! where the caret is and what is selected, what typing undoes, and what an
//! input method is composing. Indices are bytes into the text, always on a
//! character's boundary.

use std::time::{Duration, Instant};

/// A field's text as it was, for undo.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

/// What the last change was, so that typing a word undoes as one step.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    Typing,
    Erasing,
}

/// How many steps back a field keeps.
const UNDO_DEPTH: usize = 200;

/// Presses this close together are a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// What a key did to a field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    /// The field took the key; otherwise it goes on to whoever else hears
    /// keys (Escape, Tab, Up and Down).
    pub used: bool,
    /// The text changed: `on_change` is told.
    pub changed: bool,
    /// Return: `on_submit` is told.
    pub submit: bool,
    /// Text to put on the clipboard (Ctrl+C, Ctrl+X).
    pub copy: Option<String>,
    /// Ctrl+V: the clipboard's text is to be inserted.
    pub paste: bool,
}

#[derive(Debug, Default)]
pub struct Editor {
    text: String,
    caret: usize,
    anchor: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    run: Option<Run>,
    /// What an input method is composing at the caret, and the cursor
    /// inside it (bytes), before it commits.
    preedit: Option<(String, Option<usize>)>,
    /// How far the line is scrolled to keep the caret in sight, logical
    /// units.
    pub scroll: f32,
    /// The last press and how many came in a row there.
    clicks: Option<(Instant, usize)>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selection as (from, to), from ≤ to; `None` when nothing is.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    pub fn preedit(&self) -> Option<&(String, Option<usize>)> {
        self.preedit.as_ref()
    }

    /// The text set from outside (its binding changed): the caret goes to
    /// the end, and the change can be undone like any other.
    pub fn set_text(&mut self, text: &str) {
        if text == self.text {
            return;
        }
        self.remember(None);
        self.text = text.to_owned();
        self.caret = self.text.len();
        self.anchor = None;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), caret: self.caret, anchor: self.anchor }
    }

    /// Keeps the text as it is for undo, unless this change goes on the
    /// same run of typing or erasing as the last.
    fn remember(&mut self, run: Option<Run>) {
        if run.is_none() || run != self.run {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.run = run;
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
    }

    /// Where the word before `at` starts: spaces skipped, then letters.
    fn word_back(&self, at: usize) -> usize {
        let mut i = at;
        let mut seen = false;
        for (j, c) in self.text[..at].char_indices().rev() {
            if is_word(c) {
                seen = true;
            } else if seen {
                break;
            }
            i = j;
        }
        i
    }

    /// Where the word after `at` ends.
    fn word_forward(&self, at: usize) -> usize {
        let mut i = at;
        let mut seen = false;
        for (j, c) in self.text[at..].char_indices() {
            if is_word(c) {
                seen = true;
            } else if seen {
                break;
            }
            i = at + j + c.len_utf8();
        }
        i
    }

    fn move_to(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = to.min(self.text.len());
        self.run = None;
    }

    /// Replaces the selection (or nothing, at the caret) with `insert`.
    /// `fresh`: a step of its own, which the run goes on from (a space,
    /// then the word after it).
    fn replace(&mut self, insert: &str, run: Option<Run>, fresh: bool) {
        let (from, to) = self.selection().unwrap_or((self.caret, self.caret));
        // A selection replaced is never part of a run.
        self.remember(if from == to && !fresh { run } else { None });
        self.run = run;
        self.text.replace_range(from..to, insert);
        self.caret = from + insert.len();
        self.anchor = None;
    }

    fn erase(&mut self, from: usize, to: usize) -> bool {
        if from >= to {
            return false;
        }
        self.remember(Some(Run::Erasing));
        self.text.replace_range(from..to, "");
        self.caret = from;
        self.anchor = None;
        true
    }

    /// Inserts text as typed or pasted. A line takes no newline: what was
    /// copied on several becomes one, its lines joined by spaces.
    pub fn insert(&mut self, text: &str) {
        let one: String = text.split(['\n', '\r']).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        if one.is_empty() && self.selection().is_none() {
            return;
        }
        // A space starts a new step, so the words before it stay when the
        // word after it is undone.
        self.replace(&one, Some(Run::Typing), one.starts_with(char::is_whitespace));
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
        self.run = None;
    }

    pub fn selected(&self) -> &str {
        self.selection().map_or("", |(a, b)| &self.text[a..b])
    }

    pub fn undo(&mut self) -> bool {
        let Some(back) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        (self.text, self.caret, self.anchor) = (back.text, back.caret, back.anchor);
        self.run = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(forward) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        (self.text, self.caret, self.anchor) = (forward.text, forward.caret, forward.anchor);
        self.run = None;
        true
    }

    /// A key, by its xkb name, with the text it types (none while Ctrl or
    /// Alt is held). `secret`: a password, never copied out.
    pub fn key(&mut self, name: &str, typed: Option<&str>, ctrl: bool, shift: bool, secret: bool) -> Outcome {
        let mut out = Outcome { used: true, ..Outcome::default() };
        let was = self.text.clone();
        let selection = self.selection();
        let caret = self.caret;
        match (ctrl, name) {
            (true, "a" | "A") => self.select_all(),
            (true, "c" | "C") => out.copy = (!secret && selection.is_some()).then(|| self.selected().to_owned()),
            (true, "x" | "X") => {
                if let (false, Some((a, b))) = (secret, selection) {
                    out.copy = Some(self.text[a..b].to_owned());
                    self.erase(a, b);
                }
            }
            (true, "v" | "V") => out.paste = true,
            (true, "z" | "Z") if shift => {
                self.redo();
            }
            (true, "z" | "Z") => {
                self.undo();
            }
            (true, "y" | "Y") => {
                self.redo();
            }
            (true, "Left" | "KP_Left") => self.move_to(self.word_back(caret), shift),
            (true, "Right" | "KP_Right") => self.move_to(self.word_forward(caret), shift),
            (true, "BackSpace") => match selection {
                Some((a, b)) => _ = self.erase(a, b),
                None => _ = self.erase(self.word_back(caret), caret),
            },
            (true, "Delete" | "KP_Delete") => match selection {
                Some((a, b)) => _ = self.erase(a, b),
                None => _ = self.erase(caret, self.word_forward(caret)),
            },
            (_, "Left" | "KP_Left") => match selection {
                Some((a, _)) if !shift => self.move_to(a, false),
                _ => self.move_to(self.prev(caret), shift),
            },
            (_, "Right" | "KP_Right") => match selection {
                Some((_, b)) if !shift => self.move_to(b, false),
                _ => self.move_to(self.next(caret), shift),
            },
            (_, "Home" | "KP_Home") => self.move_to(0, shift),
            (_, "End" | "KP_End") => self.move_to(self.text.len(), shift),
            (_, "BackSpace") => match selection {
                Some((a, b)) => _ = self.erase(a, b),
                None => _ = self.erase(self.prev(caret), caret),
            },
            (_, "Delete" | "KP_Delete") => match selection {
                Some((a, b)) => _ = self.erase(a, b),
                None => _ = self.erase(caret, self.next(caret)),
            },
            (_, "Return" | "KP_Enter") => out.submit = true,
            (false, _) => match typed {
                Some(t) if !t.is_empty() => self.insert(t),
                _ => out.used = false,
            },
            (true, _) => out.used = false,
        }
        out.changed = self.text != was;
        out
    }

    /// A press at byte `at` (from the pointer): the caret goes there; a
    /// second press soon after selects the word, a third the whole line.
    /// With Shift, the selection reaches to it.
    pub fn press(&mut self, at: usize, shift: bool, now: Instant) {
        let count = match self.clicks {
            Some((when, n)) if !shift && now.duration_since(when) < MULTI_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.clicks = Some((now, count));
        let at = self.boundary(at);
        match count {
            2 => {
                let from = if self.text[..at].chars().next_back().is_some_and(is_word) { self.word_back(at) } else { at };
                let to = if self.text[at..].chars().next().is_some_and(is_word) { self.word_forward(at) } else { at };
                self.anchor = Some(from);
                self.caret = to;
                self.run = None;
            }
            3 => self.select_all(),
            _ => self.move_to(at, shift),
        }
    }

    /// The pointer moving with the button held, to byte `at`: the
    /// selection reaches from the press to it.
    pub fn drag(&mut self, at: usize) {
        let at = self.boundary(at);
        self.anchor.get_or_insert(self.caret);
        self.caret = at;
    }

    /// The nearest character boundary at or before `at`.
    fn boundary(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        at
    }

    /// What an input method is composing, with its cursor; `None` when it
    /// has stopped.
    pub fn set_preedit(&mut self, preedit: Option<(String, Option<usize>)>) {
        self.preedit = preedit.filter(|(s, _)| !s.is_empty());
    }

    /// An input method deleting around the caret, bytes before and after,
    /// before it commits.
    pub fn delete_surrounding(&mut self, before: usize, after: usize) -> bool {
        let from = self.boundary(self.caret.saturating_sub(before));
        let mut to = (self.caret + after).min(self.text.len());
        while !self.text.is_char_boundary(to) {
            to += 1;
        }
        self.erase(from, to)
    }

    /// The text as drawn and where the caret and selection fall in it: a
    /// secret as a dot per character, what is being composed in place at
    /// the caret.
    pub fn display(&self, secret: bool) -> Display {
        let map = |i: usize| if secret { self.text[..i].chars().count() * '•'.len_utf8() } else { i };
        let mut text = if secret { "•".repeat(self.text.chars().count()) } else { self.text.clone() };
        let caret = map(self.caret);
        let selection = self.selection().map(|(a, b)| (map(a), map(b)));
        let (mut shown_caret, mut composing) = (caret, None);
        if let Some((pre, cursor)) = &self.preedit {
            text.insert_str(caret, pre);
            composing = Some((caret, caret + pre.len()));
            shown_caret = caret + cursor.unwrap_or(pre.len()).min(pre.len());
        }
        Display { text, caret: shown_caret, selection: if composing.is_some() { None } else { selection }, composing }
    }

    /// A byte in the drawn text back to the text's own.
    pub fn from_display(&self, at: usize, secret: bool) -> usize {
        if secret {
            let n = at / '•'.len_utf8();
            self.text.char_indices().nth(n).map_or(self.text.len(), |(i, _)| i)
        } else {
            self.boundary(at)
        }
    }
}

/// A field as drawn: its text and where things fall in it, bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub text: String,
    pub caret: usize,
    pub selection: Option<(usize, usize)>,
    /// What an input method is composing, underlined.
    pub composing: Option<(usize, usize)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.key(&c.to_string(), Some(&c.to_string()), false, false, false);
        }
    }

    fn key(e: &mut Editor, name: &str) -> Outcome {
        e.key(name, None, false, false, false)
    }

    #[test]
    fn typing_moving_and_erasing() {
        let mut e = Editor::default();
        typed(&mut e, "héllo");
        assert_eq!((e.text(), e.caret()), ("héllo", 6));
        key(&mut e, "Left");
        key(&mut e, "Left");
        key(&mut e, "BackSpace");
        assert_eq!(e.text(), "hélo", "the second l went");
        key(&mut e, "BackSpace");
        assert_eq!((e.text(), e.caret()), ("hlo", 1), "a two-byte letter goes whole");
        key(&mut e, "End");
        assert_eq!(e.caret(), 3);
        assert!(!key(&mut e, "Escape").used, "Escape is not the field's");
        assert!(key(&mut e, "Return").submit);
    }

    #[test]
    fn words_and_selection() {
        let mut e = Editor::default();
        typed(&mut e, "one two  three");
        e.key("Left", None, true, false, false);
        assert_eq!(e.caret(), 9, "back to the start of three");
        e.key("Left", None, true, true, false);
        assert_eq!(e.selected(), "two  ");
        typed(&mut e, "2 ");
        assert_eq!(e.text(), "one 2 three");
        e.key("BackSpace", None, true, false, false);
        assert_eq!(e.text(), "one three", "the word before the caret, and its space");
        e.key("a", None, true, false, false);
        let out = e.key("x", None, true, false, false);
        assert_eq!((out.copy.as_deref(), e.text()), (Some("one three"), ""));
    }

    #[test]
    fn a_word_undoes_as_one() {
        let mut e = Editor::default();
        typed(&mut e, "hello world");
        e.key("z", None, true, false, false);
        assert_eq!(e.text(), "hello", "the space and the word after it are one step");
        e.key("z", None, true, false, false);
        assert_eq!(e.text(), "");
        e.key("z", None, true, true, false);
        assert_eq!(e.text(), "hello");
        key(&mut e, "BackSpace");
        key(&mut e, "BackSpace");
        e.key("z", None, true, false, false);
        assert_eq!(e.text(), "hello", "both erasures were one step");
    }

    #[test]
    fn a_secret_is_drawn_as_dots_and_never_copied() {
        let mut e = Editor::default();
        typed(&mut e, "pà");
        e.select_all();
        assert_eq!(e.key("c", None, true, false, true).copy, None);
        let d = e.display(true);
        assert_eq!((d.text.as_str(), d.selection), ("••", Some((0, 6))));
        assert_eq!(e.from_display(3, true), 1);
    }

    #[test]
    fn pasted_lines_become_one() {
        let mut e = Editor::default();
        e.insert("a\nb\r\nc");
        assert_eq!(e.text(), "a b c");
    }

    #[test]
    fn presses_place_and_select() {
        let mut e = Editor::default();
        typed(&mut e, "alpha beta");
        let t = Instant::now();
        e.press(7, false, t);
        assert_eq!((e.caret(), e.selection()), (7, None));
        e.press(7, false, t + Duration::from_millis(100));
        assert_eq!(e.selected(), "beta");
        e.press(7, false, t + Duration::from_millis(200));
        assert_eq!(e.selected(), "alpha beta");
        e.press(2, false, t + Duration::from_secs(2));
        e.drag(8);
        assert_eq!(e.selected(), "pha be");
    }

    #[test]
    fn composing_sits_at_the_caret() {
        let mut e = Editor::default();
        typed(&mut e, "ab");
        key(&mut e, "Left");
        e.set_preedit(Some(("にほ".into(), Some(3))));
        let d = e.display(false);
        assert_eq!((d.text.as_str(), d.caret, d.composing), ("aにほb", 4, Some((1, 7))));
        e.set_preedit(None);
        e.insert("日本");
        assert_eq!(e.text(), "a日本b");
    }
}
