//! Connections page: TCP and UDP endpoints.

use super::kit::{empty_state, header_toolbar, stat_row, vsplit, GAP, MARGIN};
use super::table::{self, cmp_str, Cell, Col};
use super::theme::{self};
use super::widgets::{self, Icon};
use super::{App, FrameData};
use crate::pro::connections::{Conn, Proto};
use crate::process::format_rate;
use eframe::egui::{self};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

impl App {
    pub fn connections_page(&mut self, ui: &mut egui::Ui, fd: &FrameData<'_>) {
        let pal = fd.pal;
        let body = self.page_header(ui, &pal, "Connections", Icon::Globe);
        let mut refresh = false;
        header_toolbar(ui, body, 640.0, |ui| {
            refresh = widgets::button(ui, "Refresh", &pal, Some(Icon::Refresh)).clicked();
            widgets::search_box(ui, &mut self.pro.conns_table.filter, "Filter by process, address, port or state", 300.0, &pal);
            egui::ComboBox::from_id_salt("conn-view").width(140.0).selected_text(["All", "Established", "Listening", "UDP", "Internet"][self.pro.conns_view]).show_ui(ui, |ui| {
                for (i, l) in ["All", "Established", "Listening", "UDP", "Internet"].iter().enumerate() {
                    ui.selectable_value(&mut self.pro.conns_view, i, *l);
                }
            });
        });
        self.pro.conns.poll();
        if refresh || self.pro.conns.stale(Duration::from_secs(3)) {
            self.pro.conns.start(crate::pro::connections::read);
        }
        let area = body.shrink(MARGIN);
        let rows = vsplit(area, &[118.0, -1.0], GAP);
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
        stat_row(
            &p,
            rows[0],
            &pal,
            fd.bloom,
            &[
                ("Established".into(), est.to_string(), format!("{} to the internet", internet), theme::GREEN, Some(est as f32 / total)),
                ("Listening".into(), listen.to_string(), "TCP ports accepting connections".into(), theme::BLUE, Some(listen as f32 / total)),
                ("UDP".into(), udp.to_string(), "Open UDP endpoints".into(), theme::PURPLE, Some(udp as f32 / total)),
                ("Processes".into(), procs.len().to_string(), "With established connections".into(), theme::ORANGE, None),
                ("Throughput".into(), format_rate(fd.snap.network.rx_bps + fd.snap.network.tx_bps), format!("\u{2193} {}  \u{2191} {}", format_rate(fd.snap.network.rx_bps), format_rate(fd.snap.network.tx_bps)), fd.metric("network"), Some(fd.smooth.net / 100.0)),
            ],
        );
        let view = self.pro.conns_view;
        let st = &self.pro.conns_table;
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
                keep && table::matches(&st.filter, &[&name_of(c.pid), &c.local_text(), &c.remote_text(), c.state_name(), &c.pid.to_string()])
            })
            .collect();
        st.sort(&conns, &mut order, |a, b, col| match col {
            0 => cmp_str(&name_of(a.pid), &name_of(b.pid)),
            1 => a.pid.cmp(&b.pid),
            2 => a.proto.cmp(&b.proto),
            3 => a.local.cmp(&b.local).then(a.local_port.cmp(&b.local_port)),
            4 => a.remote.cmp(&b.remote).then(a.remote_port.cmp(&b.remote_port)),
            5 => a.state_name().cmp(b.state_name()),
            _ => a.scope().cmp(b.scope()),
        });
        if conns.is_empty() {
            empty_state(&p, rows[1], &pal, self.pro.conns.error().unwrap_or("Reading connection tables\u{2026}"));
            return;
        }
        let cols = [
            Col::text("Process", 220.0),
            Col::num("PID", 70.0),
            Col::text("Protocol", 80.0),
            Col::text("Local address", 230.0),
            Col::text("Remote address", 260.0),
            Col::text("State", 120.0),
            Col::text("Scope", 120.0),
        ];
        let key = |i: usize| {
            let c = &conns[i];
            format!("{:?}{}{}{}", c.proto, c.local_text(), c.remote_text(), c.pid)
        };
        let cell = |i: usize, col: usize| -> Cell {
            let c = &conns[i];
            match col {
                0 => Cell::new(name_of(c.pid)).icon(paths.get(&c.pid).copied()),
                1 => Cell::new(c.pid.to_string()).dim(&pal),
                2 => Cell::new(if c.proto == Proto::Tcp { if c.local.is_ipv6() { "TCPv6" } else { "TCP" } } else if c.local.is_ipv6() { "UDPv6" } else { "UDP" }).dim(&pal),
                3 => Cell::new(c.local_text()),
                4 => Cell::new(c.remote_text()),
                5 => {
                    let color = if c.is_established() {
                        theme::GREEN
                    } else if c.is_listen() {
                        theme::BLUE
                    } else if c.proto == Proto::Udp {
                        theme::PURPLE
                    } else {
                        theme::ORANGE
                    };
                    Cell::new(c.state_name()).dot(color)
                }
                _ => {
                    let s = c.scope();
                    Cell::new(s).color(if s == "Internet" { theme::BLUE } else { pal.text_dim })
                }
            }
        };
        let icons = &mut self.icons;
        table::show(ui, rows[1], "conn-table", &pal, &cols, &order, &mut self.pro.conns_table, &key, &cell, icons);
    }
}
