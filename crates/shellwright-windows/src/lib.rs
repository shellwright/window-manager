//! Win32 backend for the window manager.
//!
//! # Strategy
//! Windows has no public API to replace the desktop window manager (DWM).
//! Instead, this backend operates as a client layer on top of Explorer/DWM:
//!
//! - **Enumeration** — `EnumWindows` to discover existing top-level windows.
//! - **Events**      — `SetWinEventHook` (EVENT_OBJECT_CREATE/DESTROY/FOCUS).
//! - **Layout**      — `SetWindowPos` with `SWP_NOZORDER | SWP_NOACTIVATE`.
//! - **Workspaces**  — `IVirtualDesktopManager` COM interface.
//!
//! # Permissions
//! No elevated privileges are required for basic operation.  Moving windows
//! owned by elevated processes does require matching elevation.

mod backend;
mod hotkeys;

pub use backend::WindowsBackend;
