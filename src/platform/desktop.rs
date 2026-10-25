//! The desktop around the window: whether TMOP draws its own title bar (Wayland compositors
//! without server-side decorations, such as GNOME's), and the desktop's light/dark preference
//! when the windowing layer does not report one.
//!
//! The decisions are pure functions of what was probed, tested on every host; the probes run
//! once.

use std::sync::OnceLock;

/// Environment variable that overrides the title-bar decision: `tmop` (TMOP draws it) or
/// `native` (the window manager does).
pub const TITLE_BAR_VAR: &str = "TMOP_TITLEBAR";

/// The Wayland protocol through which a compositor offers server-side decorations (the one
/// winit asks for).
const XDG_DECORATION: &str = "zxdg_decoration_manager_v1";

/// Whether TMOP draws the title bar itself. Only in a Wayland session: there `override_`
/// ([`TITLE_BAR_VAR`]) wins; else a compositor that lists `globals` without [`XDG_DECORATION`]
/// gets TMOP's bar; without the list (the probe failed), GNOME (`XDG_CURRENT_DESKTOP`) does.
pub fn decide_title_bar(wayland: bool, override_: Option<&str>, globals: Option<&[String]>, current_desktop: Option<&str>) -> bool {
    if !wayland {
        return false;
    }
    match override_.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("tmop" | "own" | "1" | "on") => return true,
        Some("native" | "system" | "0" | "off") => return false,
        _ => {}
    }
    match globals {
        Some(list) => !list.iter().any(|g| g == XDG_DECORATION),
        None => current_desktop.is_some_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("GNOME"))),
    }
}

/// [`decide_title_bar`] for this session, probed once (a Wayland round trip listing the
/// compositor's globals). Call it before the window opens: it decides the window's decorations.
pub fn own_title_bar() -> bool {
    static OWN: OnceLock<bool> = OnceLock::new();
    *OWN.get_or_init(|| {
        let wayland = super::is_wayland();
        let override_ = std::env::var(TITLE_BAR_VAR).ok();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        let globals = if wayland && override_.is_none() { wayland_globals() } else { None };
        decide_title_bar(wayland, override_.as_deref(), globals.as_deref(), desktop.as_deref())
    })
}

/// The interfaces the Wayland compositor advertises (`None` when it cannot be reached).
#[cfg(target_os = "linux")]
fn wayland_globals() -> Option<Vec<String>> {
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};
    struct Globals(Vec<String>);
    impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
        fn event(state: &mut Self, _: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let wl_registry::Event::Global { interface, .. } = event {
                state.0.push(interface);
            }
        }
    }
    let conn = Connection::connect_to_env().ok()?;
    let mut queue = conn.new_event_queue();
    let _registry = conn.display().get_registry(&queue.handle(), ());
    let mut globals = Globals(Vec::new());
    queue.roundtrip(&mut globals).ok()?;
    Some(globals.0)
}

#[cfg(not(target_os = "linux"))]
fn wayland_globals() -> Option<Vec<String>> {
    None
}

/// Dark (`Some(true)`) or light from GNOME's settings: `color-scheme` (`'prefer-dark'`,
/// `'prefer-light'`), else a GTK theme named `...-dark`. `None` for `'default'` with a light
/// theme name, or nothing readable.
pub fn gnome_dark(color_scheme: Option<&str>, gtk_theme: Option<&str>) -> Option<bool> {
    let unquote = |s: &str| s.trim().trim_matches('\'').to_ascii_lowercase();
    match color_scheme.map(unquote).as_deref() {
        Some("prefer-dark") => return Some(true),
        Some("prefer-light") => return Some(false),
        _ => {}
    }
    gtk_theme.map(unquote).filter(|t| !t.is_empty()).and_then(|t| (t.ends_with("-dark") || t.ends_with(":dark")).then_some(true))
}

/// The desktop's dark preference when egui reports no system theme: read once on a worker
/// thread (Linux: `gsettings`), `None` until it is known or where there is nothing to read.
pub fn desktop_dark() -> Option<bool> {
    static DARK: OnceLock<Option<bool>> = OnceLock::new();
    static STARTED: std::sync::Once = std::sync::Once::new();
    if super::OS != super::Os::Linux {
        return None;
    }
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            let get = |key: &str| super::run_capture("gsettings", &["get", "org.gnome.desktop.interface", key], std::time::Duration::from_secs(3)).ok();
            let _ = DARK.set(gnome_dark(get("color-scheme").as_deref(), get("gtk-theme").as_deref()));
        });
    });
    DARK.get().copied().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bar_only_on_wayland_without_decorations() {
        let kde = ["wl_compositor".to_string(), XDG_DECORATION.to_string()];
        let gnome = ["wl_compositor".to_string(), "xdg_wm_base".to_string()];
        assert!(!decide_title_bar(false, Some("tmop"), Some(&gnome), Some("GNOME")), "X11, macOS, Windows keep native decorations");
        assert!(decide_title_bar(true, None, Some(&gnome), Some("KDE")));
        assert!(!decide_title_bar(true, None, Some(&kde), Some("GNOME")));
        assert!(decide_title_bar(true, None, None, Some("ubuntu:GNOME")), "probe failed: GNOME is known to have none");
        assert!(!decide_title_bar(true, None, None, Some("KDE")));
        assert!(!decide_title_bar(true, None, None, None));
        assert!(decide_title_bar(true, Some(" TMOP "), Some(&kde), None));
        assert!(!decide_title_bar(true, Some("native"), Some(&gnome), Some("GNOME")));
        assert!(decide_title_bar(true, Some("bogus"), Some(&gnome), None), "an unknown override is ignored");
    }

    #[test]
    fn gnome_theme_preference() {
        assert_eq!(gnome_dark(Some("'prefer-dark'\n"), Some("'Adwaita'")), Some(true));
        assert_eq!(gnome_dark(Some("'prefer-light'"), Some("'Adwaita-dark'")), Some(false));
        assert_eq!(gnome_dark(Some("'default'"), Some("'Yaru-dark'\n")), Some(true));
        assert_eq!(gnome_dark(Some("'default'"), Some("'Adwaita'")), None);
        assert_eq!(gnome_dark(None, None), None);
    }

    #[test]
    fn probes_answer_off_linux() {
        if cfg!(not(target_os = "linux")) {
            assert!(!own_title_bar());
            assert_eq!(desktop_dark(), None);
        }
    }
}
