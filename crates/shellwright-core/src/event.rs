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
    /// A window was moved by the user (drag — position changed).
    WindowMoved { id: WindowId },
    /// A window was resized by the user (border drag — size changed, not position).
    ///
    /// Unlike [`WindowMoved`], the tiling layout should NOT be reapplied; the
    /// window keeps its user-defined size until the next explicit layout operation.
    WindowResized { id: WindowId },
    /// A window was minimised or restored from minimised state.
    ///
    /// Either transition triggers a full relayout so other tiles can
    /// expand into / contract out of the freed slot.
    WindowMinimizeChanged { id: WindowId },
    /// A window's size or position changed externally (not by shellwright).
    ///
    /// Fired when `EVENT_OBJECT_LOCATIONCHANGE` detects that a window's
    /// geometry no longer matches the value shellwright last stored — e.g.
    /// a browser going into or out of native fullscreen (F11 / YouTube).
    /// The event loop should respond by re-evaluating borders only (no relayout).
    WindowSizeChanged { id: WindowId },
    /// A user-configured keybinding fired.
    Keybinding(KeybindingId),
    /// A clean-shutdown request (signal, IPC command, etc.).
    Quit,
    /// Monitor work area changed (appbar registered/unregistered, e.g. YASB late-start).
    WorkAreaChanged,
}
