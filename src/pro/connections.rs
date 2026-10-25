//! Network connections: TCP and UDP endpoints (IPv4 and IPv6) with the owning process,
//! from `GetExtendedTcpTable` / `GetExtendedUdpTable` on Windows, `/proc/net` plus the
//! `/proc/*/fd` socket links on Linux and `lsof -F` on macOS. TCP states always use the
//! MIB_TCP_STATE numbering. No name lookups are made (the app never touches the network);
//! remote addresses are classified locally instead.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub proto: Proto,
    pub local: IpAddr,
    pub local_port: u16,
    pub remote: Option<IpAddr>,
    pub remote_port: u16,
    /// TCP state number (MIB_TCP_STATE); 0 for UDP.
    pub state: u32,
    pub pid: u32,
}

impl Conn {
    pub fn state_name(&self) -> &'static str {
        if self.proto == Proto::Udp {
            return "Open";
        }
        state_name(self.state)
    }

    pub fn is_listen(&self) -> bool {
        self.proto == Proto::Tcp && self.state == 2
    }

    pub fn is_established(&self) -> bool {
        self.proto == Proto::Tcp && self.state == 5
    }

    pub fn local_text(&self) -> String {
        endpoint(&self.local, self.local_port)
    }

    pub fn remote_text(&self) -> String {
        match self.remote {
            Some(r) if !r.is_unspecified() => endpoint(&r, self.remote_port),
            _ => "\u{2014}".into(),
        }
    }

    pub fn scope(&self) -> &'static str {
        match self.remote {
            Some(r) if !r.is_unspecified() => classify(&r),
            _ => {
                if self.local.is_loopback() {
                    "Loopback"
                } else {
                    "Local"
                }
            }
        }
    }
}

pub fn state_name(s: u32) -> &'static str {
    match s {
        1 => "Closed",
        2 => "Listening",
        3 => "SYN sent",
        4 => "SYN received",
        5 => "Established",
        6 => "FIN wait 1",
        7 => "FIN wait 2",
        8 => "Close wait",
        9 => "Closing",
        10 => "Last ACK",
        11 => "Time wait",
        12 => "Delete TCB",
        _ => "Unknown",
    }
}

pub fn endpoint(ip: &IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(a) => format!("{a}:{port}"),
        IpAddr::V6(a) => format!("[{a}]:{port}"),
    }
}

/// Loopback / LAN / link-local / internet, decided locally.
pub fn classify(ip: &IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(a) => {
            if a.is_loopback() {
                "Loopback"
            } else if a.is_private() || (a.octets()[0] == 100 && (a.octets()[1] & 0xC0) == 64) {
                "LAN"
            } else if a.is_link_local() {
                "Link-local"
            } else if a.is_multicast() || a.is_broadcast() {
                "Multicast"
            } else {
                "Internet"
            }
        }
        IpAddr::V6(a) => {
            let s = a.segments();
            if a.is_loopback() {
                "Loopback"
            } else if (s[0] & 0xffc0) == 0xfe80 {
                "Link-local"
            } else if (s[0] & 0xfe00) == 0xfc00 {
                "LAN"
            } else if a.is_multicast() {
                "Multicast"
            } else if let Some(v4) = a.to_ipv4_mapped() {
                classify(&IpAddr::V4(v4))
            } else {
                "Internet"
            }
        }
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Ports are stored in network byte order in the low 16 bits.
fn port_at(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

fn v4_at(b: &[u8], off: usize) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(b[off], b[off + 1], b[off + 2], b[off + 3]))
}

fn v6_at(b: &[u8], off: usize) -> IpAddr {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[off..off + 16]);
    IpAddr::V6(Ipv6Addr::from(a))
}

/// Parses a MIB_TCPTABLE_OWNER_PID / MIB_TCP6TABLE_OWNER_PID / MIB_UDPTABLE_OWNER_PID /
/// MIB_UDP6TABLE_OWNER_PID buffer.
pub fn parse_table(buf: &[u8], proto: Proto, v6: bool) -> Vec<Conn> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let n = u32_at(buf, 0) as usize;
    let row = match (proto, v6) {
        (Proto::Tcp, false) => 24,
        (Proto::Tcp, true) => 56,
        (Proto::Udp, false) => 12,
        (Proto::Udp, true) => 28,
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let o = 4 + i * row;
        if o + row > buf.len() {
            break;
        }
        let c = match (proto, v6) {
            (Proto::Tcp, false) => Conn {
                proto,
                state: u32_at(buf, o),
                local: v4_at(buf, o + 4),
                local_port: port_at(buf, o + 8),
                remote: Some(v4_at(buf, o + 12)),
                remote_port: port_at(buf, o + 16),
                pid: u32_at(buf, o + 20),
            },
            (Proto::Tcp, true) => Conn {
                proto,
                local: v6_at(buf, o),
                local_port: port_at(buf, o + 20),
                remote: Some(v6_at(buf, o + 24)),
                remote_port: port_at(buf, o + 44),
                state: u32_at(buf, o + 48),
                pid: u32_at(buf, o + 52),
            },
            (Proto::Udp, false) => Conn { proto, local: v4_at(buf, o), local_port: port_at(buf, o + 4), remote: None, remote_port: 0, state: 0, pid: u32_at(buf, o + 8) },
            (Proto::Udp, true) => Conn { proto, local: v6_at(buf, o), local_port: port_at(buf, o + 20), remote: None, remote_port: 0, state: 0, pid: u32_at(buf, o + 24) },
        };
        out.push(c);
    }
    out
}

/// Linux `tcp_states.h` number (as in `/proc/net/tcp`) -> MIB_TCP_STATE; 0 when unknown.
pub fn linux_state(st: u32) -> u32 {
    match st {
        0x01 => 5, // ESTABLISHED
        0x02 => 3, // SYN_SENT
        0x03 | 0x0C => 4, // SYN_RECV, NEW_SYN_RECV
        0x04 => 6, // FIN_WAIT1
        0x05 => 7, // FIN_WAIT2
        0x06 => 11, // TIME_WAIT
        0x07 => 1, // CLOSE
        0x08 => 8, // CLOSE_WAIT
        0x09 => 10, // LAST_ACK
        0x0A => 2, // LISTEN
        0x0B => 9, // CLOSING
        _ => 0,
    }
}

/// BSD / `lsof` TCP state name -> MIB_TCP_STATE; 0 when unknown.
pub fn bsd_state(name: &str) -> u32 {
    match name {
        "CLOSED" => 1,
        "LISTEN" => 2,
        "SYN_SENT" => 3,
        "SYN_RCVD" | "SYN_RECEIVED" => 4,
        "ESTABLISHED" => 5,
        "FIN_WAIT_1" => 6,
        "FIN_WAIT_2" => 7,
        "CLOSE_WAIT" => 8,
        "CLOSING" => 9,
        "LAST_ACK" => 10,
        "TIME_WAIT" => 11,
        _ => 0,
    }
}

/// A `/proc/net` endpoint, `0100007F:0277` or 32 hex digits for IPv6, then the port. The address
/// is printed as 32-bit words in host byte order, so each word is decoded with native endianness.
pub fn parse_hex_endpoint(s: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |i: usize| addr.get(i * 8..i * 8 + 8).and_then(|w| u32::from_str_radix(w, 16).ok()).map(u32::to_ne_bytes);
    let ip = match addr.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(0)?)),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some((ip, port))
}

/// Rows of a `/proc/net/{tcp,tcp6,udp,udp6}` table with their socket inode (pid left 0).
pub fn parse_proc_net(text: &str, proto: Proto) -> Vec<(Conn, u64)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            // sl local rem st tx:rx tr:when retrnsmt uid timeout inode ...
            let f: Vec<&str> = line.split_whitespace().collect();
            let (local, local_port) = parse_hex_endpoint(f.get(1)?)?;
            let (rem, remote_port) = parse_hex_endpoint(f.get(2)?)?;
            let st = u32::from_str_radix(f.get(3)?, 16).ok()?;
            let inode = f.get(9)?.parse().ok()?;
            let (state, remote) = match proto {
                Proto::Tcp => (linux_state(st), Some(rem)),
                // Unconnected UDP sockets have no peer; connected ones keep it.
                Proto::Udp => (0, (remote_port != 0).then_some(rem)),
            };
            Some((Conn { proto, local, local_port, remote, remote_port, state, pid: 0 }, inode))
        })
        .collect()
}

/// The inode of a `/proc/<pid>/fd` link to a socket (`socket:[12345]`).
pub fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
}

/// One side of an `lsof` name: `127.0.0.1:631`, `[::1]:631`, `[fe80::1%lo0]:123`, `*:631`, `*:*`.
fn lsof_endpoint(s: &str, v6: bool) -> Option<(IpAddr, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port = if port == "*" { 0 } else { port.parse().ok()? };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.split('%').next().unwrap_or(host);
    let ip = match host {
        "*" if v6 => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        "*" => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        h => h.parse().ok()?,
    };
    Some((ip, port))
}

/// `lsof -F pfdtPnT` output -> connections. Sockets shared by several processes (inherited
/// across fork) are listed once, for the first pid, keyed by the socket's kernel address (`d`).
pub fn parse_lsof(text: &str) -> Vec<Conn> {
    #[derive(Default)]
    struct File<'a> {
        device: &'a str,
        v6: bool,
        proto: Option<Proto>,
        name: &'a str,
        state: &'a str,
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pid = 0u32;
    let mut file: Option<File> = None;
    let mut flush = |f: Option<File>, pid: u32, out: &mut Vec<Conn>| {
        let Some(f) = f else { return };
        let Some(proto) = f.proto else { return };
        if !f.device.is_empty() && !seen.insert(f.device.to_string()) {
            return;
        }
        let (l, r) = match f.name.split_once("->") {
            Some((l, r)) => (l, Some(r)),
            None => (f.name, None),
        };
        let Some((local, local_port)) = lsof_endpoint(l, f.v6) else { return };
        let remote = r.and_then(|r| lsof_endpoint(r, f.v6));
        let state = if proto == Proto::Tcp { bsd_state(f.state) } else { 0 };
        let remote = match (proto, remote) {
            // A listening or unconnected TCP socket: an unspecified peer, as the other backends.
            (Proto::Tcp, None) => Some((if local.is_ipv6() { IpAddr::V6(Ipv6Addr::UNSPECIFIED) } else { IpAddr::V4(Ipv4Addr::UNSPECIFIED) }, 0)),
            (_, r) => r,
        };
        out.push(Conn { proto, local, local_port, remote: remote.map(|r| r.0), remote_port: remote.map_or(0, |r| r.1), state, pid });
    };
    for line in text.lines() {
        let Some(tag) = line.chars().next() else { continue };
        let v = &line[tag.len_utf8()..];
        match tag {
            'p' => {
                flush(file.take(), pid, &mut out);
                pid = v.parse().unwrap_or(0);
            }
            'f' => {
                flush(file.take(), pid, &mut out);
                file = Some(File::default());
            }
            _ => {
                let Some(f) = file.as_mut() else { continue };
                match tag {
                    'd' => f.device = v,
                    't' => f.v6 = v == "IPv6",
                    'P' => {
                        f.proto = match v {
                            "TCP" => Some(Proto::Tcp),
                            "UDP" => Some(Proto::Udp),
                            _ => None,
                        }
                    }
                    'n' => f.name = v,
                    'T' => {
                        if let Some(s) = v.strip_prefix("ST=") {
                            f.state = s;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    flush(file.take(), pid, &mut out);
    out
}

#[cfg(windows)]
pub fn read() -> Result<Vec<Conn>, String> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, GetExtendedUdpTable, TCP_TABLE_CLASS, UDP_TABLE_CLASS};
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    const TCP_TABLE_OWNER_PID_ALL: TCP_TABLE_CLASS = TCP_TABLE_CLASS(5);
    const UDP_TABLE_OWNER_PID: UDP_TABLE_CLASS = UDP_TABLE_CLASS(1);
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    fn fetch(f: &dyn Fn(Option<*mut core::ffi::c_void>, &mut u32) -> u32) -> Result<Vec<u8>, String> {
        let mut size = 0u32;
        let _ = f(None, &mut size);
        for _ in 0..4 {
            size = size.max(64) + 4096;
            let mut buf = vec![0u8; size as usize];
            let st = f(Some(buf.as_mut_ptr() as *mut _), &mut size);
            if st == 0 {
                return Ok(buf);
            }
            if st != ERROR_INSUFFICIENT_BUFFER {
                return Err(format!("IP Helper error {st}"));
            }
        }
        Err("connection table kept growing".into())
    }

    let mut out = Vec::new();
    for (af, v6) in [(AF_INET, false), (AF_INET6, true)] {
        // SAFETY: buffers are sized by the API's own size query.
        let tcp = fetch(&|p, s| unsafe { GetExtendedTcpTable(p, s, BOOL(1), af, TCP_TABLE_OWNER_PID_ALL, 0) })?;
        out.extend(parse_table(&tcp, Proto::Tcp, v6));
        let udp = fetch(&|p, s| unsafe { GetExtendedUdpTable(p, s, BOOL(1), af, UDP_TABLE_OWNER_PID, 0) })?;
        out.extend(parse_table(&udp, Proto::Udp, v6));
    }
    Ok(out)
}

/// IPv4 address of the interface that holds the default route (local routing-table
/// lookup with `GetBestInterface`; nothing is sent).
#[cfg(windows)]
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestInterface, GetIpAddrTable};
    let mut index = 0u32;
    // SAFETY: plain output parameter; 8.8.8.8 only selects the default route.
    if unsafe { GetBestInterface(u32::from_ne_bytes([8, 8, 8, 8]), &mut index) } != 0 {
        return None;
    }
    let mut size = 0u32;
    // SAFETY: size query, then a read into a buffer of that size (plus slack).
    unsafe {
        let _ = GetIpAddrTable(None, &mut size, false);
        let mut buf = vec![0u32; (size as usize / 4) + 64];
        size = (buf.len() * 4) as u32;
        if GetIpAddrTable(Some(buf.as_mut_ptr() as *mut _), &mut size, false) != 0 {
            return None;
        }
        // MIB_IPADDRTABLE: count, then rows of {addr, index, mask, bcast, reasm, (u16, u16)}.
        let n = buf[0] as usize;
        for i in 0..n {
            let o = 1 + i * 6;
            if o + 6 > buf.len() {
                break;
            }
            if buf[o + 1] == index {
                let a = Ipv4Addr::from(buf[o].to_ne_bytes());
                if !a.is_loopback() && !a.is_unspecified() {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// IPv4 address the kernel picks for the default route: a UDP socket is connected to 8.8.8.8:53,
/// which only does a local route lookup (UDP `connect` sends nothing), and its local address read.
#[cfg(not(windows))]
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(a) if !a.is_loopback() && !a.is_unspecified() => Some(a),
        _ => None,
    }
}

/// `/proc/net/{tcp,tcp6,udp,udp6}`, owners from the `socket:[inode]` links in `/proc/*/fd`
/// (only processes we may inspect: our own unless root; the rest keep pid 0).
#[cfg(target_os = "linux")]
pub fn read() -> Result<Vec<Conn>, String> {
    let mut rows = Vec::new();
    let mut err = None;
    let mut any = false;
    for (file, proto) in [("tcp", Proto::Tcp), ("tcp6", Proto::Tcp), ("udp", Proto::Udp), ("udp6", Proto::Udp)] {
        // tcp6/udp6 are absent when IPv6 is disabled; only fail when nothing could be read.
        match std::fs::read_to_string(format!("/proc/net/{file}")) {
            Ok(text) => {
                any = true;
                rows.extend(parse_proc_net(&text, proto));
            }
            Err(e) => err = Some(format!("/proc/net/{file}: {e}")),
        }
    }
    if !any {
        return Err(err.unwrap_or_else(|| "no /proc/net tables".into()));
    }
    let owners = socket_owners(rows.iter().map(|(_, inode)| *inode).filter(|i| *i != 0).collect());
    Ok(rows
        .into_iter()
        .map(|(mut c, inode)| {
            c.pid = owners.get(&inode).copied().unwrap_or(0);
            c
        })
        .collect())
}

/// Socket inode -> lowest pid holding it, from the fd links of every process we may read.
#[cfg(target_os = "linux")]
fn socket_owners(mut wanted: std::collections::HashSet<u64>) -> std::collections::HashMap<u64, u32> {
    let mut owners = std::collections::HashMap::new();
    let mut pids: Vec<u32> = std::fs::read_dir("/proc").into_iter().flatten().flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    pids.sort_unstable();
    for pid in pids {
        if wanted.is_empty() {
            break;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(link) = std::fs::read_link(fd.path()) else { continue };
            if let Some(inode) = link.to_str().and_then(socket_inode) {
                if wanted.remove(&inode) {
                    owners.insert(inode, pid);
                }
            }
        }
    }
    owners
}

/// `lsof` over all Internet sockets (own processes only unless root). Slow; loader thread only.
#[cfg(target_os = "macos")]
pub fn read() -> Result<Vec<Conn>, String> {
    let (code, out, err) = crate::platform::run_capture_any("lsof", &["-nP", "-w", "-i", "-FpfdtPnT"], std::time::Duration::from_secs(15))?;
    // lsof exits 1 when it could not see some files (or found none); its output still counts.
    if code != Some(0) && out.trim().is_empty() {
        let why = err.lines().map(str::trim).find(|l| !l.is_empty());
        return match (code, why) {
            (Some(1), None) => Ok(Vec::new()),
            (_, Some(w)) => Err(format!("lsof: {w}")),
            (_, None) => Err(format!("lsof failed ({code:?})")),
        };
    }
    Ok(parse_lsof(&out))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn read() -> Result<Vec<Conn>, String> {
    Err(format!("connections are not implemented on {}", crate::platform::os_name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp4_row() {
        let mut b = vec![0u8; 4 + 24];
        b[0] = 1;
        b[4..8].copy_from_slice(&5u32.to_ne_bytes()); // established
        b[8..12].copy_from_slice(&[127, 0, 0, 1]);
        b[12..14].copy_from_slice(&443u16.to_be_bytes());
        b[16..20].copy_from_slice(&[93, 184, 216, 34]);
        b[20..22].copy_from_slice(&50000u16.to_be_bytes());
        b[24..28].copy_from_slice(&1234u32.to_ne_bytes());
        let c = &parse_table(&b, Proto::Tcp, false)[0];
        assert_eq!(c.local_text(), "127.0.0.1:443");
        assert_eq!(c.remote_text(), "93.184.216.34:50000");
        assert_eq!(c.pid, 1234);
        assert!(c.is_established());
        assert_eq!(c.scope(), "Internet");
    }

    #[test]
    fn classifies_addresses() {
        assert_eq!(classify(&"192.168.1.2".parse().unwrap()), "LAN");
        assert_eq!(classify(&"::1".parse().unwrap()), "Loopback");
        assert_eq!(classify(&"fe80::1".parse().unwrap()), "Link-local");
        assert_eq!(classify(&"1.1.1.1".parse().unwrap()), "Internet");
    }

    #[test]
    fn primary_address_is_not_loopback() {
        if let Some(a) = primary_ipv4() {
            assert!(!a.is_loopback() && !a.is_unspecified());
        }
    }

    #[test]
    fn maps_linux_and_bsd_states_to_mib() {
        let names: Vec<&str> = (1..=12).map(|s| state_name(linux_state(s))).collect();
        assert_eq!(names, ["Established", "SYN sent", "SYN received", "FIN wait 1", "FIN wait 2", "Time wait", "Closed", "Close wait", "Last ACK", "Listening", "Closing", "SYN received"]);
        assert_eq!(linux_state(0x42), 0);
        let bsd = ["CLOSED", "LISTEN", "SYN_SENT", "SYN_RCVD", "ESTABLISHED", "FIN_WAIT_1", "FIN_WAIT_2", "CLOSE_WAIT", "CLOSING", "LAST_ACK", "TIME_WAIT"];
        assert_eq!(bsd.map(bsd_state), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(bsd_state("UNKNOWN"), 0);
    }

    // /proc/net prints address words in host byte order; the fixtures are little-endian like
    // every target TMOP ships for.
    #[cfg(target_endian = "little")]
    #[test]
    fn parses_proc_net_hex_endpoints() {
        assert_eq!(parse_hex_endpoint("0100007F:0277"), Some(("127.0.0.1".parse().unwrap(), 631)));
        assert_eq!(parse_hex_endpoint("0201A8C0:C350"), Some(("192.168.1.2".parse().unwrap(), 50000)));
        assert_eq!(parse_hex_endpoint("00000000000000000000000001000000:0016"), Some(("::1".parse().unwrap(), 22)));
        assert_eq!(parse_hex_endpoint("000080FE00000000FF005452A1B2C3D4:01BB"), Some(("fe80::5254:ff:d4c3:b2a1".parse().unwrap(), 443)));
        assert_eq!(parse_hex_endpoint("0000000000000000FFFF00000100007F:0035"), Some(("::ffff:127.0.0.1".parse().unwrap(), 53)));
        assert_eq!(parse_hex_endpoint("0100007F"), None);
        assert_eq!(parse_hex_endpoint("01007F:0277"), None);
        assert_eq!(parse_hex_endpoint("0100007G:0277"), None);
    }

    #[cfg(target_endian = "little")]
    #[test]
    fn parses_proc_net_tables() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
            0: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 23456 1 0000000000000000 100 0 0 10 0\n   \
            1: 0201A8C0:C350 22D8B85D:01BB 01 00000000:00000000 02:000AE2C1 00000000  1000        0 98765 2 0000000000000000 20 4 30 10 -1\n   \
            2: 0201A8C0:C351 22D8B85D:01BB 06 00000000:00000000 03:00001234 00000000     0        0 0 3 0000000000000000\n";
        let rows = parse_proc_net(tcp, Proto::Tcp);
        assert_eq!(rows.len(), 3);
        let (listen, inode) = &rows[0];
        assert!(listen.is_listen());
        assert_eq!((listen.local_text(), listen.remote_text(), *inode, listen.scope()), ("127.0.0.1:631".into(), "\u{2014}".into(), 23456, "Loopback"));
        let (est, inode) = &rows[1];
        assert!(est.is_established());
        assert_eq!((est.local_text(), est.remote_text(), *inode, est.scope()), ("192.168.1.2:50000".into(), "93.184.216.34:443".into(), 98765, "Internet"));
        assert_eq!((rows[2].0.state_name(), rows[2].1), ("Time wait", 0));
        let udp6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n  \
            0: 00000000000000000000000000000000:14E9 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000   104        0 3456 2 0000000000000000 0\n  \
            1: 00000000000000000000000001000000:A1B2 00000000000000000000000001000000:0035 01 00000000:00000000 00:00000000 00000000  1000        0 3457 2 0000000000000000 0\n";
        let rows = parse_proc_net(udp6, Proto::Udp);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].0.local_text(), rows[0].0.remote, rows[0].0.state, rows[0].0.state_name()), ("[::]:5353".into(), None, 0, "Open"));
        assert_eq!(rows[1].0.remote_text(), "[::1]:53");
        assert!(parse_proc_net("header only\n", Proto::Tcp).is_empty());
        assert!(parse_proc_net("h\n  0: garbage\n", Proto::Tcp).is_empty());
    }

    #[test]
    fn reads_socket_links() {
        assert_eq!(socket_inode("socket:[12345]"), Some(12345));
        assert_eq!(socket_inode("pipe:[12345]"), None);
        assert_eq!(socket_inode("/dev/null"), None);
        assert_eq!(socket_inode("socket:[x]"), None);
    }

    #[test]
    fn parses_lsof_fields() {
        let out = "p321\nccupsd\nf5\ntIPv6\nd0xaa01\nPTCP\nn*:631\nTST=LISTEN\nTQR=0\nTQS=0\n\
            f6\ntIPv4\nd0xaa02\nPTCP\nn127.0.0.1:631\nTST=LISTEN\n\
            p400\nf20\ntIPv4\nd0xbb01\nPTCP\nn192.168.1.5:50123->17.57.144.20:5223\nTST=ESTABLISHED\n\
            f21\ntIPv6\nd0xbb02\nPUDP\nn[fe80::1%lo0]:123\n\
            f22\ntIPv4\nd0xbb03\nPUDP\nn*:*\n\
            f23\ntIPv4\nd0xbb04\nPICMP\nn*:*\n\
            p401\nf3\ntIPv6\nd0xaa01\nPTCP\nn*:631\nTST=LISTEN\n";
        let c = parse_lsof(out);
        assert_eq!(c.len(), 5, "{c:?}");
        assert_eq!((c[0].pid, c[0].local_text(), c[0].is_listen(), c[0].remote_text()), (321, "[::]:631".into(), true, "\u{2014}".into()));
        assert_eq!((c[1].pid, c[1].local_text(), c[1].scope()), (321, "127.0.0.1:631".into(), "Loopback"));
        assert_eq!((c[2].pid, c[2].remote_text(), c[2].is_established(), c[2].scope()), (400, "17.57.144.20:5223".into(), true, "Internet"));
        assert_eq!((c[3].proto, c[3].local_text(), c[3].remote, c[3].state_name()), (Proto::Udp, "[fe80::1]:123".into(), None, "Open"));
        assert_eq!(c[4].local_text(), "0.0.0.0:0");
        assert!(parse_lsof("").is_empty());
    }

    /// A listener this test opens shows up as listening and owned by this process.
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    #[test]
    fn finds_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = read().unwrap();
        let c = conns.iter().find(|c| c.proto == Proto::Tcp && c.local_port == port && c.is_listen()).unwrap_or_else(|| panic!("no listener on {port} in {} rows", conns.len()));
        assert_eq!((c.local, c.pid), (IpAddr::V4(Ipv4Addr::LOCALHOST), std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn reads_live_tables() {
        let conns = read().unwrap();
        // Every Windows machine has some listening endpoint (e.g. RPC on 135).
        assert!(conns.iter().any(|c| c.is_listen() || c.proto == Proto::Udp));
    }
}
