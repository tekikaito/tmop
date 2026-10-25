//! Desktop notifications for container alerts.
//!
//! On Windows, alerts are shown as WinRT toast notifications. The app id is registered once
//! per user under `HKCU\Software\Classes\AppUserModelId` so the toasts carry the app name.

use super::backend::{Alert, AlertKind};

/// Application id used for toast notifications and its per-user registration.
pub const APP_ID: &str = "TMOP";

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

/// Shows a desktop notification.
#[cfg(windows)]
pub fn send(title: &str, body: &str) -> Result<(), String> {
    use std::sync::OnceLock;
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(register_app_id);

    let xml = format!(
        r#"<toast><visual><binding template="ToastText02"><text id="1">{}</text><text id="2">{}</text></binding></visual></toast>"#,
        xml_escape(title),
        xml_escape(body)
    );
    let doc = XmlDocument::new().map_err(|e| e.to_string())?;
    doc.LoadXml(&HSTRING::from(xml)).map_err(|e| e.to_string())?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(|e| e.to_string())?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID)).map_err(|e| e.to_string())?;
    notifier.Show(&toast).map_err(|e| e.to_string())
}

/// Shows a desktop notification.
#[cfg(not(windows))]
pub fn send(_title: &str, _body: &str) -> Result<(), String> {
    Err("Desktop notifications are not supported on this platform".into())
}

/// Writes `DisplayName = TMOP` under the per-user AppUserModelId key. Errors are ignored:
/// toasts still work without the registration, only the app name may be missing.
#[cfg(windows)]
fn register_app_id() {
    use std::process::{Command, Stdio};
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
    let mut cmd = Command::new("reg");
    cmd.args(["add", &key, "/v", "DisplayName", "/t", "REG_SZ", "/d", APP_ID, "/f"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::hide_console(&mut cmd);
    let _ = cmd.status();
}

#[cfg(windows)]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
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
    #[ignore = "shows a real desktop toast; run with --ignored"]
    fn toast_smoke() {
        assert_eq!(send("TMOP", "test"), Ok(()));
    }
}
