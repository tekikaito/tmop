//! Docker engines on this machine (Linux and macOS): the sockets the usual installs listen on,
//! the one the synthetic `default` context uses when `DOCKER_HOST` is unset, plain-language
//! connection errors and start hints for the Local Docker panel.
//!
//! The candidates, first found wins: `/var/run/docker.sock` (Docker, and Docker Desktop's
//! optional symlink on macOS), rootless Docker `$XDG_RUNTIME_DIR/docker.sock`, Podman
//! `$XDG_RUNTIME_DIR/podman/podman.sock` and `/run/podman/podman.sock`, Docker Desktop
//! `~/.docker/run/docker.sock` (macOS) and `~/.docker/desktop/docker.sock` (Linux), Colima
//! `~/.colima/default/docker.sock` and OrbStack `~/.orbstack/run/docker.sock`.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Which install a socket belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Docker,
    Rootless,
    Podman,
    DockerDesktop,
    Colima,
    OrbStack,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Docker => "Docker",
            Kind::Rootless => "Docker (rootless)",
            Kind::Podman => "Podman",
            Kind::DockerDesktop => "Docker Desktop",
            Kind::Colima => "Colima",
            Kind::OrbStack => "OrbStack",
        }
    }

    /// How to start this engine, as a command where there is one.
    pub fn start_hint(self) -> &'static str {
        match self {
            Kind::Docker if cfg!(target_os = "macos") => "Start Docker Desktop (its default socket links /var/run/docker.sock)",
            Kind::Docker => "sudo systemctl start docker",
            Kind::Rootless => "systemctl --user start docker",
            Kind::Podman => "systemctl --user start podman.socket",
            Kind::DockerDesktop => "Start Docker Desktop",
            Kind::Colima => "colima start",
            Kind::OrbStack => "orb start",
        }
    }
}

/// A socket path an install would listen on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    pub path: PathBuf,
}

/// The candidate sockets, in the order they are tried. System paths are under `root` (`/`).
pub fn candidates(root: &Path, home: Option<&Path>, runtime_dir: Option<&Path>) -> Vec<Candidate> {
    let mut out = vec![Candidate { kind: Kind::Docker, path: root.join("var/run/docker.sock") }];
    if let Some(rt) = runtime_dir {
        out.push(Candidate { kind: Kind::Rootless, path: rt.join("docker.sock") });
        out.push(Candidate { kind: Kind::Podman, path: rt.join("podman/podman.sock") });
    }
    out.push(Candidate { kind: Kind::Podman, path: root.join("run/podman/podman.sock") });
    if let Some(h) = home {
        for (kind, rel) in [
            (Kind::DockerDesktop, ".docker/run/docker.sock"),
            (Kind::DockerDesktop, ".docker/desktop/docker.sock"),
            (Kind::Colima, ".colima/default/docker.sock"),
            (Kind::OrbStack, ".orbstack/run/docker.sock"),
        ] {
            out.push(Candidate { kind, path: h.join(rel) });
        }
    }
    out
}

/// The first candidate that exists (a dangling symlink does not).
pub fn first_present(cands: &[Candidate]) -> Option<&Candidate> {
    cands.iter().find(|c| c.path.exists())
}

/// [`candidates`] for this machine: `/`, `$HOME` and `$XDG_RUNTIME_DIR` (absolute ones only).
pub fn this_machine() -> Vec<Candidate> {
    let abs = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    candidates(Path::new("/"), abs("HOME").as_deref(), abs("XDG_RUNTIME_DIR").as_deref())
}

/// The socket of the synthetic `default` context off Windows: the first present candidate,
/// else `/var/run/docker.sock`.
pub fn default_socket() -> String {
    first_present(&this_machine()).map(|c| c.path.to_string_lossy().into_owned()).unwrap_or_else(|| "/var/run/docker.sock".into())
}

/// The install a socket path belongs to, judged by where it lives.
pub fn kind_of(path: &str) -> Kind {
    let rt = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute());
    kind_in(path, rt.as_deref())
}

fn kind_in(path: &str, runtime_dir: Option<&Path>) -> Kind {
    if path.contains("/.colima/") {
        Kind::Colima
    } else if path.contains("/.orbstack/") {
        Kind::OrbStack
    } else if path.contains("/.docker/run/") || path.contains("/.docker/desktop/") {
        Kind::DockerDesktop
    } else if path.ends_with("podman.sock") {
        Kind::Podman
    } else if runtime_dir.is_some_and(|rt| Path::new(path).starts_with(rt)) || path.starts_with("/run/user/") {
        Kind::Rootless
    } else {
        Kind::Docker
    }
}

/// What connecting to a socket found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Something accepted the connection.
    Open,
    /// The socket exists but this user may not open it.
    Denied,
    /// The socket file exists but nothing listens (a stale socket of a stopped engine).
    Refused,
    /// No socket there.
    Absent,
    /// Any other failure, as text.
    Failed(String),
}

/// Connects to `path` and closes again at once. Blocks only as long as a local `connect`.
pub fn probe(path: &Path) -> Reach {
    if !path.exists() {
        return Reach::Absent;
    }
    probe_existing(path)
}

#[cfg(unix)]
fn probe_existing(path: &Path) -> Reach {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Reach::Open,
        Err(e) => match e.kind() {
            ErrorKind::PermissionDenied => Reach::Denied,
            ErrorKind::ConnectionRefused => Reach::Refused,
            ErrorKind::NotFound => Reach::Absent,
            _ => Reach::Failed(e.to_string()),
        },
    }
}

#[cfg(not(unix))]
fn probe_existing(_path: &Path) -> Reach {
    Reach::Failed("unix sockets are not available on this platform".into())
}

/// A connection failure on socket `path`, in plain words, for the error kinds that have one.
pub fn explain(path: &str, kind: ErrorKind) -> Option<String> {
    match kind {
        ErrorKind::PermissionDenied if cfg!(target_os = "linux") && kind_in(path, None) == Kind::Docker => {
            Some(format!("permission denied on {path}: your user is not in the docker group"))
        }
        ErrorKind::PermissionDenied => Some(format!("permission denied on {path}")),
        ErrorKind::NotFound => Some(format!("no Docker socket at {path}: is Docker running?")),
        ErrorKind::ConnectionRefused => Some(format!("nothing answers on {path}: is Docker running?")),
        _ => None,
    }
}

/// The io error kind behind `err`, found by walking its sources (bollard wraps hyper, which
/// wraps the connector's `io::Error`), else by the `os error` text.
pub fn io_kind(err: &(dyn std::error::Error + 'static)) -> Option<ErrorKind> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        cur = e.source();
    }
    let text = err.to_string();
    [("os error 13", ErrorKind::PermissionDenied), ("os error 2)", ErrorKind::NotFound), ("os error 111", ErrorKind::ConnectionRefused), ("os error 61", ErrorKind::ConnectionRefused)]
        .into_iter()
        .find_map(|(needle, kind)| text.contains(needle).then_some(kind))
}

/// The command that gives this user access to `/var/run/docker.sock` on Linux.
pub const DOCKER_GROUP_FIX: &str = "sudo usermod -aG docker $USER";

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh directory standing in for `/` and `$HOME`, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("tmop-docker-local-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        fn touch(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn endpoint_choice_follows_the_candidate_order() {
        let t = TempDir::new("choice");
        let (root, home, rt) = (t.0.join("root"), t.0.join("home"), t.0.join("run-user"));
        let pick = || first_present(&candidates(&root, Some(&home), Some(&rt))).map(|c| (c.kind, c.path.clone()));
        assert_eq!(pick(), None, "nothing there yet");

        let orb = t.touch("home/.orbstack/run/docker.sock");
        assert_eq!(pick(), Some((Kind::OrbStack, orb)));
        let colima = t.touch("home/.colima/default/docker.sock");
        assert_eq!(pick(), Some((Kind::Colima, colima)));
        let desktop_linux = t.touch("home/.docker/desktop/docker.sock");
        assert_eq!(pick(), Some((Kind::DockerDesktop, desktop_linux)));
        let desktop_mac = t.touch("home/.docker/run/docker.sock");
        assert_eq!(pick(), Some((Kind::DockerDesktop, desktop_mac)));
        let podman_root = t.touch("root/run/podman/podman.sock");
        assert_eq!(pick(), Some((Kind::Podman, podman_root)));
        let podman = t.touch("run-user/podman/podman.sock");
        assert_eq!(pick(), Some((Kind::Podman, podman)));
        let rootless = t.touch("run-user/docker.sock");
        assert_eq!(pick(), Some((Kind::Rootless, rootless)));
        let system = t.touch("root/var/run/docker.sock");
        assert_eq!(pick(), Some((Kind::Docker, system)));
    }

    #[test]
    fn candidates_without_home_or_runtime_dir() {
        let c = candidates(Path::new("/"), None, None);
        assert_eq!(c.iter().map(|c| c.kind).collect::<Vec<_>>(), [Kind::Docker, Kind::Podman]);
        assert_eq!(c[0].path, Path::new("/").join("var/run/docker.sock"));
    }

    #[test]
    fn kinds_by_path() {
        let rt = Path::new("/run/user/1000");
        for (path, kind) in [
            ("/var/run/docker.sock", Kind::Docker),
            ("/run/user/1000/docker.sock", Kind::Rootless),
            ("/run/user/1000/podman/podman.sock", Kind::Podman),
            ("/run/podman/podman.sock", Kind::Podman),
            ("/Users/m/.docker/run/docker.sock", Kind::DockerDesktop),
            ("/home/m/.docker/desktop/docker.sock", Kind::DockerDesktop),
            ("/Users/m/.colima/default/docker.sock", Kind::Colima),
            ("/Users/m/.orbstack/run/docker.sock", Kind::OrbStack),
        ] {
            assert_eq!(kind_in(path, Some(rt)), kind, "{path}");
        }
        assert_eq!(kind_in("/tmp/xdg/docker.sock", Some(Path::new("/tmp/xdg"))), Kind::Rootless);
    }

    #[test]
    fn explains_the_common_failures() {
        let denied = explain("/var/run/docker.sock", ErrorKind::PermissionDenied).unwrap();
        assert!(denied.starts_with("permission denied on /var/run/docker.sock"), "{denied}");
        assert_eq!(denied.contains("docker group"), cfg!(target_os = "linux"));
        assert!(!explain("/run/user/1/docker.sock", ErrorKind::PermissionDenied).unwrap().contains("group"));
        assert!(explain("/x.sock", ErrorKind::NotFound).unwrap().contains("is Docker running"));
        assert!(explain("/x.sock", ErrorKind::ConnectionRefused).unwrap().contains("nothing answers"));
        assert_eq!(explain("/x.sock", ErrorKind::TimedOut), None);
    }

    #[derive(Debug)]
    struct Wrap(std::io::Error);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("client error (Connect)")
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn io_kind_walks_the_source_chain() {
        let e = Wrap(std::io::Error::from(ErrorKind::PermissionDenied));
        assert_eq!(io_kind(&e), Some(ErrorKind::PermissionDenied));
        let plain = std::fmt::Error;
        assert_eq!(io_kind(&plain), None);
    }

    #[test]
    fn probe_of_a_missing_socket() {
        let t = TempDir::new("probe");
        assert_eq!(probe(&t.0.join("nope.sock")), Reach::Absent);
    }

    #[cfg(unix)]
    #[test]
    fn probe_open_refused_and_denied() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new("probe-unix");
        let path = t.0.join("d.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert_eq!(probe(&path), Reach::Open);
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            assert_eq!(probe(&path), Reach::Denied);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        drop(listener);
        assert_eq!(probe(&path), Reach::Refused, "the socket file stays behind");
    }
}
