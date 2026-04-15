//! Win32 global hotkey detection via `WH_KEYBOARD_LL`.
//!
//! # Strategy
//! A low-level keyboard hook (`WH_KEYBOARD_LL`) is installed on the main
//! thread.  The hook procedure is invoked *inside* `GetMessageW` whenever
//! any key is pressed system-wide.  When a configured binding matches, the
//! proc posts `WM_KBD_HOTKEY` to the thread queue and returns `LRESULT(1)`
//! to consume the key (preventing it from reaching the focused app).
//!
//! Unlike `RegisterHotKey` this is not blocked by:
//! - Alt-key menu activation
//! - UAC elevation mismatches between shellwright and the foreground window
//! - Other applications holding a global `RegisterHotKey` claim
//!
//! # Threading
//! The hook proc runs on the thread that installed the hook, called
//! re-entrantly from within `GetMessageW`.  No locks are held during the
//! proc; the binding table is write-once / read-many via `OnceLock`.

use std::sync::OnceLock;

use shellwright_core::{
    error::Error,
    event::KeybindingId,
    hotkey::{BindingMap, Key, Modifiers},
    Result,
};

/// Thread-message ID posted to the main queue when a binding fires.
/// `WM_USER + 2` = `0x0402`.  Does not conflict with `WM_SWE` (`0x0401`).
pub const WM_KBD_HOTKEY: u32 = 0x0402;

// ── Platform implementation ───────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState,
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx,
        HHOOK, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    // Internal modifier bitmask (not Win32 HOT_KEY_MODIFIERS).
    const M_ALT:   u32 = 0x01;
    const M_CTRL:  u32 = 0x02;
    const M_SHIFT: u32 = 0x04;
    const M_SUPER: u32 = 0x08;

    /// Binding table: `(KeybindingId.0, vk, required_mods_bitmask)`.
    /// Written once at startup; read-only from the hook proc.
    static BINDINGS: OnceLock<Vec<(u32, u16, u32)>> = OnceLock::new();
    /// Thread ID of the message-pump thread (set before hook install).
    static HOOK_TID: AtomicU32 = AtomicU32::new(0);

    unsafe extern "system" fn ll_hook_proc(
        code:   i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0 {
            let is_down = matches!(
                wparam.0 as u32,
                WM_KEYDOWN | WM_SYSKEYDOWN
            );
            if is_down {
                let kb  = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
                let vk  = kb.vkCode as u16;

                // Build modifier bitmask from async key state.
                // Cast to u16 first so the sign bit test is well-defined.
                let down = |k: i32| (GetAsyncKeyState(k) as u16) & 0x8000 != 0;
                let mods: u32 =
                    if down(VK_MENU.0 as i32)    { M_ALT   } else { 0 } |
                    if down(VK_CONTROL.0 as i32)  { M_CTRL  } else { 0 } |
                    if down(VK_SHIFT.0 as i32)    { M_SHIFT } else { 0 } |
                    if down(VK_LWIN.0 as i32)
                    || down(VK_RWIN.0 as i32)     { M_SUPER } else { 0 };

                if let Some(bindings) = BINDINGS.get() {
                    for &(id, bvk, bmods) in bindings {
                        if vk == bvk && mods == bmods {
                            let tid = HOOK_TID.load(Ordering::Relaxed);
                            if tid != 0 {
                                use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
                                let _ = PostThreadMessageW(
                                    tid,
                                    super::WM_KBD_HOTKEY,
                                    WPARAM(id as usize),
                                    LPARAM(0),
                                );
                            }
                            // Consume: do not pass to the focused application.
                            return LRESULT(1);
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub struct HotkeyManager {
        hook: isize,
    }

    impl HotkeyManager {
        pub fn register(map: &BindingMap) -> Result<Self> {
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::System::Threading::GetCurrentThreadId;

            // Store TID so the hook proc can post to our queue.
            let tid = unsafe { GetCurrentThreadId() };
            HOOK_TID.store(tid, Ordering::Relaxed);

            // Build binding table.
            let mut entries: Vec<(u32, u16, u32)> = Vec::new();
            let mut skipped = 0u32;

            for (id, combo, _action) in map.iter() {
                let vk = match key_to_vk(&combo.key) {
                    Ok(v)  => v,
                    Err(e) => {
                        tracing::warn!(key = %combo.key.0, err = %e, "binding skipped — unknown key");
                        skipped += 1;
                        continue;
                    }
                };
                let mods = mods_to_bits(combo.modifiers);
                tracing::info!(
                    id = id.0,
                    key = %combo.key.0,
                    mods,
                    vk,
                    "hotkey binding registered"
                );
                entries.push((id.0, vk, mods));
            }

            if skipped > 0 {
                tracing::warn!(skipped, "some bindings skipped — unknown key names");
            }

            // OnceLock write — safe to ignore error if somehow called twice.
            let _ = BINDINGS.set(entries);

            // Install the low-level keyboard hook on the current thread.
            let hmod = unsafe { GetModuleHandleW(None).unwrap_or_default() };
            let hook = unsafe {
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_hook_proc), hmod, 0)
                    .map_err(|e| Error::Backend(format!("SetWindowsHookExW: {e}")))?
            };

            tracing::info!("WH_KEYBOARD_LL hook installed");
            Ok(Self { hook: hook.0 as isize })
        }

        /// Recover `KeybindingId` from the `wparam` of `WM_KBD_HOTKEY`.
        /// The hook proc posts `id.0` directly, so no adjustment needed.
        pub fn resolve(wparam: usize) -> KeybindingId {
            KeybindingId(wparam as u32)
        }
    }

    impl Drop for HotkeyManager {
        fn drop(&mut self) {
            if self.hook != 0 {
                unsafe {
                    let _ = UnhookWindowsHookEx(HHOOK(self.hook as *mut _));
                }
                tracing::debug!("WH_KEYBOARD_LL hook removed");
            }
        }
    }

    fn mods_to_bits(m: Modifiers) -> u32 {
        let mut out = 0u32;
        if m.contains(Modifiers::ALT)   { out |= M_ALT; }
        if m.contains(Modifiers::CTRL)  { out |= M_CTRL; }
        if m.contains(Modifiers::SHIFT) { out |= M_SHIFT; }
        if m.contains(Modifiers::SUPER) { out |= M_SUPER; }
        out
    }

    /// Map a [`Key`] name to a Win32 virtual-key code.
    fn key_to_vk(key: &Key) -> Result<u16> {
        let s = key.0.as_str();

        // Single ASCII letter → VK_A..VK_Z (0x41–0x5A)
        if s.len() == 1 {
            let c = s.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                return Ok(c.to_ascii_uppercase() as u16);
            }
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
            "grave"            => 0xC0, // VK_OEM_3 (`/~)
            "bracketleft"      => 0xDB, // VK_OEM_4
            "backslash"        => 0xDC, // VK_OEM_5
            "bracketright"     => 0xDD, // VK_OEM_6
            "apostrophe"       => 0xDE, // VK_OEM_7
            other => return Err(Error::Config(format!("unsupported key: {other}"))),
        };
        Ok(vk)
    }
}

// ── Stub for non-Windows builds ───────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::*;

    pub struct HotkeyManager;

    impl HotkeyManager {
        pub fn register(_map: &BindingMap) -> Result<Self> { Ok(Self) }
        pub fn resolve(wparam: usize) -> KeybindingId { KeybindingId(wparam as u32) }
    }
}

pub use platform::HotkeyManager;
