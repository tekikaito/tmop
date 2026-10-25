//! Connection factory: turns an [`Endpoint`] into a connected `bollard::Docker`.
//!
//! SSH hosts are reached through an `ssh` child process that forwards a local TCP
//! port to the remote Docker socket. The process is killed when the [`SshTunnel`]
//! is dropped, so it lives exactly as long as the [`Connection`].

use super::context::Endpoint;
use bollard::{Docker, API_DEFAULT_VERSION};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Timeout for requests to the engine, in seconds.
pub const API_TIMEOUT_SECS: u64 = 20;

/// Remote socket that the SSH tunnel forwards to.
const REMOTE_SOCKET: &str = "/var/run/docker.sock";

/// How long to wait for the tunnel's local port to accept connections.
const TUNNEL_WAIT: Duration = Duration::from_secs(5);

/// A connected engine. Keep it alive while the engine is in use.
pub struct Connection {
    pub docker: Docker,
    pub ssh_tunnel: Option<SshTunnel>,
}

/// A running `ssh -L` process. Killed on drop.
pub struct SshTunnel {
    child: Child,
    pub local_port: u16,
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Connects to `endpoint`. Blocking: an SSH endpoint waits up to five seconds for the tunnel.
pub fn connect(endpoint: &Endpoint) -> Result<Connection, String> {
    match endpoint {
        Endpoint::NamedPipe(path) => connect_named_pipe(path).map(plain),
        Endpoint::Unix(path) => connect_unix(path).map(plain),
        Endpoint::Tcp { host, tls: None } => Docker::connect_with_http(&format!("http://{host}"), API_TIMEOUT_SECS, API_DEFAULT_VERSION)
            .map(plain)
            .map_err(|e| e.to_string()),
        Endpoint::Tcp { tls: Some(_), .. } => Err("TLS hosts are not supported in this build".into()),
        Endpoint::Ssh { host, user, port } => connect_ssh(host, user.as_deref(), *port),
    }
}

fn plain(docker: Docker) -> Connection {
    Connection { docker, ssh_tunnel: None }
}

#[cfg(windows)]
fn connect_named_pipe(path: &str) -> Result<Docker, String> {
    Docker::connect_with_named_pipe(path, API_TIMEOUT_SECS, API_DEFAULT_VERSION).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn connect_named_pipe(_path: &str) -> Result<Docker, String> {
    Err("named pipes are only available on Windows".into())
}

#[cfg(unix)]
fn connect_unix(path: &str) -> Result<Docker, String> {
    Docker::connect_with_unix(path, API_TIMEOUT_SECS, API_DEFAULT_VERSION).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn connect_unix(_path: &str) -> Result<Docker, String> {
    Err("unix sockets are not available on this platform".into())
}

fn connect_ssh(host: &str, user: Option<&str>, port: Option<u16>) -> Result<Connection, String> {
    let local_port = free_local_port()?;
    let args = ssh_args(host, user, port, local_port);
    let mut command = Command::new(ssh_program());
    command.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::hide_console(&mut command);
    let child = command.spawn().map_err(|e| format!("could not start ssh: {e}"))?;
    // Owning the tunnel now means a failed wait below still kills the process.
    let mut tunnel = SshTunnel { child, local_port };
    wait_for_port(&mut tunnel)?;
    let docker = Docker::connect_with_http(&format!("http://127.0.0.1:{local_port}"), API_TIMEOUT_SECS, API_DEFAULT_VERSION)
        .map_err(|e| e.to_string())?;
    Ok(Connection { docker, ssh_tunnel: Some(tunnel) })
}

fn ssh_program() -> &'static str {
    "ssh"
}

/// Waits until the tunnel's local port accepts connections, or ssh exits.
fn wait_for_port(tunnel: &mut SshTunnel) -> Result<(), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, tunnel.local_port));
    let deadline = Instant::now() + TUNNEL_WAIT;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if let Ok(Some(status)) = tunnel.child.try_wait() {
            return Err(format!("ssh exited before the tunnel opened ({status}); key or agent authentication is required"));
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the ssh tunnel".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A local port that was free when asked for.
fn free_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("no free local port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    Ok(port)
}

/// Arguments for the tunnel process. `BatchMode` means keys or an agent only; ssh never prompts.
pub fn ssh_args(host: &str, user: Option<&str>, port: Option<u16>, local_port: u16) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-N".to_string(),
        "-L".to_string(),
        format!("127.0.0.1:{local_port}:{REMOTE_SOCKET}"),
    ];
    if let Some(p) = port {
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    args.push(match user {
        Some(u) => format!("{u}@{host}"),
        None => host.to_string(),
    });
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_with_user_and_port() {
        let args = ssh_args("nas", Some("marc"), Some(2222), 40123);
        assert_eq!(
            args,
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-N",
                "-L",
                "127.0.0.1:40123:/var/run/docker.sock",
                "-p",
                "2222",
                "marc@nas",
            ]
        );
    }

    #[test]
    fn ssh_args_minimal() {
        let args = ssh_args("box", None, None, 1);
        assert_eq!(args.last().map(String::as_str), Some("box"));
        assert!(!args.iter().any(|a| a == "-p"));
        assert!(args.iter().any(|a| a == "127.0.0.1:1:/var/run/docker.sock"));
    }

    #[test]
    fn free_port_is_nonzero() {
        assert_ne!(free_local_port().unwrap(), 0);
    }

    #[test]
    fn tls_hosts_are_refused_without_ssl_support() {
        let ep = Endpoint::Tcp {
            host: "h:2376".into(),
            tls: Some(super::super::context::TlsFiles {
                ca: "ca".into(),
                cert: "cert".into(),
                key: "key".into(),
                skip_verify: false,
            }),
        };
        assert!(connect(&ep).is_err());
    }
}
