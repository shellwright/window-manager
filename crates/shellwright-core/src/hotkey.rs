//! Cross-platform keybinding primitives.
//!
//! [`BindingMap`] is built once from [`crate::config::Config`] at startup and
//! passed to each platform backend so it can register the hotkeys with the OS.
//! The event loop then resolves incoming [`crate::event::KeybindingId`]s back
//! to [`crate::action::Action`]s via [`BindingMap::action`].

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{
    action::Action,
    config::Keybinding,
    error::Error,
    event::KeybindingId,
    Result,
};

// ── Modifiers ─────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    /// Platform-agnostic modifier key bitmask.
    ///
    /// Each backend maps these flags to its own modifier constants:
    /// - Windows: `MOD_ALT | MOD_CONTROL | MOD_SHIFT | MOD_WIN`
    /// - macOS:   `kCGEventFlagMaskAlternate | …Command | …Control | …Shift`
    /// - Wayland: `xkb::ModMask` via xkbcommon
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Modifiers: u8 {
        const ALT   = 0b0001;
        const CTRL  = 0b0010;
        const SHIFT = 0b0100;
        /// Windows key / Cmd key / Super key.
        const SUPER = 0b1000;
    }
}

impl Modifiers {
    /// Parse a slice of modifier name strings (case-insensitive).
    ///
    /// Accepted names: `"alt"`, `"ctrl"` / `"control"`, `"shift"`,
    /// `"super"` / `"win"` / `"cmd"` / `"meta"`.
    pub fn from_strs(names: &[String]) -> Result<Self> {
        let mut mods = Modifiers::empty();
        for name in names {
            match name.to_ascii_lowercase().as_str() {
                "alt"                         => mods |= Modifiers::ALT,
                "ctrl" | "control"            => mods |= Modifiers::CTRL,
                "shift"                       => mods |= Modifiers::SHIFT,
                "super" | "win" | "cmd" | "meta" => mods |= Modifiers::SUPER,
                other => return Err(Error::Config(format!("unknown modifier: {other}"))),
            }
        }
        Ok(mods)
    }
}

// ── Key ───────────────────────────────────────────────────────────────────────

/// A platform-agnostic key name.
///
/// Stored as a lowercase string.  Each backend is responsible for translating
/// this to its own key representation (VK code, CGKeyCode, xkb keysym, etc.).
///
/// # Accepted names (non-exhaustive)
/// Letters: `"a"` – `"z"` · Numbers: `"0"` – `"9"` ·
/// Function: `"f1"` – `"f12"` ·
/// Special: `"return"`, `"space"`, `"tab"`, `"escape"`,
///          `"backspace"`, `"delete"`, `"up"`, `"down"`, `"left"`, `"right"`,
///          `"home"`, `"end"`, `"pageup"`, `"pagedown"`,
///          `"semicolon"`, `"comma"`, `"period"`, `"minus"`, `"plus"`,
///          `"slash"`, `"backslash"`, `"grave"`,
///          `"bracketleft"`, `"bracketright"`, `"apostrophe"`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key(pub String);

impl Key {
    pub fn new(s: &str) -> Self {
        Self(s.to_ascii_lowercase())
    }
}

impl FromStr for Key {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let k = s.to_ascii_lowercase();
        if k.is_empty() {
            return Err(Error::Config("key name cannot be empty".into()));
        }
        Ok(Key(k))
    }
}

// ── KeyCombo ──────────────────────────────────────────────────────────────────

/// A fully-qualified hotkey: modifier set + key name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub modifiers: Modifiers,
    pub key: Key,
}

// ── BindingMap ────────────────────────────────────────────────────────────────

/// Compiled, ready-to-register keybinding table.
///
/// Built once from [`crate::config::Config::keybindings`] at startup; then
/// handed to each platform backend so it can register hotkeys with the OS.
pub struct BindingMap {
    bindings: Vec<(KeybindingId, KeyCombo, Action)>,
}

impl BindingMap {
    /// Parse and validate all keybindings from the raw config.
    ///
    /// # Errors
    /// Returns the first [`Error::Config`] encountered (unknown modifier,
    /// empty key, unknown action string).
    pub fn from_config(keybindings: &[Keybinding]) -> Result<Self> {
        let mut bindings = Vec::with_capacity(keybindings.len());

        for (i, kb) in keybindings.iter().enumerate() {
            let id = KeybindingId(i as u32);

            let modifiers = Modifiers::from_strs(&kb.modifiers)
                .map_err(|e| Error::Config(format!("keybinding [{i}]: {e}")))?;

            let key = kb.key.parse::<Key>()
                .map_err(|e| Error::Config(format!("keybinding [{i}]: {e}")))?;

            let action = kb.action.parse::<Action>()
                .map_err(|e| Error::Config(format!("keybinding [{i}]: {e}")))?;

            bindings.push((id, KeyCombo { modifiers, key }, action));
        }

        Ok(Self { bindings })
    }

    /// Iterate over all registered bindings.
    pub fn iter(&self) -> impl Iterator<Item = (KeybindingId, &KeyCombo, &Action)> {
        self.bindings.iter().map(|(id, combo, action)| (*id, combo, action))
    }

    /// Look up the [`Action`] for a fired [`KeybindingId`].
    pub fn action(&self, id: KeybindingId) -> Option<&Action> {
        self.bindings
            .get(id.0 as usize)
            .map(|(_, _, action)| action)
    }

    /// Look up the [`KeybindingId`] for a given [`KeyCombo`] (used by backends
    /// that match combos at event time rather than at registration time).
    pub fn id_for_combo(&self, combo: &KeyCombo) -> Option<KeybindingId> {
        self.bindings
            .iter()
            .find(|(_, c, _)| c == combo)
            .map(|(id, _, _)| *id)
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Keybinding;

    fn kb(modifiers: &[&str], key: &str, action: &str) -> Keybinding {
        Keybinding {
            modifiers: modifiers.iter().map(|s| s.to_string()).collect(),
            key: key.into(),
            action: action.into(),
        }
    }

    #[test]
    fn modifiers_parse_aliases() {
        let m = Modifiers::from_strs(&["win".into(), "shift".into()]).unwrap();
        assert!(m.contains(Modifiers::SUPER));
        assert!(m.contains(Modifiers::SHIFT));
        assert!(!m.contains(Modifiers::ALT));
    }

    #[test]
    fn modifier_unknown_is_error() {
        assert!(Modifiers::from_strs(&["hyper".into()]).is_err());
    }

    #[test]
    fn binding_map_roundtrip() {
        let bindings = vec![
            kb(&["super"], "q", "kill_focused"),
            kb(&["super", "shift"], "1", "switch_workspace:1"),
        ];
        let map = BindingMap::from_config(&bindings).unwrap();
        assert_eq!(map.len(), 2);

        let id = KeybindingId(0);
        assert_eq!(map.action(id), Some(&crate::action::Action::KillFocused));
    }

    #[test]
    fn binding_map_id_for_combo() {
        let bindings = vec![kb(&["super"], "return", "focus_next")];
        let map = BindingMap::from_config(&bindings).unwrap();

        let combo = KeyCombo {
            modifiers: Modifiers::SUPER,
            key: Key::new("return"),
        };
        assert_eq!(map.id_for_combo(&combo), Some(KeybindingId(0)));
    }

    #[test]
    fn binding_map_rejects_invalid_action() {
        let bindings = vec![kb(&["super"], "q", "launch_browser")];
        assert!(BindingMap::from_config(&bindings).is_err());
    }
}
