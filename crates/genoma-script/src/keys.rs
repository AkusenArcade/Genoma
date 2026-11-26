//! A key, as a terminal sends it to the program it runs: the bytes xterm
//! sends, or, when the program asked for it, the kitty keyboard protocol's
//! (`CSI code ; modifiers u`). The text a key makes is what the keyboard
//! composed (a dead key and the letter after it are one character here), so
//! a layout's accents reach the program as typed.
//!
//! Only presses: Genoma tells no releases, so a program asking for event
//! types gets presses alone.

/// What the program asked for (the kitty protocol's flags) and the modes
/// that change the legacy bytes.
#[derive(Clone, Copy, Default)]
pub(crate) struct Asked {
    pub app_cursor: bool,
    pub disambiguate: bool,
    pub all_as_escapes: bool,
    pub alternates: bool,
    pub with_text: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub logo: bool,
}

impl Mods {
    /// The parameter both xterm and kitty send: 1 plus the bits.
    fn param(self) -> u32 {
        1 + self.shift as u32 + 2 * self.alt as u32 + 4 * self.ctrl as u32 + 8 * self.logo as u32
    }
    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl || self.logo
    }
}

/// Keys with a CSI letter: arrows, Home, End, and F1 to F4 (SS3 when
/// unmodified).
fn lettered(name: &str) -> Option<(char, bool)> {
    Some(match name {
        "Up" | "KP_Up" => ('A', true),
        "Down" | "KP_Down" => ('B', true),
        "Right" | "KP_Right" => ('C', true),
        "Left" | "KP_Left" => ('D', true),
        "Home" | "KP_Home" => ('H', true),
        "End" | "KP_End" => ('F', true),
        "F1" => ('P', false),
        "F2" => ('Q', false),
        "F3" => ('R', false),
        "F4" => ('S', false),
        _ => return None,
    })
}

/// Keys with a CSI number and a tilde.
fn tilde(name: &str) -> Option<u32> {
    Some(match name {
        "Insert" | "KP_Insert" => 2,
        "Delete" | "KP_Delete" => 3,
        "Prior" | "KP_Prior" | "Page_Up" => 5,
        "Next" | "KP_Next" | "Page_Down" => 6,
        "F5" => 15,
        "F6" => 17,
        "F7" => 18,
        "F8" => 19,
        "F9" => 20,
        "F10" => 21,
        "F11" => 23,
        "F12" => 24,
        _ => return None,
    })
}

/// The kitty protocol's code for a key that is not text.
fn functional(name: &str) -> Option<u32> {
    Some(match name {
        "Escape" => 27,
        "Return" | "KP_Enter" => 13,
        "Tab" | "ISO_Left_Tab" => 9,
        "BackSpace" => 127,
        "Insert" => 2,
        "Delete" => 3,
        "Caps_Lock" => 57358,
        "Scroll_Lock" => 57359,
        "Num_Lock" => 57360,
        "Print" => 57361,
        "Pause" => 57362,
        "Menu" => 57363,
        _ => return None,
    })
}

fn csi_u(code: u32, shifted: Option<u32>, mods: Mods, text: Option<&str>, asked: Asked) -> Vec<u8> {
    let mut s = format!("\x1b[{code}");
    if asked.alternates && let Some(alt) = shifted {
        s.push_str(&format!(":{alt}"));
    }
    let with_text = asked.with_text && text.is_some_and(|t| !t.is_empty());
    if mods.any() || with_text {
        s.push_str(&format!(";{}", mods.param()));
    }
    if with_text && let Some(t) = text {
        let codes: Vec<String> = t.chars().map(|c| (c as u32).to_string()).collect();
        s.push_str(&format!(";{}", codes.join(":")));
    }
    s.push('u');
    s.into_bytes()
}

/// What to send for a key: its keysym's name, the text it composed, the
/// modifiers held. `None`: nothing, as for a lone modifier.
pub(crate) fn encode(name: &str, text: Option<&str>, mods: Mods, asked: Asked) -> Option<Vec<u8>> {
    if matches!(
        name,
        "Shift_L" | "Shift_R" | "Control_L" | "Control_R" | "Alt_L" | "Alt_R" | "Super_L" | "Super_R" | "Meta_L"
            | "Meta_R" | "ISO_Level3_Shift" | "ISO_Level5_Shift" | "Hyper_L" | "Hyper_R"
    ) {
        return None;
    }
    let text = text.filter(|t| !t.is_empty() && !t.chars().any(|c| c.is_control()));
    // Shift+Tab is its own keysym; it is Tab with Shift.
    let mods = if name == "ISO_Left_Tab" { Mods { shift: true, ..mods } } else { mods };
    let p = mods.param();

    // Arrows, Home, End, F1–F4: the same in both protocols.
    if let Some((letter, cursor)) = lettered(name) {
        if mods.any() {
            return Some(format!("\x1b[1;{p}{letter}").into_bytes());
        }
        let ss3 = if cursor { asked.app_cursor } else { true };
        return Some(if ss3 { format!("\x1bO{letter}") } else { format!("\x1b[{letter}") }.into_bytes());
    }
    if let Some(n) = tilde(name) {
        return Some(if mods.any() { format!("\x1b[{n};{p}~") } else { format!("\x1b[{n}~") }.into_bytes());
    }

    let kitty = asked.disambiguate || asked.all_as_escapes;
    // What the shift alone turns into another character is text, and goes
    // as text, as do keys with no modifier: unless every key is wanted as an
    // escape.
    let plain = !mods.ctrl && !mods.alt && !mods.logo;

    if let Some(code) = functional(name) {
        if kitty {
            let legacy_ok = !asked.all_as_escapes && !mods.any() && matches!(code, 13 | 9 | 127);
            if !legacy_ok {
                return Some(csi_u(code, None, mods, None, asked));
            }
        }
        return Some(match code {
            27 => {
                if mods.alt {
                    b"\x1b\x1b".to_vec()
                } else {
                    b"\x1b".to_vec()
                }
            }
            13 => {
                if mods.alt {
                    b"\x1b\r".to_vec()
                } else {
                    b"\r".to_vec()
                }
            }
            9 => {
                if mods.shift {
                    b"\x1b[Z".to_vec()
                } else if mods.alt {
                    b"\x1b\t".to_vec()
                } else {
                    b"\t".to_vec()
                }
            }
            127 => {
                let b = if mods.ctrl { 0x08 } else { 0x7f };
                if mods.alt { vec![0x1b, b] } else { vec![b] }
            }
            2 | 3 => format!("\x1b[{code}~").into_bytes(),
            _ => return None,
        });
    }

    // A key that makes a character.
    let base: Option<char> = if name.chars().count() == 1 {
        name.chars().next()
    } else if name == "space" {
        Some(' ')
    } else {
        text.and_then(|t| t.chars().next())
    };

    if kitty {
        if asked.all_as_escapes || !(plain && text.is_some()) {
            let key = base.map(|c| c.to_lowercase().next().unwrap_or(c));
            if let Some(k) = key {
                let shifted = if mods.shift { text.and_then(|t| t.chars().next()).map(|c| c as u32).filter(|c| *c != k as u32) } else { None };
                return Some(csi_u(k as u32, shifted, mods, text, asked));
            }
        }
        if plain && let Some(t) = text {
            return Some(t.as_bytes().to_vec());
        }
    }

    // Legacy.
    if mods.ctrl {
        let c = base.map(|c| c.to_ascii_lowercase());
        let control = match c {
            Some(c @ 'a'..='z') => Some(c as u8 - b'a' + 1),
            Some(' ') | Some('2') | Some('@') => Some(0),
            Some('[') | Some('3') => Some(0x1b),
            Some('\\') | Some('4') => Some(0x1c),
            Some(']') | Some('5') => Some(0x1d),
            Some('^') | Some('6') => Some(0x1e),
            Some('_') | Some('7') | Some('/') | Some('-') => Some(0x1f),
            Some('8') | Some('?') => Some(0x7f),
            _ => None,
        };
        if let Some(b) = control {
            return Some(if mods.alt { vec![0x1b, b] } else { vec![b] });
        }
        // Ctrl with a key that has no control character: the text, if any.
    }
    let t = text.map(|t| t.to_owned()).or_else(|| (name == "space").then(|| " ".to_owned()))?;
    let mut out = Vec::with_capacity(t.len() + 1);
    if mods.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(t.as_bytes());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: Option<Vec<u8>>) -> String {
        String::from_utf8_lossy(&b.unwrap_or_default()).into_owned()
    }
    const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, logo: false };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };

    #[test]
    fn legacy_keys_are_xterms() {
        let a = Asked::default();
        assert_eq!(s(encode("a", Some("a"), NONE, a)), "a");
        assert_eq!(s(encode("e", Some("è"), NONE, a)), "è");
        assert_eq!(s(encode("c", Some("c"), CTRL, a)), "\x03");
        assert_eq!(s(encode("x", Some("x"), ALT, a)), "\x1bx");
        assert_eq!(s(encode("Return", None, NONE, a)), "\r");
        assert_eq!(s(encode("BackSpace", None, NONE, a)), "\x7f");
        assert_eq!(s(encode("ISO_Left_Tab", None, SHIFT, a)), "\x1b[Z");
        assert_eq!(s(encode("Up", None, NONE, a)), "\x1b[A");
        assert_eq!(s(encode("Up", None, NONE, Asked { app_cursor: true, ..a })), "\x1bOA");
        assert_eq!(s(encode("Right", None, CTRL, a)), "\x1b[1;5C");
        assert_eq!(s(encode("Delete", None, NONE, a)), "\x1b[3~");
        assert_eq!(s(encode("F5", None, SHIFT, a)), "\x1b[15;2~");
        assert_eq!(s(encode("F1", None, NONE, a)), "\x1bOP");
        assert_eq!(s(encode("space", Some(" "), CTRL, a)), "\0");
        assert!(encode("Shift_L", None, SHIFT, a).is_none());
    }

    #[test]
    fn kitty_disambiguates_what_legacy_cannot() {
        let k = Asked { disambiguate: true, ..Asked::default() };
        assert_eq!(s(encode("a", Some("a"), NONE, k)), "a");
        assert_eq!(s(encode("A", Some("A"), SHIFT, k)), "A");
        assert_eq!(s(encode("e", Some("è"), NONE, k)), "è");
        assert_eq!(s(encode("c", Some("c"), CTRL, k)), "\x1b[99;5u");
        assert_eq!(s(encode("Escape", None, NONE, k)), "\x1b[27u");
        assert_eq!(s(encode("Return", None, NONE, k)), "\r");
        assert_eq!(s(encode("Return", None, SHIFT, k)), "\x1b[13;2u");
        assert_eq!(s(encode("i", Some("i"), CTRL, k)), "\x1b[105;5u");
        assert_eq!(s(encode("Up", None, NONE, k)), "\x1b[A");
    }
}
