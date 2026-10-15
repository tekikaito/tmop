//! TMOP's own Docker engine: a WSL2 distro with dockerd, managed from the Docker page and reached through the `--engine-relay` named-pipe relay.

pub mod config;

use std::path::PathBuf;

/// WSL distro name.
pub const DEFAULT_DISTRO: &str = "tmop-engine";
/// Named pipe name (`\\.\pipe\<name>`).
pub const DEFAULT_PIPE: &str = "tmop_engine";
/// Docker CLI context name.
pub const DEFAULT_CONTEXT: &str = "tmop";
/// GitHub repository that publishes the engine tarballs.
pub const RELEASE_REPO: &str = "tekikaito/tmop";

/// Test-only overrides read from the environment (see the module doc of each field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overrides {
    /// `TMOP_ENGINE_DISTRO`, default [`DEFAULT_DISTRO`].
    pub distro: String,
    /// `TMOP_ENGINE_PIPE`, default [`DEFAULT_PIPE`].
    pub pipe: String,
    /// `TMOP_ENGINE_CONTEXT`, default [`DEFAULT_CONTEXT`].
    pub context: String,
    /// `TMOP_ENGINE_DATA_DIR`: replaces the data dir, including the settings value.
    pub data_dir: Option<PathBuf>,
    /// `TMOP_ENGINE_TARBALL`: local `.tar.gz` (or `.tar`) used instead of downloading.
    pub tarball: Option<PathBuf>,
    /// `TMOP_ENGINE_BIN_DIR`: install dir for the TMOP CLI shims.
    pub bin_dir: Option<PathBuf>,
}

impl Overrides {
    /// Reads the overrides from the process environment.
    pub fn from_env() -> Self {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    /// Builds the overrides from any lookup; empty values count as unset.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let text = |k: &str| get(k).filter(|v| !v.is_empty());
        let path = |k: &str| text(k).map(PathBuf::from);
        Self {
            distro: text("TMOP_ENGINE_DISTRO").unwrap_or_else(|| DEFAULT_DISTRO.into()),
            pipe: text("TMOP_ENGINE_PIPE").unwrap_or_else(|| DEFAULT_PIPE.into()),
            context: text("TMOP_ENGINE_CONTEXT").unwrap_or_else(|| DEFAULT_CONTEXT.into()),
            data_dir: path("TMOP_ENGINE_DATA_DIR"),
            tarball: path("TMOP_ENGINE_TARBALL"),
            bin_dir: path("TMOP_ENGINE_BIN_DIR"),
        }
    }

    /// Full named pipe path, `\\.\pipe\<pipe>`.
    pub fn pipe_path(&self) -> String {
        format!(r"\\.\pipe\{}", self.pipe)
    }

    /// Engine data dir: the env override, else `settings_value` when non-empty, else [`default_data_dir`].
    pub fn data_dir(&self, settings_value: &str) -> PathBuf {
        if let Some(dir) = &self.data_dir {
            return dir.clone();
        }
        if settings_value.is_empty() {
            default_data_dir()
        } else {
            PathBuf::from(settings_value)
        }
    }

    /// CLI install dir: the env override, else [`default_bin_dir`].
    pub fn bin_dir(&self) -> PathBuf {
        self.bin_dir.clone().unwrap_or_else(default_bin_dir)
    }
}

/// `%LOCALAPPDATA%\TMOP\engine`
pub fn default_data_dir() -> PathBuf {
    crate::pro::data_dir().join("engine")
}

/// `%LOCALAPPDATA%\TMOP\bin`
pub fn default_bin_dir() -> PathBuf {
    crate::pro::data_dir().join("bin")
}

/// Release tag for this build, e.g. `v0.1.0`.
pub fn version_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Asset architecture suffix for this build.
pub fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn from_vars_defaults_when_unset() {
        let o = Overrides::from_vars(vars(&[]));
        assert_eq!(o.distro, DEFAULT_DISTRO);
        assert_eq!(o.pipe, DEFAULT_PIPE);
        assert_eq!(o.context, DEFAULT_CONTEXT);
        assert_eq!(o.data_dir, None);
        assert_eq!(o.tarball, None);
        assert_eq!(o.bin_dir, None);
    }

    #[test]
    fn from_vars_reads_values_and_treats_empty_as_unset() {
        let o = Overrides::from_vars(vars(&[
            ("TMOP_ENGINE_DISTRO", "tmop-engine-test"),
            ("TMOP_ENGINE_PIPE", "tmop_engine_test"),
            ("TMOP_ENGINE_CONTEXT", "tmop-test"),
            ("TMOP_ENGINE_DATA_DIR", r"C:\tmp\data"),
            ("TMOP_ENGINE_TARBALL", r"C:\tmp\engine.tar.gz"),
            ("TMOP_ENGINE_BIN_DIR", ""),
        ]));
        assert_eq!(o.distro, "tmop-engine-test");
        assert_eq!(o.pipe, "tmop_engine_test");
        assert_eq!(o.context, "tmop-test");
        assert_eq!(o.data_dir, Some(PathBuf::from(r"C:\tmp\data")));
        assert_eq!(o.tarball, Some(PathBuf::from(r"C:\tmp\engine.tar.gz")));
        assert_eq!(o.bin_dir, None);

        let empty = Overrides::from_vars(vars(&[("TMOP_ENGINE_DISTRO", ""), ("TMOP_ENGINE_PIPE", "")]));
        assert_eq!(empty.distro, DEFAULT_DISTRO);
        assert_eq!(empty.pipe, DEFAULT_PIPE);
    }

    #[test]
    fn pipe_path_uses_pipe_namespace() {
        let o = Overrides::from_vars(vars(&[("TMOP_ENGINE_PIPE", "tmop_engine_test")]));
        assert_eq!(o.pipe_path(), r"\\.\pipe\tmop_engine_test");
        assert_eq!(Overrides::from_vars(vars(&[])).pipe_path(), r"\\.\pipe\tmop_engine");
    }

    #[test]
    fn data_dir_precedence() {
        let plain = Overrides::from_vars(vars(&[]));
        assert_eq!(plain.data_dir("D:\\engine"), PathBuf::from("D:\\engine"));
        assert_eq!(plain.data_dir(""), default_data_dir());

        let env = Overrides::from_vars(vars(&[("TMOP_ENGINE_DATA_DIR", "E:\\override")]));
        assert_eq!(env.data_dir("D:\\engine"), PathBuf::from("E:\\override"));
        assert_eq!(env.data_dir(""), PathBuf::from("E:\\override"));
    }

    #[test]
    fn bin_dir_precedence() {
        assert_eq!(Overrides::from_vars(vars(&[])).bin_dir(), default_bin_dir());
        let env = Overrides::from_vars(vars(&[("TMOP_ENGINE_BIN_DIR", "F:\\bin")]));
        assert_eq!(env.bin_dir(), PathBuf::from("F:\\bin"));
    }

    #[test]
    fn version_tag_starts_with_v() {
        assert!(version_tag().starts_with('v'));
        assert_eq!(version_tag(), format!("v{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn arch_is_x64_or_arm64() {
        assert!(matches!(arch(), "x64" | "arm64"));
    }
}
