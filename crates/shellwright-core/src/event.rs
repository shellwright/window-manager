//! Platform-normalised event enum.
//!
//! Each backend translates its native event stream into this enum so the
//! core event loop remains completely platform-agnostic (ISO 25010 Portability).

use crate::window::WindowId;

/// Opaque ID matching a configured [`crate::config::Keybinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeybindingId(pub u32);

/// All events the manager core needs to react to.
#[derive(Debug, Clone)]
pub enum Event {
    /// A new window has appeared and should be managed.
    WindowCreated(WindowId),
    /// A window has been destroyed by the application or the OS.
    WindowDestroyed(WindowId),
    /// A window has been given input focus.
    WindowFocused(WindowId),
    /// A floating window was moved/resized by the user.
    WindowMoved { id: WindowId },
    /// A user-configured keybinding fired.
    Keybinding(KeybindingId),
    /// A clean-shutdown request (signal, IPC command, etc.).
    Quit,
}
