//! What this build can do on this machine: a constant per OS plus a few runtime probes, built
//! once at start-up ([`Caps::get`], copied into `App::caps`). The UI asks it instead of `cfg!`,
//! so a page or control that does not apply is hidden (not shown failing), and tests can pass
//! a made-up set.
//!
//! On Windows every flag is true and the UI is the one TMOP always had.

use super::{Os, OS};
use std::sync::OnceLock;

/// Per-process features of the Processes page (columns, menu items).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcCaps {
    /// Efficiency mode (EcoQoS; SCHED_IDLE on Linux, background priority on macOS).
    pub efficiency: bool,
    /// The Realtime priority class.
    pub realtime: bool,
    /// Suspend and resume.
    pub suspend: bool,
    /// The "Not responding" status (hung windows).
    pub not_responding: bool,
    /// A Handles column (open file descriptors off Windows).
    pub handles: bool,
    /// A Session column.
    pub session: bool,
    /// An Architecture column.
    pub arch: bool,
    /// Per-process GPU use.
    pub gpu: bool,
    /// Which process owns a top-level window (the Apps grouping).
    pub window_owner: bool,
}

/// What this build can do; see the module docs. Fields are plain flags so a test can build any
/// combination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// The system inventory backends: System Info, Startup apps, Services, Drivers, Installed
    /// Apps, Connections and Power & Freq.
    pub inventory: bool,
    /// Opening TMOP in place of Windows Task Manager (Settings section, banner, menu item).
    pub taskmgr_replace: bool,
    /// TMOP's own Docker engine (WSL): the Engine tab, its wizard and its settings.
    pub engine: bool,
    /// The Windows Disk Cleanup handlers in Clean up.
    pub volcache: bool,
    /// The MFT fast scan of Disk Space.
    pub mft: bool,
    /// "Restart as administrator" and the not-elevated banner of Disk Space.
    pub elevate_relaunch: bool,
    /// The screensaver feature (`--features screensaver`) is built for this OS.
    pub screensaver: bool,
    /// Temperatures.
    pub thermal: bool,
    /// Clock speeds (none on Apple Silicon).
    pub clocks: bool,
    /// GPU engines (3D, copy, video ...) beside the total.
    pub gpu_engines: bool,
    /// Committed memory and its limit.
    pub commit_mem: bool,
    pub proc: ProcCaps,
    /// Deleted items can go to a Recycle Bin / Trash. Until the Trash backends land off Windows
    /// this is false there and only permanent deletes would exist (which [`Caps::delete_safe`]
    /// keeps off anyway).
    pub trash: bool,
    /// Disk Space may delete: the protected-paths classifier, the Trash and the clean up
    /// categories of this OS are in place. Analysis is available either way.
    pub delete_safe: bool,
    /// Window levels (always on top, popout on the desktop) and the header pin. Not on Wayland.
    pub window_levels: bool,
}

impl Caps {
    pub const WINDOWS: Caps = Caps {
        inventory: true,
        taskmgr_replace: true,
        engine: true,
        volcache: true,
        mft: true,
        elevate_relaunch: true,
        screensaver: true,
        thermal: true,
        clocks: true,
        gpu_engines: true,
        commit_mem: true,
        proc: ProcCaps { efficiency: true, realtime: true, suspend: true, not_responding: true, handles: true, session: true, arch: true, gpu: true, window_owner: true },
        trash: true,
        delete_safe: true,
        window_levels: true,
    };

    pub const LINUX: Caps = Caps {
        inventory: true,
        taskmgr_replace: false,
        engine: false,
        volcache: false,
        mft: false,
        elevate_relaunch: false,
        screensaver: false,
        thermal: true,
        clocks: true,
        gpu_engines: true,
        commit_mem: true,
        proc: ProcCaps { efficiency: true, realtime: false, suspend: true, not_responding: false, handles: true, session: false, arch: true, gpu: false, window_owner: true },
        trash: false,
        delete_safe: false,
        window_levels: true,
    };

    pub const MACOS: Caps = Caps {
        inventory: true,
        taskmgr_replace: false,
        engine: false,
        volcache: false,
        mft: false,
        elevate_relaunch: false,
        screensaver: false,
        thermal: false,
        // Apple Silicon reports no clock speeds.
        clocks: cfg!(target_arch = "x86_64"),
        gpu_engines: false,
        commit_mem: false,
        proc: ProcCaps { efficiency: true, realtime: false, suspend: true, not_responding: false, handles: true, session: false, arch: true, gpu: false, window_owner: true },
        trash: false,
        delete_safe: false,
        window_levels: true,
    };

    /// Targets without a backend: monitoring only.
    pub const OTHER: Caps = Caps {
        inventory: false,
        taskmgr_replace: false,
        engine: false,
        volcache: false,
        mft: false,
        elevate_relaunch: false,
        screensaver: false,
        thermal: false,
        clocks: false,
        gpu_engines: false,
        commit_mem: false,
        proc: ProcCaps { efficiency: false, realtime: false, suspend: false, not_responding: false, handles: false, session: false, arch: false, gpu: false, window_owner: false },
        trash: false,
        delete_safe: false,
        window_levels: false,
    };

    /// The constant part for `os`.
    pub const fn for_os(os: Os) -> Caps {
        match os {
            Os::Windows => Self::WINDOWS,
            Os::Linux => Self::LINUX,
            Os::MacOs => Self::MACOS,
            Os::Other => Self::OTHER,
        }
    }

    /// The constant part narrowed by what this session offers. `wayland`: no window levels and
    /// no window owners (X11 `_NET_CLIENT_LIST` only).
    pub const fn probed(os: Os, wayland: bool) -> Caps {
        let mut c = Self::for_os(os);
        if wayland {
            c.window_levels = false;
            c.proc.window_owner = false;
        }
        c
    }

    /// Narrowed by what the metrics backend finds: no clock meters without clocks, no Thermals
    /// without a temperature source.
    pub const fn with_metrics(mut self, probe: crate::metrics::Probe) -> Caps {
        self.clocks &= probe.clocks;
        self.thermal &= probe.thermal;
        self
    }

    /// This build on this machine, probed once.
    pub fn get() -> Caps {
        static CAPS: OnceLock<Caps> = OnceLock::new();
        *CAPS.get_or_init(|| Self::probed(OS, super::is_wayland()).with_metrics(crate::metrics::probe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_has_everything() {
        let w = Caps::WINDOWS;
        let p = w.proc;
        let all = [w.inventory, w.taskmgr_replace, w.engine, w.volcache, w.mft, w.elevate_relaunch, w.screensaver, w.thermal, w.clocks, w.gpu_engines, w.commit_mem, w.trash, w.delete_safe, w.window_levels];
        let procs = [p.efficiency, p.realtime, p.suspend, p.not_responding, p.handles, p.session, p.arch, p.gpu, p.window_owner];
        assert!(all.iter().chain(&procs).all(|b| *b));
        assert_eq!(Caps::probed(Os::Windows, false), Caps::WINDOWS);
    }

    #[test]
    fn windows_only_features_and_the_delete_gate_are_off_elsewhere() {
        for c in [Caps::LINUX, Caps::MACOS, Caps::OTHER] {
            assert!(!(c.taskmgr_replace || c.engine || c.volcache || c.mft || c.elevate_relaunch || c.screensaver));
            // Disk Space deletes stay off until the Unix classifier, Trash and categories land.
            assert!(!c.delete_safe);
        }
        assert_eq!([Caps::LINUX.inventory, Caps::MACOS.inventory, Caps::OTHER.inventory], [true, true, false]);
    }

    #[test]
    fn wayland_drops_window_levels_and_owners() {
        let c = Caps::probed(Os::Linux, true);
        assert!(!c.window_levels && !c.proc.window_owner);
        assert_eq!(Caps { window_levels: true, proc: ProcCaps { window_owner: true, ..c.proc }, ..c }, Caps::LINUX);
        assert_eq!(Caps::probed(Os::Linux, false), Caps::LINUX);
    }

    #[test]
    fn metrics_probe_only_narrows() {
        use crate::metrics::Probe;
        let none = Probe { clocks: false, thermal: false };
        let all = Probe { clocks: true, thermal: true };
        let c = Caps::WINDOWS.with_metrics(none);
        assert!(!c.clocks && !c.thermal);
        assert_eq!(Caps { clocks: true, thermal: true, ..c }, Caps::WINDOWS);
        assert_eq!(Caps::WINDOWS.with_metrics(all), Caps::WINDOWS);
        assert!(!Caps::MACOS.with_metrics(all).thermal);
        assert!(Caps::LINUX.with_metrics(Probe { clocks: true, thermal: false }).clocks);
    }

    #[test]
    fn get_matches_this_target() {
        let c = Caps::get();
        assert_eq!(c.engine, cfg!(windows));
        assert_eq!(c.delete_safe, cfg!(windows));
        if cfg!(windows) {
            assert_eq!(c, Caps::WINDOWS);
        }
        assert_eq!(c, Caps::get());
    }
}
