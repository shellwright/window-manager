//! [`WindowsBackend`] — Win32 implementation of [`shellwright_core::backend::Backend`].
//!
//! # Strategy
//! We act as a *client* on top of Explorer / DWM — never touching the shell
//! chrome ourselves.  Window geometry is applied via `SetWindowPos`; focus via
//! `SetForegroundWindow`; close via `PostMessage(WM_CLOSE)`.
//!
//! Workspace visibility is implemented with `ShowWindow(SW_HIDE/SW_SHOW)` so
//! off-screen workspaces consume no desktop space.

use shellwright_core::{
    backend::Backend,
    error::Error,
    event::Event,
    hotkey::BindingMap,
    window::{Rect, Window, WindowId},
    Result,
};

use crate::hotkeys::HotkeyManager;

// ── Window ────────────────────────────────────────────────────────────────────

pub struct WindowsWindow {
    id: WindowId,
    /// Raw HWND stored as isize so the struct is Send.
    /// Re-constituted as HWND(self.hwnd as *mut _) at every call site.
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
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
            };
            use windows::Win32::Foundation::HWND;

            unsafe {
                SetWindowPos(
                    HWND(self.hwnd as *mut _),
                    HWND(std::ptr::null_mut()), // ignored with SWP_NOZORDER
                    rect.x,
                    rect.y,
                    rect.width as i32,
                    rect.height as i32,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .map_err(|e| Error::Backend(format!("SetWindowPos: {e}")))?;
            }
        }
        self.geometry = rect;
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
            use windows::Win32::Foundation::HWND;

            // SetForegroundWindow returns FALSE when focus cannot be stolen
            // (e.g. during a UAC prompt) — log but do not error.
            let ok = unsafe { SetForegroundWindow(HWND(self.hwnd as *mut _)) };
            if !ok.as_bool() {
                tracing::debug!(id = %self.id, "SetForegroundWindow denied (elevated target?)");
            }
        }
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
            use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

            unsafe {
                PostMessageW(
                    HWND(self.hwnd as *mut _),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
                .map_err(|e| Error::Backend(format!("PostMessageW(WM_CLOSE): {e}")))?;
            }
        }
        Ok(())
    }

    fn is_floating(&self) -> bool { self.floating }
    fn set_floating(&mut self, floating: bool) { self.floating = floating; }

    fn hide(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
            use windows::Win32::Foundation::HWND;

            unsafe { ShowWindow(HWND(self.hwnd as *mut _), SW_HIDE); }
        }
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNA};
            use windows::Win32::Foundation::HWND;

            // SW_SHOWNA — show without activating (activation is handled
            // explicitly by focus() when the user switches workspaces).
            unsafe { ShowWindow(HWND(self.hwnd as *mut _), SW_SHOWNA); }
        }
        Ok(())
    }
}

// ── Initial window enumeration ────────────────────────────────────────────────

/// Collect all manageable top-level windows at startup.
///
/// Criteria (same as komorebi / GlazeWM):
/// - Visible (`IsWindowVisible`)
/// - No owner window (owned = dialogs / tooltips, not top-level)
/// - Not a tool window (`WS_EX_TOOLWINDOW`)
/// - Has a non-empty title
#[cfg(target_os = "windows")]
fn enumerate_windows() -> Vec<WindowsWindow> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
        GetWindow, GW_OWNER, WINDOW_EX_STYLE,
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);

        // Must be visible
        if !IsWindowVisible(hwnd).as_bool() { return BOOL(1); }
        // Must be a true top-level (no owner window)
        if GetWindow(hwnd, GW_OWNER).0 != std::ptr::null_mut() { return BOOL(1); }
        // Skip tool windows (system tray icons, floating toolbars, etc.)
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        if ex.contains(WS_EX_TOOLWINDOW) { return BOOL(1); }
        // Skip untitled windows
        if GetWindowTextLengthW(hwnd) == 0 { return BOOL(1); }

        list.push(hwnd);
        BOOL(1)
    }

    let mut raw: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut raw as *mut Vec<HWND> as isize),
        );
    }

    let mut next_id = 1u64;
    raw.into_iter().filter_map(|hwnd| {
        unsafe {
            // Title
            let len = GetWindowTextLengthW(hwnd) as usize;
            let mut buf = vec![0u16; len + 1];
            GetWindowTextW(hwnd, &mut buf);
            let title = String::from_utf16_lossy(&buf[..len]);

            // Geometry
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_err() { return None; }
            let geometry = Rect::new(
                r.left,
                r.top,
                (r.right - r.left) as u32,
                (r.bottom - r.top) as u32,
            );

            let id = WindowId(next_id);
            next_id += 1;

            tracing::debug!(%id, %title, ?geometry, "enumerated window");
            Some(WindowsWindow {
                id,
                hwnd: hwnd.0 as isize,
                title,
                geometry,
                floating: false,
            })
        }
    })
    .collect()
}

#[cfg(not(target_os = "windows"))]
fn enumerate_windows() -> Vec<WindowsWindow> { Vec::new() }

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct WindowsBackend {
    windows: Vec<WindowsWindow>,
    _hotkeys: HotkeyManager,
}

impl WindowsBackend {
    pub fn new(bindings: &BindingMap) -> Result<Self> {
        tracing::info!("initialising Win32 backend");

        let hotkeys = HotkeyManager::register(bindings)?;
        let windows = enumerate_windows();
        tracing::info!(count = windows.len(), "initial window list");

        // TODO: SetWinEventHook for EVENT_OBJECT_CREATE / DESTROY / FOCUS so
        //       the event loop learns about new windows without polling.
        //       The hook callback should PostMessageW a WM_APP+1 message
        //       carrying the HWND and event type, which next_event() can then
        //       translate into Event::WindowCreated / WindowDestroyed / WindowFocused.

        Ok(Self { windows, _hotkeys: hotkeys })
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
                    // TODO: handle WM_APP+1 messages posted by the WinEvent hook
                    //       to surface WindowCreated / WindowDestroyed / WindowFocused.
                    _ => continue,
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        Err(Error::Backend("Win32 backend not available on this platform".into()))
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
        Rect::new(0, 0, 1920, 1080) // compile-time stub only; never reached
    }
}
