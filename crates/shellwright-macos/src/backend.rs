//! [`MacosBackend`] — Accessibility API implementation of [`shellwright_core::backend::Backend`].
//!
//! # Strategy
//! We call `AXUIElementSetAttributeValue` for position/size (kAXPositionAttribute /
//! kAXSizeAttribute) and focus (kAXFocusedAttribute).  We never touch Quartz or the
//! compositor — the macOS shell chrome (menu bar, Dock) is untouched.
//!
//! # Known limitations
//! - Full-screen windows entering Mission Control cannot be repositioned.
//! - Sandboxed / Electron apps may deny Accessibility access per-app.
//! - Hiding windows (`NSApplication.hide`) affects the whole app process, not a
//!   single window; per-window hide via CGWindowLevel is used instead.

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

pub struct MacosWindow {
    id: WindowId,
    title: String,
    geometry: Rect,
    floating: bool,
}

impl Window for MacosWindow {
    fn id(&self) -> WindowId { self.id }
    fn title(&self) -> &str { &self.title }
    fn geometry(&self) -> Rect { self.geometry }

    fn set_geometry(&mut self, rect: Rect) -> Result<()> {
        // TODO: Build CGPoint/CGSize from rect.x/y/width/height (core-graphics).
        // TODO: AXUIElementSetAttributeValue(ax_ref, kAXPositionAttribute, position_val)
        // TODO: AXUIElementSetAttributeValue(ax_ref, kAXSizeAttribute, size_val)
        tracing::debug!(id = %self.id, ?rect, "set_geometry");
        self.geometry = rect;
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        // TODO: AXUIElementSetAttributeValue(ax_ref, kAXFocusedAttribute, kCFBooleanTrue)
        // TODO: NSRunningApplication(pid).activate(options: .activateIgnoringOtherApps)
        tracing::debug!(id = %self.id, "focus");
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        // TODO: AXUIElementCopyAttributeValue(ax_ref, kAXCloseButtonAttribute) → close_btn
        // TODO: AXUIElementPerformAction(close_btn, kAXPressAction)
        tracing::debug!(id = %self.id, "close");
        Ok(())
    }

    fn is_floating(&self) -> bool { self.floating }
    fn set_floating(&mut self, floating: bool) { self.floating = floating; }

    fn hide(&mut self) -> Result<()> {
        // TODO: CGSSetWindowLevel(connection, cg_window_id, kCGMinimumWindowLevel)
        //       or: AXUIElementSetAttributeValue(ax_ref, kAXHiddenAttribute, kCFBooleanTrue)
        //       if accessibility supports it for the process.
        tracing::debug!(id = %self.id, "hide");
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        // TODO: Restore CGSSetWindowLevel to kCGNormalWindowLevel (or saved level).
        tracing::debug!(id = %self.id, "show");
        Ok(())
    }
}

// ── Backend ───────────────────────────────────────────────────────────────────

pub struct MacosBackend {
    windows: Vec<MacosWindow>,
    hotkeys: HotkeyManager,
}

impl MacosBackend {
    pub fn new(bindings: &BindingMap) -> Result<Self> {
        // TODO: AXIsProcessTrustedWithOptions({"AXTrustedCheckOptionPrompt": true})
        //       Prompt user to grant Accessibility permission; exit if denied.
        // TODO: Enumerate running apps via NSWorkspace.sharedWorkspace.runningApplications
        //       → filter activationPolicy == .regular → AXUIElementCreateApplication(pid)
        //       → enumerate AXWindows attribute → build MacosWindow list.
        // TODO: Register AXObserver for kAXWindowCreatedNotification /
        //       kAXUIElementDestroyedNotification / kAXFocusedWindowChangedNotification
        //       on each running app; feed events into an mpsc channel.
        tracing::info!("initialising macOS Accessibility backend");

        let hotkeys = HotkeyManager::register(bindings)?;
        Ok(Self { windows: Vec::new(), hotkeys })
    }
}

impl Backend for MacosBackend {
    type W = MacosWindow;

    fn windows(&self) -> Vec<&MacosWindow> {
        self.windows.iter().collect()
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut MacosWindow> {
        self.windows.iter_mut().find(|w| w.id() == id)
    }

    fn next_event(&mut self) -> Result<Event> {
        // The CGEventTap fires on a background thread and sends KeybindingIds
        // over the channel.  Block here until either a hotkey or a window event
        // arrives.
        //
        // TODO: multiplex the hotkey receiver with an AXObserver event channel
        //       (std::sync::mpsc or crossbeam-channel select!) so both sources
        //       feed into a single blocking recv call.
        match self.hotkeys.receiver.recv() {
            Ok(id) => Ok(Event::Keybinding(id)),
            Err(_) => Err(Error::Backend("hotkey channel closed unexpectedly".into())),
        }
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn monitor_rect(&self) -> Rect {
        // TODO: NSScreen.mainScreen?.visibleFrame → convert NSRect to our Rect.
        //       visibleFrame already excludes the menu bar and Dock.
        //       For multi-monitor: NSScreen.screens[active_screen].visibleFrame.
        Rect::new(0, 0, 1920, 1080)
    }
}
