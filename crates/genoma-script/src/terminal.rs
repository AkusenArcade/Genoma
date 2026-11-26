//! `g.terminal`: a terminal emulator's state, kept in Rust. It takes what a
//! [`g.pty`](crate::pty) program writes (the bytes never cross into Luau),
//! parses it with `alacritty_terminal` and keeps the grid, its history and
//! its modes. Luau reads what it needs: lines, the cursor, the modes, and
//! the marks a shell sets.
//!
//! Lines are counted from the first line the grid ever had, so a line keeps
//! its number while the history grows and drops old lines (Genoma's copy of
//! alacritty_terminal counts the lines that leave for the history). A mark
//! (OSC 133: a prompt, a command, its output, its end) is told with the
//! line it fell on.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::Duration,
};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    grid::Dimensions,
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, Term, TermMode, cell::Flags, color::Colors},
    vte::ansi::{Color, NamedColor, Processor, Rgb, StdSyncHandler},
};
use genoma_core::reactive::{Effect, Signal, current_owner};
use genoma_scene::grid::{GridCell, GridFrame, GridRow, GridSource, flags};
use mlua::{Function, LuaString, Table, UserData, UserDataFields, UserDataMethods};

use crate::{io::call, pty::Pty};

fn err(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

/// The history kept by default, in lines.
const HISTORY: usize = 10_000;
/// An OSC longer than this is not one of ours (a clipboard's, in base64):
/// it is passed on without being kept.
const OSC_MAX: usize = 4096;

#[derive(Clone)]
pub(crate) struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// What a shell says outside the screen's content.
enum Said {
    /// OSC 133: `kind` is "prompt" (A), "command" (B), "output" (C) or
    /// "done" (D, with its exit code when given); the line and column the
    /// cursor stood on.
    Mark { kind: &'static str, line: f64, column: usize, code: Option<i32> },
    /// OSC 7: the folder, from its file:// URI.
    Cwd(String),
    /// OSC 9;4: 0 none, 1 going, 2 error, 3 not known how far, 4 paused;
    /// and how far, 0 to 100.
    Progress(u8, u8),
    /// OSC 777;notify;title;body, or OSC 9;text.
    Notify(String, String),
}

/// Finds the OSC sequences alacritty_terminal does not know, and where they
/// end in a chunk, so the chunk is parsed up to there before the mark is
/// read.
#[derive(Default)]
struct Scanner {
    state: u8,
    payload: Vec<u8>,
    long: bool,
}

const GROUND: u8 = 0;
const ESC: u8 = 1;
const OSC: u8 = 2;
const OSC_ESC: u8 = 3;

impl Scanner {
    /// The byte at hand: Some(payload) when it ends an OSC.
    fn step(&mut self, b: u8) -> Option<Vec<u8>> {
        match self.state {
            GROUND => {
                if b == 0x1b {
                    self.state = ESC;
                } else if b == 0x9d {
                    self.begin();
                }
                None
            }
            ESC => {
                if b == b']' {
                    self.begin();
                } else {
                    self.state = if b == 0x1b { ESC } else { GROUND };
                }
                None
            }
            OSC => match b {
                0x07 | 0x9c => self.end(),
                0x1b => {
                    self.state = OSC_ESC;
                    None
                }
                // CAN and SUB cancel it.
                0x18 | 0x1a => {
                    self.state = GROUND;
                    None
                }
                _ => {
                    if self.payload.len() < OSC_MAX {
                        self.payload.push(b);
                    } else {
                        self.long = true;
                    }
                    None
                }
            },
            _ => {
                if b == b'\\' {
                    self.end()
                } else {
                    self.state = if b == b']' { OSC } else { GROUND };
                    if self.state == OSC {
                        self.payload.clear();
                        self.long = false;
                    }
                    None
                }
            }
        }
    }

    fn begin(&mut self) {
        self.state = OSC;
        self.payload.clear();
        self.long = false;
    }

    fn end(&mut self) -> Option<Vec<u8>> {
        self.state = GROUND;
        if self.long {
            return None;
        }
        let p = std::mem::take(&mut self.payload);
        let ours = p.starts_with(b"133;") || p.starts_with(b"7;") || p.starts_with(b"9;") || p.starts_with(b"777;");
        ours.then_some(p)
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) struct Emulator {
    pub(crate) term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    scanner: Scanner,
    events: Rc<RefCell<Vec<Event>>>,
    pub(crate) size: Size,
    cell: (u16, u16),
    /// The colours a program is told when it asks (OSC 4, 10, 11, 12): the
    /// interface's palette, where the program has not set its own.
    palette: Colors,
    /// What is selected is drawn on this (straight RGBA): over the cell's
    /// own background, its text unchanged.
    selection_colour: [f32; 4],
    /// The last cell the pointer reported to a program, so a move inside
    /// one cell is not told again.
    last_reported: Option<(usize, usize)>,
}

impl Emulator {
    fn new(columns: usize, lines: usize, history: usize) -> Emulator {
        let events: Rc<RefCell<Vec<Event>>> = Rc::default();
        let config = Config { scrolling_history: history, kitty_keyboard: true, ..Config::default() };
        let size = Size { columns: columns.max(2), lines: lines.max(1) };
        let mut palette = Colors::default();
        // xterm's, until the interface gives its own.
        let base: [u32; 16] = [
            0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f, 0xff0000, 0x00ff00,
            0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
        ];
        for (i, c) in base.iter().enumerate() {
            palette[i] = Some(rgb(*c));
        }
        for i in 16..256 {
            palette[i] = Some(cube(i));
        }
        palette[NamedColor::Foreground] = Some(rgb(0xe5e5e5));
        palette[NamedColor::Background] = Some(rgb(0x000000));
        palette[NamedColor::Cursor] = Some(rgb(0xe5e5e5));
        Emulator {
            term: Term::new(config, &size, Listener(events.clone())),
            parser: Processor::new(),
            scanner: Scanner::default(),
            events,
            size,
            cell: (0, 0),
            palette,
            selection_colour: [0.9, 0.76, 0.38, 0.35],
            last_reported: None,
        }
    }

    /// A cell of the view (column, row from its top) as a point of the grid.
    fn point(&self, column: usize, row: usize) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let column = column.min(self.size.columns.saturating_sub(1));
        let row = row.min(self.size.lines.saturating_sub(1));
        Point::new(Line(row as i32 - offset), Column(column))
    }

    /// A pointer event told to the program, when it asked for them: SGR's
    /// form, else xterm's. `button` 0 left, 1 middle, 2 right, 64 and 65
    /// the wheel; 35 a move with nothing held.
    fn mouse_bytes(&mut self, button: u8, pressed: bool, moved: bool, column: usize, row: usize, mods: (bool, bool, bool)) -> Option<Vec<u8>> {
        let mode = *self.term.mode();
        let wants = if moved {
            if button == 3 { mode.contains(TermMode::MOUSE_MOTION) } else { mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) }
        } else {
            mode.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        };
        if !wants {
            return None;
        }
        if moved {
            if self.last_reported == Some((column, row)) {
                return Some(Vec::new());
            }
        }
        self.last_reported = Some((column, row));
        let (shift, alt, ctrl) = mods;
        let mut code = button as u32 + if moved { 32 } else { 0 };
        code += 4 * shift as u32 + 8 * alt as u32 + 16 * ctrl as u32;
        let (x, y) = (column + 1, row + 1);
        if mode.contains(TermMode::SGR_MOUSE) {
            return Some(format!("\x1b[<{code};{x};{y}{}", if pressed { 'M' } else { 'm' }).into_bytes());
        }
        // xterm's: a release is button 3, and coordinates past 223 cannot be told.
        let code = if pressed || button >= 64 { code } else { 3 + code - button as u32 };
        if x > 223 || y > 223 {
            return Some(Vec::new());
        }
        Some(vec![0x1b, b'[', b'M', (32 + code) as u8, (32 + x) as u8, (32 + y) as u8])
    }

    /// A line's number counted from the grid's first.
    fn absolute(&self, line: Line) -> f64 {
        (self.term.grid().scrolled() as i64 + line.0 as i64) as f64
    }

    /// The screen's line for a number, if it is still kept.
    fn line_of(&self, n: f64) -> Option<Line> {
        let line = n as i64 - self.term.grid().scrolled() as i64;
        let grid = self.term.grid();
        let top = -(grid.history_size() as i64);
        (line >= top && line < grid.screen_lines() as i64).then_some(Line(line as i32))
    }

    fn mark(&self, payload: &[u8], out: &mut Vec<Said>) {
        let text = String::from_utf8_lossy(payload);
        let mut parts = text.split(';');
        let first = parts.next().unwrap_or_default();
        let at = self.term.grid().cursor.point;
        match first {
            "133" => {
                let letter = parts.next().unwrap_or_default();
                let kind = match letter {
                    "A" => "prompt",
                    "B" => "command",
                    "C" => "output",
                    "D" => "done",
                    _ => return,
                };
                // D's exit code, the first number after it; the rest are
                // key=value options (fish's click_events).
                let code = if kind == "done" { parts.find_map(|p| p.parse::<i32>().ok()) } else { None };
                out.push(Said::Mark { kind, line: self.absolute(at.line), column: at.column.0, code });
            }
            "7" => {
                let uri = text.splitn(2, ';').nth(1).unwrap_or_default();
                let path = uri.strip_prefix("file://").map(|rest| match rest.find('/') {
                    Some(i) => &rest[i..],
                    None => "/",
                });
                if let Some(p) = path {
                    out.push(Said::Cwd(percent_decode(p)));
                }
            }
            "9" => {
                let rest = text.splitn(2, ';').nth(1).unwrap_or_default();
                if let Some(progress) = rest.strip_prefix("4;") {
                    let mut p = progress.split(';');
                    let state = p.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0).min(4);
                    let value = p.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0).min(100);
                    out.push(Said::Progress(state, value));
                } else if !rest.is_empty() {
                    out.push(Said::Notify(String::new(), rest.to_owned()));
                }
            }
            "777" => {
                if parts.next() == Some("notify") {
                    let title = parts.next().unwrap_or_default().to_owned();
                    let body = parts.collect::<Vec<_>>().join(";");
                    out.push(Said::Notify(title, body));
                }
            }
            _ => {}
        }
    }

    /// A chunk of what the program wrote, parsed; what the shell said in it,
    /// in order.
    fn feed(&mut self, data: &[u8]) -> Vec<Said> {
        let mut said = Vec::new();
        let mut from = 0;
        for i in 0..data.len() {
            if let Some(payload) = self.scanner.step(data[i]) {
                self.parser.advance(&mut self.term, &data[from..=i]);
                from = i + 1;
                self.mark(&payload, &mut said);
            }
        }
        if from < data.len() {
            self.parser.advance(&mut self.term, &data[from..]);
        }
        said
    }

    fn text(&self, line: Line) -> String {
        let grid = self.term.grid();
        let row = &grid[line];
        let mut s = String::with_capacity(self.size.columns);
        for c in 0..self.size.columns {
            let cell = &row[Column(c)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                s.extend(extra.iter());
            }
        }
        s.truncate(s.trim_end().len());
        s
    }

    fn color(&self, index: usize) -> Rgb {
        self.term.colors()[index].or(self.palette[index]).unwrap_or(Rgb { r: 0, g: 0, b: 0 })
    }

    /// A colour as drawn; the dim and bright variants of the named ones
    /// fall back to the plain ones.
    fn resolve(&self, c: Color) -> Rgb {
        match c {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.color(i as usize),
            Color::Named(n) => {
                let i = n as usize;
                if self.term.colors()[i].is_some() || self.palette[i].is_some() {
                    return self.color(i);
                }
                match n {
                    NamedColor::DimForeground | NamedColor::BrightForeground => self.color(NamedColor::Foreground as usize),
                    _ if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
                        self.color(i - NamedColor::DimBlack as usize)
                    }
                    _ => self.color(NamedColor::Foreground as usize),
                }
            }
        }
    }

    /// The rows in view, as the grid node draws them.
    fn frame(&self, rows: usize) -> GridFrame {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let columns = self.size.columns;
        let to = |rgb: Rgb, a: f32| [rgb.r as f32 / 255.0, rgb.g as f32 / 255.0, rgb.b as f32 / 255.0, a];
        let mut out = Vec::with_capacity(rows);
        let selected = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
        let sel = self.selection_colour;
        for r in 0..rows.min(self.size.lines) {
            let line = Line(r as i32 - offset);
            let row = &grid[line];
            let mut cells = Vec::with_capacity(columns);
            for c in 0..columns {
                let cell = &row[Column(c)];
                let mut fg = self.resolve(cell.fg);
                let default_bg = cell.bg == Color::Named(NamedColor::Background);
                let mut bg = self.resolve(cell.bg);
                let mut bg_shown = !default_bg;
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                    bg_shown = true;
                }
                let mut fg_c = to(fg, 1.0);
                if cell.flags.contains(Flags::DIM) {
                    let b = to(bg, 1.0);
                    for k in 0..3 {
                        fg_c[k] = fg_c[k] * 0.66 + b[k] * 0.34;
                    }
                }
                let mut f = 0u16;
                for (from, to_flag) in [
                    (Flags::BOLD, flags::BOLD),
                    (Flags::ITALIC, flags::ITALIC),
                    (Flags::UNDERLINE, flags::UNDERLINE),
                    (Flags::DOUBLE_UNDERLINE, flags::DOUBLE_UNDERLINE),
                    (Flags::UNDERCURL, flags::UNDERCURL),
                    (Flags::DOTTED_UNDERLINE, flags::DOTTED_UNDERLINE),
                    (Flags::DASHED_UNDERLINE, flags::DASHED_UNDERLINE),
                    (Flags::STRIKEOUT, flags::STRIKE),
                    (Flags::HIDDEN, flags::HIDDEN),
                    (Flags::WIDE_CHAR, flags::WIDE),
                    (Flags::WIDE_CHAR_SPACER, flags::SPACER),
                    (Flags::LEADING_WIDE_CHAR_SPACER, flags::SPACER),
                ] {
                    if cell.flags.contains(from) {
                        f |= to_flag;
                    }
                }
                let extra = cell.zerowidth().filter(|z| !z.is_empty()).map(|z| z.iter().collect::<String>().into_boxed_str());
                let mut bg_c = if bg_shown { to(bg, 1.0) } else { [0.0; 4] };
                if selected.is_some_and(|s| s.contains(Point::new(line, Column(c)))) {
                    // The selection's tint laid over what is there.
                    let a = sel[3];
                    bg_c = if bg_c[3] > 0.0 {
                        [bg_c[0] * (1.0 - a) + sel[0] * a, bg_c[1] * (1.0 - a) + sel[1] * a, bg_c[2] * (1.0 - a) + sel[2] * a, 1.0]
                    } else {
                        sel
                    };
                }
                cells.push(GridCell {
                    c: cell.c,
                    extra,
                    fg: fg_c,
                    bg: bg_c,
                    line: [0.0; 4],
                    flags: f,
                });
            }
            let key = genoma_scene::grid::hash_row(&cells);
            out.push(GridRow { key, cells });
        }
        GridFrame { rows: out }
    }
}

fn rgb(c: u32) -> Rgb {
    Rgb { r: (c >> 16) as u8, g: (c >> 8) as u8, b: c as u8 }
}

/// xterm's 256 colours past the first 16: a 6×6×6 cube, then greys.
fn cube(i: usize) -> Rgb {
    if i >= 232 {
        let v = (8 + (i - 232) * 10) as u8;
        return Rgb { r: v, g: v, b: v };
    }
    let i = i - 16;
    let step = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
    Rgb { r: step(i / 36), g: step(i / 6 % 6), b: step(i % 6) }
}

fn parse_colour(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    let v = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
    Some(rgb(v))
}

struct Callbacks {
    on_title: Option<Function>,
    on_bell: Option<Function>,
    on_clipboard: Option<Function>,
    on_mark: Option<Function>,
    on_cwd: Option<Function>,
    on_progress: Option<Function>,
    on_notify: Option<Function>,
    owner: Option<usize>,
}

pub(crate) struct Shared {
    pub(crate) emulator: RefCell<Emulator>,
    pty: Rc<crate::pty::Inner>,
    /// Bumped whenever the grid may have changed: a binding that reads it
    /// follows the screen.
    pub(crate) changed: Signal<f64>,
    count: Cell<f64>,
    callbacks: Callbacks,
    /// A cell of the view that draws it, logical units, as last drawn.
    cell: Cell<[f32; 2]>,
}

impl Shared {
    fn bump(&self) {
        self.count.set(self.count.get() + 1.0);
        self.changed.set(self.count.get());
    }
}

/// The grid node's side of a terminal.
struct View(Weak<Shared>);

impl GridSource for View {
    fn frame(&self, columns: usize, rows: usize, cell: [f32; 2]) -> GridFrame {
        let Some(shared) = self.0.upgrade() else { return GridFrame { rows: Vec::new() } };
        let resized = {
            let mut e = shared.emulator.borrow_mut();
            let size_changed = e.size.columns != columns || e.size.lines != rows;
            let cell_changed = shared.cell.get() != cell;
            shared.cell.set(cell);
            if size_changed || cell_changed {
                e.size = Size { columns: columns.max(2), lines: rows.max(1) };
                let px = [cell[0].round() as u16, cell[1].round() as u16];
                e.cell = (px[0], px[1]);
                let size = e.size;
                e.term.resize(size);
                shared.pty.resize(
                    size.columns as u16,
                    size.lines as u16,
                    px[0].saturating_mul(size.columns as u16),
                    px[1].saturating_mul(size.lines as u16),
                );
            }
            size_changed || cell_changed
        };
        // Told after this drawing, not inside it: a binding that follows the
        // terminal lays out again then.
        if resized {
            let me = self.0.clone();
            shared.pty.host.timer(
                Duration::ZERO,
                Box::new(move || {
                    if let Some(s) = me.upgrade() {
                        s.bump();
                    }
                    None
                }),
            );
        }
        shared.emulator.borrow().frame(rows)
    }
}

/// A `g.terminal_view`'s `terminal`: drawn from it, and drawn again
/// whenever it changes.
pub(crate) fn attach(scene: Rc<RefCell<genoma_scene::Scene>>, node: genoma_scene::NodeId, value: &mlua::Value) -> mlua::Result<()> {
    let mlua::Value::UserData(ud) = value else { return Err(err("terminal is a g.terminal")) };
    let t = ud.borrow::<Terminal>().map_err(|_| err("terminal is a g.terminal"))?;
    let shared = t.shared.clone();
    scene.borrow_mut().set_grid(node, Some(Rc::new(View(Rc::downgrade(&shared)))));
    let changed = shared.changed.clone();
    Effect::new(move || {
        changed.get();
        if let Ok(mut s) = scene.try_borrow_mut() {
            s.touch();
        }
    });
    Ok(())
}

impl Shared {
    fn take(&self, data: &[u8]) {
        let (said, events) = {
            let mut e = self.emulator.borrow_mut();
            let said = e.feed(data);
            let events = std::mem::take(&mut *e.events.borrow_mut());
            (said, events)
        };
        self.bump();
        let o = self.callbacks.owner;
        for event in events {
            match event {
                Event::PtyWrite(text) => self.pty.write(text.as_bytes()),
                Event::Title(t) => {
                    if let Some(f) = &self.callbacks.on_title {
                        call(f, "g.terminal on_title", t, o);
                    }
                }
                Event::ResetTitle => {
                    if let Some(f) = &self.callbacks.on_title {
                        call(f, "g.terminal on_title", "", o);
                    }
                }
                Event::Bell => {
                    if let Some(f) = &self.callbacks.on_bell {
                        call(f, "g.terminal on_bell", (), o);
                    }
                }
                Event::ClipboardStore(_, text) => {
                    if let Some(f) = &self.callbacks.on_clipboard {
                        call(f, "g.terminal on_clipboard", text, o);
                    }
                }
                Event::ColorRequest(index, format) => {
                    let reply = format(self.emulator.borrow().color(index));
                    self.pty.write(reply.as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let e = self.emulator.borrow();
                    let reply = format(WindowSize {
                        num_lines: e.size.lines as u16,
                        num_cols: e.size.columns as u16,
                        cell_width: e.cell.0,
                        cell_height: e.cell.1,
                    });
                    self.pty.write(reply.as_bytes());
                }
                // Reading the clipboard is not granted: a program could take
                // what was copied elsewhere.
                _ => {}
            }
        }
        for s in said {
            match s {
                Said::Mark { kind, line, column, code } => {
                    if let Some(f) = &self.callbacks.on_mark {
                        call(f, "g.terminal on_mark", (kind, line, column, code), o);
                    }
                }
                Said::Cwd(path) => {
                    if let Some(f) = &self.callbacks.on_cwd {
                        call(f, "g.terminal on_cwd", path, o);
                    }
                }
                Said::Progress(state, value) => {
                    if let Some(f) = &self.callbacks.on_progress {
                        call(f, "g.terminal on_progress", (state, value), o);
                    }
                }
                Said::Notify(title, body) => {
                    if let Some(f) = &self.callbacks.on_notify {
                        call(f, "g.terminal on_notify", (title, body), o);
                    }
                }
            }
        }
    }
}

pub struct Terminal {
    pub(crate) shared: Rc<Shared>,
}

fn mode_table(lua: &mlua::Lua, mode: TermMode) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (name, flag) in [
        ("alt_screen", TermMode::ALT_SCREEN),
        ("show_cursor", TermMode::SHOW_CURSOR),
        ("app_cursor", TermMode::APP_CURSOR),
        ("app_keypad", TermMode::APP_KEYPAD),
        ("bracketed_paste", TermMode::BRACKETED_PASTE),
        ("focus_in_out", TermMode::FOCUS_IN_OUT),
        ("mouse_report_click", TermMode::MOUSE_REPORT_CLICK),
        ("mouse_drag", TermMode::MOUSE_DRAG),
        ("mouse_motion", TermMode::MOUSE_MOTION),
        ("sgr_mouse", TermMode::SGR_MOUSE),
        ("utf8_mouse", TermMode::UTF8_MOUSE),
        ("alternate_scroll", TermMode::ALTERNATE_SCROLL),
        ("line_wrap", TermMode::LINE_WRAP),
        ("disambiguate_esc_codes", TermMode::DISAMBIGUATE_ESC_CODES),
        ("report_event_types", TermMode::REPORT_EVENT_TYPES),
        ("report_alternate_keys", TermMode::REPORT_ALTERNATE_KEYS),
        ("report_all_keys_as_esc", TermMode::REPORT_ALL_KEYS_AS_ESC),
        ("report_associated_text", TermMode::REPORT_ASSOCIATED_TEXT),
    ] {
        t.set(name, mode.contains(flag))?;
    }
    Ok(t)
}

impl UserData for Terminal {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        // t.changed: a number that grows whenever the screen may have
        // changed; a binding that reads it follows the screen.
        fields.add_field_method_get("changed", |_, this| Ok(this.shared.changed.get()));
        fields.add_field_method_get("cols", |_, this| Ok(this.shared.emulator.borrow().size.columns));
        fields.add_field_method_get("rows", |_, this| Ok(this.shared.emulator.borrow().size.lines));
        // The first line still kept, and the screen's first and last.
        fields.add_field_method_get("first_line", |_, this| {
            let e = this.shared.emulator.borrow();
            Ok(e.absolute(Line(-(e.term.grid().history_size() as i32))))
        });
        fields.add_field_method_get("screen_line", |_, this| Ok(this.shared.emulator.borrow().absolute(Line(0))));
        fields.add_field_method_get("last_line", |_, this| {
            let e = this.shared.emulator.borrow();
            Ok(e.absolute(Line(e.size.lines as i32 - 1)))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // t:line(n): its text, without the blanks at its end; nil once it
        // has left the history.
        methods.add_method("line", |_, this, n: f64| {
            let e = this.shared.emulator.borrow();
            Ok(e.line_of(n).map(|l| e.text(l)))
        });
        // t:lines(first, last): their texts, from what is still kept.
        methods.add_method("lines", |_, this, (first, last): (f64, f64)| {
            let e = this.shared.emulator.borrow();
            let mut out = Vec::new();
            let mut n = first;
            while n <= last && out.len() < 100_000 {
                if let Some(l) = e.line_of(n) {
                    out.push(e.text(l));
                }
                n += 1.0;
            }
            Ok(out)
        });
        // t:wrapped(n): whether the line goes on into the next one (it was
        // cut by the width, not ended).
        methods.add_method("wrapped", |_, this, n: f64| {
            let e = this.shared.emulator.borrow();
            Ok(e.line_of(n).is_some_and(|l| {
                let row = &e.term.grid()[l];
                row[Column(e.size.columns - 1)].flags.contains(Flags::WRAPLINE)
            }))
        });
        // t:cursor(): { line, column, shown }.
        methods.add_method("cursor", |lua, this, ()| {
            let e = this.shared.emulator.borrow();
            let Point { line, column } = e.term.grid().cursor.point;
            let t = lua.create_table()?;
            t.set("line", e.absolute(line))?;
            t.set("column", column.0)?;
            t.set("shown", e.term.mode().contains(TermMode::SHOW_CURSOR))?;
            Ok(t)
        });
        // t:view(): what the view shows: the line at its top, how far it is
        // scrolled back, a cell's size (logical units), and the cells.
        methods.add_method("view", |lua, this, ()| {
            let e = this.shared.emulator.borrow();
            let offset = e.term.grid().display_offset();
            let t = lua.create_table()?;
            t.set("first", e.absolute(Line(-(offset as i32))))?;
            t.set("offset", offset)?;
            let c = this.shared.cell.get();
            t.set("cell_width", c[0])?;
            t.set("cell_height", c[1])?;
            t.set("cols", e.size.columns)?;
            t.set("rows", e.size.lines)?;
            Ok(t)
        });
        // t:scroll(lines): the view moved back (positive) or forward into the
        // history; t:scroll() back to the bottom.
        methods.add_method("scroll", |_, this, lines: Option<i32>| {
            {
                let mut e = this.shared.emulator.borrow_mut();
                let s = match lines {
                    Some(n) => alacritty_terminal::grid::Scroll::Delta(n),
                    None => alacritty_terminal::grid::Scroll::Bottom,
                };
                e.term.scroll_display(s);
            }
            this.shared.bump();
            Ok(())
        });
        // t:modes(): what the program asked of the terminal.
        methods.add_method("modes", |lua, this, ()| mode_table(lua, *this.shared.emulator.borrow().term.mode()));
        // t:resize { cols, rows, width, height }: the grid, and the program
        // told (width and height: a cell's, in pixels).
        methods.add_method("resize", |_, this, t: Table| {
            let cols: usize = t.get::<usize>("cols")?.max(2);
            let rows: usize = t.get::<usize>("rows")?.max(1);
            let cw: u16 = t.get::<Option<u16>>("width")?.unwrap_or(0);
            let ch: u16 = t.get::<Option<u16>>("height")?.unwrap_or(0);
            {
                let mut e = this.shared.emulator.borrow_mut();
                e.size = Size { columns: cols, lines: rows };
                e.cell = (cw, ch);
                let size = e.size;
                e.term.resize(size);
            }
            this.shared.pty.resize(cols as u16, rows as u16, cw.saturating_mul(cols as u16), ch.saturating_mul(rows as u16));
            this.shared.changed.set(this.shared.count.get() + 0.5);
            Ok(())
        });
        // t:key(name, text, mods): a key from on_key, sent as the program
        // asked (xterm's bytes, or the kitty protocol's); the view goes back
        // to the bottom. False for a key that sends nothing.
        methods.add_method("key", |_, this, (name, text, mods): (String, Option<String>, Option<Table>)| {
            let flag = |t: &Option<Table>, k: &str| t.as_ref().and_then(|t| t.get::<Option<bool>>(k).ok().flatten()).unwrap_or(false);
            let m = crate::keys::Mods { shift: flag(&mods, "shift"), alt: flag(&mods, "alt"), ctrl: flag(&mods, "ctrl"), logo: flag(&mods, "logo") };
            let asked = {
                let e = this.shared.emulator.borrow();
                let mode = *e.term.mode();
                crate::keys::Asked {
                    app_cursor: mode.contains(TermMode::APP_CURSOR),
                    disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
                    all_as_escapes: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
                    alternates: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
                    with_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
                }
            };
            let Some(bytes) = crate::keys::encode(&name, text.as_deref(), m, asked) else { return Ok(false) };
            let scrolled = {
                let mut e = this.shared.emulator.borrow_mut();
                let back = e.term.grid().display_offset() != 0;
                if back {
                    e.term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                }
                back
            };
            if scrolled {
                this.shared.bump();
            }
            this.shared.pty.write(&bytes);
            Ok(true)
        });
        // t:select(kind, column, row, right): a selection begun at a cell of
        // the view ("char", "word" or "line"); `right`: on the cell's right
        // half. t:select_to(column, row, right) carries it on;
        // t:select(nil) takes it away.
        methods.add_method("select", |_, this, (kind, column, row, right): (Option<String>, Option<usize>, Option<usize>, Option<bool>)| {
            {
                let mut e = this.shared.emulator.borrow_mut();
                e.term.selection = match kind.as_deref() {
                    None => None,
                    Some(k) => {
                        let ty = match k {
                            "word" => SelectionType::Semantic,
                            "line" => SelectionType::Lines,
                            "block" => SelectionType::Block,
                            _ => SelectionType::Simple,
                        };
                        let p = e.point(column.unwrap_or(0), row.unwrap_or(0));
                        Some(Selection::new(ty, p, if right.unwrap_or(false) { Side::Right } else { Side::Left }))
                    }
                };
            }
            this.shared.bump();
            Ok(())
        });
        methods.add_method("select_to", |_, this, (column, row, right): (usize, usize, Option<bool>)| {
            {
                let mut e = this.shared.emulator.borrow_mut();
                let p = e.point(column, row);
                if let Some(s) = e.term.selection.as_mut() {
                    s.update(p, if right.unwrap_or(false) { Side::Right } else { Side::Left });
                }
            }
            this.shared.bump();
            Ok(())
        });
        // t:selected(): the selection's text, nil when nothing is selected.
        methods.add_method("selected", |_, this, ()| {
            let e = this.shared.emulator.borrow();
            Ok(e.term.selection.as_ref().filter(|s| !s.is_empty()).and(e.term.selection_to_string()).filter(|s| !s.is_empty()))
        });
        // t:mouse(event, button, column, row, mods): a pointer event at a
        // cell of the view, told to the program when it asked for them
        // (event "press", "release", "move", "wheel"; button 0 left,
        // 1 middle, 2 right, 64/65 the wheel up/down). False when the
        // program did not ask: the pointer is the terminal's then (select).
        methods.add_method("mouse", |_, this, (event, button, column, row, mods): (String, u8, usize, usize, Option<Table>)| {
            let flag = |k: &str| mods.as_ref().and_then(|t| t.get::<Option<bool>>(k).ok().flatten()).unwrap_or(false);
            // Shift held: the terminal's, as in every terminal.
            if flag("shift") {
                return Ok(false);
            }
            let bytes = {
                let mut e = this.shared.emulator.borrow_mut();
                let column = column.min(e.size.columns.saturating_sub(1));
                let row = row.min(e.size.lines.saturating_sub(1));
                let m = (false, flag("alt"), flag("ctrl"));
                match event.as_str() {
                    "press" => e.mouse_bytes(button, true, false, column, row, m),
                    "release" => e.mouse_bytes(button, false, false, column, row, m),
                    "move" => e.mouse_bytes(button, true, true, column, row, m),
                    "wheel" => e.mouse_bytes(button, true, false, column, row, m),
                    _ => None,
                }
            };
            match bytes {
                Some(b) => {
                    if !b.is_empty() {
                        this.shared.pty.write(&b);
                    }
                    Ok(true)
                }
                None => Ok(false),
            }
        });
        // t:focus(on): told to a program that asked to know (focus in, out).
        methods.add_method("focus", |_, this, on: bool| {
            if this.shared.emulator.borrow().term.mode().contains(TermMode::FOCUS_IN_OUT) {
                this.shared.pty.write(if on { b"\x1b[I" } else { b"\x1b[O" });
            }
            Ok(())
        });
        // t:write(bytes): to the program, as typed.
        methods.add_method("write", |_, this, data: LuaString| {
            this.shared.pty.write(&data.as_bytes());
            Ok(())
        });
        // t:paste(text): as pasted: inside the program's brackets when it
        // asked for them, so a pasted line does not run by itself.
        methods.add_method("paste", |_, this, text: String| {
            let bracketed = this.shared.emulator.borrow().term.mode().contains(TermMode::BRACKETED_PASTE);
            if bracketed {
                // What would close the brackets early is taken out.
                let clean = text.replace("\x1b[201~", "");
                this.shared.pty.write(b"\x1b[200~");
                this.shared.pty.write(clean.as_bytes());
                this.shared.pty.write(b"\x1b[201~");
            } else {
                this.shared.pty.write(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
            }
            Ok(())
        });
        // t:palette { colours = { 16 }, foreground, background, cursor }:
        // what a program is told when it asks.
        methods.add_method("palette", |_, this, t: Table| {
            let mut e = this.shared.emulator.borrow_mut();
            if let Some(list) = t.get::<Option<Vec<String>>>("colours")? {
                for (i, c) in list.iter().enumerate().take(16) {
                    if let Some(c) = parse_colour(c) {
                        e.palette[i] = Some(c);
                    }
                }
            }
            if let Some(c) = t.get::<Option<String>>("selection")?.as_deref().and_then(parse_colour) {
                let alpha = t.get::<Option<f32>>("selection_alpha")?.unwrap_or(0.35);
                e.selection_colour = [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, alpha];
            }
            for (key, named) in [("foreground", NamedColor::Foreground), ("background", NamedColor::Background), ("cursor", NamedColor::Cursor)] {
                if let Some(c) = t.get::<Option<String>>(key)?.as_deref().and_then(parse_colour) {
                    e.palette[named] = Some(c);
                }
            }
            Ok(())
        });
    }
}

/// `g.terminal { pty, cols, rows, history, on_title, on_bell, on_clipboard,
/// on_mark, on_cwd, on_progress, on_notify }`.
pub fn open(t: Table) -> mlua::Result<Terminal> {
    let pty = t.get::<mlua::AnyUserData>("pty").map_err(|_| err("g.terminal: pty is a g.pty"))?;
    let pty = pty.borrow::<Pty>().map_err(|_| err("g.terminal: pty is a g.pty"))?;
    let cols: usize = t.get::<Option<usize>>("cols")?.unwrap_or(80);
    let rows: usize = t.get::<Option<usize>>("rows")?.unwrap_or(24);
    let history: usize = t.get::<Option<usize>>("history")?.unwrap_or(HISTORY);
    let shared = Rc::new(Shared {
        emulator: RefCell::new(Emulator::new(cols, rows, history)),
        pty: pty.inner.clone(),
        changed: Signal::new(0.0),
        count: Cell::new(0.0),
        callbacks: Callbacks {
            on_title: t.get("on_title")?,
            on_bell: t.get("on_bell")?,
            on_clipboard: t.get("on_clipboard")?,
            on_mark: t.get("on_mark")?,
            on_cwd: t.get("on_cwd")?,
            on_progress: t.get("on_progress")?,
            on_notify: t.get("on_notify")?,
            owner: current_owner(),
        },
        cell: Cell::new([0.0; 2]),
    });
    let me = Rc::downgrade(&shared);
    pty.take_output(Box::new(move |data| {
        if let Some(me) = me.upgrade() {
            me.take(data);
        }
    }));
    Ok(Terminal { shared })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_what_spaces_bound() {
        let mut e = Emulator::new(40, 4, 100);
        e.feed(b"echo hello world foo");
        e.term.selection = Some(Selection::new(SelectionType::Semantic, Point::new(Line(0), Column(12)), Side::Left));
        assert_eq!(e.term.selection_to_string().as_deref(), Some("world"));
        e.term.selection = Some(Selection::new(SelectionType::Semantic, Point::new(Line(0), Column(12)), Side::Right));
        assert_eq!(e.term.selection_to_string().as_deref(), Some("world"));
    }
}
