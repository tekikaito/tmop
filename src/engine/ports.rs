//! LAN port forwarder for ports published by engine containers.
//!
//! The engine's own localhost forwarding covers `127.0.0.1`; this module binds the host's LAN
//! addresses (never `0.0.0.0`) and relays each published port to the distro IP.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows::core::PWSTR;

/// Delay before retrying an accept that failed (for example when out of handles).
const ACCEPT_RETRY: Duration = Duration::from_millis(200);

/// One host port that a container publishes on a non-loopback address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PublishedPort {
    pub host_port: u16,
    pub proto: String,
    pub bind_ip: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    #[serde(default)]
    ports: Vec<PortEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PortEntry {
    #[serde(default, rename = "IP")]
    ip: String,
    #[serde(default)]
    public_port: Option<u16>,
    #[serde(default, rename = "Type")]
    kind: String,
}

/// Published TCP ports from a Docker `GET /containers/json` body. Loopback-bound entries are
/// skipped, as are unparsable bodies (which yield an empty list).
pub fn published_ports(containers_json: &[u8]) -> Vec<PublishedPort> {
    let Ok(containers) = serde_json::from_slice::<Vec<Container>>(containers_json) else {
        return Vec::new();
    };
    let mut out = BTreeSet::new();
    for entry in containers.iter().flat_map(|c| c.ports.iter()) {
        let Some(host_port) = entry.public_port.filter(|p| *p != 0) else {
            continue;
        };
        if entry.kind != "tcp" || !matches!(entry.ip.as_str(), "" | "0.0.0.0" | "::") {
            continue;
        }
        out.insert(PublishedPort { host_port, proto: entry.kind.clone(), bind_ip: entry.ip.clone() });
    }
    out.into_iter().collect()
}

/// Non-loopback, non-APIPA IPv4 addresses of up adapters. Hyper-V and WSL virtual adapters are
/// skipped by description (best effort).
pub fn lan_ipv4_addresses() -> Vec<Ipv4Addr> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 15 * 1024u32;
    let mut buf: Vec<u64>;
    loop {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `buf` provides `size` bytes at an 8-byte aligned address; `size` is passed by pointer.
        let rc = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        if rc == NO_ERROR.0 {
            break;
        }
        if rc != ERROR_BUFFER_OVERFLOW.0 {
            return Vec::new();
        }
    }

    let mut out = Vec::new();
    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: the list lives in `buf`, which outlives this loop; each `Next`/unicast pointer is
    // one the API wrote into that buffer, and a null pointer ends each chain.
    unsafe {
        while !adapter.is_null() {
            let a = &*adapter;
            let description = pwstr(a.Description);
            let friendly = pwstr(a.FriendlyName);
            let is_virtual = [&description, &friendly].iter().any(|s| s.contains("Hyper-V") || s.contains("WSL"));
            if a.OperStatus == IfOperStatusUp && !is_virtual {
                let mut unicast = a.FirstUnicastAddress;
                while !unicast.is_null() {
                    let u = &*unicast;
                    let sa = u.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET {
                        let sin = &*(sa as *const SOCKADDR_IN);
                        // `S_addr` keeps the network-order bytes in memory, so native bytes are the address.
                        let ip = Ipv4Addr::from(sin.sin_addr.S_un.S_addr.to_ne_bytes());
                        if is_lan(ip) && !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                    unicast = u.Next;
                }
            }
            adapter = a.Next;
        }
    }
    out
}

/// True for addresses worth binding: not loopback, unspecified, multicast, broadcast or APIPA.
fn is_lan(ip: Ipv4Addr) -> bool {
    !(ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() || ip.is_link_local())
}

/// Copies a NUL-terminated wide string; empty for null.
///
/// # Safety
/// `p` must be null or point to a NUL-terminated UTF-16 string that stays valid for the call.
unsafe fn pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.0.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
}

type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// A live listener for one (bind address, port) pair.
struct Listener {
    target: SocketAddr,
    task: JoinHandle<()>,
}

/// Forwards `bind_addr:port` to `target_ip:port` for every desired pair.
pub struct Forwarder {
    log: Log,
    listeners: HashMap<(Ipv4Addr, u16), Listener>,
    /// Pairs whose bind failed; logged once and retried on each reconcile.
    failed: HashSet<(Ipv4Addr, u16)>,
}

impl Forwarder {
    /// An empty forwarder that reports through `log`.
    pub fn new(log: Log) -> Self {
        Self { log, listeners: HashMap::new(), failed: HashSet::new() }
    }

    /// Brings the listeners in line with `ports` on each of `bind_addrs`, forwarding to `target_ip`.
    pub async fn reconcile(&mut self, target_ip: Ipv4Addr, ports: &[u16], bind_addrs: &[Ipv4Addr]) {
        let desired: HashSet<(Ipv4Addr, u16)> = bind_addrs.iter().flat_map(|a| ports.iter().map(move |p| (*a, *p))).collect();

        // A changed distro IP invalidates every listener.
        let stale: Vec<_> = self.listeners.iter().filter(|(k, l)| !desired.contains(k) || l.target.ip() != target_ip).map(|(k, _)| *k).collect();
        for key in stale {
            if let Some(listener) = self.listeners.remove(&key) {
                close(listener).await;
                (self.log)(&format!("stopped forwarding {}:{}", key.0, key.1));
            }
        }
        self.failed.retain(|k| desired.contains(k));

        for (addr, port) in desired {
            if self.listeners.contains_key(&(addr, port)) {
                continue;
            }
            match TcpListener::bind(SocketAddr::from((addr, port))).await {
                Ok(listener) => {
                    self.failed.remove(&(addr, port));
                    let target = SocketAddr::from((target_ip, port));
                    let task = tokio::spawn(accept_loop(listener, target, self.log.clone()));
                    (self.log)(&format!("forwarding {addr}:{port} -> {target}"));
                    self.listeners.insert((addr, port), Listener { target, task });
                }
                Err(e) => {
                    if self.failed.insert((addr, port)) {
                        (self.log)(&format!("cannot bind {addr}:{port}, skipped: {e}"));
                    }
                }
            }
        }
    }

    /// Closes every listener.
    pub async fn clear(&mut self) {
        for ((addr, port), listener) in self.listeners.drain() {
            close(listener).await;
            (self.log)(&format!("stopped forwarding {addr}:{port}"));
        }
        self.failed.clear();
    }
}

/// Aborts the accept task and waits for it, so the listening socket is closed when this returns.
async fn close(listener: Listener) {
    listener.task.abort();
    let _ = listener.task.await;
}

async fn accept_loop(listener: TcpListener, target: SocketAddr, log: Log) {
    // Connection tasks live in this set: aborting the listener drops it and closes their sockets.
    let mut conns = JoinSet::new();
    loop {
        // Reap finished connections so the set does not grow without bound.
        while conns.try_join_next().is_some() {}
        match listener.accept().await {
            Ok((mut client, _)) => {
                let log = log.clone();
                conns.spawn(async move {
                    match TcpStream::connect(target).await {
                        Ok(mut upstream) => {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                        Err(e) => log(&format!("connect to {target} failed: {e}")),
                    }
                });
            }
            Err(e) => {
                log(&format!("accept on {target} failed: {e}"));
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SAMPLE: &str = r#"[
        {"Id":"a","Names":["/web"],"Ports":[
            {"IP":"0.0.0.0","PrivatePort":80,"PublicPort":18081,"Type":"tcp"},
            {"IP":"::","PrivatePort":80,"PublicPort":18081,"Type":"tcp"},
            {"PrivatePort":53,"Type":"udp"},
            {"IP":"127.0.0.1","PrivatePort":5432,"PublicPort":15432,"Type":"tcp"}
        ]},
        {"Id":"b","Names":["/db"],"Ports":[
            {"IP":"0.0.0.0","PrivatePort":53,"PublicPort":5353,"Type":"udp"},
            {"PrivatePort":3000,"Type":"tcp"},
            {"IP":"","PrivatePort":8080,"PublicPort":18080,"Type":"tcp"}
        ]}
    ]"#;

    #[test]
    fn published_ports_keeps_lan_tcp_and_dedupes() {
        let ports = published_ports(SAMPLE.as_bytes());
        let got: Vec<(u16, &str, &str)> = ports.iter().map(|p| (p.host_port, p.proto.as_str(), p.bind_ip.as_str())).collect();
        assert_eq!(got, vec![(18080, "tcp", ""), (18081, "tcp", "0.0.0.0"), (18081, "tcp", "::")]);
    }

    #[test]
    fn published_ports_tolerates_bad_input() {
        assert!(published_ports(b"").is_empty());
        assert!(published_ports(b"{not json").is_empty());
        assert!(published_ports(b"[]").is_empty());
        assert!(published_ports(br#"[{"Ports":[]}]"#).is_empty());
    }

    #[test]
    fn lan_filter_excludes_special_ranges() {
        assert!(is_lan(Ipv4Addr::new(192, 168, 178, 25)));
        assert!(!is_lan(Ipv4Addr::LOCALHOST));
        assert!(!is_lan(Ipv4Addr::new(169, 254, 3, 4)));
        assert!(!is_lan(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn adapter_enumeration_runs() {
        // Only checks that the call succeeds; the result depends on the machine.
        for ip in lan_ipv4_addresses() {
            assert!(is_lan(ip));
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn forwarder_round_trips_and_closes_on_clear() {
        let port = free_port();
        // Upstream on 127.0.0.2 (bindable on Windows) so the forwarder can use 127.0.0.1 on the same port.
        let upstream = tokio::net::TcpListener::bind(("127.0.0.2", port)).await.unwrap();
        let fake = tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });

        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = logs.clone();
        let mut fwd = Forwarder::new(Arc::new(move |m: &str| sink.lock().unwrap().push(m.to_string())));
        fwd.reconcile(Ipv4Addr::new(127, 0, 0, 2), &[port], &[Ipv4Addr::LOCALHOST]).await;

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut echo = [0u8; 5];
        tokio::time::timeout(Duration::from_secs(10), client.read_exact(&mut echo)).await.expect("timed out").unwrap();
        assert_eq!(&echo, b"hello");
        fake.await.unwrap();

        fwd.reconcile(Ipv4Addr::new(127, 0, 0, 2), &[], &[Ipv4Addr::LOCALHOST]).await;
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err(), "port must be closed after an empty reconcile");
        assert!(logs.lock().unwrap().iter().any(|l| l.starts_with("forwarding 127.0.0.1")));
    }

    #[tokio::test]
    async fn clear_closes_established_connections() {
        let port = free_port();
        let upstream = tokio::net::TcpListener::bind(("127.0.0.2", port)).await.unwrap();
        // Echoes once, then holds the connection until the forwarder closes its side.
        let fake = tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest).await;
        });

        let mut fwd = Forwarder::new(Arc::new(|_: &str| {}));
        fwd.reconcile(Ipv4Addr::new(127, 0, 0, 2), &[port], &[Ipv4Addr::LOCALHOST]).await;
        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut echo = [0u8; 5];
        tokio::time::timeout(Duration::from_secs(10), client.read_exact(&mut echo)).await.expect("timed out").unwrap();

        fwd.clear().await;
        // The session must end: the client sees EOF or a reset, not an open socket.
        let mut byte = [0u8; 1];
        match tokio::time::timeout(Duration::from_secs(10), client.read(&mut byte)).await.expect("session stayed open") {
            Ok(0) | Err(_) => {}
            Ok(n) => panic!("unexpected {n} bytes after clear"),
        }
        // The upstream side must be closed as well.
        tokio::time::timeout(Duration::from_secs(10), fake).await.expect("upstream stayed open").unwrap();
    }

    #[tokio::test]
    async fn bind_failure_is_logged_once() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = held.local_addr().unwrap().port();
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = logs.clone();
        let mut fwd = Forwarder::new(Arc::new(move |m: &str| sink.lock().unwrap().push(m.to_string())));
        for _ in 0..3 {
            fwd.reconcile(Ipv4Addr::new(127, 0, 0, 2), &[port], &[Ipv4Addr::LOCALHOST]).await;
        }
        let failures = logs.lock().unwrap().iter().filter(|l| l.starts_with("cannot bind")).count();
        assert_eq!(failures, 1);
        fwd.clear().await;
    }
}
