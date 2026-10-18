//! Image pull and search, and creating a container from a run specification.
//!
//! [`RunSpec`] holds the text the user typed in the run dialog. [`build_create`] turns it
//! into the engine request; `extra` takes docker-run style flags such as `--memory 512m`.
//! All engine traffic goes through the connected engine; nothing is fetched from the internet
//! by TMOP itself.

use super::backend::err_text;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptionsBuilder, RemoveContainerOptions, SearchImagesOptions,
};
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

/// Restart policy of a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Restart {
    #[default]
    No,
    Always,
    UnlessStopped,
    OnFailure,
}

impl Restart {
    /// Every policy, in dialog order.
    pub const ALL: [Restart; 4] = [Restart::No, Restart::Always, Restart::UnlessStopped, Restart::OnFailure];

    /// Label for the dialog.
    pub fn label(self) -> &'static str {
        match self {
            Self::No => "Never",
            Self::Always => "Always",
            Self::UnlessStopped => "Unless stopped",
            Self::OnFailure => "On failure",
        }
    }

    /// The value the engine and the CLI use.
    pub fn token(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Always => "always",
            Self::UnlessStopped => "unless-stopped",
            Self::OnFailure => "on-failure",
        }
    }

    /// Parses a policy token. An empty string means `no`.
    pub fn parse(s: &str) -> Option<Restart> {
        match s.trim() {
            "" | "no" => Some(Self::No),
            "always" => Some(Self::Always),
            "unless-stopped" => Some(Self::UnlessStopped),
            "on-failure" => Some(Self::OnFailure),
            _ => None,
        }
    }

    fn engine_name(self) -> RestartPolicyNameEnum {
        match self {
            Self::No => RestartPolicyNameEnum::NO,
            Self::Always => RestartPolicyNameEnum::ALWAYS,
            Self::UnlessStopped => RestartPolicyNameEnum::UNLESS_STOPPED,
            Self::OnFailure => RestartPolicyNameEnum::ON_FAILURE,
        }
    }
}

/// What the run dialog collects. Multi-line fields hold one entry per line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunSpec {
    pub image: String,
    pub name: String,
    /// One mapping per line: `8080:80`, `8080:80/udp`, `127.0.0.1:8080:80`, or `80`
    /// (publish container port 80 to a random host port).
    pub ports: String,
    /// `KEY=VALUE` per line.
    pub env: String,
    /// Per line: `volname:/path`, `C:\dir:/path`, `/c/dir:/path:ro`.
    pub volumes: String,
    pub restart: Restart,
    /// Optional command, split like a shell command line.
    pub command: String,
    /// Extra docker-run style flags, see [`parse_extra`].
    pub extra: String,
    /// Pull the image when it is missing locally.
    pub pull: bool,
}

/// Splits a command line into arguments. Single and double quotes group words; a backslash
/// escapes the next character outside single quotes (inside double quotes only `\"` and `\\`).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    cur.push(c);
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                '\\' if matches!(chars.peek(), Some('"') | Some('\\')) => {
                    cur.push(chars.next().unwrap_or('\\'));
                }
                _ => cur.push(c),
            },
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        cur.push(next);
                        started = true;
                    }
                }
                c if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                _ => {
                    cur.push(c);
                    started = true;
                }
            },
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Flags from the `extra` field, collected before they are mapped onto the request.
#[derive(Debug, Default, PartialEq)]
struct Extra {
    env: Vec<String>,
    publish: Vec<String>,
    volumes: Vec<String>,
    name: Option<String>,
    /// Policy and optional retry count (`on-failure:3`).
    restart: Option<(Restart, Option<i64>)>,
    network: Option<String>,
    hostname: Option<String>,
    user: Option<String>,
    workdir: Option<String>,
    privileged: bool,
    rm: bool,
    memory: Option<i64>,
    nano_cpus: Option<i64>,
    publish_all: bool,
    tty: bool,
    interactive: bool,
    labels: BTreeMap<String, String>,
    entrypoint: Option<Vec<String>>,
}

/// Parses docker-run style flags. Each flag takes its value as the next word or after `=`;
/// short flags also accept the value attached (`-p8080:80`) and bool short flags may be
/// combined (`-it`). Unknown flags are an error.
fn parse_extra(extra: &str) -> Result<Extra, String> {
    let words = split_args(extra);
    let mut out = Extra::default();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        i += 1;
        if !word.starts_with('-') || word == "-" {
            return Err(format!("Unexpected argument: {word}"));
        }
        // Combined short bool flags, such as `-it`.
        if !word.starts_with("--") && word.len() > 2 && word[1..].chars().all(|c| matches!(c, 't' | 'i' | 'P')) {
            for c in word[1..].chars() {
                match c {
                    't' => out.tty = true,
                    'i' => out.interactive = true,
                    _ => out.publish_all = true,
                }
            }
            continue;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((f, v)) if word.starts_with("--") => (f, Some(v.to_string())),
            _ if !word.starts_with("--") && word.len() > 2 && is_short_value_flag(&word[..2]) => {
                (&word[..2], Some(word[2..].to_string()))
            }
            _ => (word, None),
        };
        let kind = flag_kind(flag).ok_or_else(|| format!("Unsupported option: {flag}"))?;
        match kind {
            FlagKind::Bool => {
                if inline.is_some() {
                    return Err(format!("Option {flag} takes no value"));
                }
                match flag {
                    "--privileged" => out.privileged = true,
                    "--rm" => out.rm = true,
                    "-P" | "--publish-all" => out.publish_all = true,
                    "-t" | "--tty" => out.tty = true,
                    _ => out.interactive = true,
                }
            }
            FlagKind::Value => {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        let v = words.get(i).cloned().ok_or_else(|| format!("Missing value for {flag}"))?;
                        i += 1;
                        v
                    }
                };
                apply_value(&mut out, flag, value)?;
            }
        }
    }
    if out.rm && out.restart.is_some_and(|(r, _)| r != Restart::No) {
        return Err("Conflicting options: --restart and --rm".into());
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum FlagKind {
    Bool,
    Value,
}

fn flag_kind(flag: &str) -> Option<FlagKind> {
    use FlagKind::*;
    Some(match flag {
        "--privileged" | "--rm" | "-P" | "--publish-all" | "-t" | "--tty" | "-i" | "--interactive" => Bool,
        "-e" | "--env" | "-p" | "--publish" | "-v" | "--volume" | "--name" | "--restart" | "--network"
        | "--net" | "--hostname" | "-h" | "--user" | "-u" | "--workdir" | "-w" | "--memory" | "-m" | "--cpus"
        | "-l" | "--label" | "--entrypoint" => Value,
        _ => return None,
    })
}

/// Short value flags that may carry their value attached, such as `-p8080:80`.
fn is_short_value_flag(flag: &str) -> bool {
    matches!(flag, "-e" | "-p" | "-v" | "-h" | "-u" | "-w" | "-m" | "-l")
}

fn apply_value(out: &mut Extra, flag: &str, value: String) -> Result<(), String> {
    match flag {
        "-e" | "--env" => out.env.push(value),
        "-p" | "--publish" => out.publish.push(value),
        "-v" | "--volume" => out.volumes.push(value),
        "--name" => out.name = Some(value),
        "--restart" => {
            let (policy, retries) = match value.split_once(':') {
                Some((p, n)) => (p, Some(n)),
                None => (value.as_str(), None),
            };
            let restart = Restart::parse(policy).ok_or_else(|| format!("Invalid restart policy: {value}"))?;
            let retries = match retries {
                Some(n) => Some(n.parse::<i64>().ok().filter(|n| *n >= 0).ok_or_else(|| {
                    format!("Invalid restart retry count: {n}")
                })?),
                None => None,
            };
            out.restart = Some((restart, retries));
        }
        "--network" | "--net" => out.network = Some(value),
        "--hostname" | "-h" => out.hostname = Some(value),
        "--user" | "-u" => out.user = Some(value),
        "--workdir" | "-w" => out.workdir = Some(value),
        "--memory" | "-m" => out.memory = Some(parse_memory(&value)?),
        "--cpus" => out.nano_cpus = Some(parse_cpus(&value)?),
        "-l" | "--label" => {
            let (k, v) = value.split_once('=').unwrap_or((value.as_str(), ""));
            if k.trim().is_empty() {
                return Err(format!("Invalid label: {value}"));
            }
            out.labels.insert(k.trim().to_string(), v.to_string());
        }
        "--entrypoint" => out.entrypoint = Some(split_args(&value)),
        _ => return Err(format!("Unsupported option: {flag}")),
    }
    Ok(())
}

/// Memory size such as `512m`, `2g`, `1024` (bytes). Suffixes b, k, m, g are 1024-based.
fn parse_memory(s: &str) -> Result<i64, String> {
    let lower = s.trim().to_ascii_lowercase();
    let (digits, mult) = match lower.chars().last() {
        Some('b') => (&lower[..lower.len() - 1], 1i64),
        Some('k') => (&lower[..lower.len() - 1], 1024),
        Some('m') => (&lower[..lower.len() - 1], 1024 * 1024),
        Some('g') => (&lower[..lower.len() - 1], 1024 * 1024 * 1024),
        _ => (lower.as_str(), 1),
    };
    let n: i64 = digits.parse().map_err(|_| format!("Invalid memory limit: {s}"))?;
    if n <= 0 {
        return Err(format!("Invalid memory limit: {s}"));
    }
    n.checked_mul(mult).ok_or_else(|| format!("Invalid memory limit: {s}"))
}

/// CPU count such as `1.5`, returned as Docker `NanoCpus`.
fn parse_cpus(s: &str) -> Result<i64, String> {
    let n: f64 = s.trim().parse().map_err(|_| format!("Invalid CPU count: {s}"))?;
    if !n.is_finite() || n <= 0.0 {
        return Err(format!("Invalid CPU count: {s}"));
    }
    Ok((n * 1e9).round() as i64)
}

/// One parsed port mapping.
#[derive(Debug, PartialEq)]
struct PortSpec {
    /// Key used by the engine, such as `80/tcp`.
    key: String,
    host_ip: Option<String>,
    /// Empty string asks the engine for a random host port.
    host_port: String,
}

fn parse_port(line: &str) -> Result<PortSpec, String> {
    let bad = || format!("Invalid port mapping: {line}");
    let (spec, proto) = match line.rsplit_once('/') {
        Some((s, p)) => (s, p),
        None => (line, "tcp"),
    };
    if !matches!(proto, "tcp" | "udp" | "sctp") {
        return Err(bad());
    }
    let parts: Vec<&str> = spec.split(':').collect();
    let (ip, host, container) = match parts.as_slice() {
        [c] => (None, "", *c),
        [h, c] => (None, *h, *c),
        [ip, h, c] => (Some((*ip).to_string()).filter(|s| !s.is_empty()), *h, *c),
        _ => return Err(bad()),
    };
    let port_ok = |p: &str| p.parse::<u16>().map(|n| n > 0).unwrap_or(false);
    if !port_ok(container) || (!host.is_empty() && !port_ok(host)) {
        return Err(bad());
    }
    Ok(PortSpec { key: format!("{container}/{proto}"), host_ip: ip, host_port: host.to_string() })
}

/// Checks a volume line: at least a source and a destination (drive letters are not separators).
fn check_volume(line: &str) -> Result<(), String> {
    let rest = line.get(2..).filter(|_| {
        let b = line.as_bytes();
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    });
    let parts = line.split(':').count() - if rest.is_some() { 1 } else { 0 };
    let dest_ok = line.split(':').nth(if rest.is_some() { 2 } else { 1 }).is_some_and(|d| !d.is_empty());
    if parts >= 2 && dest_ok {
        Ok(())
    } else {
        Err(format!("Invalid volume: {line}"))
    }
}

/// Non-empty trimmed lines of a multi-line field.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// Builds the create request from a run specification. Pure: no engine access.
///
/// Flags in `extra` are merged after the dialog fields; `--restart` and `--name` override or
/// conflict with the dialog fields as documented in [`parse_extra`].
pub fn build_create(spec: &RunSpec) -> Result<(CreateContainerOptions, ContainerCreateBody), String> {
    let image = spec.image.trim();
    if image.is_empty() {
        return Err("Image is required".into());
    }
    let extra = parse_extra(&spec.extra)?;

    let name = match (spec.name.trim(), extra.name.as_deref().map(str::trim)) {
        ("", None) | ("", Some("")) => None,
        (n, None) | (n, Some("")) if !n.is_empty() => Some(n.to_string()),
        ("", Some(n)) => Some(n.to_string()),
        _ => return Err("Container name given twice".into()),
    };

    let mut env: Vec<String> = lines(&spec.env).map(str::to_string).collect();
    env.extend(extra.env.iter().map(|e| e.trim().to_string()).filter(|e| !e.is_empty()));

    let mut exposed = BTreeSet::new();
    let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    let port_lines = lines(&spec.ports).chain(extra.publish.iter().map(|p| p.trim()).filter(|p| !p.is_empty()));
    for line in port_lines {
        let p = parse_port(line)?;
        exposed.insert(p.key.clone());
        bindings.entry(p.key).or_insert_with(|| Some(Vec::new())).get_or_insert_with(Vec::new).push(PortBinding {
            host_ip: p.host_ip,
            host_port: Some(p.host_port),
        });
    }

    let mut binds: Vec<String> = Vec::new();
    for line in lines(&spec.volumes).chain(extra.volumes.iter().map(|v| v.trim()).filter(|v| !v.is_empty())) {
        check_volume(line)?;
        binds.push(line.to_string());
    }

    let restart = match extra.restart {
        Some((r, retries)) => restart_policy(r, retries),
        None => restart_policy(spec.restart, None),
    };

    let mut labels = extra.labels.clone();
    labels.retain(|k, _| !k.is_empty());
    let mut host_config = HostConfig {
        binds: (!binds.is_empty()).then_some(binds),
        port_bindings: (!bindings.is_empty()).then_some(bindings),
        restart_policy: Some(restart),
        network_mode: extra.network.clone(),
        privileged: extra.privileged.then_some(true),
        auto_remove: extra.rm.then_some(true),
        memory: extra.memory,
        nano_cpus: extra.nano_cpus,
        publish_all_ports: extra.publish_all.then_some(true),
        ..Default::default()
    };
    if extra.rm {
        host_config.restart_policy = None;
    }

    let command = split_args(&spec.command);
    let body = ContainerCreateBody {
        image: Some(image.to_string()),
        env: (!env.is_empty()).then_some(env),
        cmd: (!command.is_empty()).then_some(command),
        entrypoint: extra.entrypoint.clone(),
        exposed_ports: (!exposed.is_empty()).then(|| exposed.into_iter().collect()),
        labels: (!labels.is_empty()).then(|| labels.into_iter().collect()),
        working_dir: extra.workdir.clone(),
        user: extra.user.clone(),
        hostname: extra.hostname.clone(),
        tty: extra.tty.then_some(true),
        open_stdin: extra.interactive.then_some(true),
        host_config: Some(host_config),
        ..Default::default()
    };
    let opts = CreateContainerOptions { name, ..Default::default() };
    Ok((opts, body))
}

fn restart_policy(restart: Restart, retries: Option<i64>) -> RestartPolicy {
    RestartPolicy {
        name: Some(restart.engine_name()),
        maximum_retry_count: if restart == Restart::OnFailure { retries } else { None },
    }
}

/// Progress of an image pull, shared between the pull task and the dialog.
#[derive(Clone, Debug, Default)]
pub struct PullProgress {
    pub status: String,
    /// Layer id to `(current, total)` bytes.
    pub layers: BTreeMap<String, (u64, u64)>,
    pub done: bool,
    pub error: Option<String>,
}

impl PullProgress {
    /// Overall fraction (0 to 1) over the layers with a known total, or `None` when no
    /// layer reports a total yet.
    pub fn fraction(&self) -> Option<f32> {
        let (cur, total) = self
            .layers
            .values()
            .filter(|(_, t)| *t > 0)
            .fold((0u64, 0u64), |(c, t), (cc, tt)| (c + cc, t + tt));
        if total == 0 {
            return None;
        }
        Some((cur as f64 / total as f64).min(1.0) as f32)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Splits `name[:tag]` into the engine's `fromImage` and tag. Defaults to `latest`.
/// A digest reference (`name@sha256:...`) is passed whole.
fn split_image(image: &str) -> (String, Option<String>) {
    if image.contains('@') {
        return (image.to_string(), None);
    }
    let last_slash = image.rfind('/').map_or(0, |i| i + 1);
    match image[last_slash..].rfind(':') {
        Some(i) => {
            let at = last_slash + i;
            (image[..at].to_string(), Some(image[at + 1..].to_string()))
        }
        None => (image.to_string(), Some("latest".to_string())),
    }
}

/// Pulls an image, updating `progress` per engine message.
pub async fn pull(docker: &Docker, image: &str, progress: Arc<Mutex<PullProgress>>) -> Result<(), String> {
    let (from_image, tag) = split_image(image.trim());
    {
        let mut p = lock(&progress);
        *p = PullProgress { status: format!("Pulling {image}"), ..Default::default() };
    }
    let mut builder = CreateImageOptionsBuilder::default().from_image(&from_image);
    if let Some(t) = tag.as_deref() {
        builder = builder.tag(t);
    }
    let mut stream = docker.create_image(Some(builder.build()), None, None);
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                let mut p = lock(&progress);
                if let Some(status) = info.status {
                    p.status = status;
                }
                if let (Some(id), Some(detail)) = (info.id, info.progress_detail) {
                    if let Some(total) = detail.total.filter(|t| *t > 0) {
                        let current = detail.current.unwrap_or(0).clamp(0, total);
                        p.layers.insert(id, (current as u64, total as u64));
                    }
                }
                if let Some(message) = info.error_detail.and_then(|d| d.message) {
                    p.error = Some(message.clone());
                    return Err(message);
                }
            }
            Err(e) => {
                let text = err_text(&e);
                lock(&progress).error = Some(text.clone());
                return Err(text);
            }
        }
    }
    let mut p = lock(&progress);
    p.done = true;
    p.status = format!("Pulled {image}");
    Ok(())
}

/// One Docker Hub search result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchHit {
    pub name: String,
    pub description: String,
    pub stars: i64,
    pub official: bool,
    pub automated: bool,
}

/// Searches the registry through the engine. Results are sorted by stars, highest first.
pub async fn search(docker: &Docker, term: &str, limit: u32) -> Result<Vec<SearchHit>, String> {
    let term = term.trim();
    if term.is_empty() {
        return Err("Search term is empty".into());
    }
    let opts = SearchImagesOptions {
        term: term.to_string(),
        limit: Some(i32::try_from(limit.max(1)).unwrap_or(i32::MAX)),
        filters: None,
    };
    let items = docker.search_images(opts).await.map_err(|e| err_text(&e))?;
    let mut hits: Vec<SearchHit> = items
        .into_iter()
        .map(|i| SearchHit {
            name: i.name.unwrap_or_default(),
            description: i.description.unwrap_or_default(),
            stars: i.star_count.unwrap_or(0),
            official: i.is_official.unwrap_or(false),
            automated: i.is_automated.unwrap_or(false),
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.stars));
    Ok(hits)
}

/// Creates and starts a container from the spec. Pulls the image first when it is missing
/// and `spec.pull` is set. Returns the container id. When the start fails, the created
/// container is removed again.
pub async fn run(docker: &Docker, spec: &RunSpec, progress: Arc<Mutex<PullProgress>>) -> Result<String, String> {
    let (opts, body) = build_create(spec)?;
    let image = spec.image.trim();
    if docker.inspect_image(image).await.is_err() {
        if !spec.pull {
            return Err(format!("Image not found locally: {image}"));
        }
        pull(docker, image, progress).await?;
    }
    let created = docker.create_container(Some(opts), body).await.map_err(|e| err_text(&e))?;
    let id = created.id;
    if let Err(e) = docker.start_container(&id, None).await {
        let opts = RemoveContainerOptions { force: true, ..Default::default() };
        let _ = docker.remove_container(&id, Some(opts)).await;
        return Err(err_text(&e));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(image: &str) -> RunSpec {
        RunSpec { image: image.into(), pull: true, ..Default::default() }
    }

    #[test]
    fn split_args_quotes_and_escapes() {
        assert_eq!(split_args("sleep 60"), ["sleep", "60"]);
        assert_eq!(split_args("  a   b  "), ["a", "b"]);
        assert_eq!(split_args(r#"sh -c "echo hi there""#), ["sh", "-c", "echo hi there"]);
        assert_eq!(split_args("echo 'a \"b\" c'"), ["echo", "a \"b\" c"]);
        assert_eq!(split_args(r"echo a\ b"), ["echo", "a b"]);
        assert_eq!(split_args(r#"echo "x\"y""#), ["echo", "x\"y"]);
        assert_eq!(split_args(r#"echo "a\nb""#), ["echo", r"a\nb"]);
        assert_eq!(split_args("echo ''"), ["echo", ""]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn build_create_ports_env_volumes_restart() {
        let s = RunSpec {
            ports: "8080:80\n127.0.0.1:9000:9000/udp\n53".into(),
            env: "A=1\n\n  B=two  \n".into(),
            volumes: "data:/var/lib/data\nC:\\dir:/srv:ro".into(),
            restart: Restart::UnlessStopped,
            command: "sh -c 'echo hi'".into(),
            name: "tmop-test-unit".into(),
            ..spec("nginx:1.27")
        };
        let (opts, body) = build_create(&s).unwrap();
        assert_eq!(opts.name.as_deref(), Some("tmop-test-unit"));
        assert_eq!(body.image.as_deref(), Some("nginx:1.27"));
        assert_eq!(body.env, Some(vec!["A=1".to_string(), "B=two".to_string()]));
        assert_eq!(body.cmd, Some(vec!["sh".into(), "-c".into(), "echo hi".into()]));
        assert_eq!(body.exposed_ports, Some(vec!["53/tcp".into(), "80/tcp".into(), "9000/udp".into()]));
        let host = body.host_config.unwrap();
        let bindings = host.port_bindings.unwrap();
        assert_eq!(
            bindings["80/tcp"].as_ref().unwrap()[0],
            PortBinding { host_ip: None, host_port: Some("8080".into()) }
        );
        assert_eq!(
            bindings["9000/udp"].as_ref().unwrap()[0],
            PortBinding { host_ip: Some("127.0.0.1".into()), host_port: Some("9000".into()) }
        );
        assert_eq!(bindings["53/tcp"].as_ref().unwrap()[0].host_port.as_deref(), Some(""));
        assert_eq!(host.binds.unwrap(), ["data:/var/lib/data", "C:\\dir:/srv:ro"]);
        let policy = host.restart_policy.unwrap();
        assert_eq!(policy.name, Some(RestartPolicyNameEnum::UNLESS_STOPPED));
        assert_eq!(policy.maximum_retry_count, None);
    }

    #[test]
    fn build_create_defaults_and_errors() {
        let (opts, body) = build_create(&spec("busybox")).unwrap();
        assert_eq!(opts.name, None);
        assert_eq!(body.env, None);
        assert_eq!(body.cmd, None);
        assert_eq!(body.exposed_ports, None);
        let host = body.host_config.unwrap();
        assert_eq!(host.binds, None);
        assert_eq!(host.restart_policy.unwrap().name, Some(RestartPolicyNameEnum::NO));

        assert!(build_create(&spec("  ")).is_err());
        assert!(build_create(&RunSpec { ports: "x:80".into(), ..spec("a") }).is_err());
        assert!(build_create(&RunSpec { ports: "80/xyz".into(), ..spec("a") }).is_err());
        assert!(build_create(&RunSpec { ports: "70000".into(), ..spec("a") }).is_err());
        assert!(build_create(&RunSpec { volumes: "/only-one".into(), ..spec("a") }).is_err());
    }

    #[test]
    fn extra_flags_are_mapped() {
        let s = RunSpec {
            extra: "-e K=V --env=X=1 -p 8080:80 --publish=443:443 -v vol:/d --name=web \
                    --restart on-failure:3 --network host -h box -u 1000:1000 -w /app \
                    --privileged -P -t -i -l team=ops -l flag --entrypoint /bin/sh \
                    -m 512m --cpus 1.5"
                .into(),
            ..spec("img")
        };
        let (opts, body) = build_create(&s).unwrap();
        assert_eq!(opts.name.as_deref(), Some("web"));
        assert_eq!(body.env, Some(vec!["K=V".into(), "X=1".into()]));
        assert_eq!(body.exposed_ports, Some(vec!["443/tcp".into(), "80/tcp".into()]));
        assert_eq!(body.working_dir.as_deref(), Some("/app"));
        assert_eq!(body.user.as_deref(), Some("1000:1000"));
        assert_eq!(body.hostname.as_deref(), Some("box"));
        assert_eq!(body.entrypoint, Some(vec!["/bin/sh".to_string()]));
        assert_eq!(body.tty, Some(true));
        assert_eq!(body.open_stdin, Some(true));
        assert_eq!(body.labels.unwrap().get("team").map(String::as_str), Some("ops"));
        let host = body.host_config.unwrap();
        assert_eq!(host.network_mode.as_deref(), Some("host"));
        assert_eq!(host.privileged, Some(true));
        assert_eq!(host.publish_all_ports, Some(true));
        assert_eq!(host.memory, Some(512 * 1024 * 1024));
        assert_eq!(host.nano_cpus, Some(1_500_000_000));
        assert_eq!(host.binds, Some(vec!["vol:/d".to_string()]));
        let policy = host.restart_policy.unwrap();
        assert_eq!(policy.name, Some(RestartPolicyNameEnum::ON_FAILURE));
        assert_eq!(policy.maximum_retry_count, Some(3));
    }

    #[test]
    fn extra_short_forms_and_rm() {
        let s = RunSpec { extra: "-it -p8080:80 -m2g --rm".into(), ..spec("img") };
        let (_, body) = build_create(&s).unwrap();
        assert_eq!(body.tty, Some(true));
        assert_eq!(body.open_stdin, Some(true));
        let host = body.host_config.unwrap();
        assert_eq!(host.memory, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(host.restart_policy.and_then(|p| p.name), None);
        assert!(body.exposed_ports.is_some());
    }

    #[test]
    fn extra_flag_errors() {
        let err = |extra: &str| build_create(&RunSpec { extra: extra.into(), ..spec("img") }).unwrap_err();
        assert_eq!(err("--detach"), "Unsupported option: --detach");
        assert_eq!(err("--gpus all"), "Unsupported option: --gpus");
        assert_eq!(err("--memory"), "Missing value for --memory");
        assert_eq!(err("--memory=lots"), "Invalid memory limit: lots");
        assert_eq!(err("--cpus 0"), "Invalid CPU count: 0");
        assert_eq!(err("--privileged=yes"), "Option --privileged takes no value");
        assert_eq!(err("--rm --restart always"), "Conflicting options: --restart and --rm");
        assert_eq!(err("--restart sometimes"), "Invalid restart policy: sometimes");
        assert_eq!(err("loose"), "Unexpected argument: loose");
        let dup = RunSpec { name: "a".into(), extra: "--name b".into(), ..spec("img") };
        assert!(build_create(&dup).is_err());
    }

    #[test]
    fn memory_and_cpu_parsing() {
        assert_eq!(parse_memory("1024"), Ok(1024));
        assert_eq!(parse_memory("1k"), Ok(1024));
        assert_eq!(parse_memory("2G"), Ok(2 * 1024 * 1024 * 1024));
        assert!(parse_memory("0m").is_err());
        assert_eq!(parse_cpus("0.5"), Ok(500_000_000));
    }

    #[test]
    fn pull_progress_fraction() {
        let mut p = PullProgress::default();
        assert_eq!(p.fraction(), None);
        p.layers.insert("a".into(), (50, 100));
        p.layers.insert("b".into(), (0, 300));
        assert_eq!(p.fraction(), Some(0.125));
        p.layers.insert("c".into(), (0, 0));
        assert_eq!(p.fraction(), Some(0.125), "layers without a total are ignored");
        p.layers.insert("a".into(), (100, 100));
        p.layers.insert("b".into(), (300, 300));
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn restart_tokens() {
        for r in Restart::ALL {
            assert_eq!(Restart::parse(r.token()), Some(r));
        }
        assert_eq!(Restart::parse(""), Some(Restart::No));
        assert_eq!(Restart::parse("sometimes"), None);
        assert_eq!(Restart::default(), Restart::No);
        assert_eq!(Restart::OnFailure.token(), "on-failure");
    }

    #[test]
    fn image_reference_split() {
        assert_eq!(split_image("nginx"), ("nginx".into(), Some("latest".into())));
        assert_eq!(split_image("nginx:1.27"), ("nginx".into(), Some("1.27".into())));
        assert_eq!(split_image("reg:5000/app"), ("reg:5000/app".into(), Some("latest".into())));
        assert_eq!(split_image("reg:5000/app:v2"), ("reg:5000/app".into(), Some("v2".into())));
        assert_eq!(split_image("app@sha256:abc"), ("app@sha256:abc".into(), None));
    }
}
