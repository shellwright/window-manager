//! Platform-agnostic window primitives.
//!
//! The [`Window`] trait is the primary portability seam (ISO 25010 §4.2).
//! Platform backends implement it; the rest of the manager only sees this trait.

use crate::Result;

/// Opaque, stable identifier for a managed window.
///
/// Each backend assigns IDs; they are never reused within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u64);

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "win:{}", self.0)
    }
}

/// Axis-aligned rectangle in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// Platform-agnostic view of a managed window.
///
/// # Contract
/// - All methods are infallible where possible; they return `Result` only when
///   a syscall is required.
/// - Implementations must be `Send + Sync` so the event loop can run on any
///   thread.
pub trait Window: Send + Sync + 'static {
    fn id(&self) -> WindowId;
    fn title(&self) -> &str;
    fn geometry(&self) -> Rect;
    fn set_geometry(&mut self, rect: Rect) -> Result<()>;
    fn focus(&mut self) -> Result<()>;
    fn close(&mut self) -> Result<()>;
    fn is_floating(&self) -> bool;
    fn set_floating(&mut self, floating: bool);
    /// Make the window invisible on screen without destroying it.
    /// Used when switching workspaces to hide off-screen windows.
    fn hide(&mut self) -> Result<()>;
    /// Restore a hidden window to visibility.
    fn show(&mut self) -> Result<()>;
    /// Set the window border colour via the compositor API.
    ///
    /// `rgb` is `0x00RRGGBB`.  Backends that support coloured borders (e.g.
    /// Windows 11 via `DwmSetWindowAttribute(DWMWA_BORDER_COLOR)`) override
    /// this; all others use the provided no-op default.
    fn set_border_color(&mut self, _rgb: u32) -> Result<()> { Ok(()) }

    /// Returns `true` if the window is currently minimised (iconic).
    ///
    /// Minimised windows must not count toward the tiling layout.
    fn is_minimized(&self) -> bool { false }

    /// Create or update a thick GDI overlay border drawn around this window.
    ///
    /// `rgb` is `0x00RRGGBB`; `width` is the border thickness in physical pixels.
    /// Backends that implement overlay borders (Windows via `SetWindowRgn`) override
    /// this; all others use the provided no-op default.
    fn set_border_overlay(&mut self, _rgb: u32, _width: u32, _radius: u32) -> Result<()> { Ok(()) }

    /// Hide the border overlay without destroying it.
    ///
    /// Called for fullscreen and minimised windows where no visible border is wanted.
    fn hide_border_overlay(&mut self) -> Result<()> { Ok(()) }

    /// Set window opacity for animation purposes (0 = fully transparent, 255 = opaque).
    ///
    /// Backends that support layered windows (Win32 `WS_EX_LAYERED`) override
    /// this; all others use the provided no-op default.
    fn set_alpha(&mut self, _alpha: u8) -> Result<()> { Ok(()) }

    /// Remove the opacity override applied by [`set_alpha`] and restore normal
    /// window rendering.
    fn clear_alpha(&mut self) -> Result<()> { Ok(()) }

    /// Place this window above all non-topmost windows (`true`) or restore
    /// normal z-order (`false`).
    ///
    /// Must be called when a window enters or leaves floating/fullscreen mode.
    /// Backends implement this via `SetWindowPos(HWND_TOPMOST / HWND_NOTOPMOST)`;
    /// the default is a no-op.
    fn set_topmost(&mut self, _topmost: bool) -> Result<()> { Ok(()) }
}

// ── Tests ────────────────────────────────────────────────────────────────────
// ISO 29119 / IEEE 730: unit tests ship with every module.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_new_roundtrip() {
        let r = Rect::new(-10, 20, 1920, 1080);
        assert_eq!(r.x, -10);
        assert_eq!(r.y, 20);
        assert_eq!(r.width, 1920);
        assert_eq!(r.height, 1080);
    }

    #[test]
    fn window_id_display() {
        assert_eq!(WindowId(42).to_string(), "win:42");
    }
}
