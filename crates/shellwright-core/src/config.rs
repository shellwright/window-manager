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

/// Screen-edge padding in physical pixels.
///
/// Use this to reserve space for external status bars (e.g. YASB) that do not
/// register themselves with `SPI_SETWORKAREA`.  Shellwright subtracts these
/// values from the monitor work-area before computing tile slots.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

/// Animation settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationConfig {
    /// Master switch — `false` disables all motion effects regardless of the
    /// Windows system animation preference.
    pub enabled: bool,
    /// Total animation duration in milliseconds.  Applies to both workspace
    /// fades and window-move interpolation.
    pub duration_ms: u32,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self { enabled: true, duration_ms: 200 }
    }
}

/// A rule that causes matching windows to start in floating (non-tiled) mode.
///
/// All specified fields must match (AND logic); omitted fields are wildcards.
///
/// # Examples (config.toml)
/// ```toml
/// [[float_rules]]
/// exe = "steam.exe"
///
/// [[float_rules]]
/// class = "TaskManagerWindow"
///
/// [[float_rules]]
/// title_contains = "Properties"
/// exe = "explorer.exe"
/// ```
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct FloatRule {
    /// Exact window class name (case-insensitive), e.g. `"#32770"`.
    pub class: Option<String>,
    /// Title must contain this substring (case-insensitive).
    pub title_contains: Option<String>,
    /// Process executable filename (case-insensitive), e.g. `"steam.exe"`.
    pub exe: Option<String>,
}

impl FloatRule {
    /// Returns `true` if this rule matches the given window attributes.
    pub fn matches(&self, class: &str, title: &str, exe: &str) -> bool {
        self.class.as_deref().map_or(true, |c| class.eq_ignore_ascii_case(c))
            && self.title_contains.as_deref().map_or(true, |t| {
                title.to_lowercase().contains(&t.to_lowercase())
            })
            && self.exe.as_deref().map_or(true, |e| exe.eq_ignore_ascii_case(e))
    }
}

/// Top-level configuration loaded from `config.toml`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Gap between tiled windows in physical pixels.
    pub gap: u32,
    /// Border width in physical pixels (used by compositor / overlay backends).
    pub border_width: u32,
    /// Border colour for the focused window (hex `#RRGGBB`).
    /// On Windows 11 this is applied via `DwmSetWindowAttribute(DWMWA_BORDER_COLOR)`.
    pub border_active: String,
    /// Border colour for all unfocused windows (hex `#RRGGBB`).
    pub border_inactive: String,
    /// Border corner radius in physical pixels (0 = square corners).
    pub border_radius: u32,
    /// Screen-edge padding to reserve for external bars (e.g. YASB).
    pub padding: Padding,
    pub workspaces: Vec<WorkspaceConfig>,
    pub keybindings: Vec<Keybinding>,
    pub default_layout: LayoutKind,
    /// Animation configuration (fades, window-move easing).
    pub animations: AnimationConfig,
    /// Windows matching any of these rules start in floating (non-tiled) mode.
    ///
    /// In addition, Shellwright automatically floats windows that Win32 marks as
    /// dialogs: the `#32770` system dialog class and any fixed-size window
    /// (caption present, no resize handle, no maximise button) are floated
    /// without needing a rule here.
    pub float_rules: Vec<FloatRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            border_width: 5,
            border_active:   "#5E81AC".into(), // Nord blue
            border_inactive: "#3B4252".into(), // Nord dark
            border_radius:   8,
            padding: Padding::default(),
            workspaces: (1..=9).map(|i| WorkspaceConfig { name: i.to_string() }).collect(),
            keybindings: default_keybindings(),
            default_layout: LayoutKind::default(),
            animations: AnimationConfig::default(),
            float_rules: Vec::new(),
        }
    }
}

/// Built-in keybindings used when no config file is present.
///
/// Mirrors the defaults documented in `config.toml`.  All use the Super (Win)
/// key as the primary modifier, matching the komorebi / i3 convention.
fn default_keybindings() -> Vec<Keybinding> {
    fn kb(modifiers: &[&str], key: &str, action: &str) -> Keybinding {
        Keybinding {
            modifiers: modifiers.iter().map(|s| s.to_string()).collect(),
            key: key.into(),
            action: action.into(),
        }
    }
    vec![
        // Focus
        kb(&["alt"],         "h", "focus_prev"),
        kb(&["alt"],         "l", "focus_next"),
        // Move window in layout order
        kb(&["alt", "shift"], "h", "move_prev"),
        kb(&["alt", "shift"], "l", "move_next"),
        // Close / float / fullscreen
        kb(&["alt", "shift"], "q",     "kill_focused"),
        kb(&["alt"],          "f",     "toggle_fullscreen"),
        kb(&["alt", "shift"], "space", "toggle_float"),
        // Layouts
        kb(&["alt"], "g", "set_layout:fibonacci"),
        kb(&["alt"], "t", "set_layout:bsp"),
        kb(&["alt"], "m", "set_layout:monocle"),
        kb(&["alt"], "c", "set_layout:columns:2"),
        // Workspace switch (Alt+1..9)
        kb(&["alt"], "1", "switch_workspace:1"),
        kb(&["alt"], "2", "switch_workspace:2"),
        kb(&["alt"], "3", "switch_workspace:3"),
        kb(&["alt"], "4", "switch_workspace:4"),
        kb(&["alt"], "5", "switch_workspace:5"),
        kb(&["alt"], "6", "switch_workspace:6"),
        kb(&["alt"], "7", "switch_workspace:7"),
        kb(&["alt"], "8", "switch_workspace:8"),
        kb(&["alt"], "9", "switch_workspace:9"),
        // Move to workspace (Alt+Shift+1..9)
        kb(&["alt", "shift"], "1", "move_to_workspace:1"),
        kb(&["alt", "shift"], "2", "move_to_workspace:2"),
        kb(&["alt", "shift"], "3", "move_to_workspace:3"),
        kb(&["alt", "shift"], "4", "move_to_workspace:4"),
        kb(&["alt", "shift"], "5", "move_to_workspace:5"),
        kb(&["alt", "shift"], "6", "move_to_workspace:6"),
        kb(&["alt", "shift"], "7", "move_to_workspace:7"),
        kb(&["alt", "shift"], "8", "move_to_workspace:8"),
        kb(&["alt", "shift"], "9", "move_to_workspace:9"),
        // WM lifecycle
        kb(&["alt", "shift"], "r", "reload_config"),
        kb(&["alt", "shift"], "e", "quit"),
    ]
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
    fn default_layout_is_fibonacci() {
        assert_eq!(Config::default().default_layout, LayoutKind::Fibonacci);
    }

    #[test]
    fn load_nonexistent_file_returns_error() {
        let result = Config::load(Path::new("/does/not/exist.toml"));
        assert!(result.is_err());
    }
}
