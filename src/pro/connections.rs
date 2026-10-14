//! Network connections: TCP and UDP endpoints (IPv4 and IPv6) with the owning process,
//! from `GetExtendedTcpTable` / `GetExtendedUdpTable`. No name lookups are made (the
//! app never touches the network); remote addresses are classified locally instead.

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

#[cfg(not(windows))]
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    None
}

#[cfg(not(windows))]
pub fn read() -> Result<Vec<Conn>, String> {
    Err("connections are only implemented on Windows".into())
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

    #[cfg(windows)]
    #[test]
    fn primary_address_is_not_loopback() {
        if let Some(a) = primary_ipv4() {
            assert!(!a.is_loopback());
        }
    }

    #[cfg(windows)]
    #[test]
    fn reads_live_tables() {
        let conns = read().unwrap();
        // Every Windows machine has some listening endpoint (e.g. RPC on 135).
        assert!(conns.iter().any(|c| c.is_listen() || c.proto == Proto::Udp));
    }
}
