<p align="center">
    <picture>
      <source media="(prefers-color-scheme: light)" srcset="./docs/assets/readme/hero-light.png" />
      <img src="./assets/logo.png" />
  </picture>
</p>
<h1 align="center">
  <span>Shellwright</span>
</h1>

<p align="center">
  A cross-platform tiling window manager written in Rust — automatic, keyboard-driven window tiling with multi-monitor support, smooth animations, and a clean TOML config. Currently targeting Windows, with macOS and Linux (Wayland) backends in progress.
</p>

<h3 align="center">
  <a href="#-installation">Installation</a>
  <span> · </span>
  <a href="#-usage">Usage</a>
  <span> · </span>
  <a href="#-configuration">Configuration</a>
  <span> · </span>
  <a href="#-feature-status">Feature Status</a>
  <span> · </span>
  <a href="#-contributing">Contributing</a>
</h3>

<br/>

---

## 📋 Installation

Shellwright is built from source. You will need the [Rust toolchain](https://rustup.rs/) (stable, 1.75+).

<details open>
<summary><strong>Build from source (Windows)</strong></summary>
<br/>

```powershell
# Clone the repository
git clone https://github.com/your-username/shellwright
cd shellwright

# Build optimised release binary
cargo build --release

# The binary will be at:
.\target\release\window-manager.exe
```

</details>

<details>
<summary><strong>Register as a startup application</strong></summary>
<br/>

Shellwright can register itself to launch automatically at Windows login using a registry key:

```powershell
# Register autostart (adds a Run registry entry)
.\target\release\window-manager.exe autostart-register

# Remove autostart
.\target\release\window-manager.exe autostart-unregister
```

This writes a value to `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` pointing to the current exe path. No installer required.

</details>

<details>
<summary><strong>First-time setup</strong></summary>
<br/>

1. Run `window-manager.exe` — a default config is created automatically at `%APPDATA%\shellwright\config.toml` if one does not exist.
2. Logs are written to `%APPDATA%\shellwright\shellwright.log`.
3. If you use YASB, set `[padding]` in your config to match your bar height (see [Configuration](#-configuration)).

</details>

---

## 💻 Usage

Shellwright runs silently in the background and tiles all manageable windows automatically as they open and close. No visible UI — everything is driven by keybindings.

### Default keybindings

| Keys                  | Action                               |
| --------------------- | ------------------------------------ |
| `Alt + H`             | Focus previous window                |
| `Alt + L`             | Focus next window                    |
| `Alt + Shift + H`     | Move window left in layout order     |
| `Alt + Shift + L`     | Move window right in layout order    |
| `Alt + Shift + Q`     | Close focused window                 |
| `Alt + F`             | Toggle fullscreen                    |
| `Alt + Shift + Space` | Toggle floating / tiled              |
| `Alt + G`             | Switch to Fibonacci layout           |
| `Alt + T`             | Switch to BSP layout                 |
| `Alt + M`             | Switch to Monocle layout             |
| `Alt + C`             | Switch to Columns (2) layout         |
| `Alt + U`             | Switch to CenterMain layout          |
| `Alt + 1 … 9`         | Switch to workspace 1–9              |
| `Alt + Shift + 1 … 9` | Move focused window to workspace 1–9 |
| `Alt + Shift + R`     | Reload config                        |
| `Alt + Shift + E`     | Quit                                 |

### Layouts

| Layout                    | Description                                                                                                     |
| ------------------------- | --------------------------------------------------------------------------------------------------------------- |
| **Fibonacci** _(default)_ | Dwindle spiral — window 0 takes the first half, the rest recurse inward alternating H/V splits                  |
| **BSP**                   | Binary space partition — screen is recursively halved                                                           |
| **Monocle**               | All windows stacked full-screen; switching focus raises the top window                                          |
| **Columns**               | Fixed number of equal-width columns (`set_layout:columns:N`)                                                    |
| **CenterMain**            | Ultrawide three-column layout — 50% centre, 25% left, 25% right. Ideal for games that can't fill a wide display |
| **Float**                 | All windows unmanaged                                                                                           |

---

## ⚙️ Configuration

Config lives at `%APPDATA%\shellwright\config.toml`. Shellwright creates a default file on first run. Reload changes at any time with `Alt + Shift + R`.

<details open>
<summary><strong>Full config reference</strong></summary>
<br/>

```toml
# ── Appearance ─────────────────────────────────────────────────────────────────
gap           = 8       # pixels between tiled windows
border_width  = 2       # overlay border thickness in pixels
border_active   = "#5E81AC"   # border colour for the focused window  (#RRGGBB)
border_inactive = "#3B4252"   # border colour for all other windows   (#RRGGBB)
border_radius   = 8           # border corner radius (0 = square)

# ── Taskbar ─────────────────────────────────────────────────────────────────────
# "global" — windows on inactive workspaces are parked off-screen; they stay
#            in the taskbar at all times (default).
# "local"  — windows on inactive workspaces are hidden; each workspace has its
#            own taskbar entries.
taskbar_mode = "global"

# ── Default layout ──────────────────────────────────────────────────────────────
# One of: fibonacci | bsp | monocle | columns | center_main | float
default_layout = "fibonacci"

# ── YASB / external bar padding ─────────────────────────────────────────────────
# Shellwright reads the Windows work area (SPI_GETWORKAREA) which respects the
# OS taskbar but not third-party bars. Set these values to match your bar height
# so tiles don't slide under it.
[padding]
top    = 40   # e.g. 40 px for a top-aligned YASB bar
bottom = 0
left   = 0
right  = 0

# ── Animations ──────────────────────────────────────────────────────────────────
[animations]
enabled     = true
duration_ms = 80    # total easing time in ms — lower is faster
frames      = 6     # interpolation steps (1–60) — fewer is snappier

# ── Workspaces ───────────────────────────────────────────────────────────────────
[[workspaces]]
name = "1"
# Repeat up to 9 times. Name appears in YASB workspace widget.

# ── Float rules ──────────────────────────────────────────────────────────────────
# Windows matching any rule start in floating mode.
# All specified fields must match (AND logic); omit a field to wildcard it.
#
# [[float_rules]]
# exe   = "steam.exe"
#
# [[float_rules]]
# class = "TaskManagerWindow"
#
# [[float_rules]]
# title_contains = "Properties"
# exe            = "explorer.exe"

# ── Keybindings ───────────────────────────────────────────────────────────────────
# Modifiers: "alt", "ctrl", "shift", "super" (Win key)
# Keys:      a–z, 0–9, f1–f12, return, space, tab, escape, backspace,
#            up, down, left, right, home, end, pageup, pagedown, and more.
#
# [[keybindings]]
# modifiers = ["alt"]
# key       = "h"
# action    = "focus_prev"
```

</details>

<details>
<summary><strong>All available actions</strong></summary>
<br/>

| Action string               | Effect                                   |
| --------------------------- | ---------------------------------------- |
| `focus_next` / `focus_prev` | Cycle keyboard focus                     |
| `move_next` / `move_prev`   | Swap window position in layout order     |
| `kill_focused`              | Close focused window gracefully          |
| `toggle_float`              | Toggle between tiled and floating        |
| `toggle_fullscreen`         | Toggle true fullscreen (covers taskbar)  |
| `set_layout:fibonacci`      | Switch active workspace to Fibonacci     |
| `set_layout:bsp`            | Switch to BSP                            |
| `set_layout:monocle`        | Switch to Monocle                        |
| `set_layout:columns:N`      | Switch to N equal columns                |
| `set_layout:center_main`    | Switch to CenterMain                     |
| `set_layout:float`          | Switch to Float (all windows free)       |
| `switch_workspace:N`        | Activate workspace N (1-indexed)         |
| `move_to_workspace:N`       | Move focused window to workspace N       |
| `reload_config`             | Re-read `config.toml` without restarting |
| `quit`                      | Gracefully exit shellwright              |

</details>

---

## 📊 Feature Status

| Feature                                                  | Status         | Platform |
| -------------------------------------------------------- | -------------- | -------- |
| Fibonacci / BSP / Columns / CenterMain / Monocle layouts | ✅ Done        | All      |
| Float Layout                                             | 🚧 In Progress | All      |
| Monocle z-order focus raise                              | 🚧 In Progress | Windows  |
| Proper manual window resizing                            | 🚧 In Progress | Windows  |
| GDI overlay borders (Win10 + Win11)                      | ✅ Done        | Windows  |
| DWM border colours (Win11 22H2+)                         | ✅ Done        | Windows  |
| Multi-monitor support                                    | ✅ Done        | Windows  |
| 9 configurable workspaces                                | ✅ Done        | All      |
| Cross-monitor window movement                            | ✅ Done        | Windows  |
| Drag-to-swap tiled windows                               | ✅ Done        | Windows  |
| Minimized windows excluded from tiling                   | ✅ Done        | Windows  |
| Toggle float / fullscreen                                | ✅ Done        | Windows  |
| Float rules (by class, title, exe)                       | ✅ Done        | Windows  |
| Smooth animations (move + workspace crossfade)           | ✅ Done        | Windows  |
| TOML config with hot-reload                              | ✅ Done        | All      |
| Global / local taskbar mode                              | ✅ Done        | Windows  |
| YASB named-pipe IPC (workspace indicator)                | ✅ Done        | Windows  |
| Autostart via registry                                   | ✅ Done        | Windows  |
| macOS backend (Accessibility API)                        | 🚧 In Progress | macOS    |
| Wayland backend (Smithay)                                | 🚧 In Progress | Linux    |
| Window rules for workspace auto-assignment               | 📋 Planned     | All      |
| Per-workspace layout persistence across reloads          | 📋 Planned     | All      |
| Scratchpad windows                                       | 📋 Planned     | All      |
| X11 backend                                              | 📋 Planned     | Linux    |
| Pre-built release binaries                               | 📋 Planned     | Windows  |

---

## 🤝 Contributing

Shellwright is licensed under the **GNU General Public License v3.0** — you are free to use, modify, and distribute it under the same terms. See [`LICENSE`](LICENSE) for the full text.

Contributions of any kind are welcome — bug reports, new layout algorithms, platform backend work, documentation improvements, or just ideas. To get started:

1. Fork the repository and create a feature branch.
2. The codebase is split into platform-agnostic logic (`shellwright-core`) and platform backends (`shellwright-windows`, `shellwright-macos`, `shellwright-wayland`). New layouts and actions go in `core`; OS-specific code goes in the relevant backend crate.
3. All public items should have doc comments. Modules include unit tests — please add tests for new logic.
4. Open a pull request describing what you changed and why.

### Crate layout

```
crates/
  shellwright-core/      # Platform-agnostic: layouts, actions, config, workspace, events
  shellwright-windows/   # Win32 backend — SetWindowPos, SetWinEventHook, GDI overlays
  shellwright-macos/     # macOS backend — Accessibility API (in progress)
  shellwright-wayland/   # Wayland backend — Smithay compositor (in progress)
  shellwright/           # Binary — event loop, layout dispatch, keybinding dispatch
```
