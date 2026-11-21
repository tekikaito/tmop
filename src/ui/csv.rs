//! "Copy table as CSV" and "Save table as CSV...": the text of a table as it is shown, and how it
//! reaches the clipboard or a file.

use super::kit;
use eframe::egui;
use std::sync::{mpsc::Receiver, Arc, Mutex};

/// What the user asked for in the header menu of a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Export {
    Copy,
    Save,
}

/// True for a plain number with an optional unit: an optional sign, digits with an optional
/// decimal part, and then at most one space and a unit of letters, `%` or `/` ("-5", "+3.2 %",
/// "-1.5 MB/s"). Anything else that starts like a formula is not.
fn plain_number(s: &str) -> bool {
    let rest = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (num, unit) = match rest.split_once(' ') {
        Some((n, u)) => (n, Some(u)),
        None => (rest, None),
    };
    let (int, frac) = num.split_once('.').map_or((num, Some("0")), |(i, f)| (i, Some(f)));
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_some_and(digits) && unit.is_none_or(|u| !u.is_empty() && u.len() <= 12 && u.chars().all(|c| c.is_ascii_alphabetic() || c == '%' || c == '/'))
}

/// `s` made safe to open in a spreadsheet, which runs a cell that starts with `=`, `+`, `-` or
/// `@` (or a tab or carriage return) as a formula; process names and command lines come from
/// other programs. Such a field gets a leading apostrophe, unless it is a plain number with an
/// optional unit ("-5", "+3.2 %", see [`plain_number`]), which is what the figures of a table
/// look like.
pub fn neutralise(s: &str) -> String {
    if s.starts_with(['=', '@', '\t', '\r']) || (s.starts_with(['+', '-']) && !plain_number(s)) {
        format!("'{s}")
    } else {
        s.to_string()
    }
}

/// One field of a line: made safe by [`neutralise`] and quoted (RFC 4180) when it holds a comma,
/// a quote, a CR or an LF, with its quotes doubled.
pub fn field(s: &str) -> String {
    let s = neutralise(s);
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// A CSV line from `fields`, ended by CRLF.
pub fn line<S: AsRef<str>>(fields: &[S]) -> String {
    let mut out = fields.iter().map(|f| field(f.as_ref())).collect::<Vec<_>>().join(",");
    out.push_str("\r\n");
    out
}

/// The text of a file: the header line, then a line per row.
pub fn document<S: AsRef<str>>(header: &[S], rows: &[Vec<String>]) -> String {
    let mut out = line(header);
    for r in rows {
        out.push_str(&line(r));
    }
    out
}

/// The bytes of the file: UTF-8 with a byte order mark, which Excel needs to read it as UTF-8.
pub fn file_bytes(text: &str) -> Vec<u8> {
    let mut out = vec![0xEF, 0xBB, 0xBF];
    out.extend_from_slice(text.as_bytes());
    out
}

struct Pending {
    rx: Receiver<Option<std::path::PathBuf>>,
    text: String,
}

type Slot = Arc<Mutex<Option<Pending>>>;

fn slot_id() -> egui::Id {
    egui::Id::unique("tmop-csv-save")
}

fn owner_id() -> egui::Id {
    egui::Id::unique("tmop-csv-owner")
}

/// Tells the dialogs which window they belong to (the main window's HWND, 0 for none).
pub fn set_owner(ctx: &egui::Context, hwnd: isize) {
    ctx.data_mut(|d| d.insert_temp(owner_id(), hwnd));
}

/// Hands `text` to the clipboard, or opens a save dialog (on its own thread, like the other file
/// dialogs, see [`kit::file_dialog`]); [`poll`] writes the file once a name is chosen. `name` is
/// the suggested file name without its extension.
pub fn deliver(ctx: &egui::Context, export: Export, text: String, name: &str) {
    match export {
        Export::Copy => ctx.copy_text(text),
        Export::Save => {
            let slot: Slot = ctx.data_mut(|d| d.get_temp_mut_or_insert_with(slot_id(), Slot::default).clone());
            let mut pending = slot.lock().unwrap_or_else(|e| e.into_inner());
            if pending.is_some() {
                kit::report(ctx, "A save dialog is already open".to_string());
                return;
            }
            let owner = ctx.data(|d| d.get_temp::<isize>(owner_id())).unwrap_or(0);
            let d = kit::FileDialog { save: true, title: "Save table as CSV".into(), filter: ("CSV".into(), "csv".into()), file_name: format!("{name}.csv"), owner };
            *pending = Some(Pending { rx: kit::file_dialog(d), text });
        }
    }
}

/// Once per frame: writes the file of a save dialog that was answered, and says how it went in
/// the status line.
pub fn poll(ctx: &egui::Context) {
    let Some(slot) = ctx.data(|d| d.get_temp::<Slot>(slot_id())) else { return };
    let mut pending = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pending.as_ref() else { return };
    match p.rx.try_recv() {
        Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
        Ok(Some(path)) => {
            let message = match std::fs::write(&path, file_bytes(&p.text)) {
                Ok(()) => format!("Saved the table to {}", path.display()),
                Err(e) => format!("Could not save {}: {e}", path.display()),
            };
            kit::report(ctx, message);
            *pending = None;
        }
        Ok(None) | Err(_) => *pending = None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_quoted_the_usual_way() {
        assert_eq!(line(&["a", "b c", ""]), "a,b c,\r\n");
        assert_eq!(line(&["a,b", "say \"hi\"", "two\nlines", "cr\rhere"]), "\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\",\"cr\rhere\"\r\n");
        assert_eq!(line::<&str>(&[]), "\r\n");
    }

    #[test]
    fn formulas_get_an_apostrophe_but_numbers_do_not() {
        for f in ["=1+1", "@SUM(A1)", "+cmd|' /C calc'!A0", "-2+3", "\tx", "=HYPERLINK(\"u\")", "-", "+", "-.5", "- 5", "-5 (x)", "-1.", "--5"] {
            assert!(neutralise(f).starts_with('\''), "{f:?} must be neutralised");
        }
        for f in ["-5", "+3", "+3.2 %", "-1.5 MB/s", "12.3 %", "1.2 GB", "0", "name", "a=b", "x-y", ""] {
            assert_eq!(neutralise(f), f, "{f:?} stays as it is");
        }
        // Quoting comes after: the apostrophe is inside the quotes.
        assert_eq!(line(&["=a,b"]), "\"'=a,b\"\r\n");
    }

    #[test]
    fn a_document_has_a_header_and_crlf_lines() {
        let rows = vec![vec!["x".to_string(), "1".to_string()], vec!["y, z".to_string(), "2".to_string()]];
        assert_eq!(document(&["Name", "N"], &rows), "Name,N\r\nx,1\r\n\"y, z\",2\r\n");
    }

    #[test]
    fn the_file_starts_with_a_byte_order_mark() {
        assert_eq!(file_bytes("a,\u{e9}"), [0xEF, 0xBB, 0xBF, b'a', b',', 0xC3, 0xA9]);
    }

    #[test]
    fn copying_puts_the_text_on_the_output_not_the_clipboard() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |root| deliver(root.ctx(), Export::Copy, "a,b\r\n".into(), "t"));
        out.textures_delta.clear();
        assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "a,b\r\n")));
    }
}
