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
    /// Single maximised window; the rest are stacked behind it in z-order.
    /// All windows occupy the full screen rect; focus switching raises the
    /// focused window to the top without a layout recalculation.
    Monocle,
    /// Ultrawide-optimised three-column layout.
    ///
    /// Window 0 occupies the centre 50 % of the screen width.  Subsequent
    /// windows alternate between a 25 % left column and a 25 % right
    /// column, stacked vertically per side:
    ///
    /// ```text
    /// odd-indexed (1,3,5…) → left   25%
    /// window 0             → centre 50%
    /// even-indexed (2,4,6…)→ right  25%
    /// ```
    CenterMain,
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
        // Monocle: every window gets the full screen rect.  Z-order (raise) is
        // handled in the event loop so the focused window is always on top.
        LayoutKind::Monocle              => (0..count).map(|_| Slot { rect: screen }).collect(),
        LayoutKind::CenterMain           => center_main(screen, count),
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

// ── CenterMain ───────────────────────────────────────────────────────────────

/// Ultrawide three-column layout.
///
/// * Slot 0  → centre column (50 % width).
/// * Odd-indexed slots  (1, 3, 5 …) → left  column (25 % width), stacked top→bottom.
/// * Even-indexed slots (2, 4, 6 …) → right column (25 % width), stacked top→bottom.
fn center_main(screen: Rect, count: usize) -> Vec<Slot> {
    if count == 0 { return vec![]; }
    if count == 1 { return vec![Slot { rect: screen }]; }

    let side_w   = screen.width / 4;                // 25 % each side
    let center_w = screen.width - side_w * 2;       // 50 % centre

    let center_rect = Rect::new(
        screen.x + side_w as i32,
        screen.y,
        center_w,
        screen.height,
    );

    // Count windows destined for each side column.
    let left_count  = (count - 1 + 1) / 2; // ceil((count-1) / 2)  →  1,3,5…
    let right_count = (count - 1)     / 2; // floor((count-1) / 2) →  2,4,6…

    let left_h  = if left_count  > 0 { screen.height / left_count  as u32 } else { 0 };
    let right_h = if right_count > 0 { screen.height / right_count as u32 } else { 0 };

    let mut slots     = Vec::with_capacity(count);
    let mut left_i  = 0usize;
    let mut right_i = 0usize;

    slots.push(Slot { rect: center_rect });

    for win_i in 1..count {
        if win_i % 2 == 1 {
            // Left column
            let y_off = left_i as i32 * left_h as i32;
            let h = if left_i + 1 == left_count {
                screen.height.saturating_sub(left_i as u32 * left_h)
            } else {
                left_h
            };
            slots.push(Slot {
                rect: Rect::new(screen.x, screen.y + y_off, side_w, h),
            });
            left_i += 1;
        } else {
            // Right column
            let y_off = right_i as i32 * right_h as i32;
            let h = if right_i + 1 == right_count {
                screen.height.saturating_sub(right_i as u32 * right_h)
            } else {
                right_h
            };
            slots.push(Slot {
                rect: Rect::new(
                    screen.x + side_w as i32 + center_w as i32,
                    screen.y + y_off,
                    side_w,
                    h,
                ),
            });
            right_i += 1;
        }
    }

    slots
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
    fn monocle_returns_slot_per_window() {
        let slots = compute(&LayoutKind::Monocle, screen(), 5);
        // Every window gets the full screen rect — z-order handles focus visibility.
        assert_eq!(slots.len(), 5);
        for s in &slots {
            assert_eq!(s.rect, screen());
        }
    }

    #[test]
    fn center_main_one_window_fills_screen() {
        let slots = compute(&LayoutKind::CenterMain, screen(), 1);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].rect, screen());
    }

    #[test]
    fn center_main_two_windows_centre_plus_left() {
        let slots = compute(&LayoutKind::CenterMain, screen(), 2);
        assert_eq!(slots.len(), 2);
        // Centre must be widest.
        assert!(slots[0].rect.width > slots[1].rect.width);
        // Together they span the full width.
        assert_eq!(slots[0].rect.width + slots[1].rect.width, 1920);
    }

    #[test]
    fn center_main_three_windows_total_width() {
        let slots = compute(&LayoutKind::CenterMain, screen(), 3);
        assert_eq!(slots.len(), 3);
        // Left + centre + right should span full width.
        let total: u32 = slots[0].rect.width + slots[1].rect.width + slots[2].rect.width;
        assert_eq!(total, 1920);
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
