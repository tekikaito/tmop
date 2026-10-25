//! Pure parsers behind the Linux and macOS inventory readers (tested on every host).

use super::{CpuDetails, StartupApp};
use crate::pro::sysfmt::Plist;
use std::collections::HashSet;

/// The first processor block of `/proc/cpuinfo` plus the processor count.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuInfo {
    pub model: String,
    pub vendor: String,
    pub family: String,
    pub model_no: String,
    pub stepping: String,
    pub flags: HashSet<String>,
    pub processors: usize,
}

impl CpuInfo {
    /// `"AuthenticAMD Family 25 Model 33 Stepping 0"`, like the Windows identifier.
    pub fn identifier(&self) -> String {
        if self.vendor.is_empty() || self.family.is_empty() {
            return String::new();
        }
        format!("{} Family {} Model {} Stepping {}", self.vendor, self.family, self.model_no, self.stepping)
    }
}

pub fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut c = CpuInfo::default();
    let mut first = true;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "processor" {
            c.processors += 1;
            first = c.processors == 1;
            continue;
        }
        if !first {
            continue;
        }
        match k {
            "model name" | "Processor" if c.model.is_empty() => c.model = v.to_string(),
            "vendor_id" => c.vendor = v.to_string(),
            "cpu family" => c.family = v.to_string(),
            "model" => c.model_no = v.to_string(),
            "stepping" => c.stepping = v.to_string(),
            "flags" | "Features" => c.flags = v.split_whitespace().map(String::from).collect(),
            _ => {}
        }
    }
    c
}

/// `/proc/meminfo` value of `key` in bytes.
pub fn meminfo_bytes(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.split_whitespace().next()?.parse::<u64>().ok().map(|kb| kb * 1024))?
    })
}

/// `"32K"`, `"1024K"`, `"16M"` (sysfs cache sizes) to bytes.
pub fn parse_cache_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, mul) = match s.as_bytes().last() {
        Some(b'K') | Some(b'k') => (&s[..s.len() - 1], 1024),
        Some(b'M') | Some(b'm') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'G') | Some(b'g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().map(|n| n * mul).unwrap_or(0)
}

/// One cache of one logical processor, from `cpuN/cache/indexM/`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CacheInfo {
    pub level: u8,
    /// "Data", "Instruction" or "Unified".
    pub kind: String,
    pub size: u64,
    pub shared: String,
}

/// One online logical processor from `/sys/devices/system/cpu/cpuN/`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SysCpu {
    pub package: String,
    /// `topology/thread_siblings_list` (or `core_cpus_list`): equal for the threads of a core.
    pub siblings: String,
    pub caches: Vec<CacheInfo>,
}

/// Sockets, cores, logical processors and cache totals (each shared cache counted once).
pub fn summarize_cpus(cpus: &[SysCpu]) -> CpuDetails {
    let mut d = CpuDetails { logical: cpus.len(), ..Default::default() };
    d.sockets = cpus.iter().map(|c| c.package.as_str()).collect::<HashSet<_>>().len();
    d.cores = cpus.iter().enumerate().map(|(i, c)| if c.siblings.is_empty() { i.to_string() } else { c.siblings.clone() }).collect::<HashSet<_>>().len();
    let mut seen = HashSet::new();
    for c in cpus {
        for k in &c.caches {
            if !seen.insert((k.level, k.kind.clone(), k.shared.clone())) {
                continue;
            }
            match k.level {
                1 => d.l1 += k.size,
                2 => d.l2 += k.size,
                3 => d.l3 += k.size,
                _ => {}
            }
        }
    }
    d
}

/// True when the DMI vendor or model names a hypervisor (the list Windows uses too).
pub fn looks_virtual(maker: &str, model: &str) -> bool {
    let (maker, model) = (maker.to_lowercase(), model.to_lowercase());
    ["vmware", "qemu", "innotek", "virtualbox", "xen", "parallels", "kvm"].iter().any(|v| maker.contains(v) || model.contains(v)) || model.contains("virtual machine")
}

/// `/usr/share/zoneinfo/Europe/Berlin` (the `/etc/localtime` link) to `Europe/Berlin`.
pub fn zone_from_link(target: &str) -> String {
    match target.find("zoneinfo/") {
        Some(p) => target[p + "zoneinfo/".len()..].to_string(),
        None => String::new(),
    }
}

/// The vendor and device names of a PCI id in a `pci.ids` database.
pub fn pci_name(ids: &str, vendor: u16, device: u16) -> (Option<String>, Option<String>) {
    let (v, d) = (format!("{vendor:04x}"), format!("{device:04x}"));
    let mut vendor_name = None;
    for line in ids.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if vendor_name.is_some() {
                break;
            }
            if line.len() > 6 && line[..4].eq_ignore_ascii_case(&v) && line.as_bytes()[4] == b' ' {
                vendor_name = Some(line[4..].trim().to_string());
            }
            continue;
        }
        if vendor_name.is_some() && !line.starts_with("\t\t") {
            let l = &line[1..];
            if l.len() > 6 && l[..4].eq_ignore_ascii_case(&d) && l.as_bytes()[4] == b' ' {
                return (vendor_name, Some(l[4..].trim().to_string()));
            }
        }
    }
    (vendor_name, None)
}

/// `0x1002` (sysfs PCI ids) to a number.
pub fn hex_id(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// `systemctl --user show -p Id,Description,ExecStart <units>` to (unit, description, command).
pub fn parse_systemd_show(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let (mut id, mut desc, mut cmd) = (String::new(), String::new(), String::new());
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("Id=") {
                id = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("Description=") {
                desc = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("ExecStart=") {
                // `{ path=/usr/bin/x ; argv[]=/usr/bin/x --a ; ignore_errors=no ; ... }`
                if let Some(p) = v.find("argv[]=") {
                    let rest = &v[p + "argv[]=".len()..];
                    cmd = rest.split(" ;").next().unwrap_or("").trim().to_string();
                }
            }
        }
        if !id.is_empty() {
            out.push((id, desc, cmd));
        }
    }
    out
}

/// Unit names from `systemctl list-unit-files --no-legend --plain` (first column).
pub fn unit_names(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.split_whitespace().next()).filter(|u| u.ends_with(".service") && !u.contains('@')).map(String::from).collect()
}

/// macOS marketing name by major version.
pub fn macos_name(version: &str) -> &'static str {
    match version.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0) {
        11 => "Big Sur",
        12 => "Monterey",
        13 => "Ventura",
        14 => "Sonoma",
        15 => "Sequoia",
        26 => "Tahoe",
        _ => "",
    }
}

/// A launchd job plist as a startup entry (location and the `launchctl` override come later).
pub fn launchd_job(p: &Plist, file_stem: &str) -> StartupApp {
    let label = p.text("Label").unwrap_or_else(|| file_stem.to_string());
    let args: Vec<String> = p.get("ProgramArguments").map(|a| a.array().iter().filter_map(Plist::str).map(quote_arg).collect()).unwrap_or_default();
    let command = match p.text("Program") {
        Some(prog) if args.len() > 1 => format!("{} {}", quote_arg(&prog), args[1..].join(" ")),
        Some(prog) => quote_arg(&prog),
        None => args.join(" "),
    };
    StartupApp { enabled: !p.get("Disabled").and_then(Plist::bool).unwrap_or(false), name: label, command, ..Default::default() }
}

/// Quotes an argument containing spaces so [`super::command_path`] finds the program.
fn quote_arg(a: &str) -> String {
    if a.contains(' ') {
        format!("\"{a}\"")
    } else {
        a.to_string()
    }
}

/// `launchctl print-disabled <domain>`: label and whether it is disabled.
pub fn parse_launchctl_disabled(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once("=>")?;
            let label = k.trim().trim_matches('"');
            let v = v.trim();
            (!label.is_empty()).then(|| (label.to_string(), v == "disabled" || v == "true"))
        })
        .collect()
}

/// Rows from `system_profiler SPHardwareDataType -json` (model, chip, firmware, memory).
pub fn profiler_hardware(json: &str) -> Vec<(&'static str, String)> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let Some(hw) = v.get("SPHardwareDataType").and_then(|a| a.get(0)) else { return Vec::new() };
    let s = |k: &str| hw.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let model = match (s("machine_name"), s("machine_model")) {
        (n, m) if !n.is_empty() && !m.is_empty() => format!("{n} ({m})"),
        (n, m) => if n.is_empty() { m } else { n },
    };
    let chip = if s("chip_type").is_empty() { s("cpu_type") } else { s("chip_type") };
    vec![("Model", model), ("Chip", chip), ("Firmware version", s("boot_rom_version")), ("Memory", s("physical_memory"))]
}

/// (name, "VRAM, cores") rows from `system_profiler SPDisplaysDataType -json`.
pub fn profiler_displays(json: &str) -> Vec<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let mut out = Vec::new();
    for g in v.get("SPDisplaysDataType").and_then(|a| a.as_array()).map(Vec::as_slice).unwrap_or(&[]) {
        let s = |k: &str| g.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let name = if s("sppci_model").is_empty() { s("_name") } else { s("sppci_model") };
        if name.is_empty() {
            continue;
        }
        let vram = if s("spdisplays_vram").is_empty() { s("spdisplays_vram_shared") } else { s("spdisplays_vram") };
        let mut parts = Vec::new();
        if !vram.is_empty() {
            parts.push(vram);
        }
        if !s("sppci_cores").is_empty() {
            parts.push(format!("{} GPU cores", s("sppci_cores")));
        }
        out.push((name, parts.join(", ")));
    }
    out
}

/// L1/L2/L3 totals from macOS `hw.*` sysctls: per performance level on Apple silicon
/// (`hw.perflevelN.*`), else the per-core sizes times the physical cores.
pub fn mac_caches(get: &dyn Fn(&str) -> Option<u64>) -> (u64, u64, u64) {
    let levels = get("hw.nperflevels").unwrap_or(0);
    if levels > 0 {
        let (mut l1, mut l2) = (0, 0);
        for n in 0..levels {
            let g = |k: &str| get(&format!("hw.perflevel{n}.{k}")).unwrap_or(0);
            let cores = g("physicalcpu");
            l1 += (g("l1icachesize") + g("l1dcachesize")) * cores;
            l2 += g("l2cachesize") * cores.div_ceil(g("cpusperl2").max(1));
        }
        return (l1, l2, 0);
    }
    let cores = get("hw.physicalcpu").unwrap_or(1);
    let g = |k: &str| get(k).unwrap_or(0);
    ((g("hw.l1icachesize") + g("hw.l1dcachesize")) * cores, g("hw.l2cachesize") * cores, g("hw.l3cachesize"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::sysfmt::parse_plist_xml;

    const CPUINFO: &str = "processor\t: 0
vendor_id\t: AuthenticAMD
cpu family\t: 25
model\t\t: 33
model name\t: AMD Ryzen 9 5950X 16-Core Processor
stepping\t: 0
flags\t\t: fpu vme svm hypervisor sse2

processor\t: 1
vendor_id\t: AuthenticAMD
model name\t: should not win
";

    #[test]
    fn cpuinfo() {
        let c = parse_cpuinfo(CPUINFO);
        assert_eq!(c.processors, 2);
        assert_eq!(c.model, "AMD Ryzen 9 5950X 16-Core Processor");
        assert_eq!(c.identifier(), "AuthenticAMD Family 25 Model 33 Stepping 0");
        assert!(c.flags.contains("svm") && c.flags.contains("hypervisor"));
        let arm = parse_cpuinfo("processor\t: 0\nFeatures\t: fp asimd\nCPU implementer\t: 0x41\n");
        assert_eq!((arm.identifier(), arm.processors), (String::new(), 1));
        assert_eq!(meminfo_bytes("MemTotal:       16318504 kB\nMemFree: 1 kB\n", "MemTotal"), Some(16_318_504 * 1024));
        assert_eq!(meminfo_bytes("", "MemTotal"), None);
    }

    fn cpu(package: &str, siblings: &str, l2_shared: &str) -> SysCpu {
        let cache = |level, kind: &str, size, shared: &str| CacheInfo { level, kind: kind.into(), size, shared: shared.into() };
        SysCpu {
            package: package.into(),
            siblings: siblings.into(),
            caches: vec![cache(1, "Data", 32 * 1024, siblings), cache(1, "Instruction", 32 * 1024, siblings), cache(2, "Unified", 512 * 1024, l2_shared), cache(3, "Unified", 32 << 20, "0-7")],
        }
    }

    #[test]
    fn sysfs_topology() {
        // 1 socket, 4 cores with 2 threads each, a private L2 per core and one shared L3.
        let cpus: Vec<SysCpu> = (0..8).map(|i| {
            let core = i % 4;
            let sib = format!("{core},{}", core + 4);
            cpu("0", &sib, &sib)
        }).collect();
        let d = summarize_cpus(&cpus);
        assert_eq!((d.sockets, d.cores, d.logical), (1, 4, 8));
        assert_eq!((d.l1, d.l2, d.l3), (4 * 64 * 1024, 4 * 512 * 1024, 32 << 20));
        assert_eq!(parse_cache_size("32K"), 32 * 1024);
        assert_eq!(parse_cache_size("16M"), 16 << 20);
        assert_eq!(parse_cache_size("512"), 512);
        assert_eq!(parse_cache_size(""), 0);
    }

    #[test]
    fn small_helpers() {
        assert!(looks_virtual("QEMU", "Standard PC (Q35 + ICH9, 2009)"));
        assert!(looks_virtual("Microsoft Corporation", "Virtual Machine"));
        assert!(!looks_virtual("LENOVO", "20XW"));
        assert_eq!(zone_from_link("/usr/share/zoneinfo/Europe/Berlin"), "Europe/Berlin");
        assert_eq!(zone_from_link("/var/db/timezone/zoneinfo/America/New_York"), "America/New_York");
        assert_eq!(zone_from_link("/etc/x"), "");
        assert_eq!(hex_id("0x10de\n"), Some(0x10de));
        assert_eq!(macos_name("15.1"), "Sequoia");
        assert_eq!(macos_name("26.0"), "Tahoe");
        assert_eq!(macos_name("x"), "");
    }

    #[test]
    fn pci_ids() {
        let ids = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]\n\t\t1002 0e3a  Radeon RX 6900 XT\n10de  NVIDIA Corporation\n\t2684  AD102 [GeForce RTX 4090]\n";
        assert_eq!(pci_name(ids, 0x10de, 0x2684), (Some("NVIDIA Corporation".into()), Some("AD102 [GeForce RTX 4090]".into())));
        assert_eq!(pci_name(ids, 0x1002, 0x73bf).1.as_deref(), Some("Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]"));
        assert_eq!(pci_name(ids, 0x1002, 0x0e3a), (Some("Advanced Micro Devices, Inc. [AMD/ATI]".into()), None));
        assert_eq!(pci_name(ids, 0x8086, 0x1), (None, None));
    }

    #[test]
    fn systemd_user_units() {
        let show = "Id=pipewire.service\nDescription=PipeWire Multimedia Service\nExecStart={ path=/usr/bin/pipewire ; argv[]=/usr/bin/pipewire -v ; ignore_errors=no ; start_time=[n/a] }\n\nId=syncthing.service\nDescription=Syncthing\nExecStart=\n";
        let units = parse_systemd_show(show);
        assert_eq!(units[0], ("pipewire.service".into(), "PipeWire Multimedia Service".into(), "/usr/bin/pipewire -v".into()));
        assert_eq!(units[1].2, "");
        assert_eq!(unit_names("pipewire.service enabled enabled\nfoo@.service enabled enabled\ndefault.target enabled -\n"), ["pipewire.service"]);
    }

    #[test]
    fn launchd_jobs() {
        let p = parse_plist_xml(r#"<plist version="1.0"><dict><key>Label</key><string>com.example.helper</string><key>ProgramArguments</key><array><string>/Applications/My App.app/Contents/MacOS/helper</string><string>--bg</string></array><key>Disabled</key><true/></dict></plist>"#).unwrap();
        let j = launchd_job(&p, "file");
        assert_eq!(j.name, "com.example.helper");
        assert_eq!(j.command, "\"/Applications/My App.app/Contents/MacOS/helper\" --bg");
        assert_eq!(super::super::command_path(&j.command), "/Applications/My App.app/Contents/MacOS/helper");
        assert!(!j.enabled);
        let p = parse_plist_xml(r#"<plist><dict><key>Program</key><string>/usr/local/bin/x</string></dict></plist>"#).unwrap();
        let j = launchd_job(&p, "org.x");
        assert_eq!((j.name.as_str(), j.command.as_str(), j.enabled), ("org.x", "/usr/local/bin/x", true));
        let disabled = parse_launchctl_disabled("disabled services = {\n\t\"com.apple.ScriptMenuApp\" => disabled\n\t\"com.docker.helper\" => enabled\n\t\"old.style\" => true\n}\n");
        assert_eq!(disabled, [("com.apple.ScriptMenuApp".to_string(), true), ("com.docker.helper".to_string(), false), ("old.style".to_string(), true)]);
    }

    #[test]
    fn system_profiler() {
        let hw = r#"{"SPHardwareDataType":[{"_name":"hardware_overview","boot_rom_version":"10151.1.1","chip_type":"Apple M1","machine_model":"MacBookAir10,1","machine_name":"MacBook Air","physical_memory":"16 GB","serial_number":"X"}]}"#;
        let rows = profiler_hardware(hw);
        assert_eq!(rows[0], ("Model", "MacBook Air (MacBookAir10,1)".to_string()));
        assert_eq!(rows[1].1, "Apple M1");
        assert_eq!(rows[2].1, "10151.1.1");
        assert!(profiler_hardware("not json").is_empty());
        let gpu = r#"{"SPDisplaysDataType":[{"_name":"Apple M1","sppci_cores":"8","sppci_model":"Apple M1"},{"_name":"kHW_AMDRadeonPro5500MItem","sppci_model":"AMD Radeon Pro 5500M","spdisplays_vram":"4 GB"}]}"#;
        assert_eq!(profiler_displays(gpu), [("Apple M1".to_string(), "8 GPU cores".to_string()), ("AMD Radeon Pro 5500M".to_string(), "4 GB".to_string())]);
    }

    #[test]
    fn mac_cache_totals() {
        let m1 = |k: &str| -> Option<u64> {
            Some(match k {
                "hw.nperflevels" => 2,
                "hw.perflevel0.physicalcpu" | "hw.perflevel1.physicalcpu" => 4,
                "hw.perflevel0.l1icachesize" => 192 << 10,
                "hw.perflevel0.l1dcachesize" => 128 << 10,
                "hw.perflevel0.l2cachesize" => 12 << 20,
                "hw.perflevel0.cpusperl2" | "hw.perflevel1.cpusperl2" => 4,
                "hw.perflevel1.l1icachesize" => 128 << 10,
                "hw.perflevel1.l1dcachesize" => 64 << 10,
                "hw.perflevel1.l2cachesize" => 4 << 20,
                _ => return None,
            })
        };
        assert_eq!(mac_caches(&m1), (4 * (320 << 10) + 4 * (192 << 10), (12 << 20) + (4 << 20), 0));
        let intel = |k: &str| -> Option<u64> {
            Some(match k {
                "hw.physicalcpu" => 4,
                "hw.l1icachesize" | "hw.l1dcachesize" => 32 << 10,
                "hw.l2cachesize" => 256 << 10,
                "hw.l3cachesize" => 8 << 20,
                _ => return None,
            })
        };
        assert_eq!(mac_caches(&intel), (4 * (64 << 10), 4 * (256 << 10), 8 << 20));
    }
}
