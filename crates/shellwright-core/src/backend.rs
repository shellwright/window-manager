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

    /// Return the usable tiling area for the monitor that contains `id`.
    ///
    /// Backends with multi-monitor support override this; the default falls
    /// back to [`Backend::monitor_rect`] (primary monitor).
    fn monitor_rect_for_window(&self, _id: WindowId) -> Rect {
        self.monitor_rect()
    }

    /// Return the **full** physical bounds of the primary monitor, including the
    /// taskbar and any external bar reservations.
    ///
    /// On Windows this is `MONITORINFO.rcMonitor` (the raw pixel bounds); on
    /// other platforms the default delegates to [`monitor_rect`].
    fn monitor_full_rect(&self) -> Rect {
        self.monitor_rect()
    }

    /// Return the full physical bounds of the monitor containing `id`.
    ///
    /// Defaults to [`monitor_full_rect`] (primary monitor).
    fn monitor_full_rect_for_window(&self, _id: WindowId) -> Rect {
        self.monitor_full_rect()
    }

    /// Return the usable tiling areas for **all** connected monitors, sorted
    /// left-to-right by x coordinate.
    ///
    /// Used to assign each monitor its own workspace slice.  The default
    /// returns a single-element vec with the primary monitor rect.
    fn monitor_rects(&self) -> Vec<Rect> {
        vec![self.monitor_rect()]
    }

    /// Broadcast workspace state to any connected status-bar IPC clients.
    ///
    /// `json` is a UTF-8 string terminated with `\n` in komorebi-compatible
    /// format.  Called after every workspace-change event.  The default is a
    /// no-op; the Windows backend writes to `\\.\pipe\shellwright`.
    fn broadcast_state(&mut self, _json: &str) {}

    /// Returns `true` if the host system has client-area animations enabled.
    ///
    /// On Windows this queries `SPI_GETCLIENTAREAANIMATION`.  The default
    /// returns `true` (animations allowed) so non-Windows backends inherit the
    /// config `animations.enabled` flag without an additional system check.
    fn system_animations_enabled(&self) -> bool { true }
}
