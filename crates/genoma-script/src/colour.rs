//! `g.colour`: colour arithmetic on the strings Genoma's properties take,
//! "#rrggbb" and "#rrggbbaa". What a theme derives its colours with, done
//! once here rather than by every shell in Luau.
//!
//! The arithmetic is Qt's (QColor's HSL and HSV, Qt.hsla, Qt.alpha), and a
//! channel is rounded as QColor rounds it, kept in sixteen bits and named in
//! eight, so a palette derived here matches one derived by a QML shell to
//! the byte.

use mlua::{Lua, Table, Value as LuaValue};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(message.into())
}

/// "#rrggbb" or "#rrggbbaa", channels 0…1.
pub fn parse(text: &str) -> Option<Rgba> {
    let hex = text.strip_prefix('#')?;
    let channel = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok().map(|v| v as f64 / 255.0);
    match hex.len() {
        6 => Some(Rgba { r: channel(0)?, g: channel(2)?, b: channel(4)?, a: 1.0 }),
        8 => Some(Rgba { r: channel(0)?, g: channel(2)?, b: channel(4)?, a: channel(6)? }),
        _ => None,
    }
}

/// "#rrggbbaa", each channel rounded as QColor rounds it: to sixteen bits,
/// then named in eight by its division by 257, so a value exactly between
/// two bytes falls the way Qt's does.
pub fn format(c: Rgba) -> String {
    let byte = |v: f64| {
        let wide = (v.clamp(0.0, 1.0) * 65535.0 + 0.5).floor() as u32;
        (wide - (wide >> 8) + 0x80) >> 8
    };
    format!("#{:02x}{:02x}{:02x}{:02x}", byte(c.r), byte(c.g), byte(c.b), byte(c.a))
}

/// Hue 0…1 (-1 for an achromatic colour, as QColor's hslHue), saturation
/// and lightness 0…1.
pub fn hsl(c: Rgba) -> (f64, f64, f64) {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    let l = (max + min) / 2.0;
    if max == min {
        return (-1.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == c.r {
        (c.g - c.b) / d + if c.g < c.b { 6.0 } else { 0.0 }
    } else if max == c.g {
        (c.b - c.r) / d + 2.0
    } else {
        (c.r - c.g) / d + 4.0
    };
    (h / 6.0, s, l)
}

/// Qt.hsla: hue, saturation, lightness and alpha 0…1. A hue below 0 is
/// taken as 0, which is what an achromatic colour's -1 means here.
pub fn hsla(h: f64, s: f64, l: f64, a: f64) -> Rgba {
    let h = h.max(0.0);
    if s == 0.0 {
        return Rgba { r: l, g: l, b: l, a };
    }
    let channel = |p: f64, q: f64, t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    Rgba { r: channel(p, q, h + 1.0 / 3.0), g: channel(p, q, h), b: channel(p, q, h - 1.0 / 3.0), a }
}

/// Hue 0…1 (-1 for an achromatic colour, as QColor's hsvHue), saturation
/// and value 0…1.
pub fn hsv(c: Rgba) -> (f64, f64, f64) {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    let d = max - min;
    let s = if max > 0.0 { d / max } else { 0.0 };
    if d == 0.0 {
        return (-1.0, s, max);
    }
    let h = if max == c.r {
        ((c.g - c.b) / d).rem_euclid(6.0)
    } else if max == c.g {
        (c.b - c.r) / d + 2.0
    } else {
        (c.r - c.g) / d + 4.0
    };
    (h / 6.0, s, max)
}

/// Qt.hsva: hue, saturation, value and alpha 0…1.
pub fn hsva(h: f64, s: f64, v: f64, a: f64) -> Rgba {
    let h = h.max(0.0);
    let i = ((h * 6.0).floor() as i64).rem_euclid(6);
    let fr = h * 6.0 - (h * 6.0).floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - fr * s), v * (1.0 - (1.0 - fr) * s));
    let (r, g, b) = match i {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Rgba { r, g, b, a }
}

/// Between two colours, `t` of the way from `a` to `b`, alpha included, as
/// Qt mixes: a·(1−t) + b·t.
pub fn mix(a: Rgba, b: Rgba, t: f64) -> Rgba {
    let lerp = |p: f64, q: f64| p * (1.0 - t) + q * t;
    Rgba { r: lerp(a.r, b.r), g: lerp(a.g, b.g), b: lerp(a.b, b.b), a: lerp(a.a, b.a) }
}

/// Lightness moved by `amount` (negative darkens), hue and saturation kept.
pub fn lighten(c: Rgba, amount: f64) -> Rgba {
    let (h, s, l) = hsl(c);
    hsla(h, s, (l + amount).clamp(0.0, 1.0), c.a)
}

/// Saturation moved by `amount` (negative greys), hue and lightness kept.
pub fn saturate(c: Rgba, amount: f64) -> Rgba {
    let (h, s, l) = hsl(c);
    hsla(h, (s + amount).clamp(0.0, 1.0), l, c.a)
}

/// Rec. 709 weights on the encoded channels: quick, and enough to tell a
/// dark colour from a light one (luma < 0.5).
pub fn luma(c: Rgba) -> f64 {
    0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
}

/// WCAG 2 relative luminance, on linear channels.
pub fn luminance(c: Rgba) -> f64 {
    let linear = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// WCAG 2 contrast ratio between two colours, 1…21, in either order.
pub fn contrast(a: Rgba, b: Rgba) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn colour(name: &str, text: &str) -> mlua::Result<Rgba> {
    parse(text).ok_or_else(|| err(format!("g.colour.{name}: {text} is not #rrggbb[aa]")))
}

/// The `g.colour` table.
pub fn table(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    // g.colour.parse("#7aa2f7") → { r, g, b, a }, each 0…1; nil for what
    // is not a colour, so a palette file can be checked with it.
    t.set(
        "parse",
        lua.create_function(|lua, text: LuaValue| {
            let Some(c) = text.as_string().and_then(|s| parse(&s.to_string_lossy())) else {
                return Ok(LuaValue::Nil);
            };
            let out = lua.create_table()?;
            out.set("r", c.r)?;
            out.set("g", c.g)?;
            out.set("b", c.b)?;
            out.set("a", c.a)?;
            Ok(LuaValue::Table(out))
        })?,
    )?;
    // g.colour.format(r, g, b, a?) or g.colour.format({ r, g, b, a }).
    t.set(
        "format",
        lua.create_function(|_, (first, g, b, a): (LuaValue, Option<f64>, Option<f64>, Option<f64>)| {
            let c = match first {
                LuaValue::Table(t) => Rgba {
                    r: t.get("r")?,
                    g: t.get("g")?,
                    b: t.get("b")?,
                    a: t.get::<Option<f64>>("a")?.unwrap_or(1.0),
                },
                LuaValue::Number(r) => Rgba {
                    r,
                    g: g.ok_or_else(|| err("g.colour.format: r, g, b"))?,
                    b: b.ok_or_else(|| err("g.colour.format: r, g, b"))?,
                    a: a.unwrap_or(1.0),
                },
                LuaValue::Integer(r) => Rgba {
                    r: r as f64,
                    g: g.ok_or_else(|| err("g.colour.format: r, g, b"))?,
                    b: b.ok_or_else(|| err("g.colour.format: r, g, b"))?,
                    a: a.unwrap_or(1.0),
                },
                _ => return Err(err("g.colour.format: r, g, b[, a] or { r, g, b, a }")),
            };
            Ok(format(c))
        })?,
    )?;
    // g.colour.hsl("#…") → h, s, l, a (h -1 for a grey).
    t.set(
        "hsl",
        lua.create_function(|_, text: String| {
            let c = colour("hsl", &text)?;
            let (h, s, l) = hsl(c);
            Ok((h, s, l, c.a))
        })?,
    )?;
    t.set(
        "hsla",
        lua.create_function(|_, (h, s, l, a): (f64, f64, f64, Option<f64>)| Ok(format(hsla(h, s, l, a.unwrap_or(1.0)))))?,
    )?;
    t.set(
        "hsv",
        lua.create_function(|_, text: String| {
            let c = colour("hsv", &text)?;
            let (h, s, v) = hsv(c);
            Ok((h, s, v, c.a))
        })?,
    )?;
    t.set(
        "hsva",
        lua.create_function(|_, (h, s, v, a): (f64, f64, f64, Option<f64>)| Ok(format(hsva(h, s, v, a.unwrap_or(1.0)))))?,
    )?;
    // g.colour.alpha("#…", 0.5): the same colour with that opacity.
    t.set(
        "alpha",
        lua.create_function(|_, (text, a): (String, f64)| {
            let mut c = colour("alpha", &text)?;
            c.a = a;
            Ok(format(c))
        })?,
    )?;
    t.set(
        "mix",
        lua.create_function(|_, (a, b, t): (String, String, f64)| Ok(format(mix(colour("mix", &a)?, colour("mix", &b)?, t))))?,
    )?;
    t.set(
        "lighten",
        lua.create_function(|_, (text, amount): (String, f64)| Ok(format(lighten(colour("lighten", &text)?, amount))))?,
    )?;
    t.set(
        "saturate",
        lua.create_function(|_, (text, amount): (String, f64)| Ok(format(saturate(colour("saturate", &text)?, amount))))?,
    )?;
    // g.colour.lift("#…", 0.05, dark): away from the background's side —
    // lighter on a dark theme, darker on a light one.
    t.set(
        "lift",
        lua.create_function(|_, (text, amount, dark): (String, f64, Option<bool>)| {
            let c = colour("lift", &text)?;
            Ok(format(lighten(c, if dark.unwrap_or(true) { amount } else { -amount })))
        })?,
    )?;
    t.set("luma", lua.create_function(|_, text: String| Ok(luma(colour("luma", &text)?)))?)?;
    t.set("luminance", lua.create_function(|_, text: String| Ok(luminance(colour("luminance", &text)?)))?)?;
    t.set(
        "contrast",
        lua.create_function(|_, (a, b): (String, String)| Ok(contrast(colour("contrast", &a)?, colour("contrast", &b)?)))?,
    )?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_survives_the_round_trip() {
        for hex in ["#12160fff", "#b9de6bff", "#3fbfa080", "#00000000", "#ffffffff"] {
            assert_eq!(format(parse(hex).unwrap()), hex);
            let (h, s, l) = hsl(parse(hex).unwrap());
            assert_eq!(format(hsla(h, s, l, parse(hex).unwrap().a)), hex, "{hex} through HSL");
            let (h, s, v) = hsv(parse(hex).unwrap());
            assert_eq!(format(hsva(h, s, v, parse(hex).unwrap().a)), hex, "{hex} through HSV");
        }
        assert_eq!(format(parse("#7aa2f7").unwrap()), "#7aa2f7ff");
        assert!(parse("7aa2f7").is_none() && parse("#7aa2f").is_none());
    }

    #[test]
    fn rounds_as_qt_rounds() {
        // Just under half way between two bytes: a plain round of v·255
        // gives 0x7f, QColor's sixteen bits divided by 257 give 0x80.
        let near = Rgba { r: 127.499 / 255.0, g: 0.0, b: 0.0, a: 1.0 };
        assert_eq!(&format(near)[1..3], "80");
    }

    #[test]
    fn derives_as_bioma_did() {
        // Bioma's surface: its background lifted 0.045 on the dark side,
        // as core/theme.luau computed it in Luau.
        let bg = parse("#12160F").unwrap();
        let (h, s, l) = hsl(bg);
        assert_eq!(format(lighten(bg, 0.045)), format(hsla(h, s, l + 0.045, 1.0)));
        assert!(luma(bg) < 0.5);
        assert!((contrast(parse("#000000").unwrap(), parse("#ffffff").unwrap()) - 21.0).abs() < 1e-9);
        assert_eq!(format(mix(parse("#000000").unwrap(), parse("#ffffff").unwrap(), 0.5)), "#808080ff");
    }
}
