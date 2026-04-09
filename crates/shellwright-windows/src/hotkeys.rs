//! Win32 global hotkey registration via `RegisterHotKey`.
//!
//! # How it works
//! `RegisterHotKey` posts `WM_HOTKEY` messages to the calling thread's message
//! queue whenever a registered combo is pressed system-wide.  The backend's
//! `GetMessage` loop picks them up and converts them to
//! [`shellwright_core::event::Event::Keybinding`].
//!
//! # Limitations
//! - `RegisterHotKey` cannot capture keys that another application has already
//!   exclusively registered (rare in practice).
//! - Hotkeys are per-thread; `HotkeyManager` must be created and used on the
//!   same thread as the message loop.

use shellwright_core::{
    error::Error,
    event::KeybindingId,
    hotkey::{BindingMap, Key, Modifiers},
    Result,
};

// ── Platform implementation ───────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    };

    pub struct HotkeyManager {
        registered_ids: Vec<i32>,
    }

    impl HotkeyManager {
        pub fn register(map: &BindingMap) -> Result<Self> {
            let mut registered_ids = Vec::new();

            for (id, combo, _action) in map.iter() {
                let vk = key_to_vk(&combo.key)?;
                let mods = mods_to_win32(combo.modifiers) | MOD_NOREPEAT;
                let win_id = id.0 as i32;

                let ok = unsafe { RegisterHotKey(None, win_id, mods, vk as u32) };
                if ok.is_err() {
                    // Unregister already-registered keys before returning error.
                    for prev in &registered_ids {
                        let _ = unsafe { UnregisterHotKey(None, *prev) };
                    }
                    return Err(Error::Backend(format!(
                        "RegisterHotKey failed for id={win_id} key={} (already claimed?)",
                        combo.key.0
                    )));
                }

                tracing::debug!(
                    id = win_id,
                    key = %combo.key.0,
                    "registered hotkey"
                );
                registered_ids.push(win_id);
            }

            Ok(Self { registered_ids })
        }

        /// Called by the message loop when `WM_HOTKEY` arrives.
        /// The `wparam` of `WM_HOTKEY` is the id passed to `RegisterHotKey`.
        pub fn resolve(wparam: usize) -> KeybindingId {
            KeybindingId(wparam as u32)
        }
    }

    impl Drop for HotkeyManager {
        fn drop(&mut self) {
            for id in &self.registered_ids {
                let _ = unsafe { UnregisterHotKey(None, *id) };
            }
        }
    }

    fn mods_to_win32(m: Modifiers) -> HOT_KEY_MODIFIERS {
        let mut out = HOT_KEY_MODIFIERS(0);
        if m.contains(Modifiers::ALT)   { out |= MOD_ALT; }
        if m.contains(Modifiers::CTRL)  { out |= MOD_CONTROL; }
        if m.contains(Modifiers::SHIFT) { out |= MOD_SHIFT; }
        if m.contains(Modifiers::SUPER) { out |= MOD_WIN; }
        out
    }

    /// Map a [`Key`] name to a Win32 virtual-key code (u16).
    ///
    /// Covers letters, digits, F-keys, navigation, and common symbols.
    /// Returns `Err` for keys not (yet) in the table.
    fn key_to_vk(key: &Key) -> Result<u16> {
        let s = key.0.as_str();

        // Single ASCII letter → VK_A..VK_Z (0x41–0x5A)
        if s.len() == 1 {
            let c = s.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                return Ok(c.to_ascii_uppercase() as u16);
            }
            // Single digit → VK_0..VK_9 (0x30–0x39)
            if c.is_ascii_digit() {
                return Ok(c as u16);
            }
        }

        // Function keys f1–f12 → 0x70–0x7B
        if let Some(rest) = s.strip_prefix('f') {
            if let Ok(n) = rest.parse::<u8>() {
                if (1..=12).contains(&n) {
                    return Ok(0x70 + (n - 1) as u16);
                }
            }
        }

        let vk: u16 = match s {
            "return" | "enter" => 0x0D,
            "space"            => 0x20,
            "tab"              => 0x09,
            "escape" | "esc"   => 0x1B,
            "backspace"        => 0x08,
            "delete"           => 0x2E,
            "up"               => 0x26,
            "down"             => 0x28,
            "left"             => 0x25,
            "right"            => 0x27,
            "home"             => 0x24,
            "end"              => 0x23,
            "pageup"           => 0x21,
            "pagedown"         => 0x22,
            "semicolon"        => 0xBA, // VK_OEM_1
            "plus"             => 0xBB, // VK_OEM_PLUS
            "comma"            => 0xBC, // VK_OEM_COMMA
            "minus"            => 0xBD, // VK_OEM_MINUS
            "period"           => 0xBE, // VK_OEM_PERIOD
            "slash"            => 0xBF, // VK_OEM_2
            "grave"            => 0xC0, // VK_OEM_3  (`/~)
            "bracketleft"      => 0xDB, // VK_OEM_4
            "backslash"        => 0xDC, // VK_OEM_5
            "bracketright"     => 0xDD, // VK_OEM_6
            "apostrophe"       => 0xDE, // VK_OEM_7
            other => return Err(Error::Config(format!("unsupported key name: {other}"))),
        };
        Ok(vk)
    }
}

// ── Stub for non-Windows builds (allows the crate to compile everywhere) ──────

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::*;

    pub struct HotkeyManager;

    impl HotkeyManager {
        pub fn register(_map: &BindingMap) -> Result<Self> {
            Ok(Self)
        }
        pub fn resolve(wparam: usize) -> KeybindingId {
            KeybindingId(wparam as u32)
        }
    }
}

pub use platform::HotkeyManager;
