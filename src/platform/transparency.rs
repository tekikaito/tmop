//! Whether the popout's corners can be see-through (#166).
//!
//! The popout is a borderless window with rounded corners: the pixels outside the shape are
//! cleared to transparent black, and only a window the system composites with its alpha channel
//! shows the desktop there. Everywhere else those pixels come out black. eframe picks one OpenGL
//! config (and on macOS one context) for all of TMOP's windows, from the main window's
//! `transparent` flag, before any window opens:
//!
//! * Windows (WGL): asking for transparency adds `WGL_TRANSPARENT_ARB` (a colour-key feature) to
//!   the config search, and eframe panics when nothing matches; glutin's own examples ask for it
//!   only on CGL. It is never asked for; the configs have 8 alpha bits anyway, and since eframe 0.36.2 (egui#8423, #8616) a transparent window keeps its
//!   DWM blur-behind and composites with that alpha. Mesa's software renderer (the server
//!   package) presents through GDI, whose alpha DWM is not known to honour: square corners there.
//! * macOS (CGL): the context's surface is opaque unless the config asked for transparency, so it
//!   always does; the main window itself stays opaque.
//! * Wayland (EGL): any config with alpha bits composites; rounded.
//! * X11 (GLX): only a config asked for transparency has an ARGB visual, and only a running
//!   compositor uses it. Asking would make TMOP fail to start on a server without such a visual,
//!   so only the `--popout` process, whose window is transparent itself, asks.
//!
//! [`popout_corners`] decides from these facts; [`CORNERS_VAR`] overrides it.

use super::Os;

/// Environment variable that overrides [`popout_corners`]: `rounded` or `square`.
pub const CORNERS_VAR: &str = "TMOP_POPOUT_CORNERS";

/// How the popout draws its window shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corners {
    /// The skin's shape; what lies outside it is transparent.
    Rounded,
    /// The whole window rectangle is filled, so nothing outside the shape can show black.
    Square,
}

/// The windowing system the popout opens on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Windows,
    MacOs,
    Wayland,
    /// `compositor`: whether a compositing manager owns `_NET_WM_CM_S<screen>`; `None` unknown.
    X11 { compositor: Option<bool> },
    Other,
}

/// What the popout's corners depend on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub display: Display,
    /// The OpenGL config was asked for transparency ([`gl_config_transparency`]).
    pub gl_config_transparent: bool,
    /// The renderer is a software one (`GL_RENDERER`, see [`crate::startup::is_software_renderer`]).
    pub software_gl: bool,
    /// [`CORNERS_VAR`], see [`forced`].
    pub forced: Option<Corners>,
}

/// Whether TMOP asks eframe for an OpenGL config that supports transparency (the main
/// viewport's `transparent`; the windows' own flag is set apart from it). `popout_only`: the
/// `--popout` process, whose only window is the popout.
pub fn gl_config_transparency(os: Os, popout_only: bool) -> bool {
    match os {
        Os::Windows => false,
        Os::MacOs => true,
        Os::Linux | Os::Other => popout_only,
    }
}

/// Rounded corners where the window's transparent pixels are known to show the desktop, square
/// ones everywhere else.
pub fn popout_corners(f: &Facts) -> Corners {
    if let Some(c) = f.forced {
        return c;
    }
    let see_through = match f.display {
        Display::Windows => !f.software_gl,
        Display::MacOs => f.gl_config_transparent,
        Display::Wayland => true,
        Display::X11 { compositor } => f.gl_config_transparent && compositor == Some(true),
        Display::Other => false,
    };
    if see_through {
        Corners::Rounded
    } else {
        Corners::Square
    }
}

/// [`CORNERS_VAR`]'s value: `rounded` or `square`, anything else is ignored.
pub fn forced(value: Option<&str>) -> Option<Corners> {
    match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("rounded" | "round") => Some(Corners::Rounded),
        Some("square") => Some(Corners::Square),
        _ => None,
    }
}

/// This session's [`Display`]. `probe_compositor`: on X11, ask the server whether a compositor
/// runs (a short connection); otherwise it stays unknown.
pub fn display(probe_compositor: bool) -> Display {
    match super::OS {
        Os::Windows => Display::Windows,
        Os::MacOs => Display::MacOs,
        Os::Linux if super::is_wayland() => Display::Wayland,
        Os::Linux => Display::X11 { compositor: if probe_compositor { x11_compositor() } else { None } },
        Os::Other => Display::Other,
    }
}

/// [`popout_corners`] for this process.
pub fn current(popout_only: bool, software_gl: bool) -> (Corners, Facts) {
    let gl_config_transparent = gl_config_transparency(super::OS, popout_only);
    let facts = Facts { display: display(gl_config_transparent), gl_config_transparent, software_gl, forced: forced(std::env::var(CORNERS_VAR).ok().as_deref()) };
    (popout_corners(&facts), facts)
}

/// Whether a compositing manager runs on the X server: the EWMH way, an owner of the
/// `_NET_WM_CM_S<screen>` selection. `None` when the server cannot be asked.
#[cfg(target_os = "linux")]
fn x11_compositor() -> Option<bool> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let atom = conn.intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes()).ok()?.reply().ok()?.atom;
    let owner = conn.get_selection_owner(atom).ok()?.reply().ok()?.owner;
    Some(owner != x11rb::NONE)
}

#[cfg(not(target_os = "linux"))]
fn x11_compositor() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(display: Display, gl_config_transparent: bool) -> Facts {
        Facts { display, gl_config_transparent, software_gl: false, forced: None }
    }

    #[test]
    fn config_transparency_is_never_asked_of_wgl() {
        assert!(!gl_config_transparency(Os::Windows, false));
        assert!(!gl_config_transparency(Os::Windows, true), "WGL_TRANSPARENT_ARB need not be offered");
        assert!(gl_config_transparency(Os::MacOs, false), "CGL: an opaque context paints every window opaque");
        assert!(!gl_config_transparency(Os::Linux, false), "X11: the main window must not depend on an ARGB visual");
        assert!(gl_config_transparency(Os::Linux, true));
    }

    #[test]
    fn rounded_only_where_the_system_composites_alpha() {
        assert_eq!(popout_corners(&facts(Display::Windows, false)), Corners::Rounded);
        assert_eq!(popout_corners(&Facts { software_gl: true, ..facts(Display::Windows, false) }), Corners::Square, "llvmpipe presents through GDI");
        assert_eq!(popout_corners(&facts(Display::MacOs, true)), Corners::Rounded);
        assert_eq!(popout_corners(&facts(Display::MacOs, false)), Corners::Square);
        assert_eq!(popout_corners(&facts(Display::Wayland, false)), Corners::Rounded);
        assert_eq!(popout_corners(&Facts { software_gl: true, ..facts(Display::Wayland, false) }), Corners::Rounded, "Mesa on Wayland keeps alpha");
        assert_eq!(popout_corners(&facts(Display::Other, true)), Corners::Square);
    }

    #[test]
    fn x11_needs_an_argb_config_and_a_compositor() {
        assert_eq!(popout_corners(&facts(Display::X11 { compositor: Some(true) }, true)), Corners::Rounded);
        assert_eq!(popout_corners(&facts(Display::X11 { compositor: Some(true) }, false)), Corners::Square, "the main window's popout has no ARGB visual");
        assert_eq!(popout_corners(&facts(Display::X11 { compositor: Some(false) }, true)), Corners::Square);
        assert_eq!(popout_corners(&facts(Display::X11 { compositor: None }, true)), Corners::Square);
    }

    #[test]
    fn the_variable_overrides() {
        assert_eq!(forced(Some(" Rounded ")), Some(Corners::Rounded));
        assert_eq!(forced(Some("square")), Some(Corners::Square));
        assert_eq!(forced(Some("bogus")), None);
        assert_eq!(forced(None), None);
        assert_eq!(popout_corners(&Facts { forced: Some(Corners::Rounded), ..facts(Display::X11 { compositor: Some(false) }, false) }), Corners::Rounded);
        assert_eq!(popout_corners(&Facts { forced: Some(Corners::Square), ..facts(Display::Wayland, true) }), Corners::Square);
    }
}
