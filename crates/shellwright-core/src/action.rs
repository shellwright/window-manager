//! The [`Action`] enum — every command the window manager can execute.
//!
//! # Security (CWE-77 — Command Injection)
//! Keybinding `action` strings from `config.toml` are untrusted input.
//! They must be parsed through [`Action::from_str`] before execution.
//! Any string that does not map to a known variant is rejected with an error;
//! there is no "run arbitrary shell command" escape hatch at this layer.
//!
//! # Config syntax
//! | TOML `action` string          | Variant                              |
//! |-------------------------------|--------------------------------------|
//! | `"focus_next"`                | `FocusNext`                          |
//! | `"focus_prev"`                | `FocusPrev`                          |
//! | `"move_next"`                 | `MoveNext`                           |
//! | `"move_prev"`                 | `MovePrev`                           |
//! | `"kill_focused"`              | `KillFocused`                        |
//! | `"toggle_float"`              | `ToggleFloat`                        |
//! | `"toggle_fullscreen"          | `ToggleFullscreen`                   |
//! | `"set_layout:bsp"`            | `SetLayout(LayoutKind::Bsp)`         |
//! | `"set_layout:monocle"`        | `SetLayout(LayoutKind::Monocle)`     |
//! | `"set_layout:center_main"`    | `SetLayout(LayoutKind::CenterMain)`  |
//! | `"set_layout:float"`          | `SetLayout(LayoutKind::Float)`       |
//! | `"set_layout:columns:N"`      | `SetLayout(LayoutKind::Columns{N})`  |
//! | `"switch_workspace:N"`        | `SwitchWorkspace(N)`                 |
//! | `"move_to_workspace:N"`       | `MoveFocusedToWorkspace(N)`          |
//! | `"reload_config"`             | `ReloadConfig`                       |
//! | `"quit"`                      | `Quit`                               |

use std::str::FromStr;

use crate::{error::Error, layout::LayoutKind};

/// Every command the window manager can execute in response to a keybinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // ── Focus ─────────────────────────────────────────────────────────────────
    /// Move keyboard focus to the next window in the workspace.
    FocusNext,
    /// Move keyboard focus to the previous window in the workspace.
    FocusPrev,

    // ── Window manipulation ───────────────────────────────────────────────────
    /// Swap the focused window with the next one in the layout order.
    MoveNext,
    /// Swap the focused window with the previous one in the layout order.
    MovePrev,
    /// Close the focused window gracefully.
    KillFocused,
    /// Toggle the focused window between tiled and floating.
    ToggleFloat,
    /// Toggle a window to take the full screen real estate or go back into it's previous layout
    ToggleFullscreen,

    // ── Layout ───────────────────────────────────────────────────────────────
    /// Switch the active workspace to the given layout.
    SetLayout(LayoutKind),

    // ── Workspaces ───────────────────────────────────────────────────────────
    /// Activate workspace N (1-indexed, matches config workspace names).
    SwitchWorkspace(u8),
    /// Move the focused window to workspace N without switching to it.
    MoveFocusedToWorkspace(u8),

    // ── WM lifecycle ─────────────────────────────────────────────────────────
    /// Re-read `config.toml` and apply changes without restarting.
    ReloadConfig,
    /// Gracefully exit the window manager.
    Quit,
}

impl FromStr for Action {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on ':' to separate verb from arguments.
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        match parts.as_slice() {
            ["focus_next"]                  => Ok(Action::FocusNext),
            ["focus_prev"]                  => Ok(Action::FocusPrev),
            ["move_next"]                   => Ok(Action::MoveNext),
            ["move_prev"]                   => Ok(Action::MovePrev),
            ["kill_focused"]                => Ok(Action::KillFocused),
            ["toggle_float"]                => Ok(Action::ToggleFloat),
            ["toggle_fullscreen"]           => Ok(Action::ToggleFullscreen),
            ["reload_config"]               => Ok(Action::ReloadConfig),
            ["quit"]                        => Ok(Action::Quit),

            ["set_layout", "fibonacci"]     => Ok(Action::SetLayout(LayoutKind::Fibonacci)),
            ["set_layout", "bsp"]           => Ok(Action::SetLayout(LayoutKind::Bsp)),
            ["set_layout", "monocle"]       => Ok(Action::SetLayout(LayoutKind::Monocle)),
            ["set_layout", "center_main"]   => Ok(Action::SetLayout(LayoutKind::CenterMain)),
            ["set_layout", "float"]         => Ok(Action::SetLayout(LayoutKind::Float)),
            ["set_layout", "columns", n]    => {
                let count = n.parse::<u8>()
                    .map_err(|_| Error::Config(format!("invalid column count in action: {s}")))?;
                if count == 0 {
                    return Err(Error::Config("column count must be >= 1".into()));
                }
                Ok(Action::SetLayout(LayoutKind::Columns { count }))
            }

            ["switch_workspace", n]         => {
                let n = parse_workspace_index(n, s)?;
                Ok(Action::SwitchWorkspace(n))
            }
            ["move_to_workspace", n]        => {
                let n = parse_workspace_index(n, s)?;
                Ok(Action::MoveFocusedToWorkspace(n))
            }

            _ => Err(Error::Config(format!("unknown action: {s}"))),
        }
    }
}

fn parse_workspace_index(s: &str, original: &str) -> Result<u8, Error> {
    let n = s.parse::<u8>()
        .map_err(|_| Error::Config(format!("invalid workspace index in action: {original}")))?;
    if n == 0 {
        return Err(Error::Config("workspace index must be >= 1".into()));
    }
    Ok(n)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_actions() {
        assert_eq!("focus_next".parse::<Action>().unwrap(), Action::FocusNext);
        assert_eq!("kill_focused".parse::<Action>().unwrap(), Action::KillFocused);
        assert_eq!("quit".parse::<Action>().unwrap(), Action::Quit);
    }

    #[test]
    fn parses_set_layout() {
        assert_eq!(
            "set_layout:bsp".parse::<Action>().unwrap(),
            Action::SetLayout(LayoutKind::Bsp)
        );
        assert_eq!(
            "set_layout:columns:3".parse::<Action>().unwrap(),
            Action::SetLayout(LayoutKind::Columns { count: 3 })
        );
    }

    #[test]
    fn parses_workspace_actions() {
        assert_eq!("switch_workspace:1".parse::<Action>().unwrap(), Action::SwitchWorkspace(1));
        assert_eq!("move_to_workspace:9".parse::<Action>().unwrap(), Action::MoveFocusedToWorkspace(9));
    }

    #[test]
    fn rejects_unknown_action() {
        assert!("launch_terminal".parse::<Action>().is_err());
    }

    #[test]
    fn rejects_zero_workspace_index() {
        assert!("switch_workspace:0".parse::<Action>().is_err());
    }

    #[test]
    fn rejects_zero_column_count() {
        assert!("set_layout:columns:0".parse::<Action>().is_err());
    }
}
