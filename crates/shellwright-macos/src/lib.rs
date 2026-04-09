//! macOS Accessibility API backend.
//!
//! # Strategy
//! macOS does not allow replacing the Quartz compositor.  Window management is
//! done entirely through the Accessibility API (`AXUIElement`), which requires
//! the user to grant "Accessibility" permission in System Settings.
//!
//! - **Enumeration** — `NSWorkspace.runningApplications` → `AXUIElement`.
//! - **Events**      — `AXObserver` callbacks (window created/destroyed/moved).
//! - **Layout**      — `AXUIElementSetAttributeValue` for `AXPosition`/`AXSize`.
//!
//! # Limitations
//! - Full-screen windows managed by Mission Control cannot be repositioned.
//! - Some apps (Electron, sandboxed apps) restrict Accessibility access.
//! - Menu-bar hiding requires disabling SIP — not done by default.

mod backend;
mod hotkeys;

pub use backend::MacosBackend;
