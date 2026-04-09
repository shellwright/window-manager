//! Wayland input handling — keybinding detection via xkbcommon + calloop.
//!
//! # How it works
//! Since we are the compositor, all keyboard events arrive via libinput
//! (through Smithay's `InputBackend`).  For each `KeyAction::KeyPressed` we:
//!
//! 1. Ask xkbcommon for the active modifier state.
//! 2. Translate modifier state → [`Modifiers`] bitmask.
//! 3. Ask xkbcommon for the keysym of the pressed key.
//! 4. Translate keysym → portable [`Key`] name.
//! 5. Look up the resulting [`KeyCombo`] in the [`BindingMap`].
//! 6. If found, return `Some(Event::Keybinding(id))` and consume the event
//!    (don't pass it to the focused client).
//!
//! Unlike Windows/macOS, no separate registration step is needed — we always
//! see every key event and match it against the map at runtime.

use shellwright_core::{
    event::{Event, KeybindingId},
    hotkey::{BindingMap, Key, KeyCombo, Modifiers},
};

/// Check a key press against the binding map.
///
/// Returns `Some(Event::Keybinding(_))` if the combo is bound; `None` if the
/// event should be forwarded to the focused client.
///
/// # Arguments
/// * `map`       — compiled binding table built from config
/// * `modifiers` — current modifier state (caller resolves from xkb state)
/// * `key_name`  — portable key name resolved from the xkb keysym
pub fn check_binding(map: &BindingMap, modifiers: Modifiers, key_name: &str) -> Option<Event> {
    let combo = KeyCombo {
        modifiers,
        key: Key::new(key_name),
    };
    map.id_for_combo(&combo).map(Event::Keybinding)
}

/// Translate an xkbcommon modifier mask to our [`Modifiers`] bitmask.
///
/// The exact indices for Alt/Ctrl/Shift/Super depend on the keymap loaded by
/// xkbcommon.  Smithay exposes these via `KeyboardHandle::modifier_state()`.
///
/// `mod_indices` must be pre-fetched via `xkb::Keymap::mod_get_index`.
pub fn xkb_mods_to_modifiers(
    active: u32,
    idx_alt: u32,
    idx_ctrl: u32,
    idx_shift: u32,
    idx_super: u32,
) -> Modifiers {
    let mut m = Modifiers::empty();
    if active & (1 << idx_alt)   != 0 { m |= Modifiers::ALT; }
    if active & (1 << idx_ctrl)  != 0 { m |= Modifiers::CTRL; }
    if active & (1 << idx_shift) != 0 { m |= Modifiers::SHIFT; }
    if active & (1 << idx_super) != 0 { m |= Modifiers::SUPER; }
    m
}

/// Translate an xkb keysym (u32) to a portable [`Key`] name string.
///
/// Covers the same surface as the Windows VK table and macOS CGKeyCode table.
/// Unknown keysyms return `"unknown"`.
pub fn keysym_to_key_name(keysym: u32) -> &'static str {
    // xkb keysym values from <xkbcommon/xkbcommon-keysyms.h>
    match keysym {
        // Letters (XK_a – XK_z = 0x0061 – 0x007A)
        0x61 => "a", 0x62 => "b", 0x63 => "c", 0x64 => "d",
        0x65 => "e", 0x66 => "f", 0x67 => "g", 0x68 => "h",
        0x69 => "i", 0x6A => "j", 0x6B => "k", 0x6C => "l",
        0x6D => "m", 0x6E => "n", 0x6F => "o", 0x70 => "p",
        0x71 => "q", 0x72 => "r", 0x73 => "s", 0x74 => "t",
        0x75 => "u", 0x76 => "v", 0x77 => "w", 0x78 => "x",
        0x79 => "y", 0x7A => "z",
        // Digits (XK_0 – XK_9 = 0x0030 – 0x0039)
        0x30 => "0", 0x31 => "1", 0x32 => "2", 0x33 => "3",
        0x34 => "4", 0x35 => "5", 0x36 => "6", 0x37 => "7",
        0x38 => "8", 0x39 => "9",
        // Special
        0xFF0D => "return",    0x0020 => "space",      0xFF09 => "tab",
        0xFF1B => "escape",    0xFF08 => "backspace",   0xFFFF => "delete",
        0xFF52 => "up",        0xFF54 => "down",
        0xFF51 => "left",      0xFF53 => "right",
        0xFF50 => "home",      0xFF57 => "end",
        0xFF55 => "pageup",    0xFF56 => "pagedown",
        // F-keys (XK_F1 = 0xFFBE … XK_F12 = 0xFFC9)
        0xFFBE => "f1",  0xFFBF => "f2",  0xFFC0 => "f3",  0xFFC1 => "f4",
        0xFFC2 => "f5",  0xFFC3 => "f6",  0xFFC4 => "f7",  0xFFC5 => "f8",
        0xFFC6 => "f9",  0xFFC7 => "f10", 0xFFC8 => "f11", 0xFFC9 => "f12",
        // Punctuation
        0x003B => "semicolon",    0x002C => "comma",
        0x002E => "period",       0x002D => "minus",
        0x003D => "plus",         0x002F => "slash",
        0x0060 => "grave",        0x005B => "bracketleft",
        0x005D => "bracketright", 0x005C => "backslash",
        0x0027 => "apostrophe",
        _ => "unknown",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shellwright_core::hotkey::BindingMap;
    use shellwright_core::config::Keybinding;

    fn map_with(modifiers: &[&str], key: &str, action: &str) -> BindingMap {
        BindingMap::from_config(&[Keybinding {
            modifiers: modifiers.iter().map(|s| s.to_string()).collect(),
            key: key.into(),
            action: action.into(),
        }])
        .unwrap()
    }

    #[test]
    fn matched_combo_returns_keybinding_event() {
        let map = map_with(&["super"], "q", "kill_focused");
        let event = check_binding(&map, Modifiers::SUPER, "q");
        assert!(matches!(event, Some(shellwright_core::event::Event::Keybinding(KeybindingId(0)))));
    }

    #[test]
    fn unmatched_combo_returns_none() {
        let map = map_with(&["super"], "q", "kill_focused");
        // Wrong modifier — should not match.
        let event = check_binding(&map, Modifiers::CTRL, "q");
        assert!(event.is_none());
    }

    #[test]
    fn keysym_letter_roundtrip() {
        assert_eq!(keysym_to_key_name(0x61), "a");
        assert_eq!(keysym_to_key_name(0x7A), "z");
    }

    #[test]
    fn keysym_return() {
        assert_eq!(keysym_to_key_name(0xFF0D), "return");
    }
}
