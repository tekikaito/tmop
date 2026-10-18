//! Plain data types for engine objects, shared by the Docker backend and the UI.
//!
//! Nothing here depends on `bollard`; the client converts API responses into these types.

use std::collections::{BTreeMap, BTreeSet};

/// Engine-wide facts from the system info endpoint.
#[derive(Clone, Debug, Default)]
pub struct EngineInfo {
    pub id: String,
    pub name: String,
    pub server_version: String,
    pub api_version: String,
    pub os: String,
    pub os_type: String,
    pub arch: String,
    pub kernel: String,
    pub cpus: u64,
    pub mem_total: u64,
    pub containers: u64,
    pub containers_running: u64,
    pub containers_paused: u64,
    pub containers_stopped: u64,
    pub images: u64,
    pub driver: String,
    pub docker_root: String,
    pub cgroup_version: String,
    pub swarm: String,
    pub warnings: Vec<String>,
}

/// Container lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ContainerState {
    #[default]
    Unknown,
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
}

impl ContainerState {
    /// Parses the engine's state string (case-insensitive). Unrecognised values are `Unknown`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "created" => Self::Created,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::Unknown,
        }
    }

    /// Lower-case label as the engine writes it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Restarting => "restarting",
            Self::Removing => "removing",
            Self::Exited => "exited",
            Self::Dead => "dead",
        }
    }

    /// True only for `Running`; paused and restarting containers are not counted as running.
    pub fn is_running(self) -> bool {
        self == Self::Running
    }
}

/// One published or exposed port.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortMapping {
    pub ip: String,
    pub private_port: u16,
    pub public_port: Option<u16>,
    pub proto: String,
}

/// A container, as shown in lists and details.
#[derive(Clone, Debug, Default)]
pub struct Container {
    pub id: String,
    /// First 12 characters of `id`.
    pub short_id: String,
    /// Name without the leading `/`.
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub command: String,
    /// Creation time, unix seconds.
    pub created: i64,
    pub state: ContainerState,
    pub status: String,
    pub ports: Vec<PortMapping>,
    pub labels: BTreeMap<String, String>,
    /// Compose project (`com.docker.compose.project`).
    pub stack: Option<String>,
    /// Compose service (`com.docker.compose.service`).
    pub service: Option<String>,
    /// Compose file list (`com.docker.compose.project.config_files`).
    pub compose_file: Option<String>,
    /// Compose working directory (`com.docker.compose.project.working_dir`).
    pub compose_dir: Option<String>,
    pub exit_code: Option<i64>,
    pub oom_killed: bool,
    pub restart_count: u64,
}

/// A local image.
#[derive(Clone, Debug, Default)]
pub struct Image {
    pub id: String,
    pub short_id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    /// Creation time, unix seconds.
    pub created: i64,
    pub size: u64,
    pub shared_size: i64,
    /// Number of containers using the image; -1 when unknown.
    pub containers: i64,
    pub dangling: bool,
    pub labels: BTreeMap<String, String>,
}

impl Image {
    /// First real tag, or `<none>` when the image has none.
    pub fn display_name(&self) -> String {
        self.repo_tags
            .iter()
            .find(|t| t.as_str() != "<none>:<none>")
            .cloned()
            .unwrap_or_else(|| "<none>".into())
    }
}

/// A volume.
#[derive(Clone, Debug, Default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created: Option<String>,
    pub scope: String,
    pub labels: BTreeMap<String, String>,
    pub size: Option<u64>,
    pub ref_count: Option<i64>,
    pub stack: Option<String>,
}

/// A network.
#[derive(Clone, Debug, Default)]
pub struct Network {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub internal: bool,
    pub attachable: bool,
    pub ipv6: bool,
    pub subnets: Vec<String>,
    pub gateways: Vec<String>,
    pub created: Option<String>,
    pub labels: BTreeMap<String, String>,
    /// Names of attached containers.
    pub containers: Vec<String>,
    pub stack: Option<String>,
}

impl Network {
    /// The networks Docker creates itself.
    pub fn is_builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

/// A Compose project, grouped from containers by the `com.docker.compose.project` label.
#[derive(Clone, Debug, Default)]
pub struct Stack {
    pub name: String,
    pub compose_file: Option<String>,
    pub working_dir: Option<String>,
    /// Unique service names, sorted.
    pub services: Vec<String>,
    /// Container ids.
    pub containers: Vec<String>,
    pub running: usize,
    pub total: usize,
}

/// Groups containers by stack label. Stacks are sorted by name.
pub fn stacks_from(containers: &[Container]) -> Vec<Stack> {
    let mut by_name: BTreeMap<&str, (Stack, BTreeSet<String>)> = BTreeMap::new();
    for c in containers {
        let Some(name) = c.stack.as_deref() else {
            continue;
        };
        let (stack, services) = by_name.entry(name).or_insert_with(|| {
            (Stack { name: name.to_string(), ..Default::default() }, BTreeSet::new())
        });
        if stack.compose_file.is_none() {
            stack.compose_file = c.compose_file.clone();
        }
        if stack.working_dir.is_none() {
            stack.working_dir = c.compose_dir.clone();
        }
        if let Some(svc) = &c.service {
            services.insert(svc.clone());
        }
        stack.containers.push(c.id.clone());
        stack.total += 1;
        if c.state.is_running() {
            stack.running += 1;
        }
    }
    by_name
        .into_values()
        .map(|(mut stack, services)| {
            stack.services = services.into_iter().collect();
            stack
        })
        .collect()
}

/// One sample of a container's resource use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatsSample {
    pub cpu_pct: f32,
    pub mem_used: u64,
    pub mem_limit: u64,
    pub mem_pct: f32,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub blk_read_bps: f64,
    pub blk_write_bps: f64,
    pub pids: u64,
}

/// Sums over all containers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub containers: usize,
    pub running: usize,
    pub paused: usize,
    pub stopped: usize,
    pub cpu_pct: f32,
    pub mem_used: u64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
    pub blk_read_bps: f64,
    pub blk_write_bps: f64,
}

/// Connection state of the engine, as shown in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EngineStatus {
    #[default]
    Unknown,
    Connecting,
    Connected,
    Unreachable,
}

/// Relative age, such as "3 days ago" or "just now".
pub fn format_age(created_unix: i64, now_unix: i64) -> String {
    let secs = now_unix.saturating_sub(created_unix);
    if secs < 60 {
        return "just now".into();
    }
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    let minutes = secs / 60;
    if minutes < 60 {
        return plural(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return plural(hours, "hour");
    }
    plural(hours / 24, "day")
}

/// Human-readable byte count (`crate::process::format_bytes`).
pub fn format_bytes(b: u64) -> String {
    crate::process::format_bytes(b as f64)
}

/// CPU percentage from two consecutive samples, using the Docker stats formula:
/// `cpu_delta / system_delta * online_cpus * 100`. Returns 0 when either delta is zero.
pub fn cpu_percent(cpu_total: u64, cpu_total_prev: u64, system: u64, system_prev: u64, online_cpus: u64) -> f32 {
    let cpu_delta = cpu_total.saturating_sub(cpu_total_prev);
    let system_delta = system.saturating_sub(system_prev);
    if cpu_delta == 0 || system_delta == 0 {
        return 0.0;
    }
    ((cpu_delta as f64 / system_delta as f64) * online_cpus as f64 * 100.0) as f32
}

/// Per-second rate between two cumulative counters. Returns 0 for a non-positive interval.
pub fn rate(cur: u64, prev: u64, dt_secs: f64) -> f64 {
    if dt_secs <= 0.0 {
        return 0.0;
    }
    cur.saturating_sub(prev) as f64 / dt_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(id: &str, stack: Option<&str>, service: Option<&str>, state: ContainerState) -> Container {
        Container {
            id: id.into(),
            stack: stack.map(str::to_string),
            service: service.map(str::to_string),
            state,
            compose_file: stack.map(|s| format!("/src/{s}/compose.yaml")),
            compose_dir: stack.map(|s| format!("/src/{s}")),
            ..Default::default()
        }
    }

    #[test]
    fn stacks_group_by_label() {
        let containers = vec![
            container("c1", Some("web"), Some("nginx"), ContainerState::Running),
            container("c2", Some("web"), Some("api"), ContainerState::Exited),
            container("c3", Some("web"), Some("nginx"), ContainerState::Running),
            container("c4", Some("db"), Some("postgres"), ContainerState::Paused),
            container("c5", None, None, ContainerState::Running),
        ];
        let stacks = stacks_from(&containers);
        let names: Vec<&str> = stacks.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["db", "web"]);
        let web = &stacks[1];
        assert_eq!(web.services, ["api", "nginx"]);
        assert_eq!(web.containers, ["c1", "c2", "c3"]);
        assert_eq!((web.running, web.total), (2, 3));
        assert_eq!(web.compose_file.as_deref(), Some("/src/web/compose.yaml"));
        assert_eq!(web.working_dir.as_deref(), Some("/src/web"));
        assert_eq!((stacks[0].running, stacks[0].total), (0, 1));
        assert!(stacks_from(&[]).is_empty());
    }

    #[test]
    fn container_state_parses() {
        assert_eq!(ContainerState::parse("running"), ContainerState::Running);
        assert_eq!(ContainerState::parse("Exited"), ContainerState::Exited);
        assert_eq!(ContainerState::parse(" dead "), ContainerState::Dead);
        assert_eq!(ContainerState::parse("bogus"), ContainerState::Unknown);
        assert_eq!(ContainerState::parse(""), ContainerState::Unknown);
        assert!(ContainerState::Running.is_running());
        assert!(!ContainerState::Paused.is_running());
        assert_eq!(ContainerState::Restarting.label(), "restarting");
        assert_eq!(ContainerState::default(), ContainerState::Unknown);
    }

    #[test]
    fn age_text() {
        let now = 1_000_000_000;
        assert_eq!(format_age(now - 5, now), "just now");
        assert_eq!(format_age(now + 100, now), "just now", "future timestamps");
        assert_eq!(format_age(now - 60, now), "1 minute ago");
        assert_eq!(format_age(now - 15 * 60, now), "15 minutes ago");
        assert_eq!(format_age(now - 2 * 3600, now), "2 hours ago");
        assert_eq!(format_age(now - 3 * 86400, now), "3 days ago");
        assert_eq!(format_age(now - 86400, now), "1 day ago");
    }

    #[test]
    fn cpu_formula() {
        // cpu delta 500, system delta 2000, 4 online cpus: 500 / 2000 * 4 * 100 = 100.
        assert!((cpu_percent(1500, 1000, 12_000, 10_000, 4) - 100.0).abs() < 1e-4);
        assert_eq!(cpu_percent(1000, 1000, 12_000, 10_000, 4), 0.0, "no cpu delta");
        assert_eq!(cpu_percent(1500, 1000, 10_000, 10_000, 4), 0.0, "no system delta");
        assert_eq!(cpu_percent(1000, 1500, 12_000, 10_000, 4), 0.0, "counter went backwards");
    }

    #[test]
    fn rate_is_per_second() {
        assert_eq!(rate(3000, 1000, 2.0), 1000.0);
        assert_eq!(rate(1000, 3000, 2.0), 0.0);
        assert_eq!(rate(3000, 1000, 0.0), 0.0);
    }

    #[test]
    fn image_display_name() {
        let mut img = Image::default();
        assert_eq!(img.display_name(), "<none>");
        img.repo_tags = vec!["<none>:<none>".into()];
        assert_eq!(img.display_name(), "<none>");
        img.repo_tags = vec!["<none>:<none>".into(), "nginx:latest".into(), "nginx:1".into()];
        assert_eq!(img.display_name(), "nginx:latest");
    }

    #[test]
    fn builtin_networks() {
        let mut n = Network { name: "bridge".into(), ..Default::default() };
        assert!(n.is_builtin());
        n.name = "app_default".into();
        assert!(!n.is_builtin());
    }
}
