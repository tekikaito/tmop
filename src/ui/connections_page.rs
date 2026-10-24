//! Connections page: TCP and UDP endpoints.

use super::kit::{self, empty_state, stat_tile};
use super::pro_pages::{page_layout, tile_rects, toolbar};
use super::table::{self, cmp_str, Cell, Col};
use super::theme::{self, lighten};
use super::widgets::{self, draw_icon, tinted_card, Icon};
use super::{App, FrameData, Page};
use crate::metrics::Series;
use crate::pro::connections::{Conn, Proto};
use crate::process::format_rate;
use eframe::egui::{self, pos2, vec2, Align2, Rect, Sense};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

/// Width the toolbar needs: filter, the five-way view switch and Refresh.
const TOOLBAR_W: f32 = 780.0;
/// A process with at least this many established connections to at least this many different
/// external endpoints is called out.
const UNUSUAL_CONNS: usize = 12;
const UNUSUAL_ENDPOINTS: usize = 8;

impl App {
    pub fn connections_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let st = theme::status(&pal);
        self.pro.conns.poll();
        let subtitle = match (self.pro.conns.ok(), self.pro.conns.age()) {
            (Some(c), Some(age)) => format!("TCP + UDP tables \u{b7} {} endpoints \u{b7} refreshed {} s ago", c.len(), age.as_secs()),
            _ => "TCP + UDP tables".to_string(),
        };
        let body = self.page_title(ui, &pal, "Connections", &subtitle);
        let (tools, area) = page_layout(body, TOOLBAR_W);
        let mut refresh = false;
        toolbar(ui, tools, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            if let Some(i) = widgets::segmented(ui, &VIEWS, self.pro.conns_view, &pal, false) {
                self.pro.conns_view = i;
            }
            kit::filter_box(ui, &mut self.pro.conns_table.filter, "Filter by process, address, port or state", 280.0, &pal);
        });
        if refresh || self.pro.conns.stale(Duration::from_secs(3)) {
            self.pro.conns.start(crate::pro::connections::read);
        }
        let p = ui.painter().clone();
        let names: HashMap<u32, &str> = fd.rows.iter().map(|r| (r.pid, r.name.as_str())).collect();
        let paths: HashMap<u32, &str> = fd.rows.iter().filter_map(|r| r.path.as_deref().map(|p| (r.pid, p))).collect();
        let name_of = |pid: u32| -> String {
            match pid {
                0 => "System Idle".into(),
                4 => "System".into(),
                _ => names.get(&pid).map(|s| s.to_string()).unwrap_or_else(|| format!("PID {pid}")),
            }
        };
        let conns: Vec<Conn> = self.pro.conns.ok().cloned().unwrap_or_default();
        let est = conns.iter().filter(|c| c.is_established()).count();
        let listen = conns.iter().filter(|c| c.is_listen()).count();
        let udp = conns.iter().filter(|c| c.proto == Proto::Udp).count();
        let internet = conns.iter().filter(|c| c.is_established() && c.scope() == "Internet").count();
        let procs: HashSet<u32> = conns.iter().filter(|c| c.is_established()).map(|c| c.pid).collect();
        let total = conns.len().max(1) as f32;
        let net = &fd.snap.network;
        let tiles = [
            ("Established", est.to_string(), format!("{internet} to the internet"), Some((est as f32 / total, st.ok))),
            ("Listening", listen.to_string(), "TCP ports accepting connections".to_string(), Some((listen as f32 / total, theme::darken_for(&pal, theme::INDIGO)))),
            ("UDP", udp.to_string(), "Open UDP endpoints".to_string(), Some((udp as f32 / total, theme::darken_for(&pal, theme::PURPLE)))),
            ("Processes", procs.len().to_string(), "With established connections".to_string(), None),
            ("Throughput", String::new(), String::new(), None),
        ];
        let (rects, tiles_h) = tile_rects(area, tiles.len(), 170.0);
        for (r, (label, value, sub, bar)) in rects.iter().zip(&tiles) {
            stat_tile(&p, *r, &pal, label, value, sub, *bar, None);
        }
        // Throughput draws its own value, mono sub line and a trend of download plus upload.
        let r = rects[4];
        let spark_w = (r.width() * 0.38).clamp(50.0, 110.0);
        widgets::text_fit(&p, pos2(r.left() + 16.0, r.top() + 31.0), Align2::LEFT_TOP, &format_rate(net.rx_bps + net.tx_bps), theme::light(24.0), pal.text, r.width() - 40.0 - spark_w);
        widgets::text_fit(&p, pos2(r.left() + 16.0, r.top() + 62.0), Align2::LEFT_TOP, &format!("\u{2193} {} \u{b7} \u{2191} {}", format_rate(net.rx_bps), format_rate(net.tx_bps)), theme::mono(11.0), pal.text_faint, r.width() - 32.0);
        let mut trend = Series::new(fd.hist.net_rx.values.len().max(2));
        for (rx, tx) in fd.hist.net_rx.values.iter().zip(&fd.hist.net_tx.values) {
            trend.push(rx + tx);
        }
        let hi = trend.max().max(1.0);
        widgets::sparkline(&p.with_clip_rect(r), Rect::from_min_size(pos2(r.right() - 16.0 - spark_w, r.top() + 34.0), vec2(spark_w, 20.0)), &[(&trend, pal.text, (0.0, hi))], 0.0, false);

        let mut y = area.top() + tiles_h + 14.0;
        let flagged = unusual(&conns);
        if let Some((pid, n, m)) = flagged {
            let banner = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), 44.0));
            y = banner.bottom() + 14.0;
            tinted_card(&p, banner, &pal, st.warn);
            draw_icon(&p, Rect::from_center_size(pos2(banner.left() + 26.0, banner.center().y), vec2(18.0, 18.0)), Icon::Alert, st.warn);
            let link = "Inspect process \u{2192}";
            let link_w = widgets::text_width(&p, link, theme::ui(13.0));
            let head = p.text(pos2(banner.left() + 46.0, banner.center().y), Align2::LEFT_CENTER, "Unusual:", theme::semibold(13.0), st.yellow);
            let text = format!(" {} holds {n} connections to {m} external endpoints \u{2014} most apps use 1\u{2013}3.", name_of(pid));
            widgets::text_fit(&p, pos2(head.right(), banner.center().y), Align2::LEFT_CENTER, &text, theme::ui(13.0), st.yellow, (banner.right() - head.right() - link_w - 48.0).max(60.0));
            let hit = Rect::from_center_size(pos2(banner.right() - 20.0 - link_w / 2.0, banner.center().y), vec2(link_w + 8.0, 28.0));
            let resp = ui.interact(hit, ui.id().with("inspect-unusual"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            p.text(pos2(banner.right() - 20.0, banner.center().y), Align2::RIGHT_CENTER, link, theme::ui(13.0), if resp.hovered() { lighten(st.info, 0.25) } else { st.info });
            if resp.clicked() {
                if let Some(row) = fd.rows.iter().find(|r| r.pid == pid) {
                    self.selected = Some((row.pid, row.create_time));
                    self.page = Page::Processes;
                }
            }
        }
        let table_rect = Rect::from_min_max(pos2(area.left(), y), area.max);

        let view = self.pro.conns_view;
        let st_table = &self.pro.conns_table;
        let mut order: Vec<usize> = (0..conns.len())
            .filter(|i| {
                let c = &conns[*i];
                let keep = match view {
                    1 => c.is_established(),
                    2 => c.is_listen(),
                    3 => c.proto == Proto::Udp,
                    4 => c.is_established() && c.scope() == "Internet",
                    _ => true,
                };
                keep && table::matches(&st_table.filter, &[&name_of(c.pid), &c.local_text(), &c.remote_text(), c.state_name(), &c.pid.to_string()])
            })
            .collect();
        st_table.sort(&conns, &mut order, |a, b, col| match col {
            0 => cmp_str(&name_of(a.pid), &name_of(b.pid)),
            1 => a.pid.cmp(&b.pid),
            2 => a.proto.cmp(&b.proto),
            3 => a.local.cmp(&b.local).then(a.local_port.cmp(&b.local_port)),
            4 => a.remote.cmp(&b.remote).then(a.remote_port.cmp(&b.remote_port)),
            5 => a.state_name().cmp(b.state_name()),
            _ => a.scope().cmp(b.scope()),
        });
        if conns.is_empty() {
            empty_state(&p, table_rect, &pal, self.pro.conns.error().unwrap_or("Reading connection tables\u{2026}"));
            return;
        }
        let cols = [
            Col::text("Process", 220.0),
            Col::num("PID", 70.0),
            Col::text("Protocol", 80.0),
            Col::text("Local address", 230.0),
            Col::text("Remote address", 250.0),
            Col::text("State", 120.0),
            Col::text("Scope", 120.0),
        ];
        let key = |i: usize| {
            let c = &conns[i];
            format!("{:?}{}{}{}", c.proto, c.local_text(), c.remote_text(), c.pid)
        };
        let (indigo, info) = (theme::darken_for(&pal, theme::INDIGO), st.info);
        let cell = |i: usize, col: usize| -> Cell {
            let c = &conns[i];
            match col {
                0 => {
                    let cell = Cell::new(name_of(c.pid)).icon(paths.get(&c.pid).copied());
                    match flagged {
                        Some((pid, n, _)) if pid == c.pid => cell.badge(conn_badge(n)),
                        _ => cell,
                    }
                }
                1 => Cell::new(c.pid.to_string()).mono().color(pal.text_faint),
                2 => Cell::new(if c.proto == Proto::Tcp { if c.local.is_ipv6() { "TCPv6" } else { "TCP" } } else if c.local.is_ipv6() { "UDPv6" } else { "UDP" }).mono(),
                3 => Cell::new(c.local_text()).mono(),
                4 => match c.remote {
                    Some(r) if !r.is_unspecified() => Cell::new(c.remote_text()).mono(),
                    _ => Cell::new("*:*").mono().color(pal.text_faint),
                },
                5 => {
                    if c.proto == Proto::Udp {
                        Cell::new("\u{2014}").color(pal.text_faint)
                    } else {
                        let color = if c.is_established() {
                            st.ok
                        } else if c.is_listen() {
                            indigo
                        } else if c.state == 11 {
                            st.yellow
                        } else {
                            pal.text_dim
                        };
                        Cell::new(c.state_name()).color(color)
                    }
                }
                _ => {
                    let s = c.scope();
                    Cell::new(s).color(if s == "Internet" { info } else { pal.text_dim })
                }
            }
        };
        let icons = &mut self.icons;
        table::show(ui, table_rect, "conn-table", &pal, &cols, &order, &mut self.pro.conns_table, &key, &cell, icons);
    }
}

/// The view switch above the table.
const VIEWS: [&str; 5] = ["All", "Established", "Listening", "UDP", "Internet"];

/// The process holding the most established connections when it holds at least [`UNUSUAL_CONNS`] of
/// them to at least [`UNUSUAL_ENDPOINTS`] different external endpoints: `(pid, connections,
/// endpoints)`.
fn unusual(conns: &[Conn]) -> Option<(u32, usize, usize)> {
    let mut by_pid: HashMap<u32, (usize, HashSet<(IpAddr, u16)>)> = HashMap::new();
    for c in conns.iter().filter(|c| c.is_established()) {
        let e = by_pid.entry(c.pid).or_default();
        e.0 += 1;
        if let (Some(remote), "Internet") = (c.remote, c.scope()) {
            e.1.insert((remote, c.remote_port));
        }
    }
    by_pid.into_iter().filter(|(_, (n, ext))| *n >= UNUSUAL_CONNS && ext.len() >= UNUSUAL_ENDPOINTS).map(|(pid, (n, ext))| (pid, n, ext.len())).max_by_key(|&(pid, n, m)| (n, m, std::cmp::Reverse(pid)))
}

/// "18 CONN": the table wants a `'static` badge text, so each distinct text is allocated once.
fn conn_badge(n: usize) -> &'static str {
    static CACHE: Mutex<Vec<(usize, &'static str)>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = cache.iter().find(|(k, _)| *k == n) {
        return s;
    }
    let s: &'static str = Box::leak(format!("{n} CONN").into_boxed_str());
    cache.push((n, s));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn conn(pid: u32, remote: [u8; 4], port: u16, state: u32) -> Conn {
        Conn { proto: Proto::Tcp, local: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), local_port: 50_000 + port, remote: Some(IpAddr::V4(Ipv4Addr::from(remote))), remote_port: port, state, pid }
    }

    #[test]
    fn many_connections_to_many_endpoints_are_unusual() {
        let busy: Vec<Conn> = (0..12).map(|i| conn(7, [47, 254, 172, 10 + i], 8443, 5)).collect();
        assert_eq!(unusual(&busy), Some((7, 12, 12)));
        // Endpoints are address and port; twelve connections to one of them are normal.
        let ports: Vec<Conn> = (0..12).map(|i| conn(7, [47, 254, 172, 10], 8000 + i, 5)).collect();
        assert_eq!(unusual(&ports).map(|u| u.2), Some(12), "different ports are different endpoints");
        let same: Vec<Conn> = (0..12).map(|_| conn(7, [47, 254, 172, 10], 443, 5)).collect();
        assert_eq!(unusual(&same), None);
        assert_eq!(unusual(&busy[..11]), None);
        // Listening sockets and LAN peers do not count.
        let lan: Vec<Conn> = (0..12).map(|i| conn(7, [192, 168, 1, 30 + i], 445, 5)).collect();
        assert_eq!(unusual(&lan), None);
        let listening: Vec<Conn> = (0..12).map(|i| conn(7, [47, 254, 172, 10 + i], 443, 2)).collect();
        assert_eq!(unusual(&listening), None);
    }

    #[test]
    fn badge_texts_are_shared() {
        assert_eq!(conn_badge(18), "18 CONN");
        assert!(std::ptr::eq(conn_badge(18), conn_badge(18)));
    }
}
