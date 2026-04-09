//! Named groups of windows with an associated tiling layout.

use std::collections::HashMap;

use crate::{layout::LayoutKind, window::{Rect, WindowId}};

/// A workspace groups windows under a named label and a chosen layout.
///
/// The manager maintains a list of workspaces; each output (monitor) shows
/// exactly one workspace at a time.
#[derive(Debug)]
pub struct Workspace {
    pub name: String,
    pub layout: LayoutKind,
    windows: Vec<WindowId>,
    pub focused: Option<WindowId>,
    /// Per-window fullscreen state: maps a window to its saved pre-fullscreen
    /// geometry so it can be restored when toggled off.
    fullscreen: HashMap<WindowId, Rect>,
}

impl Workspace {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            layout: LayoutKind::default(),
            windows: Vec::new(),
            focused: None,
            fullscreen: HashMap::new(),
        }
    }

    pub fn windows(&self) -> &[WindowId] {
        &self.windows
    }

    /// Mutable access to the window order — used by `MoveNext`/`MovePrev` to
    /// swap positions in the tiling list without removing and re-inserting.
    pub fn windows_mut(&mut self) -> &mut Vec<WindowId> {
        &mut self.windows
    }

    pub fn add_window(&mut self, id: WindowId) {
        if !self.windows.contains(&id) {
            self.windows.push(id);
            if self.focused.is_none() {
                self.focused = Some(id);
            }
        }
    }

    pub fn remove_window(&mut self, id: WindowId) {
        self.windows.retain(|w| *w != id);
        if self.focused == Some(id) {
            self.focused = self.windows.last().copied();
        }
        self.fullscreen.remove(&id);
    }

    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains(&id)
    }

    // ── Fullscreen ────────────────────────────────────────────────────────────

    /// Returns `true` if `id` is currently tracked as fullscreen on this workspace.
    pub fn is_fullscreen(&self, id: WindowId) -> bool {
        self.fullscreen.contains_key(&id)
    }

    /// Mark `id` as fullscreen, saving `saved_geometry` for later restoration.
    pub fn enter_fullscreen(&mut self, id: WindowId, saved_geometry: Rect) {
        self.fullscreen.insert(id, saved_geometry);
    }

    /// Remove `id` from fullscreen tracking.  Returns the saved geometry if
    /// the window was previously fullscreen so callers can restore it.
    pub fn exit_fullscreen(&mut self, id: WindowId) -> Option<Rect> {
        self.fullscreen.remove(&id)
    }

    /// Iterate over all windows currently in fullscreen on this workspace.
    pub fn fullscreen_windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.fullscreen.keys().copied()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::Rect;

    fn r() -> Rect { Rect::new(0, 0, 1920, 1080) }

    #[test]
    fn add_sets_focus_on_first_window() {
        let mut ws = Workspace::new("1");
        ws.add_window(WindowId(1));
        assert_eq!(ws.focused, Some(WindowId(1)));
    }

    #[test]
    fn remove_clears_focus_when_last() {
        let mut ws = Workspace::new("1");
        ws.add_window(WindowId(1));
        ws.remove_window(WindowId(1));
        assert!(ws.focused.is_none());
        assert!(ws.windows().is_empty());
    }

    #[test]
    fn no_duplicate_windows() {
        let mut ws = Workspace::new("1");
        ws.add_window(WindowId(1));
        ws.add_window(WindowId(1));
        assert_eq!(ws.windows().len(), 1);
    }

    #[test]
    fn fullscreen_enter_exit_roundtrip() {
        let mut ws = Workspace::new("1");
        ws.add_window(WindowId(1));
        assert!(!ws.is_fullscreen(WindowId(1)));

        ws.enter_fullscreen(WindowId(1), r());
        assert!(ws.is_fullscreen(WindowId(1)));

        let saved = ws.exit_fullscreen(WindowId(1));
        assert_eq!(saved, Some(r()));
        assert!(!ws.is_fullscreen(WindowId(1)));
    }

    #[test]
    fn remove_clears_fullscreen_state() {
        let mut ws = Workspace::new("1");
        ws.add_window(WindowId(1));
        ws.enter_fullscreen(WindowId(1), r());
        ws.remove_window(WindowId(1));
        assert!(!ws.is_fullscreen(WindowId(1)));
    }
}
