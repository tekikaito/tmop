//! Start-up failures that would otherwise go unseen: a Windows GUI program has no console, so
//! an error from creating the window, or a panic on the UI thread, ended TMOP without a word
//! (#46). They are now shown in a message box (on Linux and macOS printed to stderr) and
//! appended to `startup-error.log` in TMOP's data folder, for bug reports.

use std::io::Write;
use std::path::PathBuf;

/// `startup-error.log` in TMOP's data folder (Windows: `%LOCALAPPDATA%\TMOP`).
pub fn log_path() -> PathBuf {
    crate::platform::data_dir().join("startup-error.log")
}

/// The OpenGL hint, when `error` looks like the window's OpenGL context failed.
fn opengl_hint(error: &str) -> Option<&'static str> {
    let e = error.to_ascii_lowercase();
    ["opengl", "glutin", "wgl", "glsl", "shader", "gl context", "pixel format", "egui_glow"].iter().any(|k| e.contains(k)).then_some(
        "TMOP draws its window with OpenGL 2.0 or newer. Without a graphics driver (for example Windows Server or a virtual machine with the Microsoft Basic Display Adapter), Windows offers only OpenGL 1.1. Install the graphics driver, or turn on GPU acceleration for the virtual machine, and start TMOP again.",
    )
}

/// The message for a failure: what went wrong, the OpenGL hint if it applies, and the log file.
pub fn message(error: &str) -> String {
    let mut m = format!("TMOP could not start.\n\n{error}");
    if let Some(h) = opengl_hint(error) {
        m.push_str("\n\n");
        m.push_str(h);
    }
    m.push_str(&format!("\n\nThis message is also saved in {}.", log_path().display()));
    m
}

/// Appends `error` to the log with the time and TMOP's version and shows [`message`].
pub fn report(error: &str) {
    append_log(error);
    show(&message(error));
}

fn append_log(error: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let (y, mo, d, h, mi, s) = crate::platform::local_time();
    let line = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} TMOP {} ({} {}): {error}\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(windows)]
fn show(text: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide(crate::APP_NAME));
    // SAFETY: both strings are NUL-terminated and alive for the call; no owner window.
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn show(text: &str) {
    eprintln!("{text}");
}

/// Logs every panic; one on the main (UI) thread is also shown, since it ends TMOP. Panics on
/// worker threads only end that worker, so they are logged without a message box.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let what = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
        let at = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let error = format!("panic in thread {name}: {what}{at}");
        append_log(&error);
        if name == "main" {
            show(&message(&error));
        }
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opengl_failures_get_the_driver_hint() {
        assert!(message("Glutin error: Could not create OpenGL context").contains("OpenGL 2.0 or newer"));
        assert!(message("egui_glow requires a GLSL version of at least 1.20").contains("graphics driver"));
        let other = message("Settings folder is not writable");
        assert!(other.starts_with("TMOP could not start.") && !other.contains("OpenGL") && other.contains("startup-error.log"));
    }
}
