# shellwright

A Rust tiling window manager for Windows (Win32), designed to be used alongside
**YASB** status bar and as a drop-in replacement for komorebi.

---

## What it is

- Automatic tiling of all visible top-level windows
- **Fibonacci dwindle spiral** layout by default (alternating H/V splits spiralling inward)
- BSP, Monocle, Columns, Float layouts also available
- Thick GDI overlay borders — works on Windows 10 and 11 (not DWM-only)
- Multi-monitor aware: each monitor tiles independently
- 9 workspaces with Alt+1…9 to switch, Alt+Shift+1…9 to move windows
- Drag-to-swap: drag a tiled window and it swaps positions with the nearest tile
- Minimized windows excluded from the tiling count
- Config file at `%APPDATA%\shellwright\config.toml` (defaults used if absent)

---

## Crate layout

```
crates/
  shellwright-core/        # Pure logic: layouts, actions, config, workspace, events
  shellwright-windows/     # Win32 backend (SetWindowPos, SetWinEventHook, GDI overlays)
  shellwright/             # Binary — event loop, layout dispatch, keybinding dispatch
```

---

## Build & run

```powershell
# debug build (fast)
cargo build -p shellwright

# optimised release
cargo build -p shellwright --release

# run directly
cargo run -p shellwright

# or after release build
.\target\release\shellwright.exe
```

Requires Rust stable + windows-rs 0.58 (no extra system deps).

---

## Using with YASB

1. Disable komorebi (`komorebic stop` or just don't start it).
2. Start shellwright: `.\shellwright.exe` in a terminal or create a startup task.
3. YASB runs independently — shellwright does not touch it. Set YASB padding in
   `config.toml` so tiles don't overlap the bar:

```toml
[padding]
top = 40   # height of your YASB bar in pixels
```

---

## Default keybindings

| Keys | Action |
|---|---|
| Alt+H / Alt+L | Focus prev / next |
| Alt+Shift+H / Alt+Shift+L | Swap window prev / next in layout |
| Alt+F | Toggle fullscreen |
| Alt+Shift+Space | Toggle float |
| Alt+Shift+Q | Close focused window |
| Alt+G | Fibonacci layout |
| Alt+T | BSP layout |
| Alt+M | Monocle layout |
| Alt+C | Columns (2) layout |
| Alt+1…9 | Switch workspace |
| Alt+Shift+1…9 | Move window to workspace |
| Alt+Shift+R | Reload config |
| Alt+Shift+E | Quit |

---

## Config reference (`config.toml`)

```toml
gap          = 8          # pixels between tiles
border_width = 4          # overlay border thickness
border_active   = "#5E81AC"
border_inactive = "#3B4252"

[padding]
top    = 0
bottom = 0
left   = 0
right  = 0

[[workspaces]]
name = "1"
# ... up to 9
```

---

## Current working state

- Tiling, focus cycling, drag-to-swap, fullscreen: **working**
- GDI overlay borders: **working** (visible on all apps, all Windows versions)
- Workspace switching (hide/show windows): **working**; per-monitor workspace
  assignment (workspaces 1-3 → monitor 1 etc.) is **not yet implemented**
- Minimized windows excluded from tiling: **working**
- YASB named-pipe IPC (workspace indicator in bar): **not yet implemented**
- ITaskbarList3 taskbar integration (hide off-workspace windows from taskbar): **not yet implemented**
- Config hot-reload: **not yet implemented**

---

## Continuing development

Key files to read first:

- `crates/shellwright-core/src/` — all platform-agnostic logic
  - `layout.rs` — add new layout algorithms here
  - `action.rs` — add new keybinding actions here
  - `config.rs` — default config and keybindings
  - `workspace.rs` — window grouping logic
- `crates/shellwright-windows/src/backend.rs` — all Win32 code
  - `WindowsWindow` struct — per-window state
  - `border_wnd_proc` + `create_overlay` / `position_overlay` — GDI border system
  - `next_event` message loop — SWE_CREATED / SWE_DESTROYED / SWE_FOCUSED / SWE_MOVESIZEEND
- `crates/shellwright/src/main.rs` — event loop + `apply_layout` + `dispatch`

Next priorities (in rough order):
1. Per-monitor workspace assignment (1-3 → mon1, 4-6 → mon2, 7-9 → mon3)
2. ITaskbarList3: hide off-workspace windows from taskbar
3. YASB named pipe IPC: `\\.\pipe\shellwright` with JSON workspace state
4. Config hot-reload on Alt+Shift+R
