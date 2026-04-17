//! [`WindowsBackend`] — Win32 implementation of [`shellwright_core::backend::Backend`].
//!
//! # Strategy
//! Acts as a *client* on top of Explorer / DWM — no shell-chrome changes.
//! Geometry via `SetWindowPos`; focus via `SetForegroundWindow`; close via
//! `PostMessage(WM_CLOSE)`; borders via GDI overlay ring windows.
//!
//! # Runtime event tracking
//! `SetWinEventHook` (OUTOFCONTEXT) posts `WM_SWE` thread messages back to the
//! main message loop so window create / destroy / focus / move events are detected
//! without polling.
//!
//! # Border overlays
//! Each managed window gets a companion `WS_POPUP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
//! WS_EX_NOACTIVATE` window whose visible region is a hollow ring (outer rect minus inner
//! rect via `CombineRgn(RGN_DIFF)`).  `WM_NCHITTEST` returns `HTTRANSPARENT` so all
//! pointer events pass through.  Works on Windows 10 and Windows 11, unlike
//! `DWMWA_BORDER_COLOR` which is Win11-only.

use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

use shellwright_core::{
    backend::Backend,
    config::FloatRule,
    error::Error,
    event::Event,
    hotkey::BindingMap,
    window::{Rect, Window, WindowId},
    Result,
};

use crate::hotkeys::HotkeyManager;

// ── WinEvent thread-message plumbing ─────────────────────────────────────────

static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// Atom returned by `RegisterClassExW` for the border overlay window class.
static BORDER_CLASS_ATOM: AtomicU16 = AtomicU16::new(0);

const WM_SWE: u32 = 0x0401; // WM_USER + 1

const SWE_CREATED:          usize = 1;
const SWE_DESTROYED:        usize = 2;
const SWE_FOCUSED:          usize = 3;
const SWE_MOVESIZEEND:      usize = 4;
const SWE_MINIMIZE_CHANGE:  usize = 5; // MINIMIZESTART or MINIMIZEEND
const SWE_LOCATIONCHANGE:   usize = 6; // EVENT_OBJECT_LOCATIONCHANGE
/// Application hid its own window (EVENT_OBJECT_HIDE 0x8003).
/// We respond by hiding our border overlay so it doesn't float orphaned.
const SWE_APP_HIDDEN:       usize = 7;
/// Work area changed — an appbar (e.g. YASB) registered or unregistered.
/// Posts WM_SETTINGCHANGE 0x001A from `border_wnd_proc` → relayout.
const SWE_SETTINGS_CHANGED: usize = 8;

// ── Border overlay WndProc ────────────────────────────────────────────────────

/// Window procedure for border overlay windows.
///
/// Fills the entire client area with the colour stored in `GWLP_USERDATA`
/// (as a Win32 `COLORREF`).  The window's region is always a hollow ring, so
/// the painted area is visually the ring only.
///
/// `WM_NCHITTEST` returns `HTTRANSPARENT` (-1) so all pointer events fall
/// through to whatever window is below.
#[cfg(target_os = "windows")]
unsafe extern "system" fn border_wnd_proc(
    hwnd:   windows::Win32::Foundation::HWND,
    msg:    u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{COLORREF, LRESULT, WPARAM, LPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
        HGDIOBJ, PAINTSTRUCT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetWindowLongPtrW, PostThreadMessageW,
        GWLP_USERDATA, WM_NCHITTEST, WM_PAINT,
    };

    const WM_SETTINGCHANGE: u32 = 0x001A;

    match msg {
        WM_PAINT => {
            let colorref = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32;
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let brush = CreateSolidBrush(COLORREF(colorref));
            FillRect(hdc, &ps.rcPaint, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        // HTTRANSPARENT = -1: pass all hit-tests through to the window below.
        WM_NCHITTEST => LRESULT(-1),
        // WM_SETTINGCHANGE is broadcast to all windows when system settings change,
        // including when an appbar (e.g. YASB) registers/unregisters and updates the
        // work area via SPI_SETWORKAREA.  Post SWE_SETTINGS_CHANGED to the WM thread
        // so the event loop can re-query monitor_rects() and relayout.
        WM_SETTINGCHANGE => {
            let tid = HOOK_THREAD_ID.load(Ordering::Relaxed);
            if tid != 0 {
                let _ = PostThreadMessageW(
                    tid,
                    WM_SWE,
                    WPARAM(SWE_SETTINGS_CHANGED),
                    LPARAM(0),
                );
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Register the `shellwright_border` window class; returns the atom (0 on failure).
#[cfg(target_os = "windows")]
unsafe fn register_border_class() -> u16 {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        RegisterClassExW, CS_HREDRAW, CS_VREDRAW, WNDCLASSEXW,
    };
    use windows::core::PCWSTR;

    let hmodule = GetModuleHandleW(None).unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);
    let class_name: Vec<u16> = "shellwright_border\0".encode_utf16().collect();

    let wc = WNDCLASSEXW {
        cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
        style:         CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc:   Some(border_wnd_proc),
        hInstance:     hinstance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    RegisterClassExW(&wc)
}

/// Create a border overlay window positioned over `rect`.  Returns the raw
/// HWND as `isize`, or 0 on failure.
#[cfg(target_os = "windows")]
unsafe fn create_overlay(rect: Rect) -> isize {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, WS_POPUP,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    use windows::core::PCWSTR;

    let atom = BORDER_CLASS_ATOM.load(Ordering::Relaxed);
    if atom == 0 { return 0; }

    let hmodule = GetModuleHandleW(None).unwrap_or_default();
    let hinstance = HINSTANCE(hmodule.0);

    // MAKEINTATOM equivalent: cast atom to *const u16
    let class_ptr = PCWSTR(atom as usize as *const u16);

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class_ptr,
        PCWSTR::null(),
        WS_POPUP,
        rect.x,
        rect.y,
        rect.width as i32,
        rect.height as i32,
        None,
        None,
        hinstance,
        None,
    );

    match hwnd {
        Ok(h) if !h.0.is_null() => h.0 as isize,
        _ => 0,
    }
}

/// Reposition the overlay and rebuild its hollow ring region.
///
/// The ring region is: full window rect **minus** the inner rect inset by
/// `border_width` on all sides.  When `radius > 0`, both regions use
/// `CreateRoundRectRgn` for rounded corners.  After `SetWindowRgn` the OS
/// owns `outer`; we delete `inner` ourselves.
///
/// `app_hwnd` is the managed window the overlay decorates.  Z-order is set so
/// the overlay is always **above** the app window but **below** floating/fullscreen
/// windows:
/// - `topmost = false` (tiled): demote overlay to non-topmost band, then place it
///   immediately above `app_hwnd` in that band via an explicit insert-after call.
/// - `topmost = true`  (floating/fullscreen): place overlay in TOPMOST band, then
///   immediately re-raise `app_hwnd` above it so the app window is on top of its
///   own ring.
#[cfg(target_os = "windows")]
unsafe fn position_overlay(overlay: isize, app_hwnd: isize, rect: Rect, border_width: u32, radius: u32, topmost: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject,
        SetWindowRgn, HGDIOBJ, RGN_DIFF,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    if overlay == 0 { return; }
    let hw     = HWND(overlay as *mut _);
    let app_hw = HWND(app_hwnd as *mut _);
    let bw = border_width as i32;
    let r  = radius as i32;
    let w  = rect.width  as i32;
    let h  = rect.height as i32;

    if topmost {
        // Step 1: place overlay in TOPMOST band at the correct position/size.
        let _ = SetWindowPos(hw, HWND_TOPMOST, rect.x, rect.y, w, h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW);
        // Step 2: re-raise the app window above its own overlay so the window
        //         content is not covered by the ring.
        let _ = SetWindowPos(app_hw, HWND_TOPMOST, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    } else {
        // Step 1: demote overlay out of TOPMOST band and set position/size.
        let _ = SetWindowPos(hw, HWND_NOTOPMOST, rect.x, rect.y, w, h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW);
        // Step 2: place overlay immediately above the app window within the
        //         non-topmost band so the ring is visible over its own window.
        let _ = SetWindowPos(hw, app_hw, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }

    // Ring = outer - inner.  Use rounded rects when radius > 0.
    let outer = if r > 0 {
        CreateRoundRectRgn(0, 0, w, h, r * 2, r * 2)
    } else {
        CreateRectRgn(0, 0, w, h)
    };
    // Inner radius shrinks by border_width; floor at 0 to avoid negative.
    let inner_r = (r - bw).max(0);
    let inner = if inner_r > 0 {
        CreateRoundRectRgn(bw, bw, (w - bw).max(bw + 1), (h - bw).max(bw + 1), inner_r * 2, inner_r * 2)
    } else {
        CreateRectRgn(bw, bw, (w - bw).max(bw + 1), (h - bw).max(bw + 1))
    };
    CombineRgn(outer, outer, inner, RGN_DIFF);
    // OS takes ownership of `outer` after SetWindowRgn.
    SetWindowRgn(hw, outer, true);
    let _ = DeleteObject(HGDIOBJ(inner.0));
}

// ── WindowsWindow ─────────────────────────────────────────────────────────────

pub struct WindowsWindow {
    id:           WindowId,
    /// Raw HWND stored as `isize` so the struct is `Send`.
    hwnd:         isize,
    title:        String,
    geometry:     Rect,
    floating:     bool,
    /// `true` while the window is floating **or** fullscreen — drives
    /// `HWND_TOPMOST` placement for the window and its border overlay.
    is_topmost:   bool,
    /// Set to `true` when *we* hide the window (workspace switch).
    /// Prevents `SWE_CREATED` from re-adopting our own hidden windows.
    wm_hidden:    bool,
    /// Companion border overlay HWND, or 0 if not yet created.
    overlay_hwnd: isize,
    /// Last border width used for the overlay ring region (pixels).
    border_width: u32,
    /// Border corner radius in pixels (0 = square corners).
    border_radius: u32,
}

impl Drop for WindowsWindow {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        if self.overlay_hwnd != 0 {
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
                let _ = DestroyWindow(HWND(self.overlay_hwnd as *mut _));
            }
        }
    }
}

impl Window for WindowsWindow {
    fn id(&self)     -> WindowId { self.id }
    fn title(&self)  -> &str    { &self.title }
    fn geometry(&self) -> Rect  { self.geometry }

    fn set_geometry(&mut self, rect: Rect) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                IsZoomed, SetWindowPos, ShowWindow,
                SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER,
            };
            let hwnd = HWND(self.hwnd as *mut _);
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            SetWindowPos(
                hwnd,
                HWND(std::ptr::null_mut()),
                rect.x,
                rect.y,
                rect.width  as i32,
                rect.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .map_err(|e| Error::Backend(format!("SetWindowPos: {e}")))?;

            // Keep overlay in sync — expand by border_width so full ring protrudes.
            if self.overlay_hwnd != 0 {
                let vr = expand_rect(visible_rect(HWND(self.hwnd as *mut _), rect), self.border_width as i32);
                position_overlay(self.overlay_hwnd, self.hwnd, vr, self.border_width, self.border_radius, self.is_topmost);
            }
        }
        self.geometry = rect;
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetForegroundWindow, SetWindowPos, HWND_TOPMOST,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            let hwnd = HWND(self.hwnd as *mut _);
            let ok = SetForegroundWindow(hwnd);
            if !ok.as_bool() {
                tracing::debug!(id = %self.id, "SetForegroundWindow denied");
            }
            // Re-assert TOPMOST so focusing doesn't sink a floating/fullscreen window.
            if self.is_topmost {
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
            PostMessageW(
                HWND(self.hwnd as *mut _),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
            .map_err(|e| Error::Backend(format!("PostMessageW(WM_CLOSE): {e}")))?;
        }
        Ok(())
    }

    fn is_floating(&self) -> bool { self.floating }

    fn set_floating(&mut self, floating: bool) {
        self.floating   = floating;
        self.is_topmost = floating;
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            let insert_after = if floating { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let _ = SetWindowPos(
                HWND(self.hwnd as *mut _), insert_after, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn set_topmost(&mut self, topmost: bool) -> Result<()> {
        self.is_topmost = topmost;
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            let insert_after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
            let _ = SetWindowPos(
                HWND(self.hwnd as *mut _), insert_after, 0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(())
    }

    fn enter_fullscreen_geometry(&mut self, rect: Rect) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                IsZoomed, SetWindowPos, ShowWindow,
                HWND_TOPMOST, SW_RESTORE, SWP_NOACTIVATE,
            };
            let hwnd = HWND(self.hwnd as *mut _);
            // Un-maximize first — maximised windows ignore SetWindowPos moves.
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            // Single atomic call: HWND_TOPMOST (without SWP_NOZORDER) + geometry.
            // This lands the window at the FRONT of the topmost stack — above YASB
            // and the taskbar — and positions it to cover the full monitor in one
            // DWM transaction (no flicker).
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.x,
                rect.y,
                rect.width  as i32,
                rect.height as i32,
                SWP_NOACTIVATE,
            )
            .map_err(|e| Error::Backend(format!("SetWindowPos (fullscreen): {e}")))?;
            self.is_topmost = true;
            // Keep overlay coords current (it will be hidden by update_borders).
            if self.overlay_hwnd != 0 {
                let vr = expand_rect(
                    visible_rect(HWND(self.hwnd as *mut _), rect),
                    self.border_width as i32,
                );
                position_overlay(
                    self.overlay_hwnd, self.hwnd, vr,
                    self.border_width, self.border_radius, true,
                );
            }
        }
        self.geometry = rect;
        Ok(())
    }

    fn is_minimized(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::IsIconic;
            unsafe { IsIconic(HWND(self.hwnd as *mut _)).as_bool() }
        }
        #[cfg(not(target_os = "windows"))]
        false
    }

    fn hide(&mut self) -> Result<()> {
        tracing::debug!(id = %self.id, hwnd = self.hwnd, "hide: SW_HIDE");
        self.wm_hidden = true;
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            let r = ShowWindow(HWND(self.hwnd as *mut _), SW_HIDE);
            tracing::debug!(id = %self.id, result = ?r, "ShowWindow(SW_HIDE)");
            // Hide overlay too.
            if self.overlay_hwnd != 0 {
                let _ = ShowWindow(HWND(self.overlay_hwnd as *mut _), SW_HIDE);
            }
        }
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        tracing::debug!(id = %self.id, hwnd = self.hwnd, "show: SW_SHOWNA");
        self.wm_hidden = false;
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNA};
            let r = ShowWindow(HWND(self.hwnd as *mut _), SW_SHOWNA);
            tracing::debug!(id = %self.id, result = ?r, "ShowWindow(SW_SHOWNA)");
            // Show overlay too.
            if self.overlay_hwnd != 0 {
                let _ = ShowWindow(HWND(self.overlay_hwnd as *mut _), SW_SHOWNA);
            }
        }
        Ok(())
    }

    fn park(&mut self) -> Result<()> {
        tracing::debug!(id = %self.id, hwnd = self.hwnd, "park: moving off-screen (-32000,0)");
        self.wm_hidden = true;
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, ShowWindow, SW_HIDE,
                SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
            };
            // Move to a position far off-screen — window stays in the OS/taskbar
            // but is not visible on any monitor.  SW_HIDE is NOT used so it stays
            // in the taskbar (global taskbar mode).
            let _ = SetWindowPos(
                HWND(self.hwnd as *mut _),
                HWND(std::ptr::null_mut()),
                -32000,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
            // Always hide the overlay — it has no useful position off-screen.
            if self.overlay_hwnd != 0 {
                let _ = ShowWindow(HWND(self.overlay_hwnd as *mut _), SW_HIDE);
            }
        }
        Ok(())
    }

    fn raise(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            // HWND_TOP brings the window to the top of the non-topmost z-band
            // without changing its activation state (SWP_NOACTIVATE).
            let _ = SetWindowPos(
                HWND(self.hwnd as *mut _),
                HWND_TOP,
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(())
    }

    fn set_border_color(&mut self, rgb: u32) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
            let colorref = rgb_to_colorref(rgb);
            // DWMWA_BORDER_COLOR = 34 — Windows 11 22H2+; silently no-ops elsewhere.
            if let Err(e) = DwmSetWindowAttribute(
                HWND(self.hwnd as *mut _),
                DWMWINDOWATTRIBUTE(34),
                &colorref as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            ) {
                tracing::debug!(id = %self.id, err = %e, "DwmSetWindowAttribute(BORDER_COLOR) failed");
            }
        }
        Ok(())
    }

    fn hide_border_overlay(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        if self.overlay_hwnd != 0 {
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
                let _ = ShowWindow(HWND(self.overlay_hwnd as *mut _), SW_HIDE);
            }
        }
        Ok(())
    }

    fn set_alpha(&mut self, alpha: u8) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            set_layered_alpha(HWND(self.hwnd as *mut _), alpha)?;
            if self.overlay_hwnd != 0 {
                let _ = set_layered_alpha(HWND(self.overlay_hwnd as *mut _), alpha);
            }
        }
        Ok(())
    }

    fn clear_alpha(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            clear_layered(HWND(self.hwnd as *mut _));
            if self.overlay_hwnd != 0 {
                clear_layered(HWND(self.overlay_hwnd as *mut _));
            }
        }
        Ok(())
    }

    fn set_border_overlay(&mut self, rgb: u32, width: u32, radius: u32) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Gdi::InvalidateRect;
            use windows::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWLP_USERDATA};

            let colorref = rgb_to_colorref(rgb);
            self.border_width  = width;
            self.border_radius = radius;

            // Create overlay on first call.
            if self.overlay_hwnd == 0 {
                self.overlay_hwnd = create_overlay(self.geometry);
                if self.overlay_hwnd == 0 {
                    tracing::warn!(id = %self.id, "failed to create border overlay");
                    return Ok(());
                }
            }

            let hw = HWND(self.overlay_hwnd as *mut _);

            // Store colour for WM_PAINT.
            SetWindowLongPtrW(hw, GWLP_USERDATA, colorref as isize);

            // Reposition + rebuild ring region using visible bounds, expanded
            // outward by border_width so the full ring protrudes past window chrome.
            let vr = expand_rect(visible_rect(HWND(self.hwnd as *mut _), self.geometry), self.border_width as i32);
            position_overlay(self.overlay_hwnd, self.hwnd, vr, width, radius, self.is_topmost);

            // Repaint.
            let _ = InvalidateRect(hw, None, false);
        }
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the *visible* window rect via `DWMWA_EXTENDED_FRAME_BOUNDS`.
///
/// On Windows 10/11 every window has an invisible resize border (~8 px on
/// left/right/bottom).  `GetWindowRect` includes that dead zone; this function
/// returns only the visually rendered area.  Falls back to the geometry stored
/// in `win` when DWM fails (e.g. on minimised or non-DWM windows).
#[cfg(target_os = "windows")]
unsafe fn visible_rect(hwnd: windows::Win32::Foundation::HWND, fallback: Rect) -> Rect {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWINDOWATTRIBUTE};

    let mut frame = RECT::default();
    if DwmGetWindowAttribute(
        hwnd,
        DWMWINDOWATTRIBUTE(9), // DWMWA_EXTENDED_FRAME_BOUNDS
        &mut frame as *mut RECT as *mut _,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_ok()
        && (frame.right - frame.left) > 0
        && (frame.bottom - frame.top) > 0
    {
        Rect::new(
            frame.left,
            frame.top,
            (frame.right  - frame.left) as u32,
            (frame.bottom - frame.top)  as u32,
        )
    } else {
        fallback
    }
}

/// Expand a rect outward by `px` pixels on every side.
fn expand_rect(r: Rect, px: i32) -> Rect {
    Rect::new(
        r.x - px,
        r.y - px,
        (r.width  as i32 + px * 2).max(0) as u32,
        (r.height as i32 + px * 2).max(0) as u32,
    )
}

/// Convert `0x00RRGGBB` to Win32 `COLORREF` (`0x00BBGGRR`).
#[cfg(target_os = "windows")]
fn rgb_to_colorref(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8)  & 0xFF;
    let b =  rgb        & 0xFF;
    r | (g << 8) | (b << 16)
}

/// Ensure `hwnd` has `WS_EX_LAYERED` set and apply `alpha` via `LWA_ALPHA`.
#[cfg(target_os = "windows")]
unsafe fn set_layered_alpha(
    hwnd:  windows::Win32::Foundation::HWND,
    alpha: u8,
) -> Result<()> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, SetLayeredWindowAttributes,
        GWL_EXSTYLE, LWA_ALPHA, WS_EX_LAYERED,
    };
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_LAYERED.0 == 0 {
        SetWindowLongW(hwnd, GWL_EXSTYLE, (ex_style | WS_EX_LAYERED.0) as i32);
    }
    SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA)
        .map_err(|e| Error::Backend(format!("SetLayeredWindowAttributes: {e}")))?;
    Ok(())
}

/// Remove `WS_EX_LAYERED` from `hwnd`, restoring normal (opaque) rendering.
#[cfg(target_os = "windows")]
unsafe fn clear_layered(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_LAYERED,
    };
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_LAYERED.0 != 0 {
        SetWindowLongW(hwnd, GWL_EXSTYLE, (ex_style & !WS_EX_LAYERED.0) as i32);
    }
}

/// Returns `true` if `hwnd` should be managed by shellwright.
#[cfg(target_os = "windows")]
unsafe fn is_manageable(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWINDOWATTRIBUTE};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextLengthW, IsWindowVisible,
        GWL_EXSTYLE, GWL_STYLE, GW_OWNER, WS_CAPTION, WS_EX_TOOLWINDOW,
    };

    if !IsWindowVisible(hwnd).as_bool() { return false; }

    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        if !owner.0.is_null() { return false; }
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 { return false; }

    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 == 0 { return false; }

    if GetWindowTextLengthW(hwnd) == 0 { return false; }

    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWINDOWATTRIBUTE(14),
        &mut cloaked as *mut u32 as *mut _,
        std::mem::size_of::<u32>() as u32,
    );
    if cloaked != 0 { return false; }

    let mut class_buf = [0u16; 256];
    let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
    if class_len > 0 {
        let class = String::from_utf16_lossy(&class_buf[..class_len]);
        match class.as_str() {
            "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Progman"
            | "WorkerW"
            | "DV2ControlHost"
            | "Windows.UI.Core.CoreWindow"
            | "shellwright_border"   // exclude our own overlays
            => return false,
            _ => {}
        }
    }

    true
}

/// Return the Win32 window class name for `hwnd`, or an empty string on failure.
#[cfg(target_os = "windows")]
unsafe fn hwnd_class(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buf) as usize;
    String::from_utf16_lossy(&buf[..len])
}

/// Return the executable filename (e.g. `"steam.exe"`) for the process that
/// owns `hwnd`.  Returns an empty string if the query fails.
#[cfg(target_os = "windows")]
unsafe fn hwnd_exe(hwnd: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    use windows::core::PWSTR;

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 { return String::new(); }

    let hproc = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        Ok(h) => h,
        Err(_) => return String::new(),
    };

    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(
        hproc,
        PROCESS_NAME_WIN32,
        PWSTR(buf.as_mut_ptr()),
        &mut len,
    ).is_ok();
    let _ = CloseHandle(hproc);

    if !ok || len == 0 { return String::new(); }
    let full_path = String::from_utf16_lossy(&buf[..len as usize]);
    std::path::Path::new(&full_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// Decide whether `hwnd` should start in floating (non-tiled) mode.
///
/// # Heuristics (no config required)
/// 1. Class `#32770` — the Windows system dialog class; covers all common
///    dialogs, NSIS/Inno Setup installers, MSI dialogs, file-open/save sheets.
/// 2. `WS_CAPTION` present but both `WS_THICKFRAME` and `WS_MAXIMIZEBOX`
///    absent — a fixed-size window (properties dialogs, settings panes, etc.).
///
/// # User rules
/// If neither heuristic fires, the caller's `float_rules` list is checked.
/// A rule matches when every specified field matches (AND logic).
#[cfg(target_os = "windows")]
unsafe fn should_float(
    hwnd:        windows::Win32::Foundation::HWND,
    title:       &str,
    float_rules: &[FloatRule],
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, GWL_STYLE, WS_CAPTION, WS_MAXIMIZEBOX, WS_THICKFRAME,
    };

    let class = hwnd_class(hwnd);

    // ── Heuristic 1: system dialog class ────────────────────────────────────
    if class == "#32770" {
        tracing::debug!(class, "auto-float: system dialog class");
        return true;
    }

    // ── Heuristic 2: fixed-size captioned window ─────────────────────────────
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 != 0
        && style & WS_THICKFRAME.0 == 0
        && style & WS_MAXIMIZEBOX.0 == 0
    {
        tracing::debug!(class, style, "auto-float: fixed-size window");
        return true;
    }

    // ── User float rules ─────────────────────────────────────────────────────
    if !float_rules.is_empty() {
        let exe = hwnd_exe(hwnd);
        for rule in float_rules {
            if rule.matches(&class, title, &exe) {
                tracing::debug!(class, %title, %exe, "auto-float: matched user rule");
                return true;
            }
        }
    }

    false
}

/// Build a [`WindowsWindow`] from a raw HWND.  Returns `None` on failure.
#[cfg(target_os = "windows")]
unsafe fn build_window(
    hwnd: windows::Win32::Foundation::HWND,
    id: WindowId,
    float_rules: &[FloatRule],
) -> Option<WindowsWindow> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    };

    let len = GetWindowTextLengthW(hwnd) as usize;
    if len == 0 { return None; }
    let mut buf = vec![0u16; len + 1];
    GetWindowTextW(hwnd, &mut buf);
    let title = String::from_utf16_lossy(&buf[..len]);

    let mut r = RECT::default();
    GetWindowRect(hwnd, &mut r).ok()?;
    let geometry = Rect::new(
        r.left,
        r.top,
        (r.right  - r.left) as u32,
        (r.bottom - r.top)  as u32,
    );

    let floating = should_float(hwnd, &title, float_rules);
    tracing::debug!(%id, %title, floating, ?geometry, "window registered");

    // Elevate floating windows to TOPMOST immediately on adoption.
    if floating {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }

    Some(WindowsWindow {
        id,
        hwnd: hwnd.0 as isize,
        title,
        geometry,
        floating,
        is_topmost:   floating,
        wm_hidden:    false,
        overlay_hwnd:  0,
        border_width:  0,
        border_radius: 0,
    })
}

/// Refresh the stored geometry from Win32 (called after a drag/resize).
#[cfg(target_os = "windows")]
unsafe fn refresh_geometry(win: &mut WindowsWindow) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let hwnd = HWND(win.hwnd as *mut _);
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_ok() {
        win.geometry = Rect::new(
            r.left,
            r.top,
            (r.right  - r.left) as u32,
            (r.bottom - r.top)  as u32,
        );
    }
}

// ── Initial window enumeration ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn enumerate_windows(next_id: &mut u64, float_rules: &[FloatRule]) -> Vec<WindowsWindow> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        if unsafe { is_manageable(hwnd) } {
            list.push(hwnd);
        }
        BOOL(1)
    }

    let mut raw: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut raw as *mut Vec<HWND> as isize),
        );
    }

    raw.into_iter()
        .filter_map(|hwnd| {
            let id = WindowId(*next_id);
            *next_id += 1;
            unsafe { build_window(hwnd, id, float_rules) }
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn enumerate_windows(_next_id: &mut u64, _float_rules: &[FloatRule]) -> Vec<WindowsWindow> { Vec::new() }

// ── WinEvent hook callback ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
unsafe extern "system" fn win_event_proc(
    _hook:         windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event:         u32,
    hwnd:          windows::Win32::Foundation::HWND,
    id_object:     i32,
    id_child:      i32,
    _event_thread: u32,
    _event_time:   u32,
) {
    if id_object != 0 || id_child != 0 { return; }
    if hwnd.0.is_null() { return; }

    let wp: usize = match event {
        0x8002 => SWE_CREATED,          // EVENT_OBJECT_SHOW
        0x8001 => SWE_DESTROYED,        // EVENT_OBJECT_DESTROY
        0x8003 => SWE_APP_HIDDEN,       // EVENT_OBJECT_HIDE — app hid its own window
        0x0003 => SWE_FOCUSED,          // EVENT_SYSTEM_FOREGROUND
        0x8005 => SWE_FOCUSED,          // EVENT_OBJECT_FOCUS — catches clicks on windows that
                                        // are already the OS foreground (no FOREGROUND event fires)
        0x000B => SWE_MOVESIZEEND,      // EVENT_SYSTEM_MOVESIZEEND
        0x0016 | 0x0017 => SWE_MINIMIZE_CHANGE, // EVENT_SYSTEM_MINIMIZESTART / MINIMIZEEND
        0x800B => SWE_LOCATIONCHANGE,   // EVENT_OBJECT_LOCATIONCHANGE
        _ => return,
    };

    let tid = HOOK_THREAD_ID.load(Ordering::Relaxed);
    if tid == 0 { return; }

    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
    let _ = PostThreadMessageW(tid, WM_SWE, WPARAM(wp), LPARAM(hwnd.0 as isize));
}

// ── Hook RAII guard ───────────────────────────────────────────────────────────

struct HookGuards(Vec<isize>);
unsafe impl Send for HookGuards {}

impl Drop for HookGuards {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::Accessibility::{UnhookWinEvent, HWINEVENTHOOK};
            for &h in &self.0 {
                if h != 0 {
                    unsafe { let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _)); }
                }
            }
        }
    }
}

// ── YASB named-pipe server ────────────────────────────────────────────────────

/// Spawn the YASB named-pipe server thread.
///
/// Creates `\\.\pipe\shellwright` and blocks until a client connects.  For each
/// JSON line pushed via the returned `SyncSender`, it calls `WriteFile`.  When
/// the client disconnects the thread re-creates the pipe and waits for the next
/// connection.  Returns `None` if the thread could not be spawned.
fn spawn_yasb_pipe() -> Option<std::sync::mpsc::SyncSender<String>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(4);

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
        use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, WriteFile};
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, NAMED_PIPE_MODE,
        };
        use windows::core::PCWSTR;

        let ok = std::thread::Builder::new()
            .name("shellwright-yasb".into())
            .spawn(move || {
                let name: Vec<u16> = "\\\\.\\pipe\\shellwright\0".encode_utf16().collect();
                loop {
                    let pipe: HANDLE = unsafe {
                        CreateNamedPipeW(
                            PCWSTR(name.as_ptr()),
                            FILE_FLAGS_AND_ATTRIBUTES(0x0000_0002), // PIPE_ACCESS_OUTBOUND
                            NAMED_PIPE_MODE(0x0000_0000),           // PIPE_TYPE_BYTE | PIPE_WAIT
                            255,   // PIPE_UNLIMITED_INSTANCES
                            65536, // outbound buffer
                            0,     // inbound buffer
                            0,     // default timeout
                            None,
                        )
                    };
                    if pipe == INVALID_HANDLE_VALUE {
                        tracing::warn!("CreateNamedPipeW failed — YASB IPC unavailable");
                        break;
                    }
                    tracing::debug!("YASB pipe created, waiting for client");

                    // Block until a client connects.
                    let _ = unsafe { ConnectNamedPipe(pipe, None) };
                    tracing::debug!("YASB client connected");

                    // Drain channel messages until WriteFile fails (client disconnected).
                    for msg in &rx {
                        let bytes = msg.as_bytes();
                        let mut written = 0u32;
                        if unsafe {
                            WriteFile(pipe, Some(bytes), Some(&mut written), None)
                        }
                        .is_err()
                        {
                            tracing::debug!("YASB client disconnected");
                            break;
                        }
                    }

                    // Drain any queued messages before re-creating the pipe.
                    while rx.try_recv().is_ok() {}
                    unsafe {
                        let _ = DisconnectNamedPipe(pipe);
                        let _ = CloseHandle(pipe);
                    }
                }
            })
            .is_ok();

        if ok { Some(tx) } else { None }
    }

    #[cfg(not(target_os = "windows"))]
    {
        drop(rx);
        None
    }
}

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct WindowsBackend {
    windows:     Vec<WindowsWindow>,
    next_id:     u64,
    _hotkeys:    HotkeyManager,
    _hooks:      HookGuards,
    /// Sender end of the channel feeding the YASB named-pipe thread.
    /// `None` when the pipe could not be created (e.g. insufficient perms).
    yasb_tx:     Option<std::sync::mpsc::SyncSender<String>>,
    float_rules: Vec<FloatRule>,
}

impl WindowsBackend {
    pub fn new(bindings: &BindingMap, float_rules: Vec<FloatRule>) -> Result<Self> {
        tracing::info!("initialising Win32 backend");

        let hotkeys = HotkeyManager::register(bindings)?;

        #[cfg(target_os = "windows")]
        let (mut next_id, hooks) = {
            use windows::Win32::Foundation::HMODULE;
            use windows::Win32::System::Threading::GetCurrentThreadId;
            use windows::Win32::UI::Accessibility::SetWinEventHook;

            let tid = unsafe { GetCurrentThreadId() };
            HOOK_THREAD_ID.store(tid, Ordering::Relaxed);

            // Register border overlay window class (idempotent).
            if BORDER_CLASS_ATOM.load(Ordering::Relaxed) == 0 {
                let atom = unsafe { register_border_class() };
                if atom != 0 {
                    BORDER_CLASS_ATOM.store(atom, Ordering::Relaxed);
                    tracing::debug!(atom, "border class registered");
                } else {
                    tracing::warn!("RegisterClassExW failed for border class");
                }
            }

            let flags: u32 = 0x0002; // WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS

            let hook_handles: Vec<isize> = unsafe {
                vec![
                    SetWinEventHook(0x8002, 0x8002, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    SetWinEventHook(0x8001, 0x8001, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    SetWinEventHook(0x0003, 0x0003, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // EVENT_OBJECT_FOCUS: fires when any object gains focus, including top-level
                    // windows that are already the OS foreground (no FOREGROUND event in that case).
                    // id_object==0 && id_child==0 filter in win_event_proc keeps only window-level
                    // focus events; sub-control focus (child HWNDs not in our list) is ignored.
                    SetWinEventHook(0x8005, 0x8005, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    SetWinEventHook(0x000B, 0x000B, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // Minimize start + minimize end (restore) — both trigger relayout.
                    SetWinEventHook(0x0016, 0x0017, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // Location/size change — detects app-native fullscreen (browser F11, games).
                    // WINEVENT_SKIPOWNPROCESS (bit 1 of flags) prevents our own SetWindowPos
                    // from triggering this; only external size changes reach us.
                    SetWinEventHook(0x800B, 0x800B, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // EVENT_OBJECT_HIDE (0x8003) — app hid its own window.
                    // Used to suppress stale border overlays when a window hides itself
                    // without going through our hide() path (e.g. system tray apps).
                    SetWinEventHook(0x8003, 0x8003, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                ]
            };
            for &h in &hook_handles {
                if h == 0 {
                    tracing::warn!("SetWinEventHook returned null — event tracking incomplete");
                }
            }
            (1u64, HookGuards(hook_handles))
        };

        #[cfg(not(target_os = "windows"))]
        let (mut next_id, hooks) = (1u64, HookGuards(vec![]));

        let windows = enumerate_windows(&mut next_id, &float_rules);
        tracing::info!(count = windows.len(), "initial window list");

        let yasb_tx = spawn_yasb_pipe();

        Ok(Self { windows, next_id, _hotkeys: hotkeys, _hooks: hooks, yasb_tx, float_rules })
    }
}

impl Backend for WindowsBackend {
    type W = WindowsWindow;

    fn windows(&self) -> Vec<&WindowsWindow> {
        self.windows.iter().collect()
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut WindowsWindow> {
        self.windows.iter_mut().find(|w| w.id() == id)
    }

    fn next_event(&mut self) -> Result<Event> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, GetMessageW, TranslateMessage, MSG, WM_HOTKEY, WM_QUIT,
            };

            let mut msg = MSG::default();
            loop {
                let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                match ret.0 {
                    -1 => return Err(Error::Backend("GetMessageW failed".into())),
                    0  => return Ok(Event::Quit),
                    _  => {}
                }

                match msg.message {
                    WM_HOTKEY => {
                        let id = HotkeyManager::resolve(msg.wParam.0);
                        tracing::info!(?id, "WM_HOTKEY");
                        return Ok(Event::Keybinding(id));
                    }

                    x if x == crate::hotkeys::WM_KBD_HOTKEY => {
                        let id = HotkeyManager::resolve(msg.wParam.0);
                        tracing::info!(?id, "keybinding fired (LL hook)");
                        return Ok(Event::Keybinding(id));
                    }

                    WM_QUIT => return Ok(Event::Quit),

                    WM_SWE => {
                        let hwnd_val: isize = msg.lParam.0;
                        let hwnd = HWND(hwnd_val as *mut _);

                        match msg.wParam.0 {
                            SWE_CREATED => {
                                // If window already tracked but hidden by us, re-hide it
                                // (SWE_CREATED fires when our SW_HIDE animates away).
                                if let Some(w) = self.windows.iter_mut().find(|w| w.hwnd == hwnd_val) {
                                    if w.wm_hidden {
                                        tracing::debug!(
                                            id = %w.id,
                                            hwnd = hwnd_val,
                                            "SWE_CREATED on wm_hidden window — re-hiding"
                                        );
                                        #[cfg(target_os = "windows")]
                                        unsafe {
                                            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
                                            let _ = ShowWindow(HWND(w.hwnd as *mut _), SW_HIDE);
                                        }
                                        continue;
                                    }
                                    tracing::debug!(id = %w.id, hwnd = hwnd_val, "SWE_CREATED already tracked + visible — ignore");
                                    continue; // already tracked and visible — ignore
                                }
                                if unsafe { is_manageable(hwnd) } {
                                    let id = WindowId(self.next_id);
                                    self.next_id += 1;
                                    if let Some(win) = unsafe { build_window(hwnd, id, &self.float_rules) } {
                                        let win_id = win.id;
                                        self.windows.push(win);
                                        return Ok(Event::WindowCreated(win_id));
                                    }
                                }
                            }

                            SWE_DESTROYED => {
                                if let Some(pos) = self.windows.iter().position(|w| w.hwnd == hwnd_val) {
                                    let id = self.windows[pos].id;
                                    // Explicitly destroy border overlay *before* remove() so we
                                    // can zero overlay_hwnd and prevent the Drop impl from
                                    // double-destroying it (belt and suspenders for stale borders).
                                    if self.windows[pos].overlay_hwnd != 0 {
                                        let ov = self.windows[pos].overlay_hwnd;
                                        self.windows[pos].overlay_hwnd = 0;
                                        unsafe {
                                            use windows::Win32::Foundation::HWND;
                                            use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
                                            let _ = DestroyWindow(HWND(ov as *mut _));
                                        }
                                    }
                                    self.windows.remove(pos);
                                    tracing::debug!(%id, hwnd = hwnd_val, "window destroyed");
                                    return Ok(Event::WindowDestroyed(id));
                                }
                            }

                            SWE_FOCUSED => {
                                if let Some(w) = self.windows.iter().find(|w| w.hwnd == hwnd_val) {
                                    let id = w.id;
                                    tracing::debug!(%id, "window focused");
                                    return Ok(Event::WindowFocused(id));
                                }
                                // Adopt untracked window gaining focus.
                                if unsafe { is_manageable(hwnd) } {
                                    let id = WindowId(self.next_id);
                                    self.next_id += 1;
                                    if let Some(win) = unsafe { build_window(hwnd, id, &self.float_rules) } {
                                        let win_id = win.id;
                                        self.windows.push(win);
                                        tracing::debug!(%win_id, "adopted focused window");
                                        return Ok(Event::WindowCreated(win_id));
                                    }
                                }
                            }

                            SWE_MOVESIZEEND => {
                                if let Some(pos) = self.windows.iter().position(|w| w.hwnd == hwnd_val) {
                                    let old = self.windows[pos].geometry;
                                    unsafe { refresh_geometry(&mut self.windows[pos]); }
                                    let new = self.windows[pos].geometry;
                                    let id  = self.windows[pos].id;
                                    // Distinguish a border-drag resize from a title-bar move:
                                    // if the window's size changed the user resized it; if only
                                    // the position changed (size unchanged) it was a drag-move.
                                    let size_changed =
                                        new.width  != old.width ||
                                        new.height != old.height;
                                    if size_changed {
                                        tracing::debug!(%id, "user resize end");
                                        return Ok(Event::WindowResized { id });
                                    } else {
                                        tracing::debug!(%id, "move end");
                                        return Ok(Event::WindowMoved { id });
                                    }
                                }
                            }

                            SWE_MINIMIZE_CHANGE => {
                                if let Some(w) = self.windows.iter().find(|w| w.hwnd == hwnd_val) {
                                    let id = w.id;
                                    tracing::debug!(%id, "minimize state changed");
                                    return Ok(Event::WindowMinimizeChanged { id });
                                }
                            }

                            SWE_LOCATIONCHANGE => {
                                // Only process tracked windows that are not hidden by us.
                                if let Some(pos) = self.windows.iter().position(|w| w.hwnd == hwnd_val && !w.wm_hidden) {
                                    use windows::Win32::Foundation::RECT;
                                    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
                                    let mut r = RECT::default();
                                    let ok = unsafe { GetWindowRect(hwnd, &mut r).is_ok() };
                                    if ok {
                                        let new_w = (r.right  - r.left).unsigned_abs();
                                        let new_h = (r.bottom - r.top).unsigned_abs();
                                        let stored = self.windows[pos].geometry;
                                        let dw = (new_w as i32 - stored.width  as i32).unsigned_abs();
                                        let dh = (new_h as i32 - stored.height as i32).unsigned_abs();
                                        let dx = (r.left - stored.x).unsigned_abs();
                                        let dy = (r.top  - stored.y).unsigned_abs();

                                        // Floating window moved: reposition overlay in real-time
                                        // without emitting an event — no main.rs round-trip needed.
                                        if self.windows[pos].floating && (dx > 0 || dy > 0 || dw > 0 || dh > 0) {
                                            let new_rect = Rect::new(r.left, r.top, new_w, new_h);
                                            self.windows[pos].geometry = new_rect;
                                            let ov  = self.windows[pos].overlay_hwnd;
                                            let bw  = self.windows[pos].border_width;
                                            let br  = self.windows[pos].border_radius;
                                            let top = self.windows[pos].is_topmost;
                                            if ov != 0 {
                                                let vr = expand_rect(
                                                    unsafe { visible_rect(hwnd, new_rect) },
                                                    bw as i32,
                                                );
                                                unsafe { position_overlay(ov, hwnd_val, vr, bw, br, top); }
                                            }
                                            continue;
                                        }

                                        // Tiled window resized by external agent (snap assist, etc.):
                                        // emit only when size change exceeds the DWM resize gutter (~8 px).
                                        if dw > 16 || dh > 16 {
                                            self.windows[pos].geometry = Rect::new(
                                                r.left,
                                                r.top,
                                                new_w,
                                                new_h,
                                            );
                                            let id = self.windows[pos].id;
                                            tracing::debug!(%id, dw, dh, "external size change detected");
                                            return Ok(Event::WindowSizeChanged { id });
                                        }
                                    }
                                }
                            }

                            SWE_APP_HIDDEN => {
                                // Application hid its own window (not via WM).
                                // Hide the border overlay so it doesn't float orphaned.
                                if let Some(w) = self.windows.iter_mut()
                                    .find(|w| w.hwnd == hwnd_val && !w.wm_hidden)
                                {
                                    if w.overlay_hwnd != 0 {
                                        let ov = w.overlay_hwnd;
                                        unsafe {
                                            use windows::Win32::Foundation::HWND;
                                            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
                                            let _ = ShowWindow(HWND(ov as *mut _), SW_HIDE);
                                        }
                                        tracing::debug!(id = %w.id, "app-hidden: border overlay hidden");
                                    }
                                }
                                continue;
                            }

                            SWE_SETTINGS_CHANGED => {
                                tracing::debug!("WM_SETTINGCHANGE received — work area may have changed");
                                return Ok(Event::WorkAreaChanged);
                            }

                            _ => {}
                        }
                        continue;
                    }

                    _ => {
                        // Dispatch to WndProc (e.g. WM_PAINT for border overlay windows).
                        unsafe {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                        continue;
                    }
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        Err(Error::Backend("Win32 backend unavailable on this platform".into()))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn monitor_rect(&self) -> Rect {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::RECT;
            use windows::Win32::UI::WindowsAndMessaging::{
                SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
            };
            let mut area = RECT::default();
            unsafe {
                let _ = SystemParametersInfoW(
                    SPI_GETWORKAREA,
                    0,
                    Some(std::ptr::addr_of_mut!(area).cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                );
            }
            Rect::new(
                area.left,
                area.top,
                (area.right  - area.left) as u32,
                (area.bottom - area.top)  as u32,
            )
        }

        #[cfg(not(target_os = "windows"))]
        Rect::new(0, 0, 1920, 1080)
    }

    fn monitor_rect_for_window(&self, id: WindowId) -> Rect {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
            };

            if let Some(win) = self.windows.iter().find(|w| w.id() == id) {
                let hwnd = HWND(win.hwnd as *mut _);
                unsafe {
                    let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut info: MONITORINFO = std::mem::zeroed();
                    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                    if GetMonitorInfoW(hmon, &mut info).as_bool() {
                        let r = info.rcWork;
                        return Rect::new(
                            r.left,
                            r.top,
                            (r.right  - r.left) as u32,
                            (r.bottom - r.top)  as u32,
                        );
                    }
                }
            }
            self.monitor_rect()
        }

        #[cfg(not(target_os = "windows"))]
        self.monitor_rect()
    }

    fn monitor_rects(&self) -> Vec<Rect> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
            use windows::Win32::Graphics::Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
            };

            unsafe extern "system" fn enum_mon(
                hmon:   HMONITOR,
                _hdc:   HDC,
                _lprect: *mut RECT,
                lparam: LPARAM,
            ) -> BOOL {
                let list = &mut *(lparam.0 as *mut Vec<Rect>);
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(hmon, &mut info).as_bool() {
                    let r = info.rcWork;
                    list.push(Rect::new(
                        r.left,
                        r.top,
                        (r.right  - r.left) as u32,
                        (r.bottom - r.top)  as u32,
                    ));
                }
                BOOL(1)
            }

            let mut rects: Vec<Rect> = Vec::new();
            unsafe {
                let _ = EnumDisplayMonitors(
                    HDC(std::ptr::null_mut()),
                    None,
                    Some(enum_mon),
                    LPARAM(&mut rects as *mut Vec<Rect> as isize),
                );
            }
            // Sort left-to-right so monitor 0 = leftmost.
            rects.sort_by_key(|r| r.x);
            if rects.is_empty() { vec![self.monitor_rect()] } else { rects }
        }

        #[cfg(not(target_os = "windows"))]
        vec![self.monitor_rect()]
    }

    fn broadcast_state(&mut self, json: &str) {
        if let Some(ref tx) = self.yasb_tx {
            let _ = tx.try_send(json.to_owned());
        }
    }

    fn system_animations_enabled(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
            };
            let mut enabled: u32 = 1;
            unsafe {
                let _ = SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    Some(std::ptr::addr_of_mut!(enabled).cast()),
                    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
                );
            }
            enabled != 0
        }
        #[cfg(not(target_os = "windows"))]
        true
    }

    fn monitor_full_rect(&self) -> Rect {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
            };
            unsafe {
                let hmon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(hmon, &mut info).as_bool() {
                    let r = info.rcMonitor;
                    return Rect::new(
                        r.left,
                        r.top,
                        (r.right  - r.left) as u32,
                        (r.bottom - r.top)  as u32,
                    );
                }
            }
            self.monitor_rect()
        }
        #[cfg(not(target_os = "windows"))]
        self.monitor_rect()
    }

    fn monitor_full_rect_for_window(&self, id: WindowId) -> Rect {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Gdi::{
                GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
            };
            if let Some(win) = self.windows.iter().find(|w| w.id() == id) {
                let hwnd = HWND(win.hwnd as *mut _);
                unsafe {
                    let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut info: MONITORINFO = std::mem::zeroed();
                    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                    if GetMonitorInfoW(hmon, &mut info).as_bool() {
                        let r = info.rcMonitor;
                        return Rect::new(
                            r.left,
                            r.top,
                            (r.right  - r.left) as u32,
                            (r.bottom - r.top)  as u32,
                        );
                    }
                }
            }
            self.monitor_full_rect()
        }
        #[cfg(not(target_os = "windows"))]
        self.monitor_full_rect()
    }
}
