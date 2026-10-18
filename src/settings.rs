//! Settings (`settings.json`, schemaVersion 12, compatible with TMOG's file).
//!
//! * the file is a JSON object with `schemaVersion`; a missing/other version means
//!   "settings definition changed; all settings reset to defaults";
//! * values are type-checked ("must be boolean", "must be an unsigned integer", ...);
//! * writes go through a temporary file (`.tmp.`) and a corrupt file is set aside (`.corrupt.`).
//!
//! TMOP stores its own copy at `%LOCALAPPDATA%\TMOP\settings.json` and can
//! import TMOG's file from `%LOCALAPPDATA%\TMOG\Task Manager\settings.json` (read-only).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SCHEMA_VERSION: u32 = 12;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub schema_version: u32,
    pub theme: String,
    pub temperature_unit: String,
    pub application_font: String,
    pub application_language: String,
    pub visual_bloom: bool,
    pub visual_bloom_level: u32,
    pub color_keyed_graphs: bool,
    pub dashed_summary_graph_lines: bool,
    pub smooth_graph_curves: bool,
    pub high_frequency_visuals: bool,
    pub visual_frame_rate: u32,
    pub show_self_in_reports: bool,
    pub per_core_cpu_statistics: bool,
    pub default_page: String,
    pub update_speed: String,
    pub graph_pixels_per_update: u32,
    pub compressed_graph_time: bool,
    pub graph_history_multiplier: u32,
    pub always_on_top: bool,
    pub hide_when_minimized: bool,
    pub show_full_account_name: bool,
    pub show_all_account_history: bool,
    pub confirm_efficiency_mode: bool,
    pub disk_space_suppress_stop_confirm: bool,
    pub uptime_visible: bool,
    pub popout_opacity: f64,
    pub popout_opaque_on_hover: bool,
    /// Kept for schema compatibility; the rewrite never contacts an update server.
    pub check_for_updates_on_start: bool,
    /// Kept for schema compatibility; ignored.
    pub auto_update_on_start: bool,
    pub show_settings_as_dialog: bool,
    pub process_table_hidden_columns: Vec<String>,
    pub process_table_column_widths: BTreeMap<String, u32>,
    pub main_window_placement: String,
    pub popout_bounds_by_skin: String,
    pub popout_skin: String,
    /// Any other keys (e.g. `diskLights.*`) are preserved verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Settings {
    /// Defaults.
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            theme: "system".into(),
            temperature_unit: "celsius".into(),
            application_font: "Michroma".into(),
            application_language: String::new(),
            visual_bloom: true,
            visual_bloom_level: 7,
            color_keyed_graphs: true,
            dashed_summary_graph_lines: false,
            smooth_graph_curves: true,
            high_frequency_visuals: true,
            visual_frame_rate: 60,
            show_self_in_reports: false,
            per_core_cpu_statistics: false,
            default_page: "summary".into(),
            update_speed: "medium".into(),
            graph_pixels_per_update: 16,
            compressed_graph_time: true,
            graph_history_multiplier: 15,
            always_on_top: false,
            hide_when_minimized: false,
            show_full_account_name: false,
            show_all_account_history: false,
            confirm_efficiency_mode: true,
            disk_space_suppress_stop_confirm: false,
            uptime_visible: true,
            popout_opacity: 1.0,
            popout_opaque_on_hover: true,
            check_for_updates_on_start: false,
            auto_update_on_start: false,
            show_settings_as_dialog: false,
            process_table_hidden_columns: Vec::new(),
            process_table_column_widths: BTreeMap::new(),
            main_window_placement: String::new(),
            popout_bounds_by_skin: String::new(),
            popout_skin: "classic".into(),
            extra: BTreeMap::new(),
        }
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
    /// schemaVersion missing or different: reset to defaults.
    ResetSchema(Option<u64>),
    /// Unreadable/invalid JSON or wrong value types; file set aside as `.corrupt.`.
    Corrupt(String),
}

impl Settings {
    pub fn update_speed(&self) -> UpdateSpeed {
        UpdateSpeed::from_token(&self.update_speed)
    }

    pub fn fahrenheit(&self) -> bool {
        self.temperature_unit.eq_ignore_ascii_case("fahrenheit")
    }

    /// Parses settings JSON; a different schemaVersion resets to defaults.
    pub fn from_json(text: &str) -> Result<(Self, LoadOutcome), String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let obj = v.as_object().ok_or("settings root must be an object")?;
        let ver = obj.get("schemaVersion").and_then(Value::as_u64);
        if ver != Some(SCHEMA_VERSION as u64) {
            return Ok((Self::default(), LoadOutcome::ResetSchema(ver)));
        }
        let mut s: Settings = serde_json::from_value(v).map_err(|e| e.to_string())?;
        s.popout_opacity = s.popout_opacity.clamp(0.1, 1.0);
        Ok((s, LoadOutcome::Loaded))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("settings serialize")
    }

    /// Loads from `path`, never failing: falls back to defaults.
    pub fn load(path: &Path) -> (Self, LoadOutcome) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return (Self::default(), LoadOutcome::Missing),
        };
        match Self::from_json(&text) {
            Ok(r) => r,
            Err(e) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let mut bad = path.as_os_str().to_owned();
                bad.push(format!(".corrupt.{stamp}"));
                let _ = std::fs::rename(path, PathBuf::from(bad));
                (Self::default(), LoadOutcome::Corrupt(e))
            }
        }
    }

    /// Atomic save: write `<path>.tmp.<pid>` then rename over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
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

/// Loads TMOP's settings; on first run imports TMOG's file if present.
pub fn load_or_import() -> (Settings, String) {
    let own = default_path();
    if own.exists() {
        let (s, outcome) = Settings::load(&own);
        return (s, format!("settings: {} ({outcome:?})", own.display()));
    }
    let theirs = tmog_path();
    if theirs.exists() {
        let (s, outcome) = match std::fs::read_to_string(&theirs).map_err(|e| e.to_string()).and_then(|t| Settings::from_json(&t)) {
            Ok(r) => r,
            Err(e) => (Settings::default(), LoadOutcome::Corrupt(e)),
        };
        let _ = s.save(&own);
        return (s, format!("settings imported from {} ({outcome:?})", theirs.display()));
    }
    (Settings::default(), "settings: defaults".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"schemaVersion":12,"theme":"system","temperatureUnit":"celsius","applicationFont":"Michroma","applicationLanguage":"","visualBloom":true,"visualBloomLevel":7,"colorKeyedGraphs":true,"dashedSummaryGraphLines":false,"smoothGraphCurves":true,"highFrequencyVisuals":true,"visualFrameRate":60,"showSelfInReports":false,"perCoreCpuStatistics":false,"defaultPage":"summary","updateSpeed":"medium","graphPixelsPerUpdate":16,"compressedGraphTime":true,"graphHistoryMultiplier":15,"alwaysOnTop":false,"hideWhenMinimized":false,"showFullAccountName":false,"showAllAccountHistory":false,"confirmEfficiencyMode":true,"diskSpaceSuppressStopConfirm":false,"uptimeVisible":true,"popoutOpacity":1,"popoutOpaqueOnHover":true,"checkForUpdatesOnStart":true,"autoUpdateOnStart":false,"showSettingsAsDialog":false,"processTableHiddenColumns":[],"processTableColumnWidths":{},"diskLights.colors":"disk-71ca69de6ab42773=red","diskLights.durationMs":"35","diskLights.initialized":"true","diskLights.monitored":"disk-71ca69de6ab42773","diskLights.order":"disk-71ca69de6ab42773","mainWindowPlacement":"0,0,1920,1080,52,52,1372,881,1","popoutBoundsBySkin":"","popoutSkin":"com.tmog.console"}"#;

    #[test]
    fn parses_sample_file_and_roundtrips_keys() {
        let (s, outcome) = Settings::from_json(SAMPLE).unwrap();
        assert!(matches!(outcome, LoadOutcome::Loaded));
        assert_eq!(s.update_speed(), UpdateSpeed::High);
        assert_eq!(s.popout_skin, "com.tmog.console");
        assert_eq!(s.extra.get("diskLights.durationMs"), Some(&Value::String("35".into())));
        // Every key survives a round trip.
        let original: serde_json::Map<String, Value> = serde_json::from_str(SAMPLE).unwrap();
        let again: serde_json::Map<String, Value> = serde_json::from_str(&s.to_json()).unwrap();
        for k in original.keys() {
            assert!(again.contains_key(k), "lost key {k}");
        }
        let (s2, _) = Settings::from_json(&s.to_json()).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn schema_mismatch_resets() {
        let (s, outcome) = Settings::from_json(r#"{"schemaVersion":11,"theme":"dark"}"#).unwrap();
        assert!(matches!(outcome, LoadOutcome::ResetSchema(Some(11))));
        assert_eq!(s, Settings::default());
        assert!(matches!(Settings::from_json("{}").unwrap().1, LoadOutcome::ResetSchema(None)));
    }

    #[test]
    fn type_errors_are_rejected() {
        assert!(Settings::from_json(r#"{"schemaVersion":12,"alwaysOnTop":"yes"}"#).is_err());
        assert!(Settings::from_json("[]").is_err());
    }

    #[test]
    fn missing_keys_take_defaults() {
        let (s, _) = Settings::from_json(r#"{"schemaVersion":12,"theme":"dark"}"#).unwrap();
        assert_eq!(s.theme, "dark");
        assert_eq!(s.graph_history_multiplier, 15);
    }

    #[test]
    fn save_and_load_file() {
        let dir = std::env::temp_dir().join(format!("tmop-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let s = Settings {
            update_speed: "high".into(),
            process_table_hidden_columns: vec!["handles".into()],
            ..Default::default()
        };
        s.save(&path).unwrap();
        let (loaded, outcome) = Settings::load(&path);
        assert!(matches!(outcome, LoadOutcome::Loaded));
        assert_eq!(loaded, s);
        std::fs::write(&path, "{ not json").unwrap();
        let (fallback, outcome) = Settings::load(&path);
        assert!(matches!(outcome, LoadOutcome::Corrupt(_)));
        assert_eq!(fallback, Settings::default());
        assert!(!path.exists(), "corrupt file should have been set aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_tmog_settings_parse_when_present() {
        let p = tmog_path();
        let Ok(text) = std::fs::read_to_string(&p) else { return };
        let (_, outcome) = Settings::from_json(&text).expect("TMOG settings should parse");
        assert!(matches!(outcome, LoadOutcome::Loaded | LoadOutcome::ResetSchema(_)));
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
