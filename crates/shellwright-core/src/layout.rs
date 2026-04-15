//! Tiling layout algorithms.
//!
//! Each variant of [`LayoutKind`] maps to a pure function that takes a screen
//! [`Rect`] and a window count and returns a `Vec<Slot>` — one slot per window.
//! No platform code touches this module (ISO 25010 Portability).

use crate::window::Rect;
use serde::{Deserialize, Serialize};

/// Available tiling strategies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    /// Fibonacci dwindle spiral — window 0 takes the first half, the rest recurse
    /// into the remaining half, alternating horizontal/vertical splits. Default.
    #[default]
    Fibonacci,
    /// Binary space partition — recursively halves the screen.
    Bsp,
    /// Fixed number of equal-width columns.
    Columns { count: u8 },
    /// Single maximised window; the rest are hidden behind it.
    Monocle,
    /// All windows unmanaged / floating.
    Float,
}

/// Geometry assignment for one window slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub rect: Rect,
}

/// Compute slot geometry for `count` windows on `screen` using `kind`.
///
/// Returns an empty `Vec` when `count == 0` or the layout produces no tiles
/// (e.g. [`LayoutKind::Float`]).
pub fn compute(kind: &LayoutKind, screen: Rect, count: usize) -> Vec<Slot> {
    match kind {
        LayoutKind::Fibonacci            => fibonacci(screen, count),
        LayoutKind::Bsp                  => bsp(screen, count),
        LayoutKind::Columns { count: c } => columns(screen, count, *c as usize),
        LayoutKind::Monocle              => (0..count.min(1)).map(|_| Slot { rect: screen }).collect(),
        LayoutKind::Float                => vec![],
    }
}

// ── Fibonacci dwindle ─────────────────────────────────────────────────────────

/// Fibonacci / dwindle spiral layout.
///
/// Window 0 occupies the "head" half of the screen.  The remaining windows
/// recurse into the "tail" half, alternating between horizontal and vertical
/// splits at each level.
///
/// ```text
/// 1 window :  [ 1 ]
/// 2 windows:  [ 1 | 2 ]           (H-split)
/// 3 windows:  [ 1 | 2 ]           (1 = left half, 2/3 split V in right half)
///                  ---
///                  [ 3 ]
/// 4 windows:  similar, spiralling inward
/// ```
fn fibonacci(screen: Rect, count: usize) -> Vec<Slot> {
    if count == 0 { return vec![]; }

    let mut slots  = Vec::with_capacity(count);
    let mut area   = screen;
    let mut split_h = true; // start with a left/right (horizontal axis) split

    for i in 0..count {
        if i == count - 1 {
            // Last window fills whatever space remains.
            slots.push(Slot { rect: area });
        } else if split_h {
            let half = area.width / 2;
            slots.push(Slot {
                rect: Rect::new(area.x, area.y, half, area.height),
            });
            area = Rect::new(area.x + half as i32, area.y, area.width - half, area.height);
            split_h = false;
        } else {
            let half = area.height / 2;
            slots.push(Slot {
                rect: Rect::new(area.x, area.y, area.width, half),
            });
            area = Rect::new(area.x, area.y + half as i32, area.width, area.height - half);
            split_h = true;
        }
    }

    slots
}

// ── BSP ───────────────────────────────────────────────────────────────────────

fn bsp(screen: Rect, count: usize) -> Vec<Slot> {
    if count == 0 { return vec![]; }
    if count == 1 { return vec![Slot { rect: screen }]; }
    let split_h = screen.width >= screen.height;
    let (a, b) = if split_h {
        let half = screen.width / 2;
        (
            Rect::new(screen.x, screen.y, half, screen.height),
            Rect::new(screen.x + half as i32, screen.y, screen.width - half, screen.height),
        )
    } else {
        let half = screen.height / 2;
        (
            Rect::new(screen.x, screen.y, screen.width, half),
            Rect::new(screen.x, screen.y + half as i32, screen.width, screen.height - half),
        )
    };
    let left = count / 2;
    let mut slots = bsp(a, left);
    slots.extend(bsp(b, count - left));
    slots
}

// ── Columns ───────────────────────────────────────────────────────────────────

fn columns(screen: Rect, count: usize, cols: usize) -> Vec<Slot> {
    if count == 0 || cols == 0 { return vec![]; }
    let col_w = screen.width / cols as u32;
    (0..count)
        .map(|i| Slot {
            rect: Rect::new(
                screen.x + (i % cols) as i32 * col_w as i32,
                screen.y,
                col_w,
                screen.height,
            ),
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect { Rect::new(0, 0, 1920, 1080) }

    #[test]
    fn fibonacci_one_window_fills_screen() {
        let s = compute(&LayoutKind::Fibonacci, screen(), 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].rect, screen());
    }

    #[test]
    fn fibonacci_two_windows_cover_width() {
        let s = compute(&LayoutKind::Fibonacci, screen(), 2);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].rect.width + s[1].rect.width, 1920);
    }

    #[test]
    fn fibonacci_zero_windows() {
        assert!(compute(&LayoutKind::Fibonacci, screen(), 0).is_empty());
    }

    #[test]
    fn bsp_zero_windows() {
        assert!(compute(&LayoutKind::Bsp, screen(), 0).is_empty());
    }

    #[test]
    fn bsp_one_window_fills_screen() {
        let slots = compute(&LayoutKind::Bsp, screen(), 1);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].rect, screen());
    }

    #[test]
    fn bsp_two_windows_cover_screen_width() {
        let slots = compute(&LayoutKind::Bsp, screen(), 2);
        assert_eq!(slots.len(), 2);
        let total_w: u32 = slots.iter().map(|s| s.rect.width).sum();
        assert_eq!(total_w, 1920);
    }

    #[test]
    fn monocle_always_one_slot() {
        let slots = compute(&LayoutKind::Monocle, screen(), 5);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].rect, screen());
    }

    #[test]
    fn float_produces_no_slots() {
        assert!(compute(&LayoutKind::Float, screen(), 3).is_empty());
    }

    #[test]
    fn columns_correct_count() {
        let slots = compute(&LayoutKind::Columns { count: 3 }, screen(), 6);
        assert_eq!(slots.len(), 6);
    }
}
