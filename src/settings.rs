//! Settings (`settings.json`): TMOP's own schema, grouped by section.
//!
//! * the file is a JSON object `{ "version": 1, "general": {..}, "window": {..},
//!   "appearance": {..}, "graphs": {..}, "popout": {..} }` with camelCase keys;
//! * an older `version` is upgraded step by step (1→2→…) by [`MIGRATIONS`];
//! * a file without `version` is TMOG's format (`schemaVersion` 12, also what TMOP 0.1 wrote) and
//!   is converted by [`migrate_v12`]; TMOG keys TMOP doesn't implement are dropped;
//! * unknown keys are dropped with a warning; values are type-checked ("expected a boolean", ...);
//! * a file that migrates is rewritten at once; the original is kept as `settings.v<old>.bak`
//!   (TMOG's format: `settings.v12.bak`);
//! * a file from a newer TMOP opens read-only with defaults and is never overwritten;
//! * writes go through a temporary file (`.tmp.`) and a corrupt file is set aside (`.corrupt.`).
//!
//! TMOP stores its file at `%LOCALAPPDATA%\TMOP\settings.json` and can import TMOG's file
//! from `%LOCALAPPDATA%\TMOG\Task Manager\settings.json` (read-only).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The schema version this build reads and writes.
pub const VERSION: u32 = 1;

/// `MIGRATIONS[i]` upgrades a version `i + 1` file to version `i + 2`.
const MIGRATIONS: &[fn(&mut Map<String, Value>)] = &[];

/// Maps whose keys are data (column ids, metric ids), not settings, so they are not checked.
const FREE_MAPS: &[&str] = &["window.processTableColumnWidths", "graphs.metricColors"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub version: u32,
    pub general: General,
    pub window: Window,
    pub appearance: Appearance,
    pub graphs: Graphs,
    pub popout: Popout,
    /// Set when the file is from a newer TMOP, or is an old one that couldn't be backed up:
    /// [`Settings::save`] refuses to overwrite it.
    #[serde(skip)]
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct General {
    /// An [`UpdateSpeed`] token.
    pub update_speed: String,
    /// A page id (`summary`, `processes`, ...).
    pub default_page: String,
    /// `celsius` or `fahrenheit`.
    pub temperature_unit: String,
    pub show_self_in_reports: bool,
    pub per_core_cpu_statistics: bool,
    pub confirm_efficiency_mode: bool,
    pub disk_space_suppress_stop_confirm: bool,
    pub show_full_account_name: bool,
    /// Empty means the default recordings folder.
    pub recording_folder: String,
    /// Empty means the temporary folder.
    pub benchmark_folder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Window {
    pub always_on_top: bool,
    pub uptime_visible: bool,
    pub process_tree: bool,
    pub process_table_hidden_columns: Vec<String>,
    pub process_table_column_widths: BTreeMap<String, u32>,
    pub performance_cpus_expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Appearance {
    /// `system`, `dark` or `light`.
    pub theme: String,
    pub display_font: String,
    pub bloom: bool,
    pub bloom_level: u32,
    pub high_frequency_visuals: bool,
    pub frame_rate: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Graphs {
    pub color_keyed: bool,
    pub smooth_curves: bool,
    pub dashed_summary: bool,
    pub compressed_time: bool,
    pub history_multiplier: u32,
    pub pixels_per_update: u32,
    /// Metric id → `#rrggbb` override. Not used yet.
    pub metric_colors: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Popout {
    /// A skin id from the catalog.
    pub skin: String,
    pub opacity: f64,
    pub opaque_on_hover: bool,
    /// `off`, `desktop`, `normal` or `topMost`.
    pub mode: String,
    /// The mode to reopen with after `off`.
    pub last_mode: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            general: General::default(),
            window: Window::default(),
            appearance: Appearance::default(),
            graphs: Graphs::default(),
            popout: Popout::default(),
            read_only: false,
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            update_speed: "medium".into(),
            default_page: "summary".into(),
            temperature_unit: "celsius".into(),
            show_self_in_reports: false,
            per_core_cpu_statistics: false,
            confirm_efficiency_mode: true,
            disk_space_suppress_stop_confirm: false,
            show_full_account_name: false,
            recording_folder: String::new(),
            benchmark_folder: String::new(),
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self {
            always_on_top: false,
            uptime_visible: true,
            process_tree: true,
            process_table_hidden_columns: Vec::new(),
            process_table_column_widths: BTreeMap::new(),
            performance_cpus_expanded: true,
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            display_font: "Michroma".into(),
            bloom: true,
            bloom_level: 7,
            high_frequency_visuals: true,
            frame_rate: 60,
        }
    }
}

impl Default for Graphs {
    fn default() -> Self {
        Self {
            color_keyed: true,
            smooth_curves: true,
            dashed_summary: false,
            compressed_time: true,
            history_multiplier: 15,
            pixels_per_update: 16,
            metric_colors: BTreeMap::new(),
        }
    }
}

impl Default for Popout {
    fn default() -> Self {
        Self { skin: "classic".into(), opacity: 1.0, opaque_on_hover: true, mode: "off".into(), last_mode: "topMost".into() }
    }
}

/// Sampling cadence: "Fast — 4/sec", "High — 2/sec",
/// "Normal — 1/sec", "Slow — 0.5/sec", "Paused".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateSpeed {
    Fast,
    High,
    Normal,
    Slow,
    Paused,
}

impl UpdateSpeed {
    pub const ALL: [UpdateSpeed; 5] = [Self::Fast, Self::High, Self::Normal, Self::Slow, Self::Paused];

    /// Token mapping: `high` = Fast, `medium` = High, `normal` = Normal, `slow` = Slow,
    /// `paused` = Paused. `fast` and `low` are accepted as aliases.
    pub fn from_token(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "high" | "fast" | "veryhigh" | "realtime" => Self::Fast,
            "medium" => Self::High,
            "slow" => Self::Slow,
            "paused" | "pause" => Self::Paused,
            _ => Self::Normal, // "normal", "low", unknown
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Fast => "high",
            Self::High => "medium",
            Self::Normal => "normal",
            Self::Slow => "slow",
            Self::Paused => "paused",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast \u{2014} 4/sec",
            Self::High => "High \u{2014} 2/sec",
            Self::Normal => "Normal \u{2014} 1/sec",
            Self::Slow => "Slow \u{2014} 0.5/sec",
            Self::Paused => "Paused",
        }
    }

    pub fn interval(self) -> Option<Duration> {
        match self {
            Self::Fast => Some(Duration::from_millis(250)),
            Self::High => Some(Duration::from_millis(500)),
            Self::Normal => Some(Duration::from_millis(1000)),
            Self::Slow => Some(Duration::from_millis(2000)),
            Self::Paused => None,
        }
    }
}

#[derive(Debug)]
pub enum LoadOutcome {
    Loaded,
    Missing,
    /// Converted from TMOG's format (`schemaVersion` 12).
    MigratedV12,
    /// Upgraded from an older TMOP `version`.
    Upgraded(u32),
    /// `version` is newer than this build: defaults, read-only, the file is left alone.
    Newer(u64),
    /// Migrated, but the original couldn't be backed up: read-only, the file is left alone.
    BackupFailed(String),
    /// Unreadable/invalid JSON, no version or wrong value types; file set aside as `.corrupt.`.
    Corrupt(String),
}

/// The result of reading settings.
#[derive(Debug)]
pub struct Load {
    pub settings: Settings,
    pub outcome: LoadOutcome,
    /// Unknown keys that were dropped, as dotted paths (`graphs.foo`).
    pub unknown_keys: Vec<String>,
}

impl Load {
    fn defaults(outcome: LoadOutcome) -> Self {
        Self { settings: Settings::default(), outcome, unknown_keys: Vec::new() }
    }

    /// One line for the status bar: `<what> (<outcome>)`, plus any dropped keys.
    pub fn describe(&self, what: &str) -> String {
        let mut s = format!("{what} ({:?})", self.outcome);
        if !self.unknown_keys.is_empty() {
            s += &format!("; ignored unknown keys: {}", self.unknown_keys.join(", "));
        }
        s
    }
}

impl Settings {
    pub fn update_speed(&self) -> UpdateSpeed {
        UpdateSpeed::from_token(&self.general.update_speed)
    }

    pub fn fahrenheit(&self) -> bool {
        self.general.temperature_unit.eq_ignore_ascii_case("fahrenheit")
    }

    /// Parses settings JSON of any known version, migrating it to [`VERSION`].
    pub fn from_json(text: &str) -> Result<Load, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let Value::Object(mut obj) = v else { return Err("settings root must be an object".into()) };
        let outcome = match obj.get("version").map(Value::as_u64) {
            Some(Some(ver)) if ver == VERSION as u64 => LoadOutcome::Loaded,
            Some(Some(ver)) if ver >= 1 && ver < VERSION as u64 => {
                for step in &MIGRATIONS[ver as usize - 1..] {
                    step(&mut obj);
                }
                LoadOutcome::Upgraded(ver as u32)
            }
            Some(Some(ver)) if ver > VERSION as u64 => {
                let settings = Settings { read_only: true, ..Default::default() };
                return Ok(Load { settings, outcome: LoadOutcome::Newer(ver), unknown_keys: Vec::new() });
            }
            Some(Some(ver)) => return Err(format!("unsupported settings version {ver}")),
            Some(None) => return Err("version must be an unsigned integer".into()),
            None if obj.contains_key("schemaVersion") => {
                obj = migrate_v12(&obj);
                LoadOutcome::MigratedV12
            }
            None => return Err("settings file has no version".into()),
        };
        obj.insert("version".into(), VERSION.into());
        let mut unknown_keys = Vec::new();
        let Value::Object(shape) = serde_json::to_value(Settings::default()).expect("settings serialize") else { unreachable!() };
        strip_unknown(&mut obj, &shape, "", &mut unknown_keys);
        let mut settings: Settings = serde_json::from_value(Value::Object(obj)).map_err(|e| e.to_string())?;
        settings.popout.opacity = settings.popout.opacity.clamp(0.1, 1.0);
        Ok(Load { settings, outcome, unknown_keys })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("settings serialize")
    }

    /// Loads from `path`, never failing: falls back to defaults.
    pub fn load(path: &Path) -> Load {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Load::defaults(LoadOutcome::Missing),
        };
        match Self::from_json(&text) {
            Ok(mut load) => {
                let old = match load.outcome {
                    LoadOutcome::MigratedV12 => Some(12),
                    LoadOutcome::Upgraded(v) => Some(v),
                    _ => None,
                };
                // Keep the original, then write the migrated file so it migrates only once.
                // Without a backup the original must never be overwritten, so go read-only.
                if let Some(v) = old {
                    match backup(path, v, &text) {
                        Ok(()) => {
                            let _ = load.settings.save(path);
                        }
                        Err(e) => {
                            load.settings.read_only = true;
                            load.outcome = LoadOutcome::BackupFailed(e.to_string());
                        }
                    }
                }
                load
            }
            Err(e) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let mut bad = path.as_os_str().to_owned();
                bad.push(format!(".corrupt.{stamp}"));
                let _ = std::fs::rename(path, PathBuf::from(bad));
                Load::defaults(LoadOutcome::Corrupt(e))
            }
        }
    }

    /// Atomic save: write `<path>.tmp.<pid>` then rename over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if self.read_only {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "settings are read-only; the file is not overwritten"));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".tmp.{}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path)
    }
}

/// How many `.bak` names [`backup`] tries before giving up.
const BACKUP_SLOTS: u32 = 10;

/// Writes `text` to `settings.v<v>.bak` next to `path`, never replacing an existing backup:
/// if that name is taken it uses `settings.v<v>.1.bak`, `.2.bak`, ...
fn backup(path: &Path, v: u32, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    for n in 0..BACKUP_SLOTS {
        let ext = if n == 0 { format!("v{v}.bak") } else { format!("v{v}.{n}.bak") };
        let bak = path.with_extension(ext);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&bak) {
            Ok(mut f) => {
                return f.write_all(text.as_bytes()).inspect_err(|_| {
                    let _ = std::fs::remove_file(&bak);
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{BACKUP_SLOTS} v{v} backups already exist")))
}

/// TMOG `schemaVersion` 12 key → TMOP `section.key`. TMOP 0.1 wrote the same format and kept
/// its own extra keys (`popoutMode`, `recordingFolder`, ...) at the top level. Other keys are dropped.
const V12_KEYS: &[(&str, &str)] = &[
    ("updateSpeed", "general.updateSpeed"),
    ("defaultPage", "general.defaultPage"),
    ("temperatureUnit", "general.temperatureUnit"),
    ("showSelfInReports", "general.showSelfInReports"),
    ("perCoreCpuStatistics", "general.perCoreCpuStatistics"),
    ("confirmEfficiencyMode", "general.confirmEfficiencyMode"),
    ("diskSpaceSuppressStopConfirm", "general.diskSpaceSuppressStopConfirm"),
    ("showFullAccountName", "general.showFullAccountName"),
    ("recordingFolder", "general.recordingFolder"),
    ("benchmarkFolder", "general.benchmarkFolder"),
    ("alwaysOnTop", "window.alwaysOnTop"),
    ("uptimeVisible", "window.uptimeVisible"),
    ("processTreeEnabled", "window.processTree"),
    ("processTableHiddenColumns", "window.processTableHiddenColumns"),
    ("processTableColumnWidths", "window.processTableColumnWidths"),
    ("performanceCpusExpanded", "window.performanceCpusExpanded"),
    ("theme", "appearance.theme"),
    ("applicationFont", "appearance.displayFont"),
    ("visualBloom", "appearance.bloom"),
    ("visualBloomLevel", "appearance.bloomLevel"),
    ("highFrequencyVisuals", "appearance.highFrequencyVisuals"),
    ("visualFrameRate", "appearance.frameRate"),
    ("colorKeyedGraphs", "graphs.colorKeyed"),
    ("smoothGraphCurves", "graphs.smoothCurves"),
    ("dashedSummaryGraphLines", "graphs.dashedSummary"),
    ("compressedGraphTime", "graphs.compressedTime"),
    ("graphHistoryMultiplier", "graphs.historyMultiplier"),
    ("graphPixelsPerUpdate", "graphs.pixelsPerUpdate"),
    ("popoutSkin", "popout.skin"),
    ("popoutOpacity", "popout.opacity"),
    ("popoutOpaqueOnHover", "popout.opaqueOnHover"),
    ("popoutMode", "popout.mode"),
    ("popoutLastMode", "popout.lastMode"),
];

/// Converts a TMOG-format object to a version 1 object.
fn migrate_v12(old: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("version".into(), 1.into());
    for (from, to) in V12_KEYS {
        if let Some(v) = old.get(*from) {
            let (section, key) = to.split_once('.').expect("section.key");
            let sec = out.entry(section).or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(sec) = sec {
                sec.insert(key.into(), v.clone());
            }
        }
    }
    out
}

/// Removes the keys of `obj` that `shape` (the serialized defaults) doesn't have, recording
/// their dotted paths in `unknown`. Values of the wrong type are left for serde to reject.
fn strip_unknown(obj: &mut Map<String, Value>, shape: &Map<String, Value>, prefix: &str, unknown: &mut Vec<String>) {
    obj.retain(|k, v| {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match (shape.get(k), v) {
            (None, _) => {
                unknown.push(path);
                false
            }
            (Some(Value::Object(sub)), Value::Object(m)) if !FREE_MAPS.contains(&path.as_str()) => {
                strip_unknown(m, sub, &path, unknown);
                true
            }
            _ => true,
        }
    });
}

/// One-time move of the pre-rename data folder (`%LOCALAPPDATA%\tmog-rs`: settings, skins,
/// recordings) to `%LOCALAPPDATA%\TMOP`. Does nothing when the new folder already exists.
pub fn migrate_legacy_dir() {
    let base = local_app_data();
    let (old, new) = (base.join(crate::LEGACY_APP_DIR), base.join(crate::APP_DIR));
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

/// `%LOCALAPPDATA%\TMOP\settings.json`
pub fn default_path() -> PathBuf {
    local_app_data().join(crate::APP_DIR).join("settings.json")
}

/// TMOG's settings file, used only for one-way import.
pub fn tmog_path() -> PathBuf {
    local_app_data().join("TMOG").join("Task Manager").join("settings.json")
}

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Reads TMOG's settings file and converts it (never modifies it).
pub fn import_tmog() -> Result<Load, String> {
    std::fs::read_to_string(tmog_path()).map_err(|e| e.to_string()).and_then(|t| Settings::from_json(&t))
}

/// Loads TMOP's settings; on first run imports TMOG's file if present.
pub fn load_or_import() -> (Settings, String) {
    let own = default_path();
    let (load, line) = if own.exists() {
        let load = Settings::load(&own);
        let line = load.describe(&format!("settings: {}", own.display()));
        (load, line)
    } else if tmog_path().exists() {
        let load = import_tmog().unwrap_or_else(|e| Load::defaults(LoadOutcome::Corrupt(e)));
        let _ = load.settings.save(&own);
        let line = load.describe(&format!("settings imported from {}", tmog_path().display()));
        (load, line)
    } else {
        return (Settings::default(), "settings: defaults".into());
    };
    if !load.unknown_keys.is_empty() {
        eprintln!("{line}");
    }
    (load.settings, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TMOG `schemaVersion` 12 file as TMOG and TMOP 0.1 wrote it.
    const SAMPLE: &str = r#"{"schemaVersion":12,"theme":"system","temperatureUnit":"celsius","applicationFont":"Michroma","applicationLanguage":"","visualBloom":true,"visualBloomLevel":7,"colorKeyedGraphs":true,"dashedSummaryGraphLines":false,"smoothGraphCurves":true,"highFrequencyVisuals":true,"visualFrameRate":60,"showSelfInReports":false,"perCoreCpuStatistics":false,"defaultPage":"summary","updateSpeed":"medium","graphPixelsPerUpdate":16,"compressedGraphTime":true,"graphHistoryMultiplier":15,"alwaysOnTop":false,"hideWhenMinimized":false,"showFullAccountName":false,"showAllAccountHistory":false,"confirmEfficiencyMode":true,"diskSpaceSuppressStopConfirm":false,"uptimeVisible":true,"popoutOpacity":1,"popoutOpaqueOnHover":true,"checkForUpdatesOnStart":true,"autoUpdateOnStart":false,"showSettingsAsDialog":false,"processTableHiddenColumns":[],"processTableColumnWidths":{},"diskLights.colors":"disk-71ca69de6ab42773=red","diskLights.durationMs":"35","diskLights.initialized":"true","diskLights.monitored":"disk-71ca69de6ab42773","diskLights.order":"disk-71ca69de6ab42773","mainWindowPlacement":"0,0,1920,1080,52,52,1372,881,1","popoutBoundsBySkin":"","popoutSkin":"com.tmog.console"}"#;

    fn parse(text: &str) -> Load {
        Settings::from_json(text).unwrap()
    }

    #[test]
    fn migrates_v12_sample() {
        let load = parse(SAMPLE);
        assert!(matches!(load.outcome, LoadOutcome::MigratedV12));
        assert!(load.unknown_keys.is_empty(), "dropped TMOG keys are not warnings: {:?}", load.unknown_keys);
        let s = load.settings;
        assert_eq!(s.version, VERSION);
        assert_eq!(s.update_speed(), UpdateSpeed::High);
        assert_eq!(s.popout.skin, "com.tmog.console");
        assert_eq!(s.popout.opacity, 1.0);
        assert_eq!(s.appearance.display_font, "Michroma");
        assert!(s.graphs.color_keyed && s.graphs.smooth_curves && !s.graphs.dashed_summary);
        // Unimplemented TMOG keys are gone from the written file.
        let out: Map<String, Value> = serde_json::from_str(&s.to_json()).unwrap();
        let keys: Vec<&str> = out.keys().map(String::as_str).collect();
        assert_eq!(keys, ["version", "general", "window", "appearance", "graphs", "popout"]);
        assert!(!s.to_json().contains("diskLights"));
    }

    #[test]
    fn migrates_v12_values_and_tmop_extras() {
        let v12 = r#"{"schemaVersion":12,"theme":"light","applicationFont":"Consolas","colorKeyedGraphs":false,"smoothGraphCurves":false,"dashedSummaryGraphLines":true,"popoutSkin":"neon","popoutOpacity":0.5,"popoutMode":"desktop","popoutLastMode":"normal","recordingFolder":"D:\\rec","benchmarkFolder":"D:\\bench","processTreeEnabled":false,"performanceCpusExpanded":false,"processTableColumnWidths":{"cpu":80},"alwaysOnTop":true}"#;
        let s = parse(v12).settings;
        assert_eq!((s.appearance.theme.as_str(), s.appearance.display_font.as_str()), ("light", "Consolas"));
        assert!(!s.graphs.color_keyed && !s.graphs.smooth_curves && s.graphs.dashed_summary);
        assert_eq!((s.popout.skin.as_str(), s.popout.opacity), ("neon", 0.5));
        assert_eq!((s.popout.mode.as_str(), s.popout.last_mode.as_str()), ("desktop", "normal"));
        assert_eq!((s.general.recording_folder.as_str(), s.general.benchmark_folder.as_str()), ("D:\\rec", "D:\\bench"));
        assert!(!s.window.process_tree && !s.window.performance_cpus_expanded && s.window.always_on_top);
        assert_eq!(s.window.process_table_column_widths.get("cpu"), Some(&80));
        // A TMOG file with another schemaVersion is converted the same way, never reset.
        let s = parse(r#"{"schemaVersion":11,"theme":"dark"}"#).settings;
        assert_eq!(s.appearance.theme, "dark");
    }

    #[test]
    fn v1_round_trips() {
        let mut s = Settings::default();
        s.general.update_speed = "high".into();
        s.window.process_table_hidden_columns = vec!["handles".into()];
        s.window.process_table_column_widths.insert("name".into(), 240);
        s.appearance.theme = "dark".into();
        s.graphs.metric_colors.insert("cpu".into(), "#4fb4ff".into());
        s.popout.mode = "topMost".into();
        let load = parse(&s.to_json());
        assert!(matches!(load.outcome, LoadOutcome::Loaded));
        assert!(load.unknown_keys.is_empty());
        assert_eq!(load.settings, s);
    }

    #[test]
    fn unknown_keys_are_dropped_with_a_warning() {
        let load = parse(r#"{"version":1,"foo":1,"graphs":{"colorKeyed":false,"bar":true},"window":{"processTableColumnWidths":{"cpu":80}},"schemaVersion":12}"#);
        assert_eq!(load.unknown_keys, ["foo", "graphs.bar", "schemaVersion"]);
        assert!(!load.settings.graphs.color_keyed);
        // Free maps keep their keys.
        assert_eq!(load.settings.window.process_table_column_widths.get("cpu"), Some(&80));
        assert!(load.describe("settings").ends_with("ignored unknown keys: foo, graphs.bar, schemaVersion"));
        assert!(!load.settings.to_json().contains("foo"));
    }

    #[test]
    fn type_errors_are_rejected() {
        assert!(Settings::from_json(r#"{"version":1,"window":{"alwaysOnTop":"yes"}}"#).is_err());
        assert!(Settings::from_json(r#"{"version":1,"graphs":true}"#).is_err());
        assert!(Settings::from_json(r#"{"version":"1"}"#).is_err());
        assert!(Settings::from_json(r#"{"schemaVersion":12,"alwaysOnTop":"yes"}"#).is_err());
        assert!(Settings::from_json("{}").is_err());
        assert!(Settings::from_json("[]").is_err());
    }

    #[test]
    fn missing_keys_take_defaults() {
        let s = parse(r#"{"version":1,"appearance":{"theme":"dark"}}"#).settings;
        assert_eq!(s.appearance.theme, "dark");
        assert_eq!(s.appearance.display_font, "Michroma");
        assert_eq!(s.graphs.history_multiplier, 15);
        assert_eq!(s.popout, Popout::default());
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join(format!("tmop-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.general.update_speed = "high".into();
        s.window.process_table_hidden_columns = vec!["handles".into()];
        s.save(&path).unwrap();
        let load = Settings::load(&path);
        assert!(matches!(load.outcome, LoadOutcome::Loaded));
        assert_eq!(load.settings, s);
        std::fs::write(&path, "{ not json").unwrap();
        let load = Settings::load(&path);
        assert!(matches!(load.outcome, LoadOutcome::Corrupt(_)));
        assert_eq!(load.settings, Settings::default());
        assert!(!path.exists(), "corrupt file should have been set aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v12_file_migrates_once_and_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("tmop-settings-v12-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, SAMPLE).unwrap();
        let first = Settings::load(&path);
        assert!(matches!(first.outcome, LoadOutcome::MigratedV12));
        assert_eq!(std::fs::read_to_string(dir.join("settings.v12.bak")).unwrap(), SAMPLE);
        // The file on disk is version 1 now, so the next load doesn't migrate again.
        let second = Settings::load(&path);
        assert!(matches!(second.outcome, LoadOutcome::Loaded));
        assert_eq!(second.settings, first.settings);
        // A later v12 file (TMOP 0.1 run again) never replaces the first backup.
        let later = r#"{"schemaVersion":12,"theme":"dark"}"#;
        std::fs::write(&path, later).unwrap();
        assert!(matches!(Settings::load(&path).outcome, LoadOutcome::MigratedV12));
        assert_eq!(std::fs::read_to_string(dir.join("settings.v12.bak")).unwrap(), SAMPLE);
        assert_eq!(std::fs::read_to_string(dir.join("settings.v12.1.bak")).unwrap(), later);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_without_a_backup_is_read_only() {
        let dir = std::env::temp_dir().join(format!("tmop-settings-nobak-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, SAMPLE).unwrap();
        for n in 0..BACKUP_SLOTS {
            let ext = if n == 0 { "v12.bak".to_string() } else { format!("v12.{n}.bak") };
            std::fs::write(path.with_extension(ext), "taken").unwrap();
        }
        let load = Settings::load(&path);
        assert!(matches!(load.outcome, LoadOutcome::BackupFailed(_)));
        assert!(load.settings.read_only);
        assert_eq!(load.settings.popout.skin, "com.tmog.console", "migrated values are still used");
        assert!(load.settings.save(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAMPLE, "an original without a backup must not be touched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_version_is_read_only_and_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("tmop-settings-newer-{}", std::process::id()));
        let path = dir.join("settings.json");
        let newer = r#"{"version":99,"appearance":{"theme":"dark"},"futureSection":{}}"#;
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, newer).unwrap();
        let load = Settings::load(&path);
        assert!(matches!(load.outcome, LoadOutcome::Newer(99)));
        assert!(load.settings.read_only);
        assert_eq!(load.settings.appearance, Appearance::default());
        let mut s = load.settings;
        s.window.always_on_top = true;
        assert!(s.save(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer, "a newer file must not be touched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_tmog_settings_parse_when_present() {
        let Ok(text) = std::fs::read_to_string(tmog_path()) else { return };
        let load = Settings::from_json(&text).expect("TMOG settings should parse");
        assert!(matches!(load.outcome, LoadOutcome::MigratedV12));
    }

    #[test]
    fn update_speed_tokens() {
        for s in UpdateSpeed::ALL {
            assert_eq!(UpdateSpeed::from_token(s.token()), s);
        }
        assert_eq!(UpdateSpeed::Paused.interval(), None);
        assert_eq!(UpdateSpeed::from_token("medium").interval(), Some(Duration::from_millis(500)));
        assert_eq!(UpdateSpeed::from_token("medium").label(), "High \u{2014} 2/sec");
        assert_eq!(UpdateSpeed::from_token("high"), UpdateSpeed::Fast);
    }
}
