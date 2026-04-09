//! macOS global hotkey capture via `CGEventTap`.
//!
//! # How it works
//! A `CGEventTap` intercepts `kCGEventKeyDown` events system-wide before they
//! reach any application.  In the callback we compare the pressed key + modifier
//! flags against the [`BindingMap`] and — if matched — suppress the event and
//! send a [`KeybindingId`] over a channel to the backend's event loop.
//!
//! # Requirements
//! The process must hold "Accessibility" permission (same permission already
//! required for `AXUIElement` window manipulation).
//!
//! # Thread model
//! The tap callback runs on a dedicated `CFRunLoop`.  A `std::sync::mpsc`
//! channel bridges it to the backend's blocking `next_event` call.

use shellwright_core::{event::KeybindingId, hotkey::BindingMap, Result};

// ── Platform implementation ───────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::mpsc;
    use super::*;
    use shellwright_core::{error::Error, hotkey::{Key, KeyCombo, Modifiers}};

    pub struct HotkeyManager {
        pub receiver: mpsc::Receiver<KeybindingId>,
        // The CFMachPort / CFRunLoopSource must stay alive for the tap to fire.
        // Held as raw pointers; dropped via invalidate on Drop.
        // TODO: store core_graphics::event::CGEventTap handle here.
    }

    impl HotkeyManager {
        pub fn register(map: &BindingMap) -> Result<Self> {
            if map.is_empty() {
                let (_tx, rx) = mpsc::channel();
                return Ok(Self { receiver: rx });
            }

            // Clone the binding map into a form the callback closure can own.
            let owned: Vec<(KeybindingId, KeyCombo)> = map
                .iter()
                .map(|(id, combo, _)| (id, combo.clone()))
                .collect();

            let (tx, rx) = mpsc::channel::<KeybindingId>();

            // TODO: CGEventTapCreate(
            //   kCGSessionEventTap,
            //   kCGHeadInsertEventTap,
            //   kCGEventTapOptionDefault,
            //   CGEventMaskBit(kCGEventKeyDown),
            //   |_proxy, _type, event, _| {
            //       let keycode  = event.get_integer_value_field(kCGKeyboardEventKeycode);
            //       let flags    = event.flags();
            //       let combo    = KeyCombo {
            //           modifiers: cg_flags_to_modifiers(flags),
            //           key:       cgkeycode_to_key(keycode),
            //       };
            //       if let Some((id, _)) = owned.iter().find(|(_, c)| c == &combo) {
            //           let _ = tx.send(*id);
            //           return None;  // suppress the event
            //       }
            //       Some(event)
            //   },
            // )
            //
            // TODO: wrap port in CFRunLoopSource, add to a new thread's CFRunLoop,
            //       CFRunLoopRun() on that thread.

            tracing::warn!("CGEventTap not yet implemented — hotkeys inactive on macOS");
            let _ = (owned, tx); // silence unused warnings until implemented
            Ok(Self { receiver: rx })
        }
    }

    /// Convert CGEventFlags to our [`Modifiers`] bitmask.
    #[allow(dead_code)]
    fn cg_flags_to_modifiers(flags: u64) -> Modifiers {
        // kCGEventFlagMaskAlternate  = 0x00080000
        // kCGEventFlagMaskControl    = 0x00040000
        // kCGEventFlagMaskShift      = 0x00020000
        // kCGEventFlagMaskCommand    = 0x00100000
        let mut m = Modifiers::empty();
        if flags & 0x0008_0000 != 0 { m |= Modifiers::ALT; }
        if flags & 0x0004_0000 != 0 { m |= Modifiers::CTRL; }
        if flags & 0x0002_0000 != 0 { m |= Modifiers::SHIFT; }
        if flags & 0x0010_0000 != 0 { m |= Modifiers::SUPER; }
        m
    }

    /// Map a CGKeyCode (u16) to a portable [`Key`] name.
    ///
    /// Reference: HIToolbox/Events.h  (kVK_* constants)
    #[allow(dead_code)]
    fn cgkeycode_to_key(code: i64) -> Key {
        let name = match code as u16 {
            // Letters
            0x00 => "a", 0x0B => "b", 0x08 => "c", 0x02 => "d",
            0x0E => "e", 0x03 => "f", 0x05 => "g", 0x04 => "h",
            0x22 => "i", 0x26 => "j", 0x28 => "k", 0x25 => "l",
            0x2E => "m", 0x2D => "n", 0x1F => "o", 0x23 => "p",
            0x0C => "q", 0x0F => "r", 0x01 => "s", 0x11 => "t",
            0x20 => "u", 0x09 => "v", 0x0D => "w", 0x07 => "x",
            0x10 => "y", 0x06 => "z",
            // Digits
            0x1D => "0", 0x12 => "1", 0x13 => "2", 0x14 => "3",
            0x15 => "4", 0x17 => "5", 0x16 => "6", 0x1A => "7",
            0x1C => "8", 0x19 => "9",
            // Special
            0x24 => "return",    0x31 => "space",   0x30 => "tab",
            0x35 => "escape",    0x33 => "backspace", 0x75 => "delete",
            0x7E => "up",        0x7D => "down",
            0x7B => "left",      0x7C => "right",
            0x73 => "home",      0x77 => "end",
            0x74 => "pageup",    0x79 => "pagedown",
            // F-keys
            0x7A => "f1",  0x78 => "f2",  0x63 => "f3",  0x76 => "f4",
            0x60 => "f5",  0x61 => "f6",  0x62 => "f7",  0x64 => "f8",
            0x65 => "f9",  0x6D => "f10", 0x67 => "f11", 0x6F => "f12",
            // Punctuation
            0x29 => "semicolon", 0x2B => "comma",   0x2F => "period",
            0x1B => "minus",     0x18 => "plus",    0x2C => "slash",
            0x32 => "grave",     0x21 => "bracketleft", 0x1E => "bracketright",
            0x2A => "backslash", 0x27 => "apostrophe",
            _ => "unknown",
        };
        Key::new(name)
    }
}

// ── Stub for non-macOS builds ─────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::sync::mpsc;
    use super::*;

    pub struct HotkeyManager {
        pub receiver: mpsc::Receiver<KeybindingId>,
    }

    impl HotkeyManager {
        pub fn register(_map: &BindingMap) -> Result<Self> {
            let (_tx, rx) = mpsc::channel();
            Ok(Self { receiver: rx })
        }
    }
}

pub use platform::HotkeyManager;
