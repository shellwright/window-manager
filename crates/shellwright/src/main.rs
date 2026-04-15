//! `shellwright` — cross-platform tiling window manager.

use anyhow::Context;
use shellwright_core::{
    action::Action,
    backend::Backend,
    config::{Config, Padding},
    event::Event,
    hotkey::BindingMap,
    layout,
    window::{Rect, Window, WindowId},
    workspace::Workspace,
};

fn main() -> anyhow::Result<()> {
    init_tracing();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting shellwright");

    let config_path = resolve_config_path()?;
    let config = if config_path.exists() {
        Config::load(&config_path)
            .with_context(|| format!("failed to load {}", config_path.display()))?
    } else {
        tracing::warn!(path = %config_path.display(), "config not found, using defaults");
        Config::default()
    };

    let bindings = BindingMap::from_config(&config.keybindings)
        .context("invalid keybindings in config")?;

    tracing::info!(count = bindings.len(), "keybindings registered");

    run(config, bindings)
}

// ── Platform dispatch ─────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn run(config: Config, bindings: BindingMap) -> anyhow::Result<()> {
    let backend = shellwright_windows::WindowsBackend::new(&bindings)?;
    event_loop(backend, bindings, config)
}

#[cfg(target_os = "macos")]
fn run(config: Config, bindings: BindingMap) -> anyhow::Result<()> {
    let backend = shellwright_macos::MacosBackend::new(&bindings)?;
    event_loop(backend, bindings, config)
}

#[cfg(target_os = "linux")]
fn run(config: Config, bindings: BindingMap) -> anyhow::Result<()> {
    let backend = shellwright_wayland::WaylandCompositor::new(bindings)?;
    let empty = BindingMap::from_config(&[]).unwrap();
    event_loop(backend, empty, config)
}

// ── Core event loop ───────────────────────────────────────────────────────────

fn event_loop<B: Backend>(
    mut backend: B,
    bindings: BindingMap,
    config: Config,
) -> anyhow::Result<()> {
    let mut workspaces: Vec<Workspace> = config
        .workspaces
        .iter()
        .map(|w| Workspace::new(&w.name))
        .collect();
    let mut active_ws: usize = 0;

    tracing::info!(workspaces = workspaces.len(), "entering event loop");

    // ── Seed workspace 0 with all windows already open at startup ────────────
    // `enumerate_windows` in the backend captures pre-existing windows but
    // never fires WindowCreated events for them — so we seed manually.
    {
        let ids: Vec<WindowId> = backend.windows().iter().map(|w| w.id()).collect();
        tracing::info!(count = ids.len(), "seeding startup windows into workspace 1");
        for id in ids {
            workspaces[0].add_window(id);
        }
        if !workspaces[0].windows().is_empty() {
            apply_layout(&mut backend, &workspaces[0], &config);
            update_borders(&mut backend, &workspaces[0], &config);
        }
    }

    loop {
        let event = backend.next_event()?;
        tracing::debug!(?event, "event");

        match event {
            Event::Quit => {
                tracing::info!("quit — shutting down");
                break;
            }

            Event::WindowCreated(id) => {
                tracing::info!(%id, workspace = %workspaces[active_ws].name, "window created");
                workspaces[active_ws].add_window(id);
                apply_layout(&mut backend, &workspaces[active_ws], &config);
                update_borders(&mut backend, &workspaces[active_ws], &config);
            }

            Event::WindowDestroyed(id) => {
                tracing::info!(%id, "window destroyed");
                for ws in &mut workspaces {
                    ws.remove_window(id);
                }
                apply_layout(&mut backend, &workspaces[active_ws], &config);
                update_borders(&mut backend, &workspaces[active_ws], &config);
            }

            Event::WindowFocused(id) => {
                tracing::debug!(%id, "focused");
                // Only update focus if the window belongs to the active workspace.
                // Spurious focus events from hiding workspace windows during a switch
                // must not corrupt the focus state of the incoming workspace.
                if workspaces[active_ws].contains(id) {
                    workspaces[active_ws].focused = Some(id);
                    update_borders(&mut backend, &workspaces[active_ws], &config);
                }
            }

            Event::WindowMoved { id } => {
                // Non-floating windows snap back to their tile slot (komorebi-style).
                // Floating windows keep wherever the user dragged them.
                if workspaces[active_ws].contains(id) {
                    if backend.window_mut(id).map_or(false, |w| !w.is_floating()) {
                        apply_layout(&mut backend, &workspaces[active_ws], &config);
                        update_borders(&mut backend, &workspaces[active_ws], &config);
                    }
                }
            }

            Event::Keybinding(kb_id) => {
                if let Some(action) = bindings.action(kb_id) {
                    tracing::debug!(?action, "action");
                    dispatch(
                        action.clone(),
                        &mut backend,
                        &mut workspaces,
                        &mut active_ws,
                        &config,
                    );
                }
            }
        }

        backend.flush()?;
    }

    Ok(())
}

// ── Layout helpers ────────────────────────────────────────────────────────────

/// Compute and apply tiling geometry for every window in `workspace`.
///
/// Windows are grouped by the monitor they currently occupy so that a
/// workspace spanning multiple monitors tiles each monitor independently.
///
/// Order of operations per monitor:
/// 1. `monitor_rect_for_window()` — work area for that monitor.
/// 2. Apply `config.padding` — reserves space for external bars (e.g. YASB).
/// 3. Partition windows into tiled / floating / fullscreen.
/// 4. `layout::compute` for the tiled set; shrink each slot by `gap`.
/// 5. Fullscreen windows receive the full padded area of their monitor.
fn apply_layout<B: Backend>(backend: &mut B, workspace: &Workspace, config: &Config) {
    use std::collections::HashMap;

    // ── Tiled windows grouped by monitor ─────────────────────────────────────
    let tiled: Vec<WindowId> = workspace
        .windows()
        .iter()
        .copied()
        .filter(|&id| {
            !workspace.is_fullscreen(id)
                && backend.window_mut(id).map_or(false, |w| !w.is_floating())
        })
        .collect();

    // Key: (x, y, w, h) of the raw monitor work-area (before padding).
    let mut by_monitor: HashMap<(i32, i32, u32, u32), Vec<WindowId>> = HashMap::new();
    for &id in &tiled {
        let mr = backend.monitor_rect_for_window(id);
        by_monitor
            .entry((mr.x, mr.y, mr.width, mr.height))
            .or_default()
            .push(id);
    }

    for ((x, y, w, h), ids) in &by_monitor {
        let monitor = apply_padding(Rect::new(*x, *y, *w, *h), config.padding);
        let slots = layout::compute(&workspace.layout, monitor, ids.len());
        for (&id, slot) in ids.iter().zip(slots.iter()) {
            let rect = inset(slot.rect, config.gap);
            if let Some(win) = backend.window_mut(id) {
                if let Err(e) = win.set_geometry(rect) {
                    tracing::warn!(%id, err = %e, "set_geometry failed");
                }
            }
        }
    }

    // ── Fullscreen windows ────────────────────────────────────────────────────
    for id in workspace.fullscreen_windows() {
        let mr = backend.monitor_rect_for_window(id);
        let monitor = apply_padding(mr, config.padding);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.set_geometry(monitor) {
                tracing::warn!(%id, err = %e, "set_geometry (fullscreen) failed");
            }
        }
    }
}

/// Shrink a rect inward by `gap` pixels on all sides.
fn inset(r: Rect, gap: u32) -> Rect {
    let g = gap as i32;
    Rect::new(
        r.x + g,
        r.y + g,
        r.width.saturating_sub(2 * gap),
        r.height.saturating_sub(2 * gap),
    )
}

/// Subtract padding from a monitor rect (reserves space for external bars).
fn apply_padding(r: Rect, p: Padding) -> Rect {
    Rect::new(
        r.x + p.left as i32,
        r.y + p.top as i32,
        r.width.saturating_sub(p.left + p.right),
        r.height.saturating_sub(p.top + p.bottom),
    )
}

// ── Border helpers ────────────────────────────────────────────────────────────

/// Update every window's border colour: active for the focused window,
/// inactive for all others.
///
/// On Windows 11+ this drives `DwmSetWindowAttribute(DWMWA_BORDER_COLOR)`.
/// All other backends use the default no-op `set_border_color`.
fn update_borders<B: Backend>(backend: &mut B, workspace: &Workspace, config: &Config) {
    let active_color   = parse_hex_color(&config.border_active);
    let inactive_color = parse_hex_color(&config.border_inactive);

    for &id in workspace.windows() {
        let color = if workspace.focused == Some(id) { active_color } else { inactive_color };
        if let Some(w) = backend.window_mut(id) {
            let _ = w.set_border_color(color);
        }
    }
}

/// Parse `#RRGGBB` (or `RRGGBB`) to a `0x00RRGGBB` `u32`.
/// Falls back to mid-grey on invalid input.
fn parse_hex_color(s: &str) -> u32 {
    let s = s.trim_start_matches('#');
    u32::from_str_radix(s, 16).unwrap_or(0x88_88_88)
}

// ── Action dispatch ───────────────────────────────────────────────────────────

fn dispatch<B: Backend>(
    action: Action,
    backend: &mut B,
    workspaces: &mut Vec<Workspace>,
    active_ws: &mut usize,
    config: &Config,
) {
    match action {
        // ── Focus ─────────────────────────────────────────────────────────────
        Action::FocusNext => {
            let ws = &mut workspaces[*active_ws];
            if ws.windows().is_empty() { return; }
            let windows = ws.windows().to_vec();
            let current = ws.focused
                .and_then(|f| windows.iter().position(|w| *w == f))
                .unwrap_or(0);
            let next_id = windows[(current + 1) % windows.len()];
            ws.focused = Some(next_id);
            if let Some(w) = backend.window_mut(next_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %next_id, err = %e, "focus failed");
                }
            }
            update_borders(backend, &workspaces[*active_ws], config);
        }

        Action::FocusPrev => {
            let ws = &mut workspaces[*active_ws];
            if ws.windows().is_empty() { return; }
            let windows = ws.windows().to_vec();
            let current = ws.focused
                .and_then(|f| windows.iter().position(|w| *w == f))
                .unwrap_or(0);
            let prev = if current == 0 { windows.len() - 1 } else { current - 1 };
            let prev_id = windows[prev];
            ws.focused = Some(prev_id);
            if let Some(w) = backend.window_mut(prev_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %prev_id, err = %e, "focus failed");
                }
            }
            update_borders(backend, &workspaces[*active_ws], config);
        }

        // ── Window manipulation ───────────────────────────────────────────────
        Action::MoveNext => {
            let ws = &mut workspaces[*active_ws];
            if let Some(focused) = ws.focused {
                let windows = ws.windows_mut();
                if let Some(pos) = windows.iter().position(|&w| w == focused) {
                    let next = (pos + 1) % windows.len();
                    windows.swap(pos, next);
                }
            }
            apply_layout(backend, &workspaces[*active_ws], config);
        }

        Action::MovePrev => {
            let ws = &mut workspaces[*active_ws];
            if let Some(focused) = ws.focused {
                let windows = ws.windows_mut();
                if let Some(pos) = windows.iter().position(|&w| w == focused) {
                    let prev = if pos == 0 { windows.len() - 1 } else { pos - 1 };
                    windows.swap(pos, prev);
                }
            }
            apply_layout(backend, &workspaces[*active_ws], config);
        }

        Action::KillFocused => {
            if let Some(id) = workspaces[*active_ws].focused {
                tracing::info!(%id, "kill focused");
                if let Some(w) = backend.window_mut(id) {
                    if let Err(e) = w.close() {
                        tracing::warn!(%id, err = %e, "close failed");
                    }
                }
            }
        }

        Action::ToggleFloat => {
            if let Some(id) = workspaces[*active_ws].focused {
                if let Some(w) = backend.window_mut(id) {
                    let now = !w.is_floating();
                    w.set_floating(now);
                    tracing::debug!(%id, floating = now, "toggle float");
                }
                apply_layout(backend, &workspaces[*active_ws], config);
            }
        }

        Action::ToggleFullscreen => {
            toggle_fullscreen_for(
                workspaces[*active_ws].focused,
                backend,
                &mut workspaces[*active_ws],
                config,
            );
        }

        // ── Layout ───────────────────────────────────────────────────────────
        Action::SetLayout(kind) => {
            tracing::info!(?kind, workspace = %workspaces[*active_ws].name, "set layout");
            workspaces[*active_ws].layout = kind;
            apply_layout(backend, &workspaces[*active_ws], config);
        }

        // ── Workspaces ───────────────────────────────────────────────────────
        Action::SwitchWorkspace(n) => {
            let idx = (n as usize).saturating_sub(1);
            if idx >= workspaces.len() || idx == *active_ws { return; }

            // Hide every window on the outgoing workspace.
            for &id in workspaces[*active_ws].windows() {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.hide();
                }
            }

            *active_ws = idx;

            // Show and re-tile the incoming workspace.
            for &id in workspaces[*active_ws].windows() {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.show();
                }
            }
            apply_layout(backend, &workspaces[*active_ws], config);
            update_borders(backend, &workspaces[*active_ws], config);

            // Restore focus on the incoming workspace.
            if let Some(id) = workspaces[*active_ws].focused {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.focus();
                }
            }

            tracing::info!(workspace = %workspaces[*active_ws].name, "switch workspace");
        }

        Action::MoveFocusedToWorkspace(n) => {
            let target = (n as usize).saturating_sub(1);
            if target >= workspaces.len() || target == *active_ws { return; }

            if let Some(id) = workspaces[*active_ws].focused {
                // Clear fullscreen before moving.
                workspaces[*active_ws].exit_fullscreen(id);
                workspaces[*active_ws].remove_window(id);

                // Hide: the window is now on an inactive workspace.
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.hide();
                    // Reset to inactive border before moving out of visible workspace.
                    let inactive = parse_hex_color(&config.border_inactive);
                    let _ = w.set_border_color(inactive);
                }

                workspaces[target].add_window(id);
                tracing::info!(%id, to = %workspaces[target].name, "move window to workspace");
                apply_layout(backend, &workspaces[*active_ws], config);
                update_borders(backend, &workspaces[*active_ws], config);
            }
        }

        // ── WM lifecycle ─────────────────────────────────────────────────────
        Action::ReloadConfig => {
            // TODO: re-read config.toml; rebuild BindingMap; re-register hotkeys.
            tracing::info!("reload config requested — not yet implemented");
        }

        Action::Quit => {
            tracing::info!("quit action");
            std::process::exit(0);
        }
    }
}

// ── Fullscreen ────────────────────────────────────────────────────────────────

/// Toggle fullscreen for a specific window by ID (or focused if `target` is `None`).
///
/// - **Entering**: saves current geometry, sets window to padded monitor rect.
/// - **Exiting**: restores saved geometry and re-tiles the workspace.
fn toggle_fullscreen_for<B: Backend>(
    target: Option<WindowId>,
    backend: &mut B,
    workspace: &mut Workspace,
    config: &Config,
) {
    let id = match target {
        Some(id) => id,
        None => return,
    };

    if workspace.is_fullscreen(id) {
        workspace.exit_fullscreen(id);
        tracing::info!(%id, "exit fullscreen");
        apply_layout(backend, workspace, config);
    } else {
        let saved = backend
            .window_mut(id)
            .map(|w| w.geometry())
            .unwrap_or_else(|| backend.monitor_rect());
        workspace.enter_fullscreen(id, saved);
        tracing::info!(%id, "enter fullscreen");

        let monitor = apply_padding(backend.monitor_rect(), config.padding);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.set_geometry(monitor) {
                tracing::warn!(%id, err = %e, "set_geometry (enter fullscreen) failed");
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_env("SHELLWRIGHT_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn resolve_config_path() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").context("APPDATA not set")?;
        Ok(std::path::PathBuf::from(appdata)
            .join("shellwright")
            .join("config.toml"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home).join(".config")
            });
        Ok(base.join("shellwright").join("config.toml"))
    }
}
