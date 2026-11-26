//! A terminal's grid drawn (`g.terminal_view`): cells of one size, each a
//! character on a background, from a [`GridSource`] the script side keeps
//! (an emulator's screen). A row is shaped once and kept while it stays the
//! same; backgrounds, lines under and through, the box drawing characters and
//! the block elements are shapes, drawn by Genoma rather than by the font,
//! so they meet from cell to cell.

use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::Arc,
};

use genoma_render::cosmic_text as ct;
use genoma_render::{DrawList, Rect, Text};

/// What a cell is drawn with, besides its colours.
pub mod flags {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 3;
    pub const UNDERCURL: u16 = 1 << 4;
    pub const STRIKE: u16 = 1 << 5;
    /// The second half of a wide character: nothing of its own.
    pub const SPACER: u16 = 1 << 6;
    pub const HIDDEN: u16 = 1 << 7;
    pub const DOTTED_UNDERLINE: u16 = 1 << 8;
    pub const DASHED_UNDERLINE: u16 = 1 << 9;
    /// A character two cells wide.
    pub const WIDE: u16 = 1 << 10;
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridCell {
    pub c: char,
    /// Combining characters after it.
    pub extra: Option<Box<str>>,
    /// Straight sRGB RGBA, resolved (inverse and dim already applied).
    pub fg: [f32; 4],
    /// Alpha 0: the default background, not drawn.
    pub bg: [f32; 4],
    /// Alpha 0: the foreground's.
    pub line: [f32; 4],
    pub flags: u16,
}

pub struct GridRow {
    /// The same key, the same row: its shaping is kept.
    pub key: u64,
    pub cells: Vec<GridCell>,
}

pub struct GridFrame {
    pub rows: Vec<GridRow>,
}

/// What a grid node shows. Asked once per drawing with the cells that fit
/// (`columns` × `rows`) and a cell's size in logical units: a source that is
/// a terminal resizes itself to it.
pub trait GridSource {
    fn frame(&self, columns: usize, rows: usize, cell: [f32; 2]) -> GridFrame;
}

/// What a grid node keeps between drawings.
#[derive(Default)]
pub(crate) struct GridCache {
    /// A row's pieces: where each starts, in columns, and its text shaped.
    rows: HashMap<u64, Arc<Vec<(usize, Arc<ct::Buffer>)>>>,
    /// font, size, scale → a cell's width and height, physical pixels, and
    /// the font's own advance, logical units.
    metrics: Option<(String, u32, u32, [f32; 2], f32)>,
}

pub fn hash_row(cells: &[GridCell]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in cells {
        c.c.hash(&mut h);
        c.extra.hash(&mut h);
        for v in c.fg {
            v.to_bits().hash(&mut h);
        }
        c.flags.hash(&mut h);
    }
    h.finish()
}

/// Logical units: `cell`'s place in physical pixels, snapped.
/// A cell, physical pixels, on whole pixels; and the font's own advance,
/// logical units.
fn cell_size(fonts: &mut ct::FontSystem, attrs: &ct::Attrs<'_>, size: f32, scale: f32) -> ([f32; 2], f32) {
    let metrics = ct::Metrics::new(size, size);
    let mut b = ct::Buffer::new(fonts, metrics);
    b.set_text("MMMMMMMMMM", attrs, ct::Shaping::Advanced, None);
    b.shape_until_scroll(fonts, false);
    let advance = b.layout_runs().next().map_or(size * 0.6, |r| r.line_w / 10.0);
    ([(advance * scale).round().max(1.0), (size * 1.25 * scale).round().max(1.0)], advance)
}

/// Set in the grid's own font, a run with its letters spaced to the cells;
/// anything else (another script, an emoji, a symbol the font may lack) is
/// a piece of its own at its column, whatever width its font gives it.
fn in_run(c: char) -> bool {
    (c as u32) < 0x250 || matches!(c as u32, 0x2010..=0x2027 | 0x2030..=0x205E)
}

/// The four arms of a box drawing character, by weight: 0 none, 1 light,
/// 2 heavy, 3 double; left, right, up, down. `round` for ╭╮╯╰.
fn arms(c: char) -> Option<([u8; 4], bool)> {
    let a = |l, r, u, d| Some(([l, r, u, d], false));
    match c as u32 {
        0x2500 | 0x2504 | 0x2508 | 0x254C => a(1, 1, 0, 0),
        0x2501 | 0x2505 | 0x2509 | 0x254D => a(2, 2, 0, 0),
        0x2502 | 0x2506 | 0x250A | 0x254E => a(0, 0, 1, 1),
        0x2503 | 0x2507 | 0x250B | 0x254F => a(0, 0, 2, 2),
        0x250C => a(0, 1, 0, 1),
        0x250D => a(0, 2, 0, 1),
        0x250E => a(0, 1, 0, 2),
        0x250F => a(0, 2, 0, 2),
        0x2510 => a(1, 0, 0, 1),
        0x2511 => a(2, 0, 0, 1),
        0x2512 => a(1, 0, 0, 2),
        0x2513 => a(2, 0, 0, 2),
        0x2514 => a(0, 1, 1, 0),
        0x2515 => a(0, 2, 1, 0),
        0x2516 => a(0, 1, 2, 0),
        0x2517 => a(0, 2, 2, 0),
        0x2518 => a(1, 0, 1, 0),
        0x2519 => a(2, 0, 1, 0),
        0x251A => a(1, 0, 2, 0),
        0x251B => a(2, 0, 2, 0),
        0x251C => a(0, 1, 1, 1),
        0x251D => a(0, 2, 1, 1),
        0x251E => a(0, 1, 2, 1),
        0x251F => a(0, 1, 1, 2),
        0x2520 => a(0, 1, 2, 2),
        0x2521 => a(0, 2, 2, 1),
        0x2522 => a(0, 2, 1, 2),
        0x2523 => a(0, 2, 2, 2),
        0x2524 => a(1, 0, 1, 1),
        0x2525 => a(2, 0, 1, 1),
        0x2526 => a(1, 0, 2, 1),
        0x2527 => a(1, 0, 1, 2),
        0x2528 => a(1, 0, 2, 2),
        0x2529 => a(2, 0, 2, 1),
        0x252A => a(2, 0, 1, 2),
        0x252B => a(2, 0, 2, 2),
        0x252C => a(1, 1, 0, 1),
        0x252D => a(2, 1, 0, 1),
        0x252E => a(1, 2, 0, 1),
        0x252F => a(2, 2, 0, 1),
        0x2530 => a(1, 1, 0, 2),
        0x2531 => a(2, 1, 0, 2),
        0x2532 => a(1, 2, 0, 2),
        0x2533 => a(2, 2, 0, 2),
        0x2534 => a(1, 1, 1, 0),
        0x2535 => a(2, 1, 1, 0),
        0x2536 => a(1, 2, 1, 0),
        0x2537 => a(2, 2, 1, 0),
        0x2538 => a(1, 1, 2, 0),
        0x2539 => a(2, 1, 2, 0),
        0x253A => a(1, 2, 2, 0),
        0x253B => a(2, 2, 2, 0),
        0x253C => a(1, 1, 1, 1),
        0x253D => a(2, 1, 1, 1),
        0x253E => a(1, 2, 1, 1),
        0x253F => a(2, 2, 1, 1),
        0x2540 => a(1, 1, 2, 1),
        0x2541 => a(1, 1, 1, 2),
        0x2542 => a(1, 1, 2, 2),
        0x2543 => a(2, 1, 2, 1),
        0x2544 => a(1, 2, 2, 1),
        0x2545 => a(2, 1, 1, 2),
        0x2546 => a(1, 2, 1, 2),
        0x2547 => a(2, 2, 2, 1),
        0x2548 => a(2, 2, 1, 2),
        0x2549 => a(2, 1, 2, 2),
        0x254A => a(1, 2, 2, 2),
        0x254B => a(2, 2, 2, 2),
        0x2550 => a(3, 3, 0, 0),
        0x2551 => a(0, 0, 3, 3),
        0x2552 => a(0, 3, 0, 1),
        0x2553 => a(0, 1, 0, 3),
        0x2554 => a(0, 3, 0, 3),
        0x2555 => a(3, 0, 0, 1),
        0x2556 => a(1, 0, 0, 3),
        0x2557 => a(3, 0, 0, 3),
        0x2558 => a(0, 3, 1, 0),
        0x2559 => a(0, 1, 3, 0),
        0x255A => a(0, 3, 3, 0),
        0x255B => a(3, 0, 1, 0),
        0x255C => a(1, 0, 3, 0),
        0x255D => a(3, 0, 3, 0),
        0x255E => a(0, 3, 1, 1),
        0x255F => a(0, 1, 3, 3),
        0x2560 => a(0, 3, 3, 3),
        0x2561 => a(3, 0, 1, 1),
        0x2562 => a(1, 0, 3, 3),
        0x2563 => a(3, 0, 3, 3),
        0x2564 => a(3, 3, 0, 1),
        0x2565 => a(1, 1, 0, 3),
        0x2566 => a(3, 3, 0, 3),
        0x2567 => a(3, 3, 1, 0),
        0x2568 => a(1, 1, 3, 0),
        0x2569 => a(3, 3, 3, 0),
        0x256A => a(3, 3, 1, 1),
        0x256B => a(1, 1, 3, 3),
        0x256C => a(3, 3, 3, 3),
        0x256D => Some(([0, 1, 0, 1], true)),
        0x256E => Some(([1, 0, 0, 1], true)),
        0x256F => Some(([1, 0, 1, 0], true)),
        0x2570 => Some(([0, 1, 1, 0], true)),
        0x2574 => a(1, 0, 0, 0),
        0x2575 => a(0, 0, 1, 0),
        0x2576 => a(0, 1, 0, 0),
        0x2577 => a(0, 0, 0, 1),
        0x2578 => a(2, 0, 0, 0),
        0x2579 => a(0, 0, 2, 0),
        0x257A => a(0, 2, 0, 0),
        0x257B => a(0, 0, 0, 2),
        0x257C => a(1, 2, 0, 0),
        0x257D => a(0, 0, 1, 2),
        0x257E => a(2, 1, 0, 0),
        0x257F => a(0, 0, 2, 1),
        _ => None,
    }
}

/// A block element as rectangles in eighths of the cell (x0, y0, x1, y1)
/// and how opaque.
fn block(c: char) -> Option<Vec<([u8; 4], f32)>> {
    let one = |r: [u8; 4]| Some(vec![(r, 1.0)]);
    Some(match c as u32 {
        0x2580 => return one([0, 0, 8, 4]),
        0x2581..=0x2587 => return one([0, 8 - (c as u32 - 0x2580) as u8, 8, 8]),
        0x2588 => return one([0, 0, 8, 8]),
        0x2589..=0x258F => return one([0, 0, 8 - (c as u32 - 0x2588) as u8, 8]),
        0x2590 => return one([4, 0, 8, 8]),
        0x2591 => vec![([0, 0, 8, 8], 0.25)],
        0x2592 => vec![([0, 0, 8, 8], 0.5)],
        0x2593 => vec![([0, 0, 8, 8], 0.75)],
        0x2594 => return one([0, 0, 8, 1]),
        0x2595 => return one([7, 0, 8, 8]),
        0x2596 => return one([0, 4, 4, 8]),
        0x2597 => return one([4, 4, 8, 8]),
        0x2598 => return one([0, 0, 4, 4]),
        0x2599 => vec![([0, 0, 4, 8], 1.0), ([4, 4, 8, 8], 1.0)],
        0x259A => vec![([0, 0, 4, 4], 1.0), ([4, 4, 8, 8], 1.0)],
        0x259B => vec![([0, 0, 8, 4], 1.0), ([0, 4, 4, 8], 1.0)],
        0x259C => vec![([0, 0, 8, 4], 1.0), ([4, 4, 8, 8], 1.0)],
        0x259D => return one([4, 0, 8, 4]),
        0x259E => vec![([4, 0, 8, 4], 1.0), ([0, 4, 4, 8], 1.0)],
        0x259F => vec![([4, 0, 8, 8], 1.0), ([0, 4, 4, 8], 1.0)],
        _ => return None,
    })
}

/// Drawn as shapes, not by the font.
pub(crate) fn drawn(c: char) -> bool {
    arms(c).is_some() || block(c).is_some()
}

pub(crate) struct Place {
    pub origin: [f32; 2],
    pub scale: f32,
    pub alpha: f32,
    pub clip: [f32; 4],
    pub fade: [f32; 4],
    pub transform: [[f32; 4]; 4],
}

fn rect(p: &Place, origin: [f32; 2], size: [f32; 2], fill: [f32; 4]) -> Rect {
    Rect {
        origin,
        size,
        radius: 0.0,
        rim: 0.0,
        level: 1.0,
        cut: 0.0,
        fill: [fill[0], fill[1], fill[2], fill[3] * p.alpha],
        rim_top: [0.0; 4],
        rim_bottom: [0.0; 4],
        wave: [0.0; 4],
        arc: Rect::WHOLE,
        fill_to: [0.0; 4],
        stops: Rect::NO_STOPS,
        shapes: Rect::NO_SHAPES,
        clip: p.clip,
        path: [0.0; 4],
        path_end: [0.0; 4],
        bud: [0.0; 4],
        bud_shape: [0.0; 4],
        fade: p.fade,
        transform: p.transform,
    }
}

/// A box drawing character in the cell at `at`, physical pixels.
fn draw_arms(list: &mut DrawList, p: &Place, at: [f32; 2], cell: [f32; 2], arms: [u8; 4], round: bool, colour: [f32; 4]) {
    let light = p.scale.round().max(1.0);
    let thick = |w: u8| if w == 2 { light * 2.0 } else { light };
    let cx = (at[0] + (cell[0] / 2.0).floor()).round();
    let cy = (at[1] + (cell[1] / 2.0).floor()).round();
    let [l, r, u, d] = arms;
    if round {
        // A quarter of a rounded rectangle's outline, clipped to the cell:
        // the corner whose arms it has.
        let t = light;
        let w = cell[0];
        let h = cell[1];
        let radius = (w.min(h) / 2.0).round();
        let x = if r > 0 { cx - t / 2.0 } else { cx - w + t / 2.0 };
        let y = if d > 0 { cy - t / 2.0 } else { cy - h + t / 2.0 };
        let mut shape = rect(p, [x, y], [w, h], [0.0; 4]);
        shape.radius = radius;
        shape.rim = t;
        shape.rim_top = [colour[0], colour[1], colour[2], colour[3] * p.alpha];
        shape.rim_bottom = shape.rim_top;
        shape.clip = [
            at[0].max(p.clip[0]),
            at[1].max(p.clip[1]),
            (at[0] + cell[0]).min(p.clip[2]),
            (at[1] + cell[1]).min(p.clip[3]),
        ];
        list.push_rect(shape);
        return;
    }
    // Each arm from the cell's edge to past its middle, so arms meet.
    let horizontal = |from: f32, to: f32, w: u8, list: &mut DrawList| {
        if w == 0 {
            return;
        }
        if w == 3 {
            for dy in [-light, light] {
                list.push_rect(rect(p, [from.round(), (cy + dy - light / 2.0).round()], [(to - from).round(), light], colour));
            }
        } else {
            let t = thick(w);
            list.push_rect(rect(p, [from.round(), cy - (t / 2.0).floor()], [(to - from).round(), t], colour));
        }
    };
    let reach = |w: u8| (if w == 3 { light * 1.5 } else { thick(w) / 2.0 }).ceil();
    let over_v = reach(u.max(d));
    horizontal(at[0], cx + over_v, l, list);
    horizontal(cx - over_v, at[0] + cell[0], r, list);
    let vertical = |from: f32, to: f32, w: u8, list: &mut DrawList| {
        if w == 0 {
            return;
        }
        if w == 3 {
            for dx in [-light, light] {
                list.push_rect(rect(p, [(cx + dx - light / 2.0).round(), from.round()], [light, (to - from).round()], colour));
            }
        } else {
            let t = thick(w);
            list.push_rect(rect(p, [cx - (t / 2.0).floor(), from.round()], [t, (to - from).round()], colour));
        }
    };
    let over_h = reach(l.max(r));
    vertical(at[1], cy + over_h, u, list);
    vertical(cy - over_h, at[1] + cell[1], d, list);
}

/// The grid of `source` in a box `size` logical units large, drawn at
/// `place`. `font`, `size` set the cells.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    fonts: &mut ct::FontSystem,
    cache: &mut GridCache,
    source: &dyn GridSource,
    attrs: &ct::Attrs<'_>,
    font_key: &str,
    font_size: f32,
    size: [f32; 2],
    p: &Place,
    list: &mut DrawList,
) {
    let scale = p.scale;
    let key = (font_key.to_owned(), font_size.to_bits(), scale.to_bits());
    let (cell, advance) = match &cache.metrics {
        Some((f, s, sc, c, a)) if *f == key.0 && *s == key.1 && *sc == key.2 => (*c, *a),
        _ => {
            let (c, a) = cell_size(fonts, attrs, font_size, scale);
            cache.metrics = Some((key.0, key.1, key.2, c, a));
            cache.rows.clear();
            (c, a)
        }
    };
    let columns = ((size[0] * scale) / cell[0]).floor().max(1.0) as usize;
    let rows = ((size[1] * scale) / cell[1]).floor().max(1.0) as usize;
    let frame = source.frame(columns, rows, [cell[0] / scale, cell[1] / scale]);
    let origin = [p.origin[0].round(), p.origin[1].round()];
    // What each letter is spaced by so it fills its cell exactly, em.
    let spacing = (cell[0] / scale - advance) / font_size;
    let line_height = cell[1] / scale;
    let mut used = HashSet::new();
    for (r, row) in frame.rows.iter().enumerate().take(rows) {
        let y = origin[1] + r as f32 * cell[1];
        // Backgrounds, merged where they run on.
        let mut run: Option<(usize, [f32; 4])> = None;
        let flush = |run: &mut Option<(usize, [f32; 4])>, end: usize, list: &mut DrawList| {
            if let Some((start, bg)) = run.take() {
                list.push_rect(rect(p, [origin[0] + start as f32 * cell[0], y], [(end - start) as f32 * cell[0], cell[1]], bg));
            }
        };
        for (c, cellv) in row.cells.iter().enumerate().take(columns) {
            let bg = cellv.bg;
            match &run {
                Some((_, b)) if *b == bg => {}
                _ => {
                    flush(&mut run, c, list);
                    if bg[3] > 0.0 {
                        run = Some((c, bg));
                    }
                }
            }
        }
        flush(&mut run, row.cells.len().min(columns), list);

        // The text, shaped once for this row's content and these cells.
        let shaped_key = row.key ^ cell[0].to_bits() as u64 ^ ((cell[1].to_bits() as u64) << 32);
        let wanted = row.cells.iter().any(|c| c.c != ' ' && c.flags & (flags::SPACER | flags::HIDDEN) == 0 && !drawn(c.c));
        if wanted {
            used.insert(shaped_key);
            let pieces = match cache.rows.get(&shaped_key) {
                Some(b) => b.clone(),
                None => {
                    let b = Arc::new(shape_row(fonts, attrs, &row.cells, columns, font_size, line_height, spacing));
                    cache.rows.insert(shaped_key, b.clone());
                    b
                }
            };
            for (column, buffer) in pieces.iter() {
                list.push_text(
                    Text { buffer: buffer.clone(), origin: [origin[0] + *column as f32 * cell[0], y], scale, color: [1.0, 1.0, 1.0, p.alpha], clip: p.clip },
                    [(columns - column) as f32 * cell[0], cell[1]],
                );
            }
        }

        // Shapes: box drawing, blocks, lines under and through.
        for (c, cellv) in row.cells.iter().enumerate().take(columns) {
            if cellv.flags & flags::HIDDEN != 0 {
                continue;
            }
            let x = origin[0] + c as f32 * cell[0];
            if let Some((a, round)) = arms(cellv.c) {
                draw_arms(list, p, [x, y], cell, a, round, cellv.fg);
            } else if let Some(parts) = block(cellv.c) {
                for (r8, opacity) in parts {
                    let x0 = (x + cell[0] * r8[0] as f32 / 8.0).round();
                    let x1 = (x + cell[0] * r8[2] as f32 / 8.0).round();
                    let y0 = (y + cell[1] * r8[1] as f32 / 8.0).round();
                    let y1 = (y + cell[1] * r8[3] as f32 / 8.0).round();
                    let f = cellv.fg;
                    list.push_rect(rect(p, [x0, y0], [x1 - x0, y1 - y0], [f[0], f[1], f[2], f[3] * opacity]));
                }
            }
            let line = if cellv.line[3] > 0.0 { cellv.line } else { cellv.fg };
            let thin = scale.round().max(1.0);
            let base = (y + cell[1] * 0.86).round();
            let under = flags::UNDERLINE | flags::DOUBLE_UNDERLINE | flags::UNDERCURL | flags::DOTTED_UNDERLINE | flags::DASHED_UNDERLINE;
            if cellv.flags & under != 0 {
                if cellv.flags & flags::DOUBLE_UNDERLINE != 0 {
                    list.push_rect(rect(p, [x, base - thin], [cell[0], thin], line));
                    list.push_rect(rect(p, [x, base + thin], [cell[0], thin], line));
                } else if cellv.flags & (flags::DOTTED_UNDERLINE | flags::DASHED_UNDERLINE | flags::UNDERCURL) != 0 {
                    // Dashes for all three: a wave is read as one at this size.
                    let dash = (cell[0] / 4.0).max(thin);
                    let mut dx = 0.0;
                    while dx < cell[0] {
                        let lift = if cellv.flags & flags::UNDERCURL != 0 && (dx / dash) as i32 % 2 == 1 { -thin } else { 0.0 };
                        list.push_rect(rect(p, [x + dx, base + lift], [dash.min(cell[0] - dx), thin], line));
                        dx += dash * 2.0;
                    }
                } else {
                    list.push_rect(rect(p, [x, base], [cell[0], thin], line));
                }
            }
            if cellv.flags & flags::STRIKE != 0 {
                list.push_rect(rect(p, [x, (y + cell[1] * 0.5).round()], [cell[0], thin], cellv.fg));
            }
        }
    }
    // What is no longer shown is not kept.
    cache.rows.retain(|k, _| used.contains(k));
}

fn shape_row(
    fonts: &mut ct::FontSystem,
    attrs: &ct::Attrs<'_>,
    cells: &[GridCell],
    columns: usize,
    font_size: f32,
    line_height: f32,
    spacing: f32,
) -> Vec<(usize, Arc<ct::Buffer>)> {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let style = |fg: [f32; 4], bold: bool, italic: bool, spaced: bool| {
        let mut a = attrs.clone().color(ct::Color::rgba(to_u8(fg[0]), to_u8(fg[1]), to_u8(fg[2]), to_u8(fg[3])));
        if bold {
            a = a.weight(ct::Weight::BOLD);
        }
        if italic {
            a = a.style(ct::Style::Italic);
        }
        if spaced {
            a = a.letter_spacing(spacing);
        }
        a
    };
    let shape = |fonts: &mut ct::FontSystem, text: &str, spans: &[(std::ops::Range<usize>, [f32; 4], bool, bool)], spaced: bool| {
        let mut b = ct::Buffer::new(fonts, ct::Metrics::new(font_size, line_height));
        b.set_wrap(ct::Wrap::None);
        b.set_size(None, None);
        let rich = spans.iter().map(|(range, fg, bold, italic)| (&text[range.clone()], style(*fg, *bold, *italic, spaced)));
        b.set_rich_text(rich, attrs, ct::Shaping::Advanced, None);
        b.shape_until_scroll(fonts, false);
        Arc::new(b)
    };
    let mut pieces = Vec::new();
    // The run in the grid's font, from `start`: its text and its spans.
    let mut start = 0usize;
    let mut text = String::with_capacity(columns + 8);
    let mut spans: Vec<(std::ops::Range<usize>, [f32; 4], bool, bool)> = Vec::new();
    let flush = |fonts: &mut ct::FontSystem, start: usize, text: &mut String, spans: &mut Vec<_>, pieces: &mut Vec<_>| {
        if !text.trim_end().is_empty() {
            pieces.push((start, shape(fonts, text, spans, true)));
        }
        text.clear();
        spans.clear();
    };
    for (col, c) in cells.iter().enumerate().take(columns) {
        if c.flags & flags::SPACER != 0 {
            continue;
        }
        let bold = c.flags & flags::BOLD != 0;
        let italic = c.flags & flags::ITALIC != 0;
        let blank = c.flags & flags::HIDDEN != 0 || drawn(c.c) || c.c == '\0';
        if !blank && !in_run(c.c) {
            // A piece of its own, and the run goes on after it.
            flush(fonts, start, &mut text, &mut spans, &mut pieces);
            let mut one = String::new();
            one.push(c.c);
            if let Some(extra) = &c.extra {
                one.push_str(extra);
            }
            let span = [(0..one.len(), c.fg, bold, italic)];
            pieces.push((col, shape(fonts, &one, &span, false)));
            start = col + if c.flags & flags::WIDE != 0 { 2 } else { 1 };
            continue;
        }
        if text.is_empty() {
            start = col;
        }
        let from = text.len();
        if blank {
            text.push(' ');
        } else {
            text.push(c.c);
            if let Some(extra) = &c.extra {
                text.push_str(extra);
            }
        }
        match spans.last_mut() {
            Some((range, fg, b, i)) if *fg == c.fg && *b == bold && *i == italic => range.end = text.len(),
            _ => spans.push((from..text.len(), c.fg, bold, italic)),
        }
    }
    flush(fonts, start, &mut text, &mut spans, &mut pieces);
    pieces
}
