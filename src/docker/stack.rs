//! Compose stacks: group actions applied through the engine API, and `docker compose up`
//! through the CLI for stacks whose compose file is available locally.

use super::backend::err_text;
use super::model::{Container, Network, Stack, Volume};
use crate::platform::hide_console;
use bollard::errors::Error as BollardError;
use bollard::query_parameters::{RemoveContainerOptions, RemoveVolumeOptions};
use bollard::Docker;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// Label written by Compose on every object of a project.
const PROJECT_LABEL: &str = "com.docker.compose.project";

/// An action applied to every container of a stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackAction {
    Start,
    Stop,
    Restart,
    /// Stops and removes the containers and the stack's networks; `volumes` also removes its volumes.
    Down { volumes: bool },
}

impl StackAction {
    /// Button or menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
            Self::Down { volumes: false } => "Down",
            Self::Down { volumes: true } => "Down (with volumes)",
        }
    }
}

/// Applies the action to every container of the stack, concurrently.
///
/// `Down` stops and removes the containers, then the networks and (if `volumes`) the
/// volumes labelled with the stack name. Returns a summary such as "Stopped 3 containers",
/// or the text of the first error.
pub async fn stack_action(
    docker: &Docker,
    stack: &Stack,
    containers: &[Container],
    networks: &[Network],
    volumes: &[Volume],
    action: StackAction,
) -> Result<String, String> {
    let members: Vec<&Container> =
        containers.iter().filter(|c| c.stack.as_deref() == Some(stack.name.as_str())).collect();
    let futs = members.iter().map(|c| {
        let id = c.id.clone();
        async move { container_action(docker, &id, action).await }
    });
    let results = futures_util::future::join_all(futs).await;
    let count = results.len();
    if let Some(err) = results.into_iter().find_map(Result::err) {
        return Err(err);
    }

    let StackAction::Down { volumes: with_volumes } = action else {
        let verb = match action {
            StackAction::Start => "Started",
            StackAction::Stop => "Stopped",
            _ => "Restarted",
        };
        return Ok(format!("{verb} {}", plural(count, "container")));
    };

    let mut removed_networks = 0;
    for net in networks.iter().filter(|n| label_is(&n.labels, &stack.name)) {
        docker.remove_network(&net.id).await.map_err(|e| err_text(&e))?;
        removed_networks += 1;
    }
    let mut removed_volumes = 0;
    if with_volumes {
        for vol in volumes.iter().filter(|v| label_is(&v.labels, &stack.name)) {
            let opts = RemoveVolumeOptions { force: false };
            docker.remove_volume(&vol.name, Some(opts)).await.map_err(|e| err_text(&e))?;
            removed_volumes += 1;
        }
    }
    let mut text = format!("Removed {}", plural(count, "container"));
    if removed_networks > 0 {
        text.push_str(&format!(", {}", plural(removed_networks, "network")));
    }
    if with_volumes && removed_volumes > 0 {
        text.push_str(&format!(", {}", plural(removed_volumes, "volume")));
    }
    Ok(text)
}

/// Runs one container action. Engine answers 304 (already in that state) count as success.
async fn container_action(docker: &Docker, id: &str, action: StackAction) -> Result<(), String> {
    let not_modified = |e: &BollardError| matches!(e, BollardError::DockerResponseServerError { status_code: 304, .. });
    let ok_or = |r: Result<(), BollardError>| match r {
        Ok(()) => Ok(()),
        Err(e) if not_modified(&e) => Ok(()),
        Err(e) => Err(err_text(&e)),
    };
    match action {
        StackAction::Start => ok_or(docker.start_container(id, None).await),
        StackAction::Stop => {
            ok_or(docker.stop_container(id, None).await)?;
            wait_exited(docker, id).await
        }
        StackAction::Restart => ok_or(docker.restart_container(id, None).await),
        StackAction::Down { .. } => {
            ok_or(docker.stop_container(id, None).await)?;
            wait_exited(docker, id).await?;
            let opts = RemoveContainerOptions { force: true, ..Default::default() };
            docker.remove_container(id, Some(opts)).await.map_err(|e| err_text(&e))
        }
    }
}

/// Polls until the container is no longer running, for up to about 60 seconds.
///
/// The engine can answer a stop request before the container has actually exited, so the
/// action is only reported as done once the state confirms it.
async fn wait_exited(docker: &Docker, id: &str) -> Result<(), String> {
    const POLLS: u32 = 120;
    const INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    for _ in 0..POLLS {
        let info = docker.inspect_container(id, None).await.map_err(|e| err_text(&e))?;
        let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        if !running {
            return Ok(());
        }
        tokio::time::sleep(INTERVAL).await;
    }
    Err(format!("Container {} did not stop in time", short(id)))
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

fn label_is(labels: &std::collections::BTreeMap<String, String>, project: &str) -> bool {
    labels.get(PROJECT_LABEL).map(String::as_str) == Some(project)
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Arguments for `docker compose up`: `[--context <ctx>] compose -f <file> [-p <project>] up -d`.
/// The context is passed only when it is not empty.
pub fn compose_up_args(compose_file: &Path, project: Option<&str>, context: &str) -> Vec<String> {
    let mut args = Vec::new();
    if !context.is_empty() {
        args.push("--context".to_string());
        args.push(context.to_string());
    }
    args.push("compose".to_string());
    args.push("-f".to_string());
    args.push(compose_file.display().to_string());
    if let Some(p) = project {
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    args.push("up".to_string());
    args.push("-d".to_string());
    args
}

/// A running `docker compose up`. `output` grows while the process runs; `result` is set once it exits.
pub struct ComposeRun {
    /// Combined stdout and stderr, appended live.
    pub output: Arc<Mutex<String>>,
    pub result: Arc<Mutex<Option<Result<(), String>>>>,
    pub started: Instant,
}

/// Number of trailing output lines kept in the error text of a failed run.
const ERROR_TAIL_LINES: usize = 6;

/// Starts `docker compose up -d` in the compose file's directory and pumps its output on a
/// thread named `tmop-compose`.
pub fn compose_up(compose_file: &Path, project: Option<&str>, context: &str) -> Result<ComposeRun, String> {
    let dir = compose_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut cmd = Command::new("docker");
    cmd.args(compose_up_args(compose_file, project, context))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Could not start docker: {e}"))?;

    let output = Arc::new(Mutex::new(String::new()));
    let result = Arc::new(Mutex::new(None));
    let started = Instant::now();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (out_w, res_w) = (output.clone(), result.clone());
    std::thread::Builder::new()
        .name("tmop-compose".into())
        .spawn(move || {
            std::thread::scope(|s| {
                if let Some(r) = stdout {
                    let out = &out_w;
                    s.spawn(move || pump(r, out));
                }
                if let Some(r) = stderr {
                    let out = &out_w;
                    s.spawn(move || pump(r, out));
                }
            });
            let outcome = match child.wait() {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => {
                    let text = out_w.lock().map(|g| g.clone()).unwrap_or_default();
                    Err(failure_text(&text, &status.to_string()))
                }
                Err(e) => Err(e.to_string()),
            };
            *res_w.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        })
        .map_err(|e| e.to_string())?;
    Ok(ComposeRun { output, result, started })
}

/// Copies a child pipe into the shared output until EOF.
fn pump(mut reader: impl Read, out: &Mutex<String>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                out.lock().unwrap_or_else(|e| e.into_inner()).push_str(&text);
            }
        }
    }
}

/// Error text for a failed compose run: the last non-empty output lines, or the exit status.
fn failure_text(output: &str, status: &str) -> String {
    let lines: Vec<&str> = output.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return format!("docker compose exited with {status}");
    }
    let tail = &lines[lines.len().saturating_sub(ERROR_TAIL_LINES)..];
    tail.join("\n")
}

/// True when `docker compose version` succeeds. The answer is cached for the process lifetime.
pub fn compose_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let mut cmd = Command::new("docker");
        cmd.args(["compose", "version"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        hide_console(&mut cmd);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    })
}

/// Opens a file with its default application.
pub fn open_in_editor(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("File not found: {}", path.display()));
    }
    crate::platform::open_path(path)
}

/// The compose file of a stack, when it exists on this machine.
///
/// `compose_file` is used when it is an absolute path to an existing file (the first entry
/// when the label lists several files). Otherwise the file name is looked up in `working_dir`.
/// Returns `None` when nothing exists locally, as for stacks on remote hosts.
pub fn local_compose_file(stack: &Stack) -> Option<PathBuf> {
    let listed = stack.compose_file.as_deref()?.split(',').next()?.trim();
    if listed.is_empty() {
        return None;
    }
    let listed = PathBuf::from(listed);
    if listed.is_absolute() && listed.is_file() {
        return Some(listed);
    }
    let dir = stack.working_dir.as_deref().filter(|d| !d.is_empty())?;
    let candidate = Path::new(dir).join(listed.file_name()?);
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(StackAction::Start.label(), "Start");
        assert_eq!(StackAction::Stop.label(), "Stop");
        assert_eq!(StackAction::Restart.label(), "Restart");
        assert_eq!(StackAction::Down { volumes: false }.label(), "Down");
        assert_eq!(StackAction::Down { volumes: true }.label(), "Down (with volumes)");
    }

    #[test]
    fn compose_args_variants() {
        let file = Path::new("/srv/app/compose.yaml");
        assert_eq!(
            compose_up_args(file, None, ""),
            ["compose", "-f", "/srv/app/compose.yaml", "up", "-d"]
        );
        assert_eq!(
            compose_up_args(file, Some("app"), "desktop-linux"),
            ["--context", "desktop-linux", "compose", "-f", "/srv/app/compose.yaml", "-p", "app", "up", "-d"]
        );
        assert_eq!(
            compose_up_args(file, Some("app"), ""),
            ["compose", "-f", "/srv/app/compose.yaml", "-p", "app", "up", "-d"]
        );
    }

    #[test]
    fn failure_text_keeps_tail() {
        assert_eq!(failure_text("", "exit code: 1"), "docker compose exited with exit code: 1");
        let many: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let text = failure_text(&many, "exit code: 1");
        assert!(text.ends_with("line 10"));
        assert!(!text.contains("line 1\n"));
    }

    #[test]
    fn plural_nouns() {
        assert_eq!(plural(1, "container"), "1 container");
        assert_eq!(plural(0, "container"), "0 containers");
        assert_eq!(plural(3, "network"), "3 networks");
    }

    #[test]
    fn local_compose_file_lookup() {
        let base = std::env::temp_dir().join(format!("tmop-stack-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("compose.yaml");
        std::fs::write(&file, "services: {}\n").unwrap();
        let dir = base.display().to_string();

        // Absolute path that exists.
        let stack = Stack {
            name: "app".into(),
            compose_file: Some(file.display().to_string()),
            working_dir: None,
            ..Default::default()
        };
        assert_eq!(local_compose_file(&stack), Some(file.clone()));

        // Absolute path missing, but the file name exists in the working directory.
        let stack = Stack {
            name: "app".into(),
            compose_file: Some(base.join("gone").join("compose.yaml").display().to_string()),
            working_dir: Some(dir.clone()),
            ..Default::default()
        };
        assert_eq!(local_compose_file(&stack), Some(file.clone()));

        // Nothing exists locally.
        let stack = Stack {
            name: "remote".into(),
            compose_file: Some("/remote/compose.yaml".into()),
            working_dir: Some("/remote".into()),
            ..Default::default()
        };
        assert_eq!(local_compose_file(&stack), None);
        assert_eq!(local_compose_file(&Stack::default()), None);

        let _ = std::fs::remove_dir_all(&base);
    }
}
