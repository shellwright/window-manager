//! [`WindowsBackend`] — Win32 implementation of [`shellwright_core::backend::Backend`].
//!
//! # Strategy
//! Acts as a *client* on top of Explorer / DWM — no shell-chrome changes.
//! Geometry via `SetWindowPos`; focus via `SetForegroundWindow`; close via
//! `PostMessage(WM_CLOSE)`; borders via `DwmSetWindowAttribute(DWMWA_BORDER_COLOR)`.
//!
//! # Runtime event tracking
//! `SetWinEventHook` (OUTOFCONTEXT) posts `WM_SWE` thread messages back to the
//! main message loop so window create / destroy / focus / move events are detected
//! without polling.
//!
//! # Border colours
//! `DWMWA_BORDER_COLOR` (attribute 34) is available on Windows 11 22H2+.
//! On Windows 10 the call silently fails — no error is surfaced.

use std::sync::atomic::{AtomicU32, Ordering};

use shellwright_core::{
    backend::Backend,
    error::Error,
    event::Event,
    hotkey::BindingMap,
    window::{Rect, Window, WindowId},
    Result,
};

use crate::hotkeys::HotkeyManager;

// ── WinEvent thread-message plumbing ─────────────────────────────────────────

static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

const WM_SWE: u32 = 0x0401; // WM_USER + 1

const SWE_CREATED:       usize = 1;
const SWE_DESTROYED:     usize = 2;
const SWE_FOCUSED:       usize = 3;
const SWE_MOVESIZEEND:   usize = 4; // EVENT_SYSTEM_MOVESIZEEND — drag/resize finished

// ── WindowsWindow ─────────────────────────────────────────────────────────────

pub struct WindowsWindow {
    id: WindowId,
    /// Raw HWND stored as `isize` so the struct is `Send`.
    hwnd: isize,
    title: String,
    geometry: Rect,
    floating: bool,
}

impl Window for WindowsWindow {
    fn id(&self) -> WindowId { self.id }
    fn title(&self) -> &str { &self.title }
    fn geometry(&self) -> Rect { self.geometry }

    fn set_geometry(&mut self, rect: Rect) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                IsZoomed, SetWindowPos, ShowWindow,
                SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER,
            };
            let hwnd = HWND(self.hwnd as *mut _);
            // Maximized windows silently ignore SetWindowPos geometry.
            // Restore first so the new tile dimensions take effect.
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            SetWindowPos(
                hwnd,
                HWND(std::ptr::null_mut()),
                rect.x,
                rect.y,
                rect.width as i32,
                rect.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .map_err(|e| Error::Backend(format!("SetWindowPos: {e}")))?;
        }
        self.geometry = rect;
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
            let ok = SetForegroundWindow(HWND(self.hwnd as *mut _));
            if !ok.as_bool() {
                tracing::debug!(id = %self.id, "SetForegroundWindow denied");
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
    fn set_floating(&mut self, floating: bool) { self.floating = floating; }

    fn hide(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            let _ = ShowWindow(HWND(self.hwnd as *mut _), SW_HIDE);
        }
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNA};
            let _ = ShowWindow(HWND(self.hwnd as *mut _), SW_SHOWNA);
        }
        Ok(())
    }

    fn set_border_color(&mut self, rgb: u32) -> Result<()> {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
            let colorref = rgb_to_colorref(rgb);
            // DWMWA_BORDER_COLOR = 34 — Windows 11 22H2+; silently no-ops on older builds.
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
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert `0x00RRGGBB` to Win32 `COLORREF` (`0x00BBGGRR`).
#[cfg(target_os = "windows")]
fn rgb_to_colorref(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    r | (g << 8) | (b << 16)
}

/// Returns `true` if `hwnd` should be managed by shellwright.
///
/// Matches the filtering strategy used by komorebi / GlazeWM:
/// - Visible, no owner, no WS_EX_TOOLWINDOW, has WS_CAPTION, non-empty title.
/// - Not cloaked (virtual desktop, UWP shell windows, etc.).
/// - Not a known system shell window class.
#[cfg(target_os = "windows")]
unsafe fn is_manageable(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWINDOWATTRIBUTE};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindow, GetWindowLongW, GetWindowTextLengthW, IsWindowVisible,
        GWL_EXSTYLE, GWL_STYLE, GW_OWNER, WS_CAPTION, WS_EX_TOOLWINDOW,
    };

    if !IsWindowVisible(hwnd).as_bool() { return false; }

    // Owned windows (dialogs, popups) are child surfaces, not top-level apps.
    if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
        if !owner.0.is_null() { return false; }
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 { return false; }

    // WS_CAPTION = WS_BORDER | WS_DLGFRAME.  Windows without a caption bar
    // (popup menus, splash screens, overlays) must not be tiled.
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 == 0 { return false; }

    if GetWindowTextLengthW(hwnd) == 0 { return false; }

    // DWMWA_CLOAKED (14) — non-zero for windows on other virtual desktops,
    // UWP shell windows, and Windows animations.  Do not manage cloaked windows.
    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWINDOWATTRIBUTE(14),
        &mut cloaked as *mut u32 as *mut _,
        std::mem::size_of::<u32>() as u32,
    );
    if cloaked != 0 { return false; }

    // Exclude known system shell window classes that pass all other filters.
    let mut class_buf = [0u16; 256];
    let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
    if class_len > 0 {
        let class = String::from_utf16_lossy(&class_buf[..class_len]);
        match class.as_str() {
            "Shell_TrayWnd"           // Windows taskbar
            | "Shell_SecondaryTrayWnd" // Secondary monitor taskbar
            | "Progman"               // Program Manager / desktop
            | "WorkerW"               // Desktop wallpaper worker
            | "DV2ControlHost"        // Start menu host
            | "Windows.UI.Core.CoreWindow" // UWP shell window
            => return false,
            _ => {}
        }
    }

    true
}

/// Build a [`WindowsWindow`] from a raw HWND.  Returns `None` on any failure.
#[cfg(target_os = "windows")]
unsafe fn build_window(
    hwnd: windows::Win32::Foundation::HWND,
    id: WindowId,
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
        (r.right - r.left) as u32,
        (r.bottom - r.top) as u32,
    );

    tracing::debug!(%id, %title, ?geometry, "window registered");
    Some(WindowsWindow { id, hwnd: hwnd.0 as isize, title, geometry, floating: false })
}

// ── Update stored geometry for an existing window from Win32 ─────────────────

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
            (r.right - r.left) as u32,
            (r.bottom - r.top) as u32,
        );
    }
}

// ── Initial window enumeration ────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn enumerate_windows(next_id: &mut u64) -> Vec<WindowsWindow> {
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
            unsafe { build_window(hwnd, id) }
        })
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn enumerate_windows(_next_id: &mut u64) -> Vec<WindowsWindow> { Vec::new() }

// ── WinEvent hook callback ────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
unsafe extern "system" fn win_event_proc(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    id_object: i32,
    id_child: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if id_object != 0 || id_child != 0 { return; }
    if hwnd.0.is_null() { return; }

    let wp: usize = match event {
        0x8002 => SWE_CREATED,      // EVENT_OBJECT_SHOW — window becomes visible (more reliable than CREATE)
        0x8001 => SWE_DESTROYED,    // EVENT_OBJECT_DESTROY
        0x0003 => SWE_FOCUSED,      // EVENT_SYSTEM_FOREGROUND
        0x000B => SWE_MOVESIZEEND,  // EVENT_SYSTEM_MOVESIZEEND
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

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct WindowsBackend {
    windows: Vec<WindowsWindow>,
    next_id: u64,
    _hotkeys: HotkeyManager,
    _hooks: HookGuards,
}

impl WindowsBackend {
    pub fn new(bindings: &BindingMap) -> Result<Self> {
        tracing::info!("initialising Win32 backend");

        let hotkeys = HotkeyManager::register(bindings)?;

        #[cfg(target_os = "windows")]
        let (mut next_id, hooks) = {
            use windows::Win32::Foundation::HMODULE;
            use windows::Win32::System::Threading::GetCurrentThreadId;
            use windows::Win32::UI::Accessibility::SetWinEventHook;

            let tid = unsafe { GetCurrentThreadId() };
            HOOK_THREAD_ID.store(tid, Ordering::Relaxed);

            // WINEVENT_OUTOFCONTEXT (0x0000) | WINEVENT_SKIPOWNPROCESS (0x0002)
            let flags: u32 = 0x0002;

            let hook_handles: Vec<isize> = unsafe {
                vec![
                    // Window show — fires when window first becomes visible; title is set by now.
                    // More reliable than EVENT_OBJECT_CREATE (0x8000) which fires too early.
                    SetWinEventHook(0x8002, 0x8002, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // Window destroy
                    SetWinEventHook(0x8001, 0x8001, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // Foreground (focus) change
                    SetWinEventHook(0x0003, 0x0003, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
                    // Move/resize end — for snap-to-tile on drag
                    SetWinEventHook(0x000B, 0x000B, HMODULE(std::ptr::null_mut()), Some(win_event_proc), 0, 0, flags).0 as isize,
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

        let windows = enumerate_windows(&mut next_id);
        tracing::info!(count = windows.len(), "initial window list");

        Ok(Self { windows, next_id, _hotkeys: hotkeys, _hooks: hooks })
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
                GetMessageW, MSG, WM_HOTKEY, WM_QUIT,
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
                        tracing::debug!(?id, "WM_HOTKEY");
                        return Ok(Event::Keybinding(id));
                    }

                    WM_QUIT => return Ok(Event::Quit),

                    WM_SWE => {
                        let hwnd_val: isize = msg.lParam.0;
                        let hwnd = HWND(hwnd_val as *mut _);

                        match msg.wParam.0 {
                            SWE_CREATED => {
                                if self.windows.iter().any(|w| w.hwnd == hwnd_val) {
                                    continue;
                                }
                                if unsafe { is_manageable(hwnd) } {
                                    let id = WindowId(self.next_id);
                                    self.next_id += 1;
                                    if let Some(win) = unsafe { build_window(hwnd, id) } {
                                        let win_id = win.id;
                                        self.windows.push(win);
                                        return Ok(Event::WindowCreated(win_id));
                                    }
                                }
                            }

                            SWE_DESTROYED => {
                                if let Some(pos) = self.windows.iter().position(|w| w.hwnd == hwnd_val) {
                                    let id = self.windows[pos].id;
                                    self.windows.remove(pos);
                                    tracing::debug!(%id, "window destroyed");
                                    return Ok(Event::WindowDestroyed(id));
                                }
                            }

                            SWE_FOCUSED => {
                                if let Some(w) = self.windows.iter().find(|w| w.hwnd == hwnd_val) {
                                    let id = w.id;
                                    tracing::debug!(%id, "window focused");
                                    return Ok(Event::WindowFocused(id));
                                }
                                // Adopt previously untracked window that gains focus.
                                if unsafe { is_manageable(hwnd) } {
                                    let id = WindowId(self.next_id);
                                    self.next_id += 1;
                                    if let Some(win) = unsafe { build_window(hwnd, id) } {
                                        let win_id = win.id;
                                        self.windows.push(win);
                                        tracing::debug!(%win_id, "adopted focused window");
                                        return Ok(Event::WindowCreated(win_id));
                                    }
                                }
                            }

                            SWE_MOVESIZEEND => {
                                if let Some(pos) = self.windows.iter().position(|w| w.hwnd == hwnd_val) {
                                    // Refresh geometry so the WM knows where the user dragged it.
                                    unsafe { refresh_geometry(&mut self.windows[pos]); }
                                    let id = self.windows[pos].id;
                                    tracing::debug!(%id, "move/resize end");
                                    return Ok(Event::WindowMoved { id });
                                }
                            }

                            _ => {}
                        }
                        continue;
                    }

                    _ => continue,
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
                        let r = info.rcWork; // work area (excludes taskbar on that monitor)
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
}
