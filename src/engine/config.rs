//! `engine.json` in the engine data dir: written by the manager, read by the relay.
//!
//! Unknown keys are kept verbatim so newer versions can add fields without losing them.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// File name inside the data dir.
pub const FILE_NAME: &str = "engine.json";

/// Resource limits for the engine distro; 0 means unlimited.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub cpus: u32,
    pub memory_mb: u32,
}

/// Contents of `engine.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineConfig {
    pub schema_version: u32,
    pub engine_version: String,
    pub arch: String,
    pub distro: String,
    /// Unix seconds.
    pub installed_at: u64,
    /// When true the relay binds no LAN port forwarders.
    pub ports_local_only: bool,
    pub limits: Limits,
    /// Whether the TMOP CLI is the default `docker` command.
    pub default_docker: bool,
    pub autostart: bool,
    /// Docker context that was active before "Use TMOP as default Docker", restored on undo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_context: Option<String>,
    /// The inbound firewall rule for the published ports was added (by the user's UAC approval);
    /// [`remove`](crate::engine::manager) deletes it again only when this is set.
    #[serde(default)]
    pub firewall_rule: bool,
    /// Any other keys are preserved verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for EngineConfig {
    /// Schema 1, nothing installed.
    fn default() -> Self {
        Self {
            schema_version: 1,
            engine_version: String::new(),
            arch: String::new(),
            distro: String::new(),
            installed_at: 0,
            ports_local_only: false,
            limits: Limits::default(),
            default_docker: false,
            autostart: false,
            previous_context: None,
            firewall_rule: false,
            extra: BTreeMap::new(),
        }
    }
}

impl EngineConfig {
    /// A fresh config for an engine installed now, with all options off.
    pub fn new(version: &str, arch: &str, distro: &str) -> Self {
        let installed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            engine_version: version.into(),
            arch: arch.into(),
            distro: distro.into(),
            installed_at,
            ..Default::default()
        }
    }

    /// Reads `<data_dir>\engine.json`; `None` if missing or unreadable/unparsable.
    pub fn load(data_dir: &Path) -> Option<EngineConfig> {
        let text = std::fs::read_to_string(data_dir.join(FILE_NAME)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Atomic save: write `engine.json.tmp.<pid>` then rename over the target.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(FILE_NAME);
        let tmp = data_dir.join(format!("{FILE_NAME}.tmp.{}", std::process::id()));
        let text = serde_json::to_string_pretty(self).expect("engine config serialize");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tmop-engine-cfg-{}-{tag}", std::process::id()))
    }

    #[test]
    fn roundtrip_through_temp_dir() {
        let dir = temp_dir("roundtrip");
        let mut cfg = EngineConfig::new("v0.1.0", "x64", "tmop-engine");
        cfg.limits = Limits { cpus: 4, memory_mb: 8192 };
        cfg.ports_local_only = true;
        cfg.previous_context = Some("desktop-linux".into());
        cfg.save(&dir).unwrap();
        assert!(!dir.join(format!("{FILE_NAME}.tmp.{}", std::process::id())).exists());
        let back = EngineConfig::load(&dir).expect("config loads");
        assert_eq!(back, cfg);
        assert_eq!(back.schema_version, 1);
        assert!(back.installed_at > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn firewall_flag_defaults_to_false_when_missing() {
        let dir = temp_dir("firewall");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), r#"{"schemaVersion":1,"engineVersion":"v0.1.0"}"#).unwrap();
        assert!(!EngineConfig::load(&dir).expect("config loads").firewall_rule);
        let mut cfg = EngineConfig::load(&dir).unwrap();
        cfg.firewall_rule = true;
        cfg.save(&dir).unwrap();
        assert!(EngineConfig::load(&dir).unwrap().firewall_rule);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_camel_case_keys() {
        let dir = temp_dir("keys");
        EngineConfig::new("v0.1.0", "arm64", "tmop-engine").save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        for key in ["schemaVersion", "engineVersion", "installedAt", "portsLocalOnly", "memoryMb", "defaultDocker", "firewallRule"] {
            assert!(text.contains(&format!("\"{key}\"")), "missing {key}");
        }
        assert!(!text.contains("previousContext"), "absent option is not written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_keys_are_preserved() {
        let dir = temp_dir("unknown");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{"schemaVersion":1,"engineVersion":"v0.2.0","futureKey":{"a":[1,2]},"autostart":true}"#,
        )
        .unwrap();
        let cfg = EngineConfig::load(&dir).expect("config loads");
        assert!(cfg.autostart);
        assert_eq!(cfg.extra.get("futureKey"), Some(&serde_json::json!({"a": [1, 2]})));
        cfg.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["futureKey"], serde_json::json!({"a": [1, 2]}));
        assert_eq!(v["engineVersion"], "v0.2.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_corrupt_file_is_none() {
        let dir = temp_dir("missing");
        assert!(EngineConfig::load(&dir).is_none());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        assert!(EngineConfig::load(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
