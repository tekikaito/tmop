//! Egui view of a terminal session: draws an [`Emulator`] into a rectangle and turns
//! keyboard, mouse and paste input into bytes for a [`Transport`].
//!
//! The pure helpers (colour resolution, run building, grid sizing) take no egui context
//! and are covered by the unit tests at the bottom of the file.

mod altgr;

use crate::terminal::emulator::{Flags, NamedColor};
use crate::terminal::{
    encode_key, encode_paste, encode_text, CellColor, CellInfo, CursorShape, Emulator, KeyModes, SelectionKind, Transport, TransportState,
};
use crate::ui::theme::{self, Palette};
use crate::ui::widgets;
use altgr::altgr_char;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Event, EventFilter, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Inset between the widget rect and the character grid.
const PAD: f32 = 4.0;
/// Smallest grid the emulator is ever resized to.
const MIN_GRID: usize = 2;
/// Font size of a new view.
const DEFAULT_FONT_SIZE: f32 = 13.0;
/// Factor applied to dimmed colours.
const DIM_FACTOR: f32 = 0.6;
/// How long the frame flashes after a bell.
const BELL_FLASH: Duration = Duration::from_millis(150);
/// Repaint rate while the transport is still starting.
const STARTING_POLL: Duration = Duration::from_millis(100);
/// Thickness of underline and beam cursors.
const BAR: f32 = 2.0;
/// Ctrl+C.
const ETX: u8 = 0x03;
/// Ctrl+X.
const CAN: u8 = 0x18;

/// The 16 ANSI colours on the dark theme: normal, then bright.
const ANSI_DARK: [[u8; 3]; 16] = [
    [0x1e, 0x1e, 0x1e],
    [0xf1, 0x4c, 0x4c],
    [0x23, 0xd1, 0x8b],
    [0xf5, 0xf5, 0x43],
    [0x3b, 0x8e, 0xea],
    [0xd6, 0x70, 0xd6],
    [0x29, 0xb8, 0xdb],
    [0xe5, 0xe5, 0xe5],
    [0x66, 0x66, 0x66],
    [0xff, 0x6b, 0x6b],
    [0x5f, 0xf0, 0xa8],
    [0xff, 0xff, 0x8a],
    [0x6a, 0xa9, 0xff],
    [0xe8, 0x8a, 0xe8],
    [0x5f, 0xd7, 0xf0],
    [0xff, 0xff, 0xff],
];

/// The 16 ANSI colours on the light theme: darker and more saturated.
const ANSI_LIGHT: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xc5, 0x0f, 0x1f],
    [0x10, 0x7c, 0x10],
    [0x9d, 0x5d, 0x00],
    [0x00, 0x37, 0xda],
    [0x88, 0x17, 0x98],
    [0x3a, 0x96, 0xdd],
    [0x76, 0x76, 0x76],
    [0x76, 0x76, 0x76],
    [0xe7, 0x48, 0x56],
    [0x16, 0xc6, 0x0c],
    [0xb4, 0xa0, 0x00],
    [0x3b, 0x78, 0xff],
    [0xb4, 0x00, 0x9e],
    [0x61, 0xd6, 0xd6],
    [0x3a, 0x3a, 0x3a],
];

/// Colour levels of the 6x6x6 colour cube (indexed colours 16..=231).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The normal ANSI colours, in index order.
const NORMAL: [NamedColor; 8] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
];

/// The bright ANSI colours, in index order (8..=15).
const BRIGHT: [NamedColor; 8] = [
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
];

/// The dimmed ANSI colours, in index order.
const DIMMED: [NamedColor; 8] = [
    NamedColor::DimBlack,
    NamedColor::DimRed,
    NamedColor::DimGreen,
    NamedColor::DimYellow,
    NamedColor::DimBlue,
    NamedColor::DimMagenta,
    NamedColor::DimCyan,
    NamedColor::DimWhite,
];

/// Per-view state of a terminal widget (one per session tab).
pub struct TerminalView {
    id_salt: String,
    grid: Option<(usize, usize)>,
    font_size: f32,
    /// Wheel movement in lines that has not yet scrolled a whole line.
    wheel_lines: f32,
    want_focus: bool,
    bell_until: Option<Instant>,
}

impl TerminalView {
    /// A view whose widget id is derived from `id_salt`.
    pub fn new(id_salt: impl Into<String>) -> Self {
        Self {
            id_salt: id_salt.into(),
            grid: None,
            font_size: DEFAULT_FONT_SIZE,
            wheel_lines: 0.0,
            want_focus: false,
            bell_until: None,
        }
    }

    /// Presets the grid the emulator was created with, so the first [`TerminalView::show`]
    /// does not resize it again when the size matches.
    pub fn with_grid(mut self, grid: (usize, usize)) -> Self {
        self.grid = Some(grid);
        self
    }

    /// Last computed grid size in cells, `(cols, rows)`.
    pub fn grid(&self) -> Option<(usize, usize)> {
        self.grid
    }

    /// Focuses the terminal on the next [`TerminalView::show`].
    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    /// Draws the terminal into `rect` and handles its input.
    ///
    /// Keeps the emulator and the transport sized to `rect`. Returns true when the user
    /// clicked or typed in the terminal, so the caller can mark the tab active.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, pal: &Palette, emulator: &Arc<Mutex<Emulator>>, transport: &mut dyn Transport) -> bool {
        let font = theme::mono(self.font_size);
        let (cell_w, row_h) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let inner = rect.shrink(PAD);
        let (cols, rows) = grid_for(ui, rect);
        if self.grid != Some((cols, rows)) {
            lock(emulator).resize(cols, rows);
            transport.resize(cols as u16, rows as u16);
            self.grid = Some((cols, rows));
        }

        let id = ui.make_persistent_id(&self.id_salt);
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        if self.want_focus {
            resp.request_focus();
            self.want_focus = false;
        }
        let clicked = resp.clicked() || resp.drag_started() || resp.secondary_clicked();
        if clicked {
            resp.request_focus();
        }
        let focused = resp.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(id, EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
            });
        }
        let running = transport.state() == TransportState::Running;
        let pointer_cell = |p: Pos2| cell_at(p, inner, cell_w, row_h, cols, rows);

        let mut out = Vec::new();
        {
            let mut emu = lock(emulator);
            let modes = KeyModes { app_cursor: emu.app_cursor(), app_keypad: emu.app_keypad() };

            // Mouse wheel: scrolls the history, or sends arrow keys on the alternate screen
            // (less, vim, ...). Fractional lines accumulate.
            if resp.hovered() {
                let dy = ui.input(|i| i.smooth_scroll_delta.y);
                if dy != 0.0 {
                    self.wheel_lines += dy / row_h.max(1.0);
                    let whole = self.wheel_lines.trunc();
                    if whole != 0.0 {
                        self.wheel_lines -= whole;
                        if emu.alt_screen() && !emu.mouse_mode() {
                            out.extend(wheel_arrows(whole as i32, modes));
                        } else {
                            emu.scroll(whole as i32);
                        }
                    }
                }
            }

            // Mouse selection.
            let at_pointer = resp.interact_pointer_pos().map(pointer_cell);
            if resp.triple_clicked() || resp.double_clicked() {
                let kind = if resp.triple_clicked() { SelectionKind::Line } else { SelectionKind::Word };
                if let Some((c, r)) = at_pointer {
                    emu.selection_start(c, r, kind);
                }
            } else if resp.drag_started() {
                let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
                if let Some((c, r)) = origin.map(pointer_cell) {
                    emu.selection_start(c, r, SelectionKind::Simple);
                }
            } else if resp.dragged() {
                if let Some((c, r)) = at_pointer {
                    emu.selection_update(c, r);
                }
            } else if resp.clicked() {
                emu.selection_clear();
            }
            if resp.secondary_clicked() {
                copy_selection(ui, &mut emu);
            }

            // Keyboard and paste, only while focused.
            if focused {
                let bracketed = emu.bracketed_paste();
                let events = ui.input(|i| i.events.clone());
                let mods = ui.input(|i| i.modifiers);
                let mut router = KeyRouter::new(KeyPolicy::NATIVE, modes, bracketed);
                for event in &events {
                    match router.route(event, mods, altgr_char) {
                        Input::Send(bytes) => out.extend(bytes),
                        Input::Copy => {
                            copy_selection(ui, &mut emu);
                        }
                        Input::InterruptOrCopy { shift } => interrupt_or_copy(ui, &mut emu, shift, &mut out),
                        Input::Nothing => {}
                    }
                }
            }

            if running && !out.is_empty() {
                emu.scroll_to_bottom();
            }
            let replies = emu.take_pty_writes();
            if emu.take_bell() {
                self.bell_until = Some(Instant::now() + BELL_FLASH);
            }
            drop(emu);

            if running {
                if !replies.is_empty() {
                    transport.write(&replies);
                }
                if !out.is_empty() {
                    transport.write(&out);
                }
            }
        }

        self.paint(ui, rect, inner, pal, emulator, transport, &font, cell_w, row_h, focused);
        clicked || !out.is_empty()
    }

    /// Paints the frame, the cells, the cursor, the scrollback label and the state overlay.
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        inner: Rect,
        pal: &Palette,
        emulator: &Arc<Mutex<Emulator>>,
        transport: &dyn Transport,
        font: &FontId,
        cell_w: f32,
        row_h: f32,
        focused: bool,
    ) {
        let bg = terminal_bg(pal);
        let flashing = match self.bell_until {
            Some(until) if Instant::now() < until => {
                ui.ctx().request_repaint_after(until.saturating_duration_since(Instant::now()));
                true
            }
            _ => {
                self.bell_until = None;
                false
            }
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, bg);
        let frame = if flashing { pal.accent } else { pal.panel_stroke };
        painter.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, frame));

        let (cells, cursor, offset, history) = {
            let emu = lock(emulator);
            (emu.cells(), emu.cursor(), emu.display_offset(), emu.history_size())
        };
        let clip = painter.with_clip_rect(inner);
        let runs = build_runs(&cells, pal);
        paint_runs(&clip, inner.min, cell_w, row_h, &runs, font);

        if let Some(cur) = cursor {
            let origin = pos2(inner.min.x + cur.col as f32 * cell_w, inner.min.y + cur.row as f32 * row_h);
            let cols = self.grid.map_or(0, |(c, _)| c);
            let under = cell_index(cur.row, cur.col, cols, cells.len())
                .and_then(|i| cells.get(i))
                .filter(|c| c.row == cur.row && c.col == cur.col);
            let width = if under.is_some_and(|c| c.wide) { 2.0 * cell_w } else { cell_w };
            let cell_rect = Rect::from_min_size(origin, vec2(width, row_h));
            match cur.shape {
                CursorShape::Block if focused => {
                    clip.rect_filled(cell_rect, 0.0, pal.accent);
                    if let Some(c) = under.map(|c| c.ch).filter(|c| *c != ' ') {
                        clip.text(origin, Align2::LEFT_TOP, c, font.clone(), bg);
                    }
                }
                CursorShape::Block => {
                    clip.rect_stroke(cell_rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, pal.accent));
                }
                CursorShape::Underline => {
                    clip.rect_filled(Rect::from_min_size(pos2(origin.x, cell_rect.bottom() - BAR), vec2(width, BAR)), 0.0, pal.accent);
                }
                CursorShape::Beam => {
                    clip.rect_filled(Rect::from_min_size(origin, vec2(BAR, row_h)), 0.0, pal.accent);
                }
                CursorShape::Hidden => {}
            }
        }

        if offset > 0 {
            let label = format!("\u{2191} {offset}/{history}");
            let label_font = theme::ui(11.0);
            let w = widgets::text_width(painter, &label, label_font.clone());
            let pill = Rect::from_min_size(pos2(inner.max.x - w - 14.0, inner.min.y + 4.0), vec2(w + 10.0, 16.0));
            painter.rect_filled(pill, 4.0, pal.panel);
            widgets::text(painter, pill.center(), Align2::CENTER_CENTER, &label, label_font, pal.text_dim);
        }

        match transport.state() {
            TransportState::Starting => {
                widgets::text(painter, inner.center(), Align2::CENTER_CENTER, "Connecting\u{2026}", theme::ui(13.0), pal.text_dim);
                ui.ctx().request_repaint_after(STARTING_POLL);
            }
            TransportState::Ended(text) => {
                let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - row_h - 6.0), rect.max);
                painter.rect_filled(bar, 0.0, pal.panel);
                painter.line_segment([bar.left_top(), bar.right_top()], Stroke::new(1.0_f32, pal.panel_stroke));
                let msg = format!("Session ended \u{2014} {text}");
                widgets::text(painter, pos2(bar.min.x + PAD + 2.0, bar.center().y), Align2::LEFT_CENTER, &msg, theme::ui(12.0), pal.warn);
            }
            TransportState::Running => {}
        }
    }
}

/// Locks the emulator, ignoring poisoning.
fn lock(emulator: &Arc<Mutex<Emulator>>) -> MutexGuard<'_, Emulator> {
    emulator.lock().unwrap_or_else(|e| e.into_inner())
}

/// Copies the selection to the clipboard and clears it. Returns false when there is none.
fn copy_selection(ui: &egui::Ui, emu: &mut Emulator) -> bool {
    match emu.selection_text().filter(|t| !t.is_empty()) {
        Some(text) => {
            ui.output_mut(|o| o.copied_text = text);
            emu.selection_clear();
            true
        }
        None => false,
    }
}

/// Ctrl+C: copies a selection, otherwise sends an interrupt unless Shift is held.
fn interrupt_or_copy(ui: &egui::Ui, emu: &mut Emulator, shift: bool, out: &mut Vec<u8>) {
    if !copy_selection(ui, emu) && !shift {
        out.push(ETX);
    }
}

/// Background of the terminal: the darker of the graph and panel colours.
fn terminal_bg(pal: &Palette) -> Color32 {
    let graph = pal.graph_bg();
    if luma(graph) <= luma(pal.panel) {
        graph
    } else {
        pal.panel
    }
}

fn luma(c: Color32) -> u32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    r as u32 + g as u32 + b as u32
}

/// Character cells that fit in the widget `rect` at the default font, as [`TerminalView::show`]
/// computes them. Use it to open a session at the size the view will show.
pub fn grid_for(ui: &egui::Ui, rect: Rect) -> (usize, usize) {
    let font = theme::mono(DEFAULT_FONT_SIZE);
    let (cell_w, row_h) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
    grid_size(rect.shrink(PAD), cell_w, row_h)
}

/// Character cells that fit in `inner`, at least [`MIN_GRID`] in each direction.
fn grid_size(inner: Rect, cell_w: f32, row_h: f32) -> (usize, usize) {
    let cols = (inner.width() / cell_w.max(1.0)).floor().max(0.0) as usize;
    let rows = (inner.height() / row_h.max(1.0)).floor().max(0.0) as usize;
    (cols.max(MIN_GRID), rows.max(MIN_GRID))
}

/// Grid cell under `p`, clamped to the grid.
fn cell_at(p: Pos2, inner: Rect, cell_w: f32, row_h: f32, cols: usize, rows: usize) -> (usize, usize) {
    let col = ((p.x - inner.min.x) / cell_w.max(1.0)).floor().clamp(0.0, (cols - 1) as f32) as usize;
    let row = ((p.y - inner.min.y) / row_h.max(1.0)).floor().clamp(0.0, (rows - 1) as f32) as usize;
    (col, row)
}

/// Arrow keys for `lines` of wheel movement, three per line: up for a positive count.
fn wheel_arrows(lines: i32, modes: KeyModes) -> Vec<u8> {
    let key = if lines > 0 { Key::ArrowUp } else { Key::ArrowDown };
    let arrow = encode_key(key, Modifiers::NONE, modes).unwrap_or_default();
    arrow.repeat(3 * lines.unsigned_abs() as usize)
}

/// Index of row `row`, column `col` in a row-major grid of `cols` columns and `len` cells.
fn cell_index(row: usize, col: usize, cols: usize, len: usize) -> Option<usize> {
    if col >= cols {
        return None;
    }
    let index = row.checked_mul(cols)?.checked_add(col)?;
    (index < len).then_some(index)
}

/// How modifier keys reach the terminal.
///
/// * `Pc` (Windows, Linux): Ctrl+C copies a selection or interrupts, Ctrl+Insert copies, Alt+key
///   is Meta (ESC prefix), and Windows' AltGr (Ctrl+Alt) types the layout's character.
/// * `Mac`: Cmd belongs to the app (Cmd+C copies, Cmd+V pastes, other Cmd shortcuts are not
///   typed), Ctrl+key sends control codes (Ctrl+C always interrupts), and Option types the
///   layout's characters (`@`, `{`, `|` on many layouts) instead of acting as Meta; Option still
///   modifies arrows, Backspace and the other named keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPolicy {
    Pc,
    Mac,
}

impl KeyPolicy {
    /// The policy of this build's OS.
    pub const NATIVE: KeyPolicy = if cfg!(target_os = "macos") { KeyPolicy::Mac } else { KeyPolicy::Pc };
}

/// What one input event asks of the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Send(Vec<u8>),
    /// Copy the selection, if any.
    Copy,
    /// Copy the selection, else interrupt (unless Shift is held).
    InterruptOrCopy { shift: bool },
    Nothing,
}

/// Turns one frame's input events into [`Input`]s under a [`KeyPolicy`].
struct KeyRouter {
    policy: KeyPolicy,
    modes: KeyModes,
    bracketed: bool,
    /// An Alt+letter went out as a key (ESC+letter); its text event is a duplicate.
    alt_key_sent: bool,
}

impl KeyRouter {
    fn new(policy: KeyPolicy, modes: KeyModes, bracketed: bool) -> Self {
        Self { policy, modes, bracketed, alt_key_sent: false }
    }

    /// `event` under the frame's modifiers `mods`; `altgr` gives the layout's AltGr character
    /// of a physical key (Windows).
    fn route(&mut self, event: &Event, mods: Modifiers, altgr: impl Fn(Key) -> Option<char>) -> Input {
        if let Event::Paste(text) = event {
            return Input::Send(encode_paste(text, self.bracketed));
        }
        match self.policy {
            KeyPolicy::Pc => self.route_pc(event, mods, altgr),
            KeyPolicy::Mac => self.route_mac(event),
        }
    }

    fn route_pc(&mut self, event: &Event, mods: Modifiers, altgr: impl Fn(Key) -> Option<char>) -> Input {
        match event {
            Event::Copy => Input::InterruptOrCopy { shift: mods.shift },
            Event::Cut => {
                // Shift+Delete arrives as a cut on Windows; without Ctrl it is a forward delete.
                if !mods.ctrl {
                    Input::Send(encode_key(Key::Delete, Modifiers::NONE, self.modes).unwrap_or_default())
                } else if !mods.shift {
                    Input::Send(vec![CAN])
                } else {
                    Input::Nothing
                }
            }
            Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                let ctrl = modifiers.ctrl || modifiers.command;
                // Windows reports AltGr as Ctrl+Alt, and egui-winit sends no text for it.
                // The character comes from the layout; other keys still go through encode_key,
                // and letters without an AltGr character are dropped so they never become ESC+Ctrl+letter.
                if ctrl && modifiers.alt {
                    if let Some(c) = altgr(physical_key.unwrap_or(*key)) {
                        return Input::Send(encode_text(&c.to_string(), Modifiers::NONE));
                    }
                    if is_letter(*key) {
                        return Input::Nothing;
                    }
                }
                match key {
                    Key::C if ctrl => Input::InterruptOrCopy { shift: modifiers.shift },
                    Key::Insert if ctrl => Input::Copy,
                    Key::V if ctrl => Input::Nothing,
                    _ => match encode_key(*key, *modifiers, self.modes) {
                        Some(bytes) => {
                            self.alt_key_sent |= modifiers.alt && !ctrl && is_letter(*key);
                            Input::Send(bytes)
                        }
                        None => Input::Nothing,
                    },
                }
            }
            Event::Text(text) => {
                // Alt+letter is already sent as a key; skip the duplicate text.
                let alt_only = mods.alt && !mods.ctrl;
                if alt_only && self.alt_key_sent && is_ascii_letter(text) {
                    self.alt_key_sent = false;
                    return Input::Nothing;
                }
                Input::Send(encode_text(text, Modifiers { alt: alt_only, ..Modifiers::NONE }))
            }
            _ => Input::Nothing,
        }
    }

    fn route_mac(&mut self, event: &Event) -> Input {
        match event {
            // Cmd+C and Cmd+X: a terminal cannot cut, both copy.
            Event::Copy | Event::Cut => Input::Copy,
            Event::Key { key, pressed: true, modifiers, .. } => {
                if modifiers.mac_cmd || modifiers.command {
                    return Input::Nothing;
                }
                // Option composes a character, which arrives as text.
                if modifiers.alt && !modifiers.ctrl && types_text(*key) {
                    return Input::Nothing;
                }
                encode_key(*key, *modifiers, self.modes).map_or(Input::Nothing, Input::Send)
            }
            Event::Text(text) => Input::Send(encode_text(text, Modifiers::NONE)),
            _ => Input::Nothing,
        }
    }
}

/// Keys that type a character (letters, digits, punctuation, space), as opposed to named keys.
fn types_text(key: Key) -> bool {
    use Key::*;
    is_letter(key)
        || matches!(
            key,
            Num0 | Num1 | Num2 | Num3 | Num4 | Num5 | Num6 | Num7 | Num8 | Num9 | Space | Colon | Comma | Minus | Period | Plus | Equals | Semicolon | Backslash | Slash | Pipe | Questionmark | OpenBracket | CloseBracket | Backtick | Quote
        )
}

fn is_letter(key: Key) -> bool {
    matches!(key.name().as_bytes(), [b'A'..=b'Z'])
}

fn is_ascii_letter(text: &str) -> bool {
    text.len() == 1 && text.as_bytes()[0].is_ascii_alphabetic()
}

/// Position of an ANSI colour: index 0..=15 and whether it is a dimmed variant.
fn ansi_position(n: NamedColor) -> Option<(u8, bool)> {
    let find = |list: &[NamedColor; 8]| list.iter().position(|c| *c == n).map(|i| i as u8);
    if let Some(i) = find(&NORMAL) {
        Some((i, false))
    } else if let Some(i) = find(&BRIGHT) {
        Some((i + 8, false))
    } else {
        find(&DIMMED).map(|i| (i, true))
    }
}

/// RGB of a 256-colour index. 0..=15 follow the theme's ANSI table.
fn indexed_rgb(n: u8, dark: bool) -> Color32 {
    match n {
        0..=15 => {
            let [r, g, b] = if dark { ANSI_DARK[n as usize] } else { ANSI_LIGHT[n as usize] };
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let i = (n - 16) as usize;
            Color32::from_rgb(CUBE_LEVELS[i / 36], CUBE_LEVELS[(i / 6) % 6], CUBE_LEVELS[i % 6])
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn named_color(n: NamedColor, pal: &Palette) -> Color32 {
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => pal.text,
        NamedColor::DimForeground => theme::dim(pal.text, DIM_FACTOR),
        NamedColor::Background => terminal_bg(pal),
        _ => match ansi_position(n) {
            Some((i, dim)) => {
                let c = indexed_rgb(i, pal.dark);
                if dim {
                    theme::dim(c, DIM_FACTOR)
                } else {
                    c
                }
            }
            None => pal.text,
        },
    }
}

/// Concrete colour of a cell colour. `is_fg` picks the default foreground or background.
pub fn resolve(c: CellColor, pal: &Palette, is_fg: bool) -> Color32 {
    match c {
        CellColor::Default if is_fg => pal.text,
        CellColor::Default => terminal_bg(pal),
        CellColor::Named(n) => named_color(n, pal),
        CellColor::Indexed(n) => indexed_rgb(n, pal.dark),
        CellColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// Bold text in a normal ANSI colour uses the bright variant.
fn brighten(c: CellColor) -> CellColor {
    match c {
        CellColor::Named(n) => match ansi_position(n) {
            Some((i, false)) if i < 8 => CellColor::Indexed(i + 8),
            _ => c,
        },
        _ => c,
    }
}

/// Visual attributes shared by a run of cells.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Style {
    fg: Color32,
    /// `None` leaves the terminal background showing.
    bg: Option<Color32>,
    underline: bool,
    strike: bool,
}

/// Consecutive cells of one row with the same style, drawn as one text string.
#[derive(Clone, Debug, PartialEq)]
struct Run {
    row: usize,
    col: usize,
    /// Cells covered, two for a wide character.
    cols: usize,
    text: String,
    style: Style,
}

fn cell_style(cell: &CellInfo, pal: &Palette) -> Style {
    let flags = cell.flags;
    let fg_src = if flags.contains(Flags::BOLD) { brighten(cell.fg) } else { cell.fg };
    let (mut fg, bg) = if flags.contains(Flags::INVERSE) {
        let fg = match cell.bg {
            CellColor::Default => terminal_bg(pal),
            c => resolve(c, pal, false),
        };
        let bg = match fg_src {
            CellColor::Default => pal.text,
            c => resolve(c, pal, true),
        };
        (fg, Some(bg))
    } else {
        let fg = resolve(fg_src, pal, true);
        let bg = match cell.bg {
            CellColor::Default | CellColor::Named(NamedColor::Background) => None,
            c => Some(resolve(c, pal, false)),
        };
        (fg, bg)
    };
    if flags.contains(Flags::DIM) {
        fg = theme::dim(fg, DIM_FACTOR);
    }
    let bg = if cell.selected { Some(pal.selection) } else { bg };
    Style { fg, bg, underline: flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE), strike: flags.contains(Flags::STRIKEOUT) }
}

/// Groups cells (row-major) into runs. Wide characters get a run of their own, spacer
/// cells are skipped, and blank cells without decoration are left out.
fn build_runs(cells: &[CellInfo], pal: &Palette) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for cell in cells {
        if cell.spacer {
            continue;
        }
        let style = cell_style(cell, pal);
        let ch = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.ch };
        if cell.wide {
            runs.push(Run { row: cell.row, col: cell.col, cols: 2, text: ch.to_string(), style });
            continue;
        }
        if ch == ' ' && style.bg.is_none() && !style.underline && !style.strike {
            continue;
        }
        if let Some(last) = runs.last_mut() {
            let plain = last.cols == last.text.chars().count();
            if plain && last.row == cell.row && last.col + last.cols == cell.col && last.style == style {
                last.text.push(ch);
                last.cols += 1;
                continue;
            }
        }
        runs.push(Run { row: cell.row, col: cell.col, cols: 1, text: ch.to_string(), style });
    }
    runs
}

/// Paints run backgrounds first, then the text, decorations last.
fn paint_runs(painter: &egui::Painter, origin: Pos2, cell_w: f32, row_h: f32, runs: &[Run], font: &FontId) {
    let rect_of = |run: &Run| {
        let pos = pos2(origin.x + run.col as f32 * cell_w, origin.y + run.row as f32 * row_h);
        (pos, Rect::from_min_size(pos, vec2(run.cols as f32 * cell_w, row_h)))
    };
    for run in runs {
        if let Some(bg) = run.style.bg {
            painter.rect_filled(rect_of(run).1, 0.0, bg);
        }
    }
    for run in runs {
        let (pos, rect) = rect_of(run);
        if run.text.chars().any(|c| c != ' ') {
            painter.text(pos, Align2::LEFT_TOP, &run.text, font.clone(), run.style.fg);
        }
        let stroke = Stroke::new(1.0_f32, run.style.fg);
        if run.style.underline {
            let y = rect.bottom() - 1.0;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        }
        if run.style.strike {
            let y = rect.center().y;
            painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(col: usize, ch: char) -> CellInfo {
        CellInfo { col, row: 0, ch, fg: CellColor::Default, bg: CellColor::Default, flags: Flags::empty(), selected: false, wide: false, spacer: false }
    }

    #[test]
    fn indexed_grey_ramp_and_cube_endpoints() {
        let pal = &theme::DARK;
        assert_eq!(resolve(CellColor::Indexed(16), pal, true), Color32::from_rgb(0, 0, 0));
        assert_eq!(resolve(CellColor::Indexed(231), pal, true), Color32::from_rgb(255, 255, 255));
        assert_eq!(resolve(CellColor::Indexed(232), pal, true), Color32::from_rgb(8, 8, 8));
        assert_eq!(resolve(CellColor::Indexed(255), pal, true), Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn rgb_passes_through() {
        assert_eq!(resolve(CellColor::Rgb(1, 2, 3), &theme::LIGHT, false), Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn defaults_follow_the_palette() {
        assert_eq!(resolve(CellColor::Default, &theme::DARK, true), theme::DARK.text);
        assert_eq!(resolve(CellColor::Default, &theme::DARK, false), terminal_bg(&theme::DARK));
    }

    #[test]
    fn named_colours_use_the_theme_table() {
        assert_eq!(resolve(CellColor::Named(NamedColor::Red), &theme::DARK, true), Color32::from_rgb(0xf1, 0x4c, 0x4c));
        assert_eq!(resolve(CellColor::Named(NamedColor::BrightWhite), &theme::DARK, true), Color32::from_rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn runs_split_on_style_and_gaps() {
        let pal = &theme::DARK;
        let mut red = cell(4, 'c');
        red.fg = CellColor::Named(NamedColor::Red);
        let mut red_d = cell(5, 'd');
        red_d.fg = CellColor::Named(NamedColor::Red);
        let cells = vec![cell(0, 'a'), cell(1, 'b'), cell(2, ' '), cell(3, ' '), red, red_d];
        let runs = build_runs(&cells, pal);
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].col, runs[0].cols, runs[0].text.as_str()), (0, 2, "ab"));
        assert_eq!((runs[1].col, runs[1].cols, runs[1].text.as_str()), (4, 2, "cd"));
        assert_eq!(runs[1].style.fg, resolve(CellColor::Named(NamedColor::Red), pal, true));
    }

    #[test]
    fn blanks_inside_a_styled_run_are_kept() {
        let pal = &theme::DARK;
        let mut sel_a = cell(0, 'a');
        sel_a.selected = true;
        let mut sel_space = cell(1, ' ');
        sel_space.selected = true;
        let runs = build_runs(&[sel_a, sel_space, cell(2, 'b')], pal);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "a ");
        assert_eq!(runs[0].cols, 2);
        assert_eq!(runs[0].style.bg, Some(pal.selection));
        assert_eq!(runs[1].text, "b");
    }

    #[test]
    fn wide_characters_get_their_own_run() {
        let mut wide = cell(1, '\u{4e2d}');
        wide.wide = true;
        let mut spacer = cell(2, ' ');
        spacer.spacer = true;
        let runs = build_runs(&[cell(0, 'a'), wide, spacer, cell(3, 'b')], &theme::DARK);
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].col, runs[0].cols, runs[0].text.as_str()), (0, 1, "a"));
        assert_eq!((runs[1].col, runs[1].cols, runs[1].text.as_str()), (1, 2, "\u{4e2d}"));
        assert_eq!((runs[2].col, runs[2].cols, runs[2].text.as_str()), (3, 1, "b"));
    }

    #[test]
    fn inverse_swaps_default_colours() {
        let pal = &theme::DARK;
        let mut c = cell(0, 'x');
        c.flags = Flags::INVERSE;
        let style = cell_style(&c, pal);
        assert_eq!(style.fg, terminal_bg(pal));
        assert_eq!(style.bg, Some(pal.text));
    }

    #[test]
    fn grid_and_pointer_mapping() {
        let inner = Rect::from_min_size(pos2(10.0, 10.0), vec2(80.0 * 8.0, 24.0 * 16.0));
        assert_eq!(grid_size(inner, 8.0, 16.0), (80, 24));
        assert_eq!(grid_size(Rect::from_min_size(pos2(0.0, 0.0), vec2(3.0, 3.0)), 8.0, 16.0), (MIN_GRID, MIN_GRID));
        assert_eq!(cell_at(pos2(10.0 + 17.0, 10.0 + 33.0), inner, 8.0, 16.0, 80, 24), (2, 2));
        assert_eq!(cell_at(pos2(-500.0, 9999.0), inner, 8.0, 16.0, 80, 24), (0, 23));
    }

    #[test]
    fn cell_index_is_row_major_and_bounded() {
        assert_eq!(cell_index(0, 0, 80, 80 * 24), Some(0));
        assert_eq!(cell_index(2, 3, 80, 80 * 24), Some(163));
        assert_eq!(cell_index(23, 79, 80, 80 * 24), Some(1919));
        assert_eq!(cell_index(24, 0, 80, 80 * 24), None);
        assert_eq!(cell_index(0, 80, 80, 80 * 24), None);
        assert_eq!(cell_index(0, 0, 0, 0), None);
    }

    #[test]
    fn wheel_sends_three_arrows_per_line() {
        let normal = KeyModes::default();
        assert_eq!(wheel_arrows(1, normal), b"\x1b[A\x1b[A\x1b[A".to_vec());
        assert_eq!(wheel_arrows(-2, normal), b"\x1b[B".repeat(6));
        let app = KeyModes { app_cursor: true, app_keypad: false };
        assert_eq!(wheel_arrows(1, app), b"\x1bOA\x1bOA\x1bOA".to_vec());
        assert!(wheel_arrows(0, normal).is_empty());
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }
    }

    fn text(t: &str) -> Event {
        Event::Text(t.into())
    }

    /// Routes `events` (each with the frame's modifiers) through one router.
    fn route(policy: KeyPolicy, events: &[(Event, Modifiers)], altgr: impl Fn(Key) -> Option<char> + Copy) -> Vec<Input> {
        let mut router = KeyRouter::new(policy, KeyModes::default(), false);
        events.iter().map(|(e, m)| router.route(e, *m, altgr)).collect()
    }

    const NO_ALTGR: fn(Key) -> Option<char> = |_| None;
    const CTRL: Modifiers = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    const ALT: Modifiers = Modifiers { alt: true, ..Modifiers::NONE };
    const CMD: Modifiers = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
    const MAC_CTRL: Modifiers = Modifiers { ctrl: true, ..Modifiers::NONE };
    const OPTION: Modifiers = Modifiers { alt: true, ..Modifiers::NONE };

    /// Windows and Linux: Ctrl+C copies or interrupts, Alt is Meta with no duplicate text, AltGr
    /// types the layout's character, Ctrl+V leaves the paste to the paste event.
    #[test]
    fn pc_keys() {
        let p = KeyPolicy::Pc;
        assert_eq!(route(p, &[(Event::Copy, Modifiers::NONE), (key(Key::C, CTRL), CTRL)], NO_ALTGR), [Input::InterruptOrCopy { shift: false }, Input::InterruptOrCopy { shift: false }]);
        let shift_ctrl = Modifiers { shift: true, ..CTRL };
        assert_eq!(route(p, &[(key(Key::C, shift_ctrl), shift_ctrl)], NO_ALTGR), [Input::InterruptOrCopy { shift: true }]);
        assert_eq!(route(p, &[(key(Key::Insert, CTRL), CTRL), (key(Key::V, CTRL), CTRL)], NO_ALTGR), [Input::Copy, Input::Nothing]);
        assert_eq!(route(p, &[(key(Key::A, CTRL), CTRL)], NO_ALTGR), [Input::Send(vec![0x01])]);
        assert_eq!(route(p, &[(key(Key::X, ALT), ALT), (text("x"), ALT), (text("y"), ALT)], NO_ALTGR), [Input::Send(b"\x1bx".to_vec()), Input::Nothing, Input::Send(b"\x1by".to_vec())]);
        let altgr = Modifiers { alt: true, ..CTRL };
        assert_eq!(route(p, &[(key(Key::Q, altgr), altgr)], |k| (k == Key::Q).then_some('@')), [Input::Send(b"@".to_vec())]);
        assert_eq!(route(p, &[(key(Key::Q, altgr), altgr)], NO_ALTGR), [Input::Nothing], "never ESC+Ctrl+letter");
        assert_eq!(route(p, &[(Event::Cut, Modifiers { shift: true, ..Modifiers::NONE }), (Event::Cut, CTRL)], NO_ALTGR), [Input::Send(b"\x1b[3~".to_vec()), Input::Send(vec![CAN])]);
        assert_eq!(route(p, &[(text("\u{e9}"), Modifiers::NONE), (Event::Paste("a\nb".into()), Modifiers::NONE)], NO_ALTGR), [Input::Send("\u{e9}".as_bytes().to_vec()), Input::Send(b"a\rb".to_vec())]);
    }

    /// macOS: Cmd is the app's, Ctrl sends control codes, Option types characters.
    #[test]
    fn mac_keys() {
        let p = KeyPolicy::Mac;
        // Cmd+C / Cmd+X arrive as copy and cut events and only copy; Ctrl+C always interrupts.
        assert_eq!(route(p, &[(Event::Copy, CMD), (Event::Cut, CMD), (key(Key::C, MAC_CTRL), MAC_CTRL)], NO_ALTGR), [Input::Copy, Input::Copy, Input::Send(vec![ETX])]);
        // Other Cmd shortcuts type nothing.
        assert_eq!(route(p, &[(key(Key::K, CMD), CMD), (key(Key::ArrowLeft, CMD), CMD)], NO_ALTGR), [Input::Nothing, Input::Nothing]);
        // Option+L types the layout's "@" (German), with no ESC prefix; Option+Left is still Alt+Left.
        let got = route(p, &[(key(Key::L, OPTION), OPTION), (text("@"), OPTION), (key(Key::Num7, OPTION), OPTION), (text("|"), OPTION), (key(Key::ArrowLeft, OPTION), OPTION)], NO_ALTGR);
        assert_eq!(got, [Input::Nothing, Input::Send(b"@".to_vec()), Input::Nothing, Input::Send(b"|".to_vec()), Input::Send(b"\x1b[1;3D".to_vec())]);
        assert_eq!(route(p, &[(key(Key::Backspace, OPTION), OPTION), (key(Key::Enter, Modifiers::NONE), Modifiers::NONE)], NO_ALTGR), [Input::Send(vec![0x1b, 0x7f]), Input::Send(b"\r".to_vec())]);
        // Ctrl+Option+letter is still a control code with Meta.
        let ctrl_opt = Modifiers { alt: true, ..MAC_CTRL };
        assert_eq!(route(p, &[(key(Key::A, ctrl_opt), ctrl_opt)], NO_ALTGR), [Input::Send(vec![0x1b, 0x01])]);
        assert_eq!(route(p, &[(Event::Paste("p".into()), CMD)], NO_ALTGR), [Input::Send(b"p".to_vec())]);
    }

    #[test]
    fn native_policy_matches_the_target() {
        #[cfg(target_os = "macos")]
        assert_eq!(KeyPolicy::NATIVE, KeyPolicy::Mac);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(KeyPolicy::NATIVE, KeyPolicy::Pc);
    }
}
