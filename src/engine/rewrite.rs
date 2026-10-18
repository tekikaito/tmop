//! Windows bind-mount sources to WSL paths for the engine relay.
//!
//! The Docker CLI on Windows sends drive paths as `C:\x`, `c:/x`, `\\?\C:\x` or `/c/x`.
//! The engine runs Linux Docker, so those sources become `/mnt/c/x`.

use serde_json::Value;

/// Which request body, if any, may need rewriting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteKind {
    None,
    ContainerCreate,
    VolumeCreate,
}

/// Converts a Windows drive path to its `/mnt/<drive>` form; `None` for anything else.
pub fn rewrite_host_path(s: &str) -> Option<String> {
    let s = s.strip_prefix(r"\\?\").unwrap_or(s);
    let b = s.as_bytes();
    // `C:\x`, `c:/x`, `C:`
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/') {
        return Some(mount_path(b[0] as char, &s[2..]));
    }
    // `/c/x` or `//c/x`, as sent by the Docker CLI on Windows
    let stripped = s.strip_prefix("//").or_else(|| s.strip_prefix('/'))?;
    let drive = *stripped.as_bytes().first()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    let rest = &stripped[1..];
    if rest.is_empty() || rest.starts_with('/') {
        return Some(mount_path(drive as char, rest));
    }
    None
}

fn mount_path(drive: char, rest: &str) -> String {
    let norm = rest.replace('\\', "/");
    let rest = norm.trim_matches('/');
    if rest.is_empty() {
        format!("/mnt/{}", drive.to_ascii_lowercase())
    } else {
        format!("/mnt/{}/{rest}", drive.to_ascii_lowercase())
    }
}

/// Rewrites the source of one `<source>:<target>[:<opts>]` bind entry.
///
/// A source that starts with a drive letter keeps its own colon; the target starts after the next colon.
pub fn rewrite_bind(spec: &str) -> String {
    let off = if spec.starts_with(r"\\?\") { 4 } else { 0 };
    let b = spec.as_bytes();
    let is_drive = b.len() >= off + 3
        && b[off].is_ascii_alphabetic()
        && b[off + 1] == b':'
        && (b[off + 2] == b'\\' || b[off + 2] == b'/');
    let split = if is_drive {
        spec[off + 2..].find(':').map(|i| i + off + 2)
    } else {
        spec.find(':')
    };
    let Some(idx) = split else {
        return spec.to_string();
    };
    let (source, rest) = spec.split_at(idx);
    match rewrite_host_path(source) {
        Some(path) => format!("{path}{rest}"),
        None => spec.to_string(),
    }
}

/// Rewrites `HostConfig.Binds` and bind `HostConfig.Mounts[].Source` in a container create body.
///
/// Returns `None` when the body is not JSON or nothing changed.
pub fn rewrite_create_body(json: &[u8]) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(json).ok()?;
    let mut changed = false;
    if let Some(host) = v.get_mut("HostConfig") {
        if let Some(binds) = host.get_mut("Binds").and_then(Value::as_array_mut) {
            for bind in binds.iter_mut() {
                let Some(spec) = bind.as_str() else { continue };
                let new = rewrite_bind(spec);
                if new != spec {
                    *bind = Value::String(new);
                    changed = true;
                }
            }
        }
        if let Some(mounts) = host.get_mut("Mounts").and_then(Value::as_array_mut) {
            for mount in mounts.iter_mut() {
                let kind = mount.get("Type").and_then(Value::as_str).map(str::to_owned);
                let Some(source) = mount.get("Source").and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                let is_bind = match kind.as_deref() {
                    Some(kind) => kind == "bind",
                    None => rewrite_host_path(&source).is_some(),
                };
                if !is_bind {
                    continue;
                }
                if let Some(new) = rewrite_host_path(&source) {
                    mount["Source"] = Value::String(new);
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&v).ok()
}

/// Rewrites `DriverOpts.device` in a volume create body when it is a Windows path.
///
/// Returns `None` when the body is not JSON or nothing changed.
pub fn rewrite_volume_body(json: &[u8]) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(json).ok()?;
    let device = v.get("DriverOpts")?.get("device")?.as_str()?;
    let new = rewrite_host_path(device)?;
    if let Some(slot) = v.get_mut("DriverOpts").and_then(|o| o.get_mut("device")) {
        *slot = Value::String(new);
    }
    serde_json::to_vec(&v).ok()
}

/// Classifies a request by method and path (optional `/v1.xx` prefix, query ignored).
pub fn needs_rewrite(method: &str, path: &str) -> RewriteKind {
    if method != "POST" {
        return RewriteKind::None;
    }
    match strip_version(path.split('?').next().unwrap_or_default()) {
        "/containers/create" => RewriteKind::ContainerCreate,
        "/volumes/create" => RewriteKind::VolumeCreate,
        _ => RewriteKind::None,
    }
}

fn strip_version(path: &str) -> &str {
    let Some(rest) = path.strip_prefix("/v") else {
        return path;
    };
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let (version, remainder) = rest.split_at(end);
    if version.starts_with(|c: char| c.is_ascii_digit()) && remainder.starts_with('/') {
        remainder
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create(body: &str) -> Option<Value> {
        rewrite_create_body(body.as_bytes()).map(|b| serde_json::from_slice(&b).unwrap())
    }

    #[test]
    fn drive_paths_become_mnt_paths() {
        assert_eq!(rewrite_host_path(r"C:\Users\me\proj").as_deref(), Some("/mnt/c/Users/me/proj"));
        assert_eq!(rewrite_host_path("c:/x/y").as_deref(), Some("/mnt/c/x/y"));
        assert_eq!(rewrite_host_path(r"\\?\C:\x").as_deref(), Some("/mnt/c/x"));
        assert_eq!(rewrite_host_path(r"D:\my\dir\").as_deref(), Some("/mnt/d/my/dir"));
    }

    #[test]
    fn drive_root_and_case() {
        assert_eq!(rewrite_host_path(r"C:\").as_deref(), Some("/mnt/c"));
        assert_eq!(rewrite_host_path("D:").as_deref(), Some("/mnt/d"));
        assert_eq!(rewrite_host_path(r"e:\X").as_deref(), Some("/mnt/e/X"));
    }

    #[test]
    fn docker_cli_slash_form() {
        assert_eq!(rewrite_host_path("/c/Users/x").as_deref(), Some("/mnt/c/Users/x"));
        assert_eq!(rewrite_host_path("//c/x/").as_deref(), Some("/mnt/c/x"));
        assert_eq!(rewrite_host_path("/c").as_deref(), Some("/mnt/c"));
    }

    #[test]
    fn non_windows_paths_are_left_alone() {
        for s in ["/mnt/c/x", "/var/lib/x", "data", "./rel", r"rel\path", r"\\server\share\x", "//server/share", "", "C"] {
            assert_eq!(rewrite_host_path(s), None, "{s}");
        }
    }

    #[test]
    fn bind_specs() {
        assert_eq!(rewrite_bind(r"C:\a:/b:ro"), "/mnt/c/a:/b:ro");
        assert_eq!(rewrite_bind(r"D:\my data\:/data"), "/mnt/d/my data:/data");
        assert_eq!(rewrite_bind("/c/x:/y"), "/mnt/c/x:/y");
        assert_eq!(rewrite_bind("data:/data"), "data:/data");
        assert_eq!(rewrite_bind("/host/path:/c"), "/host/path:/c");
        assert_eq!(rewrite_bind("/var/run/docker.sock:/var/run/docker.sock"), "/var/run/docker.sock:/var/run/docker.sock");
        assert_eq!(rewrite_bind(r"C:\x"), r"C:\x");
    }

    #[test]
    fn compose_style_body() {
        let body = r#"{"Image":"app","HostConfig":{"Binds":["C:\\proj:/src:rw","data:/data"],
            "Mounts":[{"Type":"bind","Source":"D:\\cache","Target":"/c"},
            {"Type":"volume","Source":"C:\\vol","Target":"/v"},{"Type":"tmpfs","Target":"/t"}]}}"#;
        let v = create(body).expect("changed");
        assert_eq!(v["Image"], "app");
        assert_eq!(v["HostConfig"]["Binds"], json!(["/mnt/c/proj:/src:rw", "data:/data"]));
        assert_eq!(v["HostConfig"]["Mounts"][0]["Source"], "/mnt/d/cache");
        assert_eq!(v["HostConfig"]["Mounts"][1]["Source"], "C:\\vol");
        assert_eq!(v["HostConfig"]["Mounts"][2], json!({"Type":"tmpfs","Target":"/t"}));
    }

    #[test]
    fn mount_without_type_uses_path_shape() {
        let v = create(r#"{"HostConfig":{"Mounts":[{"Source":"C:\\x","Target":"/x"},{"Source":"vol","Target":"/y"}]}}"#)
            .expect("changed");
        assert_eq!(v["HostConfig"]["Mounts"][0]["Source"], "/mnt/c/x");
        assert_eq!(v["HostConfig"]["Mounts"][1]["Source"], "vol");
    }

    #[test]
    fn no_change_or_invalid_json_is_none() {
        assert_eq!(rewrite_create_body(b"not json"), None);
        assert_eq!(create(r#"{"HostConfig":{"Binds":["/var/x:/x","data:/d"]}}"#), None);
        assert_eq!(create(r#"{"Image":"alpine"}"#), None);
    }

    #[test]
    fn volume_device_is_rewritten() {
        let body = br#"{"Name":"v","Driver":"local","DriverOpts":{"type":"none","o":"bind","device":"C:\\data"}}"#;
        let v: Value = serde_json::from_slice(&rewrite_volume_body(body).expect("changed")).unwrap();
        assert_eq!(v["DriverOpts"]["device"], "/mnt/c/data");
        assert_eq!(v["DriverOpts"]["o"], "bind");
        assert_eq!(rewrite_volume_body(br#"{"DriverOpts":{"device":"/srv/x"}}"#), None);
        assert_eq!(rewrite_volume_body(br#"{"Name":"plain"}"#), None);
        assert_eq!(rewrite_volume_body(b"[]"), None);
    }

    #[test]
    fn request_classification() {
        assert_eq!(needs_rewrite("POST", "/containers/create"), RewriteKind::ContainerCreate);
        assert_eq!(needs_rewrite("POST", "/v1.47/containers/create?name=x"), RewriteKind::ContainerCreate);
        assert_eq!(needs_rewrite("POST", "/volumes/create"), RewriteKind::VolumeCreate);
        assert_eq!(needs_rewrite("POST", "/v1.41/volumes/create"), RewriteKind::VolumeCreate);
        assert_eq!(needs_rewrite("GET", "/containers/create"), RewriteKind::None);
        assert_eq!(needs_rewrite("POST", "/containers/x/start"), RewriteKind::None);
        assert_eq!(needs_rewrite("POST", "/vx/containers/create"), RewriteKind::None);
    }
}
