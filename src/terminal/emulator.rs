//! VT emulation on top of `alacritty_terminal`.
//!
//! An [`Emulator`] owns one terminal grid and its parser. It does no locking; callers
//! share it behind a `Mutex`. Coordinates handed in and out are viewport coordinates:
//! `row` 0 is the top of the visible area, which is shifted into the scrollback by
//! [`Emulator::scroll`].

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape as AnsiCursorShape, Processor};
use std::sync::{Arc, Mutex};

pub use alacritty_terminal::term::cell::Flags;
pub use alacritty_terminal::vte::ansi::NamedColor;

/// Resolved colour of a cell. Palette lookups are left to the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    /// The terminal's default foreground or background.
    Default,
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Cursor shape as the renderer should draw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
    Hidden,
}

/// One visible cell of the viewport.
#[derive(Clone, Debug)]
pub struct CellInfo {
    pub col: usize,
    /// 0 is the top of the viewport.
    pub row: usize,
    pub ch: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub flags: Flags,
    pub selected: bool,
    /// First cell of a double-width character.
    pub wide: bool,
    /// Trailing cell of a double-width character.
    pub spacer: bool,
}

/// Cursor position in viewport coordinates.
#[derive(Clone, Copy, Debug)]
pub struct CursorInfo {
    pub col: usize,
    pub row: usize,
    pub shape: CursorShape,
}

/// How a selection grows from its starting point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}

/// Grid size handed to alacritty.
#[derive(Clone, Copy, Debug)]
struct Size {
    cols: usize,
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
        self.cols
    }
}

/// Collects events raised by the terminal while parsing.
#[derive(Clone)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
    }
}

/// Terminal state: grid, parser and the side effects collected while parsing.
pub struct Emulator {
    term: Term<Listener>,
    parser: Processor,
    events: Arc<Mutex<Vec<Event>>>,
    title: Option<String>,
    pty_writes: Vec<u8>,
    bell: bool,
    cols: usize,
    rows: usize,
}

impl Emulator {
    /// Creates an emulator with at least one column and one row.
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let events = Arc::new(Mutex::new(Vec::new()));
        let config = Config { scrolling_history: scrollback, ..Default::default() };
        let term = Term::new(config, &Size { cols, lines: rows }, Listener(Arc::clone(&events)));
        Self {
            term,
            parser: Processor::new(),
            events,
            title: None,
            pty_writes: Vec::new(),
            bell: false,
            cols,
            rows,
        }
    }

    /// Parses output bytes and applies the events they raise.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        let events = std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner()));
        for event in events {
            match event {
                Event::Title(title) => self.title = Some(title),
                Event::ResetTitle => self.title = None,
                Event::PtyWrite(text) => self.pty_writes.extend_from_slice(text.as_bytes()),
                Event::Bell => self.bell = true,
                _ => {}
            }
        }
    }

    /// Changes the grid size. Does nothing when the size is unchanged.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.term.resize(Size { cols, lines: rows });
        self.cols = cols;
        self.rows = rows;
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Scrolls the viewport. Positive values move further into the history.
    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Number of lines the viewport is shifted up into the history.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Number of lines currently stored in the scrollback.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Bytes the terminal wants to send back to the process (query replies).
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pty_writes)
    }

    /// Whether a bell arrived since the last call.
    pub fn take_bell(&mut self) -> bool {
        std::mem::replace(&mut self.bell, false)
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn app_cursor(&self) -> bool {
        self.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn app_keypad(&self) -> bool {
        self.mode().contains(TermMode::APP_KEYPAD)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub fn alt_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn mouse_mode(&self) -> bool {
        self.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Cursor in the viewport, or `None` when it is hidden or scrolled out of view.
    pub fn cursor(&self) -> Option<CursorInfo> {
        let content = self.term.renderable_content();
        let shape = match content.cursor.shape {
            AnsiCursorShape::Block | AnsiCursorShape::HollowBlock => CursorShape::Block,
            AnsiCursorShape::Underline => CursorShape::Underline,
            AnsiCursorShape::Beam => CursorShape::Beam,
            AnsiCursorShape::Hidden => return None,
        };
        let row = content.cursor.point.line.0 + content.display_offset as i32;
        let col = content.cursor.point.column.0;
        if row < 0 || row >= self.rows as i32 || col >= self.cols {
            return None;
        }
        Some(CursorInfo { col, row: row as usize, shape })
    }

    /// All viewport cells, row-major, blanks included.
    pub fn cells(&self) -> Vec<CellInfo> {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let selection = content.selection;
        content
            .display_iter
            .map(|indexed| {
                let cell = indexed.cell;
                CellInfo {
                    col: indexed.point.column.0,
                    row: (indexed.point.line.0 + offset) as usize,
                    ch: cell.c,
                    fg: cell_color(cell.fg, true),
                    bg: cell_color(cell.bg, false),
                    flags: cell.flags,
                    selected: selection.as_ref().is_some_and(|range| range.contains(indexed.point)),
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                    spacer: cell.flags.contains(Flags::WIDE_CHAR_SPACER),
                }
            })
            .collect()
    }

    /// Text of one viewport row with trailing whitespace removed.
    pub fn row_text(&self, row: usize) -> String {
        let grid = self.term.grid();
        let text: String = (0..self.cols)
            .filter_map(|col| {
                let cell = &grid[self.viewport_point(col, row)];
                (!cell.flags.contains(Flags::WIDE_CHAR_SPACER)).then_some(cell.c)
            })
            .collect();
        text.trim_end().to_string()
    }

    /// All viewport rows joined with newlines, trailing blank rows removed.
    pub fn screen_text(&self) -> String {
        let mut lines: Vec<String> = (0..self.rows).map(|row| self.row_text(row)).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Starts a selection at a viewport position.
    pub fn selection_start(&mut self, col: usize, row: usize, kind: SelectionKind) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let point = self.viewport_point(col, row);
        self.term.selection = Some(Selection::new(ty, point, Side::Left));
    }

    /// Moves the end of the active selection to a viewport position.
    pub fn selection_update(&mut self, col: usize, row: usize) {
        let point = self.viewport_point(col, row);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, Side::Right);
        }
    }

    pub fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    /// Whether a non-empty selection exists.
    pub fn has_selection(&self) -> bool {
        self.selection_text().is_some_and(|text| !text.is_empty())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    fn mode(&self) -> &TermMode {
        self.term.mode()
    }

    /// Grid point of a viewport position. Viewport row 0 is grid line `-display_offset`.
    fn viewport_point(&self, col: usize, row: usize) -> Point {
        let line = row as i32 - self.display_offset() as i32;
        Point::new(Line(line), Column(col))
    }
}

/// Maps an alacritty colour to a renderer colour.
/// The default fg and bg become [`CellColor::Default`].
fn cell_color(color: Color, foreground: bool) -> CellColor {
    match color {
        Color::Named(NamedColor::Foreground) if foreground => CellColor::Default,
        Color::Named(NamedColor::Background) if !foreground => CellColor::Default,
        Color::Named(named) => CellColor::Named(named),
        Color::Indexed(index) => CellColor::Indexed(index),
        Color::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(emu: &Emulator, col: usize, row: usize) -> CellInfo {
        emu.cells()[row * emu.cols() + col].clone()
    }

    #[test]
    fn plain_text_lands_in_first_row_and_moves_cursor() {
        let mut emu = Emulator::new(10, 3, 100);
        emu.feed(b"hello");
        let text: String = (0..5).map(|c| cell(&emu, c, 0).ch).collect();
        assert_eq!(text, "hello");
        assert_eq!(emu.row_text(0), "hello");
        let cursor = emu.cursor().expect("cursor visible");
        assert_eq!((cursor.col, cursor.row), (5, 0));
        assert_eq!(cursor.shape, CursorShape::Block);
    }

    #[test]
    fn sgr_foreground_colour() {
        let mut emu = Emulator::new(20, 2, 100);
        emu.feed(b"\x1b[31mred\x1b[0m plain");
        for col in 0..3 {
            assert_eq!(cell(&emu, col, 0).fg, CellColor::Named(NamedColor::Red));
        }
        for col in 3..9 {
            assert_eq!(cell(&emu, col, 0).fg, CellColor::Default);
        }
    }

    #[test]
    fn sgr_flags_and_colours() {
        let mut emu = Emulator::new(20, 2, 100);
        emu.feed(b"\x1b[1;4mX");
        let x = cell(&emu, 0, 0);
        assert!(x.flags.contains(Flags::BOLD | Flags::UNDERLINE));

        let mut emu = Emulator::new(20, 2, 100);
        emu.feed(b"\x1b[7mX");
        assert!(cell(&emu, 0, 0).flags.contains(Flags::INVERSE));

        let mut emu = Emulator::new(20, 2, 100);
        emu.feed(b"\x1b[38;2;10;20;30mX");
        assert_eq!(cell(&emu, 0, 0).fg, CellColor::Rgb(10, 20, 30));

        let mut emu = Emulator::new(20, 2, 100);
        emu.feed(b"\x1b[48;5;200mX");
        assert_eq!(cell(&emu, 0, 0).bg, CellColor::Indexed(200));
        assert_eq!(cell(&emu, 1, 0).bg, CellColor::Default);
    }

    #[test]
    fn scrollback_keeps_history_and_moves_viewport() {
        let mut emu = Emulator::new(10, 5, 100);
        for i in 0..20 {
            emu.feed(format!("line{i}\r\n").as_bytes());
        }
        assert!(emu.history_size() > 0);
        assert_eq!(emu.display_offset(), 0);
        assert_eq!(emu.row_text(0), "line16");
        // The cursor sits on an empty fifth row, which screen_text drops.
        assert_eq!(emu.screen_text().lines().count(), 4);

        emu.scroll(3);
        assert_eq!(emu.display_offset(), 3);
        assert_eq!(emu.row_text(0), "line13");

        // Alacritty keeps the cursor at its grid position, which is now below the
        // viewport, so it is reported as absent while scrolled up.
        assert!(emu.cursor().is_none());

        emu.scroll_to_bottom();
        assert_eq!(emu.display_offset(), 0);
        assert_eq!(emu.row_text(0), "line16");
        assert!(emu.cursor().is_some());
    }

    #[test]
    fn resize_keeps_text_and_updates_size() {
        let mut emu = Emulator::new(10, 5, 100);
        emu.feed(b"hello");
        emu.resize(20, 3);
        assert_eq!((emu.cols(), emu.rows()), (20, 3));
        assert_eq!(emu.row_text(0), "hello");
        let cursor = emu.cursor().expect("cursor visible");
        assert!(cursor.col < 20 && cursor.row < 3);
        assert_eq!(emu.cells().len(), 60);
    }

    #[test]
    fn resize_clamps_minimum_size() {
        let mut emu = Emulator::new(0, 0, 10);
        assert_eq!((emu.cols(), emu.rows()), (1, 1));
        emu.resize(0, 0);
        assert_eq!((emu.cols(), emu.rows()), (1, 1));
    }

    #[test]
    fn alternate_screen_and_modes() {
        let mut emu = Emulator::new(10, 3, 0);
        emu.feed(b"\x1b[?1049h");
        assert!(emu.alt_screen());
        emu.feed(b"\x1b[?1049l");
        assert!(!emu.alt_screen());

        emu.feed(b"\x1b[?2004h");
        assert!(emu.bracketed_paste());

        assert!(!emu.app_cursor());
        emu.feed(b"\x1b[?1h");
        assert!(emu.app_cursor());

        assert!(!emu.app_keypad());
        emu.feed(b"\x1b=");
        assert!(emu.app_keypad());

        assert!(!emu.mouse_mode());
        emu.feed(b"\x1b[?1000h");
        assert!(emu.mouse_mode());
    }

    #[test]
    fn title_is_set_and_reset() {
        let mut emu = Emulator::new(10, 3, 0);
        emu.feed(b"\x1b]2;hi\x07");
        assert_eq!(emu.title(), Some("hi"));
        emu.feed(b"\x1b]2;\x07");
        assert_eq!(emu.title(), Some(""));
    }

    #[test]
    fn bell_is_taken_once() {
        let mut emu = Emulator::new(10, 3, 0);
        emu.feed(b"\x07");
        assert!(emu.take_bell());
        assert!(!emu.take_bell());
    }

    #[test]
    fn device_attributes_query_is_answered() {
        let mut emu = Emulator::new(10, 3, 0);
        emu.feed(b"\x1b[c");
        let reply = emu.take_pty_writes();
        assert!(reply.starts_with(b"\x1b[?"));
        assert!(emu.take_pty_writes().is_empty());
    }

    #[test]
    fn cursor_position_report_is_answered() {
        let mut emu = Emulator::new(10, 3, 0);
        emu.feed(b"ab\x1b[6n");
        assert_eq!(emu.take_pty_writes(), b"\x1b[1;3R");
    }

    #[test]
    fn simple_selection_and_clear() {
        let mut emu = Emulator::new(20, 2, 0);
        emu.feed(b"hello world");
        emu.selection_start(0, 0, SelectionKind::Simple);
        emu.selection_update(4, 0);
        assert_eq!(emu.selection_text().as_deref(), Some("hello"));
        assert!(emu.has_selection());
        assert!(cell(&emu, 4, 0).selected);
        assert!(!cell(&emu, 5, 0).selected);
        emu.selection_clear();
        assert!(!emu.has_selection());
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn word_selection() {
        let mut emu = Emulator::new(20, 2, 0);
        emu.feed(b"hello world");
        emu.selection_start(7, 0, SelectionKind::Word);
        assert_eq!(emu.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut emu = Emulator::new(10, 2, 0);
        emu.feed("日本".as_bytes());
        assert!(cell(&emu, 0, 0).wide);
        assert!(cell(&emu, 1, 0).spacer);
        assert!(cell(&emu, 2, 0).wide);
        assert_eq!(emu.row_text(0), "日本");
    }
}
