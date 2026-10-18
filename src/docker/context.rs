//! Docker host contexts, read from the Docker CLI configuration directory.
//!
//! The directory is `$DOCKER_CONFIG` or `%USERPROFILE%\.docker` (`$HOME/.docker`
//! elsewhere). Contexts live in `contexts/meta/<hash>/meta.json`; their TLS
//! material, if any, lives in `contexts/tls/<hash>/docker/`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// Where a Docker engine is reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Windows named pipe, stored without the `npipe://` prefix (e.g. `//./pipe/docker_engine`).
    NamedPipe(String),
    /// Unix socket path.
    Unix(String),
    /// TCP host as `host:port` (no scheme).
    Tcp { host: String, tls: Option<TlsFiles> },
    /// Engine behind SSH, reached through a forwarded socket.
    Ssh { host: String, user: Option<String>, port: Option<u16> },
}

/// TLS material for a TCP endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsFiles {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub skip_verify: bool,
}

/// A named Docker host context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub name: String,
    pub description: String,
    pub endpoint: Endpoint,
    pub current: bool,
}

impl Context {
    /// True for endpoints that leave the local machine (TCP or SSH).
    pub fn is_remote(&self) -> bool {
        matches!(self.endpoint, Endpoint::Tcp { .. } | Endpoint::Ssh { .. })
    }

    /// Short human-readable form of the endpoint.
    pub fn host_label(&self) -> String {
        match &self.endpoint {
            Endpoint::NamedPipe(p) | Endpoint::Unix(p) => p.clone(),
            Endpoint::Tcp { host, .. } => host.clone(),
            Endpoint::Ssh { host, user, port } => {
                let mut s = String::new();
                if let Some(u) = user {
                    s.push_str(u);
                    s.push('@');
                }
                s.push_str(host);
                if let Some(p) = port {
                    s.push_str(&format!(":{p}"));
                }
                s
            }
        }
    }
}

impl Endpoint {
    /// Parses a Docker host string: `npipe:////./pipe/x`, `unix:///var/run/docker.sock`,
    /// `tcp://host:2376`, `ssh://[user@]host[:port]`, or a plain `host:port` (TCP).
    pub fn parse(host: &str) -> Result<Endpoint, String> {
        let host = host.trim();
        if host.is_empty() {
            return Err("empty docker host".into());
        }
        if let Some(path) = host.strip_prefix("npipe://") {
            if path.is_empty() {
                return Err(format!("missing pipe path in {host}"));
            }
            return Ok(Endpoint::NamedPipe(path.to_string()));
        }
        if let Some(path) = host.strip_prefix("unix://") {
            if path.is_empty() {
                return Err(format!("missing socket path in {host}"));
            }
            return Ok(Endpoint::Unix(path.to_string()));
        }
        if let Some(rest) = host.strip_prefix("ssh://") {
            return parse_ssh(rest.trim_end_matches('/'));
        }
        let rest = host.strip_prefix("tcp://").or_else(|| host.strip_prefix("http://")).unwrap_or(host);
        if rest.is_empty() || rest.contains('/') {
            return Err(format!("unsupported docker host: {host}"));
        }
        Ok(Endpoint::Tcp { host: rest.to_string(), tls: None })
    }

    /// The docker host string for this endpoint (inverse of [`Endpoint::parse`]).
    pub fn to_host_string(&self) -> String {
        match self {
            Endpoint::NamedPipe(p) => format!("npipe://{p}"),
            Endpoint::Unix(p) => format!("unix://{p}"),
            Endpoint::Tcp { host, .. } => format!("tcp://{host}"),
            Endpoint::Ssh { host, user, port } => {
                let mut s = String::from("ssh://");
                if let Some(u) = user {
                    s.push_str(u);
                    s.push('@');
                }
                s.push_str(host);
                if let Some(p) = port {
                    s.push_str(&format!(":{p}"));
                }
                s
            }
        }
    }
}

fn parse_ssh(rest: &str) -> Result<Endpoint, String> {
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => {
            let port = p.parse::<u16>().map_err(|_| format!("invalid ssh port in {rest}"))?;
            (h, Some(port))
        }
        None => (hostport, None),
    };
    // Values are passed to ssh as arguments; a leading '-' would be read as an option.
    if host.is_empty() || host.starts_with('-') {
        return Err(format!("invalid ssh host in {rest}"));
    }
    if let Some(u) = user {
        if u.is_empty() || u.starts_with('-') {
            return Err(format!("invalid ssh user in {rest}"));
        }
    }
    Ok(Endpoint::Ssh { host: host.to_string(), user: user.map(str::to_string), port })
}

/// The Docker CLI configuration directory: `$DOCKER_CONFIG`, else the home folder's `.docker`.
pub fn docker_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DOCKER_CONFIG").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE") } else { std::env::var_os("HOME") }?;
    Some(PathBuf::from(home).join(".docker"))
}

/// Name of the context in use: `DOCKER_CONTEXT`, then `currentContext` in `config.json`,
/// then `"default"`.
pub fn current_context_name(dir: &Path) -> String {
    current_context_with(dir, std::env::var("DOCKER_CONTEXT").ok().as_deref())
}

fn current_context_with(dir: &Path, env_context: Option<&str>) -> String {
    if let Some(name) = env_context.map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("currentContext").and_then(Value::as_str).map(str::to_string))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "default".into())
}

/// Lists the synthetic `default` context followed by the contexts in `dir`, sorted by name.
/// Exactly one entry is marked `current`.
pub fn list_contexts(dir: &Path) -> Vec<Context> {
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let env_context = std::env::var("DOCKER_CONTEXT").ok();
    list_contexts_with(dir, docker_host.as_deref(), env_context.as_deref())
}

fn list_contexts_with(dir: &Path, docker_host: Option<&str>, env_context: Option<&str>) -> Vec<Context> {
    let current = current_context_with(dir, env_context);
    let default_endpoint = docker_host
        .filter(|h| !h.trim().is_empty())
        .and_then(|h| Endpoint::parse(h).ok())
        .unwrap_or_else(platform_default_endpoint);

    let mut others = read_meta_contexts(dir);
    others.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = vec![Context {
        name: "default".into(),
        description: "Current DOCKER_HOST based configuration".into(),
        endpoint: default_endpoint,
        current: false,
    }];
    out.extend(others);
    for c in &mut out {
        c.current = c.name == current;
    }
    if !out.iter().any(|c| c.current) {
        out[0].current = true;
    }
    out
}

fn platform_default_endpoint() -> Endpoint {
    if cfg!(windows) {
        Endpoint::NamedPipe("//./pipe/docker_engine".into())
    } else {
        Endpoint::Unix("/var/run/docker.sock".into())
    }
}

/// Reads `contexts/meta/*/meta.json`. Unreadable or unrecognised entries are skipped.
fn read_meta_contexts(dir: &Path) -> Vec<Context> {
    let Ok(entries) = fs::read_dir(dir.join("contexts").join("meta")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let meta_dir = entry.path();
        let Ok(text) = fs::read_to_string(meta_dir.join("meta.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(name) = v.get("Name").and_then(Value::as_str).filter(|n| !n.is_empty() && *n != "default") else {
            continue;
        };
        let docker = v.get("Endpoints").and_then(|e| e.get("docker"));
        let Some(host) = docker.and_then(|d| d.get("Host")).and_then(Value::as_str) else {
            continue;
        };
        let Ok(endpoint) = Endpoint::parse(host) else {
            continue;
        };
        let skip_verify = docker.and_then(|d| d.get("SkipTLSVerify")).and_then(Value::as_bool).unwrap_or(false);
        let endpoint = match endpoint {
            Endpoint::Tcp { host, .. } => Endpoint::Tcp {
                host,
                tls: tls_files(dir, &entry.file_name().to_string_lossy(), skip_verify),
            },
            other => other,
        };
        let description = v
            .get("Metadata")
            .and_then(|m| m.get("Description"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        out.push(Context { name: name.to_string(), description, endpoint, current: false });
    }
    out
}

/// TLS files for a context, when its certificate and key are present.
fn tls_files(dir: &Path, hash: &str, skip_verify: bool) -> Option<TlsFiles> {
    let base = dir.join("contexts").join("tls").join(hash).join("docker");
    let (cert, key) = (base.join("cert.pem"), base.join("key.pem"));
    if !cert.is_file() || !key.is_file() {
        return None;
    }
    Some(TlsFiles { ca: base.join("ca.pem"), cert, key, skip_verify })
}

/// [`list_contexts`] for the configured Docker directory, or only the synthetic
/// `default` context when there is none.
pub fn default_contexts() -> Vec<Context> {
    let dir = docker_dir().unwrap_or_default();
    list_contexts(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp folder, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("tmop-docker-ctx-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_each_endpoint_form() {
        let cases = [
            ("npipe:////./pipe/dockerDesktopLinuxEngine", Endpoint::NamedPipe("//./pipe/dockerDesktopLinuxEngine".into())),
            ("unix:///var/run/docker.sock", Endpoint::Unix("/var/run/docker.sock".into())),
            ("tcp://10.0.0.5:2375", Endpoint::Tcp { host: "10.0.0.5:2375".into(), tls: None }),
            ("10.0.0.5:2375", Endpoint::Tcp { host: "10.0.0.5:2375".into(), tls: None }),
            ("ssh://marc@nas:2222", Endpoint::Ssh { host: "nas".into(), user: Some("marc".into()), port: Some(2222) }),
            ("ssh://nas", Endpoint::Ssh { host: "nas".into(), user: None, port: None }),
        ];
        for (text, expected) in cases {
            let parsed = Endpoint::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed, expected, "{text}");
            let again = Endpoint::parse(&parsed.to_host_string()).unwrap();
            assert_eq!(again, parsed, "round trip of {text}");
        }
    }

    #[test]
    fn rejects_bad_hosts() {
        for text in ["", "npipe://", "unix://", "ssh://-oProxy@h", "ssh://u@h:99999", "https://h/x", "ssh://@h"] {
            assert!(Endpoint::parse(text).is_err(), "{text} should be rejected");
        }
    }

    #[test]
    fn current_context_from_config_and_env() {
        let dir = TempDir::new("current");
        assert_eq!(current_context_with(&dir.0, None), "default", "no config.json");
        dir.write("config.json", r#"{"auths":{},"currentContext":"work","extra":1}"#);
        assert_eq!(current_context_with(&dir.0, None), "work");
        assert_eq!(current_context_with(&dir.0, Some("env-ctx")), "env-ctx", "env wins");
        assert_eq!(current_context_with(&dir.0, Some("  ")), "work", "blank env is ignored");
        dir.write("config.json", "not json");
        assert_eq!(current_context_with(&dir.0, None), "default", "bad config.json falls back");
    }

    #[test]
    fn lists_contexts_with_default_first_and_skips_bad_entries() {
        let dir = TempDir::new("list");
        dir.write("config.json", r#"{"currentContext":"work"}"#);
        dir.write(
            "contexts/meta/aaa/meta.json",
            r#"{"Name":"work","Metadata":{"Description":"office"},"Endpoints":{"docker":{"Host":"tcp://10.0.0.5:2375","SkipTLSVerify":false}},"Extra":true}"#,
        );
        dir.write(
            "contexts/meta/bbb/meta.json",
            r#"{"Name":"zeta","Endpoints":{"docker":{"Host":"ssh://me@box"}}}"#,
        );
        dir.write("contexts/meta/ccc/meta.json", "{ broken");
        dir.write("contexts/meta/ddd/meta.json", r#"{"Name":"nohost","Endpoints":{}}"#);
        dir.write(
            "contexts/meta/eee/meta.json",
            r#"{"Name":"secure","Endpoints":{"docker":{"Host":"tcp://h:2376","SkipTLSVerify":true}}}"#,
        );
        dir.write("contexts/tls/eee/docker/cert.pem", "c");
        dir.write("contexts/tls/eee/docker/key.pem", "k");

        let list = list_contexts_with(&dir.0, Some("tcp://localhost:2375"), None);
        let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["default", "secure", "work", "zeta"]);
        assert_eq!(list.iter().filter(|c| c.current).count(), 1);
        assert!(list[2].current);
        assert_eq!(list[0].endpoint, Endpoint::Tcp { host: "localhost:2375".into(), tls: None });
        assert_eq!(list[2].description, "office");
        assert!(list[2].is_remote());
        assert_eq!(list[3].host_label(), "me@box");
        match &list[1].endpoint {
            Endpoint::Tcp { host, tls: Some(t) } => {
                assert_eq!(host, "h:2376");
                assert!(t.skip_verify);
                assert!(t.cert.ends_with("cert.pem"));
            }
            other => panic!("expected TLS endpoint, got {other:?}"),
        }
    }

    #[test]
    fn default_context_fallback() {
        let missing = std::env::temp_dir().join(format!("tmop-docker-ctx-{}-missing", std::process::id()));
        let list = list_contexts_with(&missing, None, None);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "default");
        assert!(list[0].current);
        assert_eq!(list[0].endpoint, platform_default_endpoint());
        assert!(!list[0].is_remote());
    }

    #[test]
    fn default_endpoint_follows_docker_host() {
        let missing = std::env::temp_dir().join("tmop-docker-ctx-nowhere");
        let list = list_contexts_with(&missing, Some("unix:///run/x.sock"), Some("default"));
        assert_eq!(list[0].endpoint, Endpoint::Unix("/run/x.sock".into()));
        assert_eq!(list[0].host_label(), "/run/x.sock");
    }
}
