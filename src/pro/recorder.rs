//! Flight recorder and CSV recording.
//!
//! The flight recorder always keeps the most recent samples (and process start/exit
//! events) in memory, so the moments before a problem can be saved after the fact.
//! Recording writes every sample to a CSV file in `%LOCALAPPDATA%\TMOP\recordings`
//! (or a folder the user picks) until stopped or until an optional time limit.

use crate::docker::model::Totals;
use crate::metrics::Snapshot;
use crate::process::ProcessRow;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CSV_HEADER: &str = "time,cpu_pct,kernel_pct,clock_mhz,memory_pct,memory_used_bytes,disk_active_pct,disk_read_bps,disk_write_bps,net_rx_bps,net_tx_bps,gpu_pct,temperature_c,processes,top_processes,docker_running,docker_cpu_pct,docker_memory_bytes";

/// One recorded sample.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// Local time `YYYY-MM-DD HH:MM:SS`.
    pub time: String,
    pub cpu: f32,
    pub kernel: f32,
    pub clock_mhz: u32,
    pub mem_pct: f32,
    pub mem_used: u64,
    pub disk_pct: f32,
    pub disk_read: f64,
    pub disk_write: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub gpu: f32,
    pub temp_c: Option<f32>,
    pub processes: usize,
    /// Top three processes by CPU, `name 12.3%|name 4.0%|...`.
    pub top: String,
    /// Running Docker containers; 0 when the engine is not connected.
    pub docker_running: u32,
    /// Summed CPU of the running containers, in percent (can exceed 100).
    pub docker_cpu: f32,
    /// Summed memory of the running containers, in bytes.
    pub docker_mem: u64,
}

impl Record {
    pub fn from_snapshot(s: &Snapshot, rows: &[ProcessRow]) -> Self {
        let mut top: Vec<&ProcessRow> = rows.iter().filter(|r| r.pid != 0).collect();
        top.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct));
        let top = top.iter().take(3).map(|r| format!("{} {:.1}%", r.name.replace([',', '|', '\n'], " "), r.cpu_pct)).collect::<Vec<_>>().join("|");
        Record {
            time: super::display_stamp(),
            cpu: s.cpu.total_pct,
            kernel: s.cpu.kernel_pct,
            clock_mhz: s.cpu.current_mhz.unwrap_or(0),
            mem_pct: s.memory.used_pct,
            mem_used: s.memory.total.saturating_sub(s.memory.available),
            disk_pct: s.disk.active_pct,
            disk_read: s.disk.read_bps,
            disk_write: s.disk.write_bps,
            net_rx: s.network.rx_bps,
            net_tx: s.network.tx_bps,
            gpu: s.gpu.utilization_pct,
            temp_c: s.thermal.max_c(),
            processes: s.process_count,
            top,
            ..Default::default()
        }
    }

    pub fn to_csv(&self) -> String {
        format!(
            "{},{:.2},{:.2},{},{:.2},{},{:.2},{:.0},{:.0},{:.0},{:.0},{:.2},{},{},{},{},{:.2},{}",
            self.time,
            self.cpu,
            self.kernel,
            self.clock_mhz,
            self.mem_pct,
            self.mem_used,
            self.disk_pct,
            self.disk_read,
            self.disk_write,
            self.net_rx,
            self.net_tx,
            self.gpu,
            self.temp_c.map(|t| format!("{t:.1}")).unwrap_or_default(),
            self.processes,
            self.top,
            self.docker_running,
            self.docker_cpu,
            self.docker_mem
        )
    }

    /// Parses a CSV row. Rows written before the Docker columns were added (15 fields) parse
    /// with zero Docker totals.
    pub fn from_csv(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < 14 {
            return None;
        }
        Some(Record {
            time: f[0].to_string(),
            cpu: f[1].parse().ok()?,
            kernel: f[2].parse().ok()?,
            clock_mhz: f[3].parse().unwrap_or(0),
            mem_pct: f[4].parse().ok()?,
            mem_used: f[5].parse().unwrap_or(0),
            disk_pct: f[6].parse().unwrap_or(0.0),
            disk_read: f[7].parse().unwrap_or(0.0),
            disk_write: f[8].parse().unwrap_or(0.0),
            net_rx: f[9].parse().unwrap_or(0.0),
            net_tx: f[10].parse().unwrap_or(0.0),
            gpu: f[11].parse().unwrap_or(0.0),
            temp_c: f[12].parse().ok(),
            processes: f[13].parse().unwrap_or(0),
            top: f.get(14).map(|s| s.to_string()).unwrap_or_default(),
            docker_running: f.get(15).and_then(|s| s.parse().ok()).unwrap_or(0),
            docker_cpu: f.get(16).and_then(|s| s.parse().ok()).unwrap_or(0.0),
            docker_mem: f.get(17).and_then(|s| s.parse().ok()).unwrap_or(0),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    Started,
    Exited,
    Note,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub time: String,
    pub kind: EventKind,
    pub text: String,
}

struct RecFile {
    path: PathBuf,
    out: std::io::BufWriter<std::fs::File>,
    events: Option<std::io::BufWriter<std::fs::File>>,
    started: Instant,
    rows: u64,
    limit: Option<Duration>,
}

pub struct Recorder {
    /// Flight recorder ring of recent samples.
    pub ring: VecDeque<Record>,
    pub ring_capacity: usize,
    pub events: VecDeque<Event>,
    known: HashMap<u32, (i64, String)>,
    file: Option<RecFile>,
    /// Folder new recordings go to.
    pub dir: PathBuf,
    pub last_error: Option<String>,
    pub last_saved: Option<PathBuf>,
    /// Docker totals for the next sample; `None` when the engine is not connected.
    pub docker: Option<Totals>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            ring: VecDeque::new(),
            ring_capacity: 3600,
            events: VecDeque::new(),
            known: HashMap::new(),
            file: None,
            dir: default_dir(),
            last_error: None,
            last_saved: None,
            docker: None,
        }
    }
}

/// `%LOCALAPPDATA%\TMOP\recordings`
pub fn default_dir() -> PathBuf {
    super::data_dir().join("recordings")
}

impl Recorder {
    /// Feeds one sample (called once per sampler generation).
    pub fn on_sample(&mut self, snap: &Snapshot, rows: &[ProcessRow]) {
        if snap.seq == 0 {
            return;
        }
        let mut rec = Record::from_snapshot(snap, rows);
        if let Some(t) = self.docker {
            rec.docker_running = t.running as u32;
            rec.docker_cpu = t.cpu_pct;
            rec.docker_mem = t.mem_used;
        }
        self.track_processes(&rec.time, rows);
        if let Some(f) = &mut self.file {
            let res = writeln!(f.out, "{}", rec.to_csv()).and_then(|_| {
                f.rows += 1;
                if f.rows % 10 == 0 {
                    f.out.flush()
                } else {
                    Ok(())
                }
            });
            if let Err(e) = res {
                self.last_error = Some(format!("Recording stopped: {e}"));
                self.stop();
            }
        }
        if self.file.as_ref().and_then(|f| f.limit.map(|l| f.started.elapsed() >= l)).unwrap_or(false) {
            self.stop();
        }
        self.ring.push_back(rec);
        while self.ring.len() > self.ring_capacity {
            self.ring.pop_front();
        }
    }

    fn track_processes(&mut self, time: &str, rows: &[ProcessRow]) {
        let first = self.known.is_empty();
        let mut now: HashMap<u32, (i64, String)> = HashMap::with_capacity(rows.len());
        for r in rows.iter().filter(|r| r.pid != 0) {
            now.insert(r.pid, (r.create_time, r.name.clone()));
        }
        if !first {
            let mut new_events = Vec::new();
            for (pid, (ct, name)) in &now {
                if self.known.get(pid).map(|k| k.0 != *ct).unwrap_or(true) {
                    new_events.push(Event { time: time.to_string(), kind: EventKind::Started, text: format!("{name} ({pid})") });
                }
            }
            for (pid, (ct, name)) in &self.known {
                if now.get(pid).map(|n| n.0 != *ct).unwrap_or(true) {
                    new_events.push(Event { time: time.to_string(), kind: EventKind::Exited, text: format!("{name} ({pid})") });
                }
            }
            new_events.sort_by(|a, b| a.text.cmp(&b.text));
            for e in new_events {
                self.push_event(e);
            }
        }
        self.known = now;
    }

    pub fn push_event(&mut self, e: Event) {
        if let Some(f) = self.file.as_mut().and_then(|f| f.events.as_mut()) {
            let tag = match e.kind {
                EventKind::Started => "started",
                EventKind::Exited => "exited",
                EventKind::Note => "note",
            };
            let _ = writeln!(f, "{},{tag},{}", e.time, e.text.replace(',', " "));
        }
        self.events.push_back(e);
        while self.events.len() > 1000 {
            self.events.pop_front();
        }
    }

    pub fn is_recording(&self) -> bool {
        self.file.is_some()
    }

    /// Time since the recording started.
    pub fn elapsed(&self) -> Option<Duration> {
        self.file.as_ref().map(|f| f.started.elapsed())
    }

    pub fn recording_path(&self) -> Option<&Path> {
        self.file.as_ref().map(|f| f.path.as_path())
    }

    pub fn recorded_rows(&self) -> u64 {
        self.file.as_ref().map(|f| f.rows).unwrap_or(0)
    }

    pub fn limit(&self) -> Option<Duration> {
        self.file.as_ref().and_then(|f| f.limit)
    }

    /// Starts writing samples to a new CSV in `self.dir`.
    pub fn start(&mut self, limit: Option<Duration>) -> Result<PathBuf, String> {
        if self.file.is_some() {
            return Err("already recording".into());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let stem = format!("tmop-{}", super::file_stamp());
        let path = self.dir.join(format!("{stem}.csv"));
        let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut out = std::io::BufWriter::new(file);
        writeln!(out, "{CSV_HEADER}").map_err(|e| e.to_string())?;
        let events = std::fs::File::create(self.dir.join(format!("{stem}-events.csv"))).ok().map(|f| {
            let mut w = std::io::BufWriter::new(f);
            let _ = writeln!(w, "time,event,process");
            w
        });
        self.file = Some(RecFile { path: path.clone(), out, events, started: Instant::now(), rows: 0, limit });
        self.last_error = None;
        self.push_event(Event { time: super::display_stamp(), kind: EventKind::Note, text: format!("Recording started: {}", path.display()) });
        Ok(path)
    }

    /// Stops recording; returns the finished file.
    pub fn stop(&mut self) -> Option<PathBuf> {
        let mut f = self.file.take()?;
        let _ = f.out.flush();
        if let Some(e) = f.events.as_mut() {
            let _ = e.flush();
        }
        self.last_saved = Some(f.path.clone());
        self.push_event(Event { time: super::display_stamp(), kind: EventKind::Note, text: format!("Recording saved: {} ({} samples)", f.path.display(), f.rows) });
        Some(f.path)
    }

    /// Writes the flight recorder ring to a new CSV.
    pub fn save_flight(&mut self) -> Result<PathBuf, String> {
        if self.ring.is_empty() {
            return Err("the flight recorder is empty".into());
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let path = self.dir.join(format!("tmop-flight-{}.csv", super::file_stamp()));
        let mut text = String::with_capacity(self.ring.len() * 120);
        text.push_str(CSV_HEADER);
        text.push('\n');
        for r in &self.ring {
            text.push_str(&r.to_csv());
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.last_saved = Some(path.clone());
        Ok(path)
    }
}

/// A recording on disk.
#[derive(Clone, Debug)]
pub struct RecordingInfo {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// Sample CSVs in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<RecordingInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<RecordingInfo> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(".csv") && !n.ends_with("-events.csv")
        })
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some(RecordingInfo { path: e.path(), name: e.file_name().to_string_lossy().to_string(), size: md.len(), modified: md.modified().ok() })
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.name.cmp(&a.name)));
    out
}

/// Reads a recording written by [`Recorder`]. Lines that do not parse are skipped.
pub fn load(path: &Path) -> Result<Vec<Record>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    match lines.next() {
        Some(h) if h.starts_with("time,cpu_pct") => {}
        _ => return Err("not a TMOP recording".into()),
    }
    Ok(lines.filter_map(Record::from_csv).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(seq: u64, cpu: f32) -> Snapshot {
        let mut s = Snapshot { seq, ..Default::default() };
        s.cpu.total_pct = cpu;
        s.memory.total = 1000;
        s.memory.available = 400;
        s.memory.used_pct = 60.0;
        s
    }

    fn row(pid: u32, name: &str, cpu: f32) -> ProcessRow {
        ProcessRow { pid, name: name.into(), cpu_pct: cpu, create_time: pid as i64, ..Default::default() }
    }

    #[test]
    fn csv_roundtrip() {
        let mut r = Record::from_snapshot(&snap(1, 12.5), &[row(4, "a,b.exe", 3.0), row(8, "c.exe", 9.0)]);
        assert!(r.top.starts_with("c.exe 9.0%|a b.exe"));
        r.docker_running = 3;
        r.docker_cpu = 42.5;
        r.docker_mem = 512 * 1024 * 1024;
        let back = Record::from_csv(&r.to_csv()).unwrap();
        assert_eq!(back.cpu, 12.5);
        assert_eq!(back.mem_used, 600);
        assert_eq!(back.top, r.top);
        assert_eq!(back.docker_running, 3);
        assert_eq!(back.docker_cpu, 42.5);
        assert_eq!(back.docker_mem, 512 * 1024 * 1024);
        assert_eq!(CSV_HEADER.split(',').count(), 18);
        assert_eq!(r.to_csv().split(',').count(), 18);
    }

    #[test]
    fn legacy_csv_row_parses_with_zero_docker_totals() {
        let legacy = "2026-10-15 10:00:00,12.50,3.00,2400,55.00,1000,1.00,0,0,0,0,0.00,45.0,120,a 1.0%|b 2.0%";
        let r = Record::from_csv(legacy).unwrap();
        assert_eq!(r.cpu, 12.5);
        assert_eq!(r.processes, 120);
        assert_eq!(r.top, "a 1.0%|b 2.0%");
        assert_eq!((r.docker_running, r.docker_cpu, r.docker_mem), (0, 0.0, 0));
    }

    #[test]
    fn records_to_folder_and_tracks_events() {
        let dir = std::env::temp_dir().join(format!("tmop-rec-test-{}", std::process::id()));
        let mut rec = Recorder { dir: dir.clone(), ..Default::default() };
        rec.on_sample(&snap(1, 5.0), &[row(10, "a.exe", 1.0)]);
        let path = rec.start(None).unwrap();
        assert!(rec.is_recording());
        rec.on_sample(&snap(2, 6.0), &[row(10, "a.exe", 1.0), row(11, "b.exe", 2.0)]);
        rec.docker = Some(Totals { running: 2, cpu_pct: 7.5, mem_used: 100, ..Default::default() });
        rec.on_sample(&snap(3, 7.0), &[row(11, "b.exe", 2.0)]);
        assert_eq!(rec.ring.back().map(|r| (r.docker_running, r.docker_cpu, r.docker_mem)), Some((2, 7.5, 100)));
        assert!(rec.events.iter().any(|e| e.kind == EventKind::Started && e.text.starts_with("b.exe")));
        assert!(rec.events.iter().any(|e| e.kind == EventKind::Exited && e.text.starts_with("a.exe")));
        assert_eq!(rec.stop(), Some(path.clone()));
        let rows = load(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].cpu, 7.0);
        let flight = rec.save_flight().unwrap();
        assert_eq!(load(&flight).unwrap().len(), 3);
        assert!(list(&dir).len() >= 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn time_limit_stops_recording() {
        let dir = std::env::temp_dir().join(format!("tmop-rec-limit-{}", std::process::id()));
        let mut rec = Recorder { dir: dir.clone(), ..Default::default() };
        rec.start(Some(Duration::from_millis(1))).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        rec.on_sample(&snap(1, 1.0), &[]);
        assert!(!rec.is_recording());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
