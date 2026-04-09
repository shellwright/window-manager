//! `shellwright` — cross-platform tiling window manager.

use anyhow::Context;
use shellwright_core::{
    action::Action,
    backend::Backend,
    config::Config,
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
                apply_layout(&mut backend, &workspaces[active_ws], config.gap);
            }

            Event::WindowDestroyed(id) => {
                tracing::info!(%id, "window destroyed");
                for ws in &mut workspaces {
                    ws.remove_window(id);
                }
                apply_layout(&mut backend, &workspaces[active_ws], config.gap);
            }

            Event::WindowFocused(id) => {
                tracing::debug!(%id, "focused");
                workspaces[active_ws].focused = Some(id);
            }

            Event::WindowMoved { .. } => {
                // Floating window moved by user — no layout recomputation needed.
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
/// - Floating windows are skipped (their geometry is user-managed).
/// - Fullscreen windows always receive the full `monitor_rect`.
/// - All other windows receive layout-computed slots shrunk by `gap` pixels.
fn apply_layout<B: Backend>(backend: &mut B, workspace: &Workspace, gap: u32) {
    let monitor = backend.monitor_rect();

    // Collect windows that should participate in tiling (not floating, not fullscreen).
    let tiled: Vec<WindowId> = workspace
        .windows()
        .iter()
        .copied()
        .filter(|&id| {
            !workspace.is_fullscreen(id)
                && backend.window_mut(id).map_or(false, |w| !w.is_floating())
        })
        .collect();

    let slots = layout::compute(&workspace.layout, monitor, tiled.len());

    for (&id, slot) in tiled.iter().zip(slots.iter()) {
        let rect = inset(slot.rect, gap);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.set_geometry(rect) {
                tracing::warn!(%id, err = %e, "set_geometry failed");
            }
        }
    }

    // Fullscreen windows cover the entire monitor regardless of layout.
    for id in workspace.fullscreen_windows() {
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
            apply_layout(backend, &workspaces[*active_ws], config.gap);
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
            apply_layout(backend, &workspaces[*active_ws], config.gap);
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
                apply_layout(backend, &workspaces[*active_ws], config.gap);
            }
        }

        Action::ToggleFullscreen => {
            toggle_fullscreen_for(
                workspaces[*active_ws].focused,
                backend,
                &mut workspaces[*active_ws],
                config.gap,
            );
        }

        // ── Layout ───────────────────────────────────────────────────────────
        Action::SetLayout(kind) => {
            tracing::info!(?kind, workspace = %workspaces[*active_ws].name, "set layout");
            workspaces[*active_ws].layout = kind;
            apply_layout(backend, &workspaces[*active_ws], config.gap);
        }

        // ── Workspaces ───────────────────────────────────────────────────────
        Action::SwitchWorkspace(n) => {
            let idx = (n as usize).saturating_sub(1);
            if idx >= workspaces.len() || idx == *active_ws { return; }

            // Hide all windows on the current workspace.
            for &id in workspaces[*active_ws].windows() {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.hide();
                }
            }

            *active_ws = idx;

            // Restore and re-tile the target workspace.
            for &id in workspaces[*active_ws].windows() {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.show();
                }
            }
            apply_layout(backend, &workspaces[*active_ws], config.gap);

            // Focus the previously-focused window on the new workspace.
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
                // Clear fullscreen state before moving.
                workspaces[*active_ws].exit_fullscreen(id);
                workspaces[*active_ws].remove_window(id);

                // Hide the window since it's now on an inactive workspace.
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.hide();
                }

                workspaces[target].add_window(id);
                tracing::info!(%id, to = %workspaces[target].name, "move window to workspace");
                apply_layout(backend, &workspaces[*active_ws], config.gap);
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

/// Toggle fullscreen for a specific window by ID (or the focused window when
/// `target` is `None`).
///
/// This is the core of the per-window fullscreen feature:
/// - **Entering**: saves the current geometry, sets the window to the full
///   monitor rect, and removes it from tiling layout.
/// - **Exiting**: restores the saved geometry and re-tiles the workspace.
fn toggle_fullscreen_for<B: Backend>(
    target: Option<WindowId>,
    backend: &mut B,
    workspace: &mut Workspace,
    gap: u32,
) {
    let id = match target {
        Some(id) => id,
        None => return,
    };

    if workspace.is_fullscreen(id) {
        // ── Exit fullscreen ────────────────────────────────────────────────
        workspace.exit_fullscreen(id);
        tracing::info!(%id, "exit fullscreen");
        // Re-tile everything; apply_layout will skip fullscreen windows, of
        // which there are now fewer (possibly zero).
        apply_layout(backend, workspace, gap);
    } else {
        // ── Enter fullscreen ───────────────────────────────────────────────
        // Save the current geometry so we can restore it on exit.
        let saved = backend
            .window_mut(id)
            .map(|w| w.geometry())
            .unwrap_or_else(|| backend.monitor_rect());
        workspace.enter_fullscreen(id, saved);
        tracing::info!(%id, "enter fullscreen");

        // Apply the full monitor rect immediately without waiting for the next
        // layout pass.
        let monitor = backend.monitor_rect();
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
