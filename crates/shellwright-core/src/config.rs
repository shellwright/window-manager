//! TOML-based user configuration.
//!
//! # Security (CWE-20 — Improper Input Validation)
//! Config is loaded from a user-writable file and must be treated as untrusted
//! input.  The `toml` parser provides type-safe deserialisation; values that
//! reach the backend (e.g. keybinding `action` strings) must be validated
//! against an allowlist before execution to prevent command injection (CWE-77).

use crate::layout::LayoutKind;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Top-level configuration loaded from `config.toml`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Gap between tiled windows in physical pixels.
    pub gap: u32,
    /// Border width in physical pixels.
    pub border_width: u32,
    pub workspaces: Vec<WorkspaceConfig>,
    pub keybindings: Vec<Keybinding>,
    pub default_layout: LayoutKind,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            border_width: 2,
            workspaces: (1..=9).map(|i| WorkspaceConfig { name: i.to_string() }).collect(),
            keybindings: vec![],
            default_layout: LayoutKind::default(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub name: String,
}

/// A user keybinding declaration.
///
/// `action` must be validated against a known-good list before execution.
#[derive(Debug, Serialize, Deserialize)]
pub struct Keybinding {
    pub modifiers: Vec<String>,
    pub key: String,
    /// Named action string, e.g. `"focus_next"`, `"kill_focused"`.
    pub action: String,
}

impl Config {
    /// Load and deserialise a TOML config file.
    ///
    /// # Errors
    /// Returns [`crate::Error::Config`] if the file cannot be read or if the
    /// TOML is malformed.
    pub fn load(path: &Path) -> crate::Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(crate::Error::Io)?;
        toml::from_str(&raw).map_err(|e| crate::Error::Config(e.to_string()))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_nine_workspaces() {
        assert_eq!(Config::default().workspaces.len(), 9);
    }

    #[test]
    fn default_layout_is_bsp() {
        assert_eq!(Config::default().default_layout, LayoutKind::Bsp);
    }

    #[test]
    fn load_nonexistent_file_returns_error() {
        let result = Config::load(Path::new("/does/not/exist.toml"));
        assert!(result.is_err());
    }
}
