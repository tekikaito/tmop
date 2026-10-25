//! Desktop notifications for container alerts, shown through [`crate::platform::notify`]
//! (a WinRT toast, notify-send or gdbus, osascript).

use super::backend::{Alert, AlertKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Title and body for an alert, such as ("Container exited unexpectedly", "web (desktop-linux) exited with code 137").
pub fn alert_text(alert: &Alert) -> (String, String) {
    let who = format!("{} ({})", alert.container, alert.context);
    match alert.kind {
        AlertKind::UnexpectedExit { code } => {
            ("Container exited unexpectedly".into(), format!("{who} exited with code {code}"))
        }
        AlertKind::OomKilled => ("Container ran out of memory".into(), format!("{who} was killed by the OOM killer")),
        AlertKind::RestartLoop => ("Container is restarting repeatedly".into(), format!("{who} keeps restarting")),
    }
}

/// The first failure of [`send`], until [`take_first_failure`] collects it.
static FIRST_FAILURE: Mutex<Option<String>> = Mutex::new(None);
/// A failure was recorded once; later ones are not.
static FAILED: AtomicBool = AtomicBool::new(false);

/// Shows a desktop notification. Blocks for a moment; call it off the UI thread. The first
/// failure of the process is kept for [`take_first_failure`].
pub fn send(title: &str, body: &str) -> Result<(), String> {
    let result = crate::platform::notify(title, body);
    if let Err(e) = &result {
        if !FAILED.swap(true, Ordering::Relaxed) {
            *FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
        }
    }
    result
}

/// The first failed [`send`] of this process, once, so the UI can explain why no desktop
/// notification appeared (for example notify-send missing).
pub fn take_first_failure() -> Option<String> {
    FIRST_FAILURE.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn alert(kind: AlertKind) -> Alert {
        Alert { kind, container: "web".into(), context: "desktop-linux".into(), at: Instant::now() }
    }

    #[test]
    fn alert_text_variants() {
        assert_eq!(
            alert_text(&alert(AlertKind::UnexpectedExit { code: 137 })),
            ("Container exited unexpectedly".into(), "web (desktop-linux) exited with code 137".into())
        );
        let (title, body) = alert_text(&alert(AlertKind::OomKilled));
        assert_eq!(title, "Container ran out of memory");
        assert_eq!(body, "web (desktop-linux) was killed by the OOM killer");
        let (title, body) = alert_text(&alert(AlertKind::RestartLoop));
        assert_eq!(title, "Container is restarting repeatedly");
        assert_eq!(body, "web (desktop-linux) keeps restarting");
    }

    #[test]
    #[ignore = "shows a real desktop notification; run with --ignored"]
    fn toast_smoke() {
        assert_eq!(send("TMOP", "test"), Ok(()));
    }
}
