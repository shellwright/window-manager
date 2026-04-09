//! The [`Backend`] trait — the primary portability seam.
//!
//! ISO 25010 §4.2 (Portability / Adaptability): platform-specific behaviour is
//! fully encapsulated behind this interface.  The core event loop only ever
//! calls methods defined here.

use crate::{event::Event, window::{Rect, Window, WindowId}, Result};

/// Contract every platform backend must satisfy.
///
/// # Threading
/// The event loop owns the backend exclusively; `Send` is required so it can
/// be moved across threads (e.g. for a dedicated event thread).
pub trait Backend: Send + 'static {
    /// The concrete window type this backend manages.
    type W: Window;

    /// Return immutable views of all currently managed windows.
    fn windows(&self) -> Vec<&Self::W>;

    /// Return a mutable reference to a window by id.
    fn window_mut(&mut self, id: WindowId) -> Option<&mut Self::W>;

    /// Block until the next platform event arrives and return it normalised.
    ///
    /// Implementations should translate every native event into
    /// [`Event`]; unknown events should be silently dropped (do not error).
    fn next_event(&mut self) -> Result<Event>;

    /// Called after layout has been applied so the backend can flush
    /// buffered changes to the display server in one round-trip.
    fn flush(&mut self) -> Result<()>;

    /// Return the usable tiling area for the primary monitor.
    ///
    /// On Windows this is the work area (excluding the taskbar).
    /// On macOS this is `NSScreen.visibleFrame` (excluding menu bar and Dock).
    /// On Wayland this is the output geometry minus any wlr-layer-shell reservations.
    fn monitor_rect(&self) -> Rect;
}
