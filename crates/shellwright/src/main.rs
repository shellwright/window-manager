//! `shellwright` — cross-platform tiling window manager.
//!
//! On Windows the binary is compiled as a GUI subsystem app (`windows_subsystem =
//! "windows"`) so no console window appears when it starts.  Logs are written to
//! `%APPDATA%\shellwright\shellwright.log` instead of stdout.
//!
//! CLI commands (run before the WM starts):
//!   `shellwright autostart-register`   — adds a Run registry key so the WM
//!                                        starts automatically at Windows login.
//!   `shellwright autostart-unregister` — removes that key.

// Hide the console window on Windows — this is what makes it a background daemon.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::Context;
use shellwright_core::{
    action::Action,
    backend::Backend,
    config::{Config, Padding, TaskbarMode},
    event::Event,
    hotkey::BindingMap,
    layout::{self, LayoutKind},
    window::{Rect, Window, WindowId},
    workspace::Workspace,
};

fn main() -> anyhow::Result<()> {
    // ── Handle CLI sub-commands before starting the WM ────────────────────────
    let args: Vec<String> = std::env::args().collect();
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        match cmd {
            "autostart-register" | "--autostart-register" => {
                autostart_register()?;
                return Ok(());
            }
            "autostart-unregister" | "--autostart-unregister" => {
                autostart_unregister()?;
                return Ok(());
            }
            other => {
                eprintln!("unknown command: {other}");
                eprintln!("usage: shellwright [autostart-register|autostart-unregister]");
                std::process::exit(1);
            }
        }
    }

    // ── Normal WM startup ─────────────────────────────────────────────────────
    let config_dir = resolve_config_dir()?;
    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create config dir {}", config_dir.display()))?;

    init_tracing(&config_dir.join("shellwright.log"));
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting shellwright");

    let config_path = config_dir.join("config.toml");
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
    let float_rules = config.float_rules.clone();
    let backend = shellwright_windows::WindowsBackend::new(&bindings, float_rules)?;
    event_loop(backend, bindings, config)
}

#[cfg(target_os = "macos")]
fn run(config: Config, bindings: BindingMap) -> anyhow::Result<()> {
    let backend = shellwright_macos::MacosBackend::new(&bindings)?;
    event_loop(backend, bindings, config)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn run(_config: Config, _bindings: BindingMap) -> anyhow::Result<()> {
    anyhow::bail!("no backend available for this platform")
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

    // Per-monitor workspace assignment.
    let mut monitor_rects = backend.monitor_rects();
    let n_mons    = monitor_rects.len().max(1);
    let n_ws      = workspaces.len().max(1);
    // Integer div_ceil: (a + b - 1) / b
    let ws_per_mon = (n_ws + n_mons - 1) / n_mons;

    // Active workspace index for each monitor; monitor m starts at m * ws_per_mon.
    let mut monitor_ws: Vec<usize> = (0..n_mons)
        .map(|m| (m * ws_per_mon).min(n_ws - 1))
        .collect();
    let mut focused_mon: usize = 0;

    tracing::info!(
        workspaces = n_ws,
        monitors   = n_mons,
        ws_per_mon,
        "entering event loop"
    );

    // ── Seed startup windows into correct per-monitor workspaces ─────────────
    {
        let ids: Vec<WindowId> = backend.windows().iter().map(|w| w.id()).collect();
        tracing::info!(count = ids.len(), "seeding startup windows");
        for id in ids {
            let wr  = backend.monitor_rect_for_window(id);
            let mon = find_monitor_idx(&monitor_rects, wr.x);
            let ws_idx = monitor_ws[mon];
            tracing::debug!(
                %id,
                window_x = wr.x,
                monitor  = mon,
                workspace = %workspaces[ws_idx].name,
                ws_idx,
                "seed: assigned window to workspace"
            );
            workspaces[ws_idx].add_window(id);
        }
        for mon in 0..n_mons {
            let ws_idx = monitor_ws[mon];
            if !workspaces[ws_idx].windows().is_empty() {
                apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                update_borders(&mut backend, &workspaces[ws_idx], &config);
            }
        }
        broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
    }

    loop {
        let event = backend.next_event()?;
        tracing::debug!(?event, "event");

        match event {
            Event::Quit => {
                tracing::info!("quit — restoring all hidden windows before shutdown");
                // Show all WM-hidden windows so they don't become orphaned after exit.
                let all_ids: Vec<WindowId> = backend.windows().iter().map(|w| w.id()).collect();
                for id in all_ids {
                    if let Some(w) = backend.window_mut(id) {
                        let _ = w.show();
                    }
                }
                break;
            }

            Event::WindowCreated(id) => {
                let wr  = backend.monitor_rect_for_window(id);
                let mon = find_monitor_idx(&monitor_rects, wr.x);
                let ws_idx = monitor_ws[mon];
                tracing::info!(%id, workspace = %workspaces[ws_idx].name, "window created");
                workspaces[ws_idx].add_window(id);
                apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                update_borders(&mut backend, &workspaces[ws_idx], &config);
                // Monocle: raise the focused window so it stays on top.
                if workspaces[ws_idx].layout == LayoutKind::Monocle {
                    if let Some(fid) = workspaces[ws_idx].focused {
                        if let Some(w) = backend.window_mut(fid) { let _ = w.raise(); }
                    }
                }
                broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
            }

            Event::WindowDestroyed(id) => {
                tracing::info!(%id, "window destroyed");
                for ws in &mut workspaces {
                    ws.remove_window(id);
                }
                // Relayout every monitor's active workspace.
                for mon in 0..n_mons {
                    let ws_idx = monitor_ws[mon];
                    apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
                    // Monocle: keep focused window on top after relayout.
                    if workspaces[ws_idx].layout == LayoutKind::Monocle {
                        if let Some(fid) = workspaces[ws_idx].focused {
                            if let Some(w) = backend.window_mut(fid) { let _ = w.raise(); }
                        }
                    }
                }
                broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
            }

            Event::WindowFocused(id) => {
                tracing::debug!(%id, "focused");
                if let Some((ws_idx, mon)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    // Window is already on a visible workspace — just update focus.
                    focused_mon = mon;
                    set_global_focus(id, ws_idx, &mut backend, &mut workspaces, &monitor_ws, &config);
                    broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
                } else if let Some(ws_idx) = find_any_workspace(&workspaces, id) {
                    // Window is on a hidden workspace (e.g. taskbar click).
                    // Switch the owning monitor to that workspace, exactly as SwitchWorkspace does,
                    // then focus the specific clicked window.
                    let n_mons = monitor_ws.len();
                    let mon = (ws_idx / ws_per_mon).min(n_mons - 1);
                    let current = monitor_ws[mon];
                    tracing::info!(
                        %id,
                        target_ws  = ws_idx,
                        target_mon = mon,
                        current_ws = current,
                        "WindowFocused: switching to hidden workspace for taskbar click"
                    );

                    let outgoing: Vec<WindowId> = workspaces[current].windows().to_vec();
                    let animate = config.animations.enabled && backend.system_animations_enabled();

                    monitor_ws[mon] = ws_idx;
                    focused_mon = mon;

                    let mr = monitor_rects.get(mon).copied().unwrap_or(monitor_rects[0]);
                    let incoming: Vec<WindowId> = workspaces[ws_idx].windows().to_vec();

                    // Pre-show incoming at alpha=0 for crossfade.
                    if animate {
                        for &w_id in &incoming {
                            if let Some(w) = backend.window_mut(w_id) { let _ = w.set_alpha(0); }
                        }
                    }
                    for &w_id in &incoming {
                        if let Some(w) = backend.window_mut(w_id) { let _ = w.show(); }
                    }
                    apply_layout(&mut backend, &workspaces[ws_idx], mr, &config);
                    update_borders(&mut backend, &workspaces[ws_idx], &config);

                    // Crossfade outgoing out / incoming in.
                    if animate {
                        let frames = config.animations.frames.clamp(1, 60);
                        let total  = std::time::Duration::from_millis(config.animations.duration_ms as u64);
                        let start  = std::time::Instant::now();
                        for frame in 1..=frames {
                            let t         = frame as f32 / frames as f32;
                            let alpha_out = ((1.0 - t) * 255.0) as u8;
                            let alpha_in  = (t * 255.0) as u8;
                            for &w_id in &outgoing {
                                if let Some(w) = backend.window_mut(w_id) { let _ = w.set_alpha(alpha_out); }
                            }
                            for &w_id in &incoming {
                                if let Some(w) = backend.window_mut(w_id) { let _ = w.set_alpha(alpha_in); }
                            }
                            let frame_target = start + total.mul_f32(t);
                            let now = std::time::Instant::now();
                            if frame_target > now { std::thread::sleep(frame_target - now); }
                        }
                    }
                    for &w_id in &outgoing {
                        if let Some(w) = backend.window_mut(w_id) {
                            if config.taskbar_mode == TaskbarMode::Global {
                                let _ = w.park();
                            } else {
                                let _ = w.hide();
                            }
                        }
                    }
                    if animate {
                        for &w_id in outgoing.iter().chain(incoming.iter()) {
                            if let Some(w) = backend.window_mut(w_id) { let _ = w.clear_alpha(); }
                        }
                    }

                    // Focus the specific window that was clicked.
                    set_global_focus(id, ws_idx, &mut backend, &mut workspaces, &monitor_ws, &config);
                    if let Some(w) = backend.window_mut(id) { let _ = w.focus(); }
                    broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
                }
            }

            Event::WindowMinimizeChanged { id } => {
                if let Some((ws_idx, mon)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
                }
            }

            Event::WindowSizeChanged { id } => {
                // External resize (e.g. browser native fullscreen / exit fullscreen).
                // Re-evaluate borders only — do not re-tile.
                if let Some((ws_idx, _)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
                }
            }

            Event::WindowResized { id } => {
                // User manually resized a tiled window by dragging its border.
                // Do NOT re-tile — the window keeps its user-defined size until
                // the next explicit layout operation (new window, workspace switch…).
                // Only refresh border overlays so they track the new geometry.
                if let Some((ws_idx, _)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
                }
            }

            Event::WindowMoved { id } => {
                if let Some((ws_idx, mon)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    if backend.window_mut(id).map_or(false, |w| !w.is_floating()) {
                        let new_x = backend
                            .window_mut(id)
                            .map(|w| w.geometry().x)
                            .unwrap_or(0);
                        let new_mon = find_monitor_idx(&monitor_rects, new_x);

                        if new_mon != mon {
                            // Window crossed to a different monitor — migrate to
                            // that monitor's active workspace.
                            let target_ws = monitor_ws[new_mon];
                            tracing::info!(
                                %id,
                                from_ws = %workspaces[ws_idx].name,
                                to_ws   = %workspaces[target_ws].name,
                                from_mon = mon,
                                to_mon   = new_mon,
                                "window dragged to new monitor — migrating workspace"
                            );
                            workspaces[ws_idx].remove_window(id);
                            workspaces[target_ws].add_window(id);
                            apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                            update_borders(&mut backend, &workspaces[ws_idx], &config);
                            apply_layout(&mut backend, &workspaces[target_ws], monitor_rects[new_mon], &config);
                            update_borders(&mut backend, &workspaces[target_ws], &config);
                        } else {
                            // Same monitor — swap tile positions within workspace.
                            let dragged_center = backend
                                .window_mut(id)
                                .map(|w| center_of(w.geometry()))
                                .unwrap_or((0, 0));

                            let candidates: Vec<(WindowId, (i32, i32))> = workspaces[ws_idx]
                                .windows()
                                .iter()
                                .copied()
                                .filter(|&wid| wid != id)
                                .filter_map(|wid| {
                                    backend.window_mut(wid).and_then(|w| {
                                        if !w.is_floating() && !w.is_minimized() {
                                            Some((wid, center_of(w.geometry())))
                                        } else {
                                            None
                                        }
                                    })
                                })
                                .collect();

                            if let Some((target, _)) = candidates
                                .into_iter()
                                .min_by_key(|(_, c)| dist_sq(dragged_center, *c))
                            {
                                let windows = workspaces[ws_idx].windows_mut();
                                if let (Some(a), Some(b)) = (
                                    windows.iter().position(|&w| w == id),
                                    windows.iter().position(|&w| w == target),
                                ) {
                                    windows.swap(a, b);
                                }
                            }

                            apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                            update_borders(&mut backend, &workspaces[ws_idx], &config);
                        }
                    }
                }
            }

            Event::WorkAreaChanged => {
                // An appbar (e.g. YASB) registered or unregistered — re-query work areas
                // and relayout every monitor's active workspace so windows don't overlap bars.
                let new_rects = backend.monitor_rects();
                tracing::info!(
                    old = ?monitor_rects,
                    new = ?new_rects,
                    "WorkAreaChanged: re-querying monitor rects"
                );
                monitor_rects = new_rects;
                for mon in 0..n_mons {
                    let ws_idx = monitor_ws[mon];
                    apply_layout(&mut backend, &workspaces[ws_idx], monitor_rects[mon], &config);
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
                }
                broadcast_yasb(&mut backend, &workspaces, &monitor_ws, ws_per_mon, focused_mon);
            }

            Event::Keybinding(kb_id) => {
                if let Some(action) = bindings.action(kb_id) {
                    tracing::info!(?action, "keybinding fired");
                    dispatch(
                        action.clone(),
                        &mut backend,
                        &mut workspaces,
                        &mut monitor_ws,
                        &mut focused_mon,
                        ws_per_mon,
                        &monitor_rects,
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

/// Apply tiling layout to all non-floating, non-minimised windows in `workspace`
/// using `monitor` as the usable area.
///
/// The monitor rect is supplied by the caller — it is the rect for the monitor
/// this workspace is assigned to, NOT derived from the windows' current physical
/// positions.  This avoids misassignment when a newly-spawned window has not yet
/// been moved to the correct physical location.
fn apply_layout<B: Backend>(
    backend: &mut B,
    workspace: &Workspace,
    monitor: Rect,
    config: &Config,
) {
    let tiled: Vec<WindowId> = workspace
        .windows()
        .iter()
        .copied()
        .filter(|&id| {
            !workspace.is_fullscreen(id)
                && backend
                    .window_mut(id)
                    .map_or(false, |w| !w.is_floating() && !w.is_minimized())
        })
        .collect();

    let area  = apply_padding(monitor, config.padding);
    let slots = layout::compute(&workspace.layout, area, tiled.len());
    for (&id, slot) in tiled.iter().zip(slots.iter()) {
        let rect = inset(slot.rect, config.gap);
        if let Some(win) = backend.window_mut(id) {
            if let Err(e) = win.set_geometry(rect) {
                tracing::warn!(%id, err = %e, "set_geometry failed");
            }
        }
    }

    for id in workspace.fullscreen_windows() {
        let rect = backend.monitor_full_rect_for_window(id);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.enter_fullscreen_geometry(rect) {
                tracing::warn!(%id, err = %e, "enter_fullscreen_geometry failed");
            }
        }
    }
}

fn inset(r: Rect, gap: u32) -> Rect {
    let g = gap as i32;
    Rect::new(
        r.x + g,
        r.y + g,
        r.width.saturating_sub(2 * gap),
        r.height.saturating_sub(2 * gap),
    )
}

fn apply_padding(r: Rect, p: Padding) -> Rect {
    Rect::new(
        r.x + p.left as i32,
        r.y + p.top as i32,
        r.width.saturating_sub(p.left + p.right),
        r.height.saturating_sub(p.top + p.bottom),
    )
}

/// Return the monitor rect for a given workspace index.
fn ws_monitor_rect(ws_idx: usize, ws_per_mon: usize, monitor_rects: &[Rect]) -> Rect {
    let mon = (ws_idx / ws_per_mon).min(monitor_rects.len().saturating_sub(1));
    monitor_rects
        .get(mon)
        .copied()
        .unwrap_or(Rect::new(0, 0, 1920, 1080))
}

// ── Border helpers ────────────────────────────────────────────────────────────

/// Set `id` as the one focused window globally:
/// - Clears `focused` on every other currently-visible workspace so only one
///   window across all monitors ever shows the active border colour.
/// - Repaints borders for every visible workspace in one pass.
fn set_global_focus<B: Backend>(
    id: WindowId,
    focused_ws: usize,
    backend: &mut B,
    workspaces: &mut Vec<Workspace>,
    monitor_ws: &[usize],
    config: &Config,
) {
    for &ws_idx in monitor_ws {
        workspaces[ws_idx].focused = if ws_idx == focused_ws { Some(id) } else { None };
    }
    for &ws_idx in monitor_ws {
        update_borders(backend, &workspaces[ws_idx], config);
    }
    // Monocle: all windows share the same screen rect — raise the focused
    // window to the top of the non-topmost z-band so it is visible.
    if workspaces[focused_ws].layout == LayoutKind::Monocle {
        if let Some(w) = backend.window_mut(id) {
            let _ = w.raise();
        }
    }
}

fn update_borders<B: Backend>(backend: &mut B, workspace: &Workspace, config: &Config) {
    let active_color   = parse_hex_color(&config.border_active);
    let inactive_color = parse_hex_color(&config.border_inactive);
    let is_monocle     = workspace.layout == LayoutKind::Monocle;

    for &id in workspace.windows() {
        let is_full      = workspace.is_fullscreen(id);
        let is_minimized = backend.window_mut(id).map_or(false, |w| w.is_minimized());

        // Detect app-native fullscreen (browser F11, game, YouTube fullscreen):
        // window geometry covers the full monitor rect including taskbar area.
        let geometry      = backend.window_mut(id).map(|w| w.geometry());
        let monitor_full  = backend.monitor_full_rect_for_window(id);
        let is_native_full = geometry.map_or(false, |g| rect_covers(g, monitor_full));

        if is_full || is_minimized || is_native_full {
            if let Some(w) = backend.window_mut(id) {
                let _ = w.hide_border_overlay();
            }
            continue;
        }

        // In monocle mode all windows share the same rect stacked in z-order.
        // Showing a border overlay on a non-focused window would cause the
        // WS_EX_TOPMOST overlay to appear above the focused window.  Only the
        // focused window gets a visible border; all others have their overlay
        // hidden so they don't bleed through.
        let is_focused = workspace.focused == Some(id);
        if is_monocle && !is_focused {
            if let Some(w) = backend.window_mut(id) {
                let _ = w.hide_border_overlay();
            }
            continue;
        }

        let color = if is_focused { active_color } else { inactive_color };
        if let Some(w) = backend.window_mut(id) {
            let _ = w.set_border_overlay(color, config.border_width, config.border_radius);
            let _ = w.set_border_color(color);
        }
    }
}

/// Returns `true` if `window` fully covers `monitor`.
///
/// Uses `<=` / `>=` (not `==`) because `GetWindowRect` includes invisible DWM
/// resize borders (~8 px on Win10/11), making a maximised window slightly larger
/// than `rcMonitor`.  Any window that covers the monitor — including ones with
/// invisible gutters — counts as native-fullscreen.
fn rect_covers(window: Rect, monitor: Rect) -> bool {
    window.x <= monitor.x
        && window.y <= monitor.y
        && (window.x + window.width  as i32) >= (monitor.x + monitor.width  as i32)
        && (window.y + window.height as i32) >= (monitor.y + monitor.height as i32)
}

fn parse_hex_color(s: &str) -> u32 {
    let s = s.trim_start_matches('#');
    u32::from_str_radix(s, 16).unwrap_or(0x88_88_88)
}

// ── YASB broadcast ────────────────────────────────────────────────────────────

fn broadcast_yasb<B: Backend>(
    backend: &mut B,
    workspaces: &[Workspace],
    monitor_ws: &[usize],
    ws_per_mon: usize,
    focused_mon: usize,
) {
    // Build JSON with an immutable borrow, then broadcast with a mutable one.
    let json = build_yasb_json(backend as &B, workspaces, monitor_ws, ws_per_mon, focused_mon);
    backend.broadcast_state(&json);
}

/// Build the JSON state payload sent to YASB over the named pipe.
///
/// Extends the komorebi-compatible monitor/workspace envelope with shellwright-
/// specific fields: `layout` (current tiling strategy) and `focused_window`
/// (title of the focused window on each workspace).
fn build_yasb_json<B: Backend>(
    backend: &B,
    workspaces: &[Workspace],
    monitor_ws: &[usize],
    ws_per_mon: usize,
    focused_mon: usize,
) -> String {
    let n_mons = monitor_ws.len();
    let n_ws   = workspaces.len();
    let all_windows = backend.windows();

    let mut monitors_json = String::new();
    for mon_idx in 0..n_mons {
        if mon_idx > 0 {
            monitors_json.push(',');
        }

        let ws_start      = mon_idx * ws_per_mon;
        let ws_end        = (ws_start + ws_per_mon).min(n_ws);
        let active_ws     = monitor_ws[mon_idx];
        let focused_local = if active_ws >= ws_start && active_ws < ws_end {
            active_ws - ws_start
        } else {
            0
        };

        let mut ws_arr = String::new();
        for ws_idx in ws_start..ws_end {
            if ws_idx > ws_start {
                ws_arr.push(',');
            }
            let ws        = &workspaces[ws_idx];
            let local_idx = ws_idx - ws_start;
            let n_windows = ws.windows().len();

            // Window elements — just sequential IDs for compatibility.
            let win_elements: String = (0..n_windows)
                .map(|i| format!(r#"{{"id":{}}}"#, i))
                .collect::<Vec<_>>()
                .join(",");

            // Index of focused window within this workspace's window list.
            let focused_win_idx = ws.focused
                .and_then(|fid| ws.windows().iter().position(|&w| w == fid))
                .unwrap_or(0);

            // Title of the focused window (empty string if none).
            let focused_title = ws.focused
                .and_then(|fid| all_windows.iter().find(|w| w.id() == fid))
                .map(|w| w.title().to_string())
                .unwrap_or_default();

            let layout_name = layout_kind_str(&ws.layout);

            ws_arr.push_str(&format!(
                r#"{{"name":"{}","index":{},"layout":"{}","focused_window":"{}","windows":{{"elements":[{}],"focused":{}}}}}"#,
                json_escape(&ws.name),
                local_idx,
                layout_name,
                json_escape(&focused_title),
                win_elements,
                focused_win_idx,
            ));
        }

        monitors_json.push_str(&format!(
            r#"{{"name":"Monitor {}","index":{},"workspaces":{{"elements":[{}],"focused":{}}}}}"#,
            mon_idx + 1,
            mon_idx,
            ws_arr,
            focused_local,
        ));
    }

    format!(
        "{}\n",
        format!(
            r#"{{"monitors":{{"elements":[{}],"focused":{}}}}}"#,
            monitors_json,
            focused_mon,
        )
    )
}

fn layout_kind_str(kind: &LayoutKind) -> &'static str {
    match kind {
        LayoutKind::Fibonacci      => "fibonacci",
        LayoutKind::Bsp            => "bsp",
        LayoutKind::Columns { .. } => "columns",
        LayoutKind::Monocle        => "monocle",
        LayoutKind::CenterMain     => "center_main",
        LayoutKind::Float          => "float",
    }
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

// ── Animation helpers ─────────────────────────────────────────────────────────

fn ease_out(t: f32) -> f32 { 1.0 - (1.0 - t).powi(2) }

fn lerp_i32(a: i32, b: i32, t: f32) -> i32 {
    (a as f32 + (b - a) as f32 * t).round() as i32
}

fn lerp_u32(a: u32, b: u32, t: f32) -> u32 {
    ((a as f32 + (b as i32 - a as i32) as f32 * t).max(0.0)).round() as u32
}

fn lerp_rect(from: Rect, to: Rect, t: f32) -> Rect {
    Rect::new(
        lerp_i32(from.x,      to.x,      t),
        lerp_i32(from.y,      to.y,      t),
        lerp_u32(from.width,  to.width,  t),
        lerp_u32(from.height, to.height, t),
    )
}


/// Like [`apply_layout`] but smoothly interpolates each window from its current
/// position to the target rect over `duration_ms` using an ease-out curve.
fn apply_layout_animated<B: Backend>(
    backend:     &mut B,
    workspace:   &Workspace,
    monitor:     Rect,
    config:      &Config,
    duration_ms: u32,
) {
    let tiled: Vec<WindowId> = workspace
        .windows()
        .iter()
        .copied()
        .filter(|&id| {
            !workspace.is_fullscreen(id)
                && backend.window_mut(id).map_or(false, |w| !w.is_floating() && !w.is_minimized())
        })
        .collect();

    let area  = apply_padding(monitor, config.padding);
    let slots = layout::compute(&workspace.layout, area, tiled.len());

    let from_rects: Vec<Rect> = tiled
        .iter()
        .map(|&id| backend.window_mut(id).map(|w| w.geometry()).unwrap_or(area))
        .collect();
    let to_rects: Vec<Rect> = tiled
        .iter()
        .zip(slots.iter())
        .map(|(_, s)| inset(s.rect, config.gap))
        .collect();

    let frames   = config.animations.frames.clamp(1, 60);
    let total    = std::time::Duration::from_millis(duration_ms as u64);
    let deadline = std::time::Instant::now() + total;
    let start    = deadline - total;
    for frame in 1..=frames {
        let t = ease_out(frame as f32 / frames as f32);
        for (i, &id) in tiled.iter().enumerate() {
            if from_rects[i] == to_rects[i] { continue; }
            let rect = lerp_rect(from_rects[i], to_rects[i], t);
            if let Some(win) = backend.window_mut(id) { let _ = win.set_geometry(rect); }
        }
        let frame_target = start + total.mul_f32(frame as f32 / frames as f32);
        let now = std::time::Instant::now();
        if frame_target > now { std::thread::sleep(frame_target - now); }
    }
    // Snap to exact target positions.
    for (&id, &to) in tiled.iter().zip(to_rects.iter()) {
        if let Some(win) = backend.window_mut(id) { let _ = win.set_geometry(to); }
    }
    // Fullscreen windows always snap immediately (use raw monitor bounds).
    for id in workspace.fullscreen_windows() {
        let rect = backend.monitor_full_rect_for_window(id);
        if let Some(w) = backend.window_mut(id) { let _ = w.enter_fullscreen_geometry(rect); }
    }
}

/// Pre-compute `(id, from_rect, to_rect)` for every tiled window in `workspace`.
///
/// Uses immutable access so the caller can collect targets for two workspaces
/// before starting a combined animation loop.
fn compute_layout_targets<B: Backend>(
    backend:   &B,
    workspace: &Workspace,
    monitor:   Rect,
    config:    &Config,
) -> Vec<(WindowId, Rect, Rect)> {
    let area  = apply_padding(monitor, config.padding);
    let wins  = backend.windows();

    let tiled: Vec<WindowId> = workspace
        .windows()
        .iter()
        .copied()
        .filter(|&id| {
            !workspace.is_fullscreen(id)
                && wins.iter().find(|w| w.id() == id)
                    .map_or(false, |w| !w.is_floating() && !w.is_minimized())
        })
        .collect();

    let slots = layout::compute(&workspace.layout, area, tiled.len());
    tiled
        .iter()
        .zip(slots.iter())
        .map(|(&id, slot)| {
            let from = wins.iter().find(|w| w.id() == id)
                .map(|w| w.geometry())
                .unwrap_or(area);
            let to = inset(slot.rect, config.gap);
            (id, from, to)
        })
        .collect()
}

/// Animate windows on *two* monitors simultaneously: each window moves from
/// `from` to `to` in a single shared ease-out loop, then snaps to exact targets.
///
/// `targets` is a flat slice of `(id, from, to)` entries for all windows on
/// both the source and destination monitors.
fn animate_all_targets<B: Backend>(
    backend:     &mut B,
    targets:     &[(WindowId, Rect, Rect)],
    duration_ms: u32,
    frames:      u32,
) {
    let frames = frames.clamp(1, 60);
    let total   = std::time::Duration::from_millis(duration_ms as u64);
    let start   = std::time::Instant::now();
    for frame in 1..=frames {
        let t = ease_out(frame as f32 / frames as f32);
        for &(id, from, to) in targets {
            if from == to { continue; }
            let rect = lerp_rect(from, to, t);
            if let Some(win) = backend.window_mut(id) { let _ = win.set_geometry(rect); }
        }
        let frame_target = start + total.mul_f32(frame as f32 / frames as f32);
        let now = std::time::Instant::now();
        if frame_target > now { std::thread::sleep(frame_target - now); }
    }
    // Snap every window to its exact target.
    for &(id, _, to) in targets {
        if let Some(win) = backend.window_mut(id) { let _ = win.set_geometry(to); }
    }
}

// ── Action dispatch ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn dispatch<B: Backend>(
    action: Action,
    backend: &mut B,
    workspaces: &mut Vec<Workspace>,
    monitor_ws: &mut Vec<usize>,
    focused_mon: &mut usize,
    ws_per_mon: usize,
    monitor_rects: &[Rect],
    config: &Config,
) {
    let n_mons = monitor_ws.len();
    let n_ws   = workspaces.len();
    let cur_ws = monitor_ws[*focused_mon];
    let cur_mon_rect = ws_monitor_rect(cur_ws, ws_per_mon, monitor_rects);

    match action {
        // ── Focus ─────────────────────────────────────────────────────────────
        Action::FocusNext => {
            // Gather non-minimised windows from all visible workspaces,
            // ordered monitor-by-monitor so focus wraps across screens.
            let candidates: Vec<(WindowId, usize)> = (0..n_mons)
                .flat_map(|m| {
                    let ws = monitor_ws[m];
                    workspaces[ws]
                        .windows()
                        .iter()
                        .copied()
                        .filter(|&id| !backend.window_mut(id).map_or(false, |w| w.is_minimized()))
                        .map(move |id| (id, m))
                        .collect::<Vec<_>>()
                })
                .collect();
            if candidates.is_empty() { return; }
            let cur_focused = workspaces[cur_ws].focused;
            let pos = cur_focused
                .and_then(|f| candidates.iter().position(|(id, _)| *id == f))
                .unwrap_or(0);
            let (next_id, next_mon) = candidates[(pos + 1) % candidates.len()];
            let next_ws = monitor_ws[next_mon];
            *focused_mon = next_mon;
            set_global_focus(next_id, next_ws, backend, workspaces, monitor_ws, config);
            if let Some(w) = backend.window_mut(next_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %next_id, err = %e, "focus failed");
                }
            }
        }

        Action::FocusPrev => {
            let candidates: Vec<(WindowId, usize)> = (0..n_mons)
                .flat_map(|m| {
                    let ws = monitor_ws[m];
                    workspaces[ws]
                        .windows()
                        .iter()
                        .copied()
                        .filter(|&id| !backend.window_mut(id).map_or(false, |w| w.is_minimized()))
                        .map(move |id| (id, m))
                        .collect::<Vec<_>>()
                })
                .collect();
            if candidates.is_empty() { return; }
            let cur_focused = workspaces[cur_ws].focused;
            let pos = cur_focused
                .and_then(|f| candidates.iter().position(|(id, _)| *id == f))
                .unwrap_or(0);
            let prev = if pos == 0 { candidates.len() - 1 } else { pos - 1 };
            let (prev_id, prev_mon) = candidates[prev];
            let prev_ws = monitor_ws[prev_mon];
            *focused_mon = prev_mon;
            set_global_focus(prev_id, prev_ws, backend, workspaces, monitor_ws, config);
            if let Some(w) = backend.window_mut(prev_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %prev_id, err = %e, "focus failed");
                }
            }
        }

        // ── Window manipulation ───────────────────────────────────────────────
        Action::MoveNext => {
            if let Some(focused) = workspaces[cur_ws].focused {
                // Only count non-minimised windows for position and edge detection.
                // Minimised windows are invisible to the user and should be skipped.
                let active: Vec<WindowId> = workspaces[cur_ws]
                    .windows()
                    .iter()
                    .copied()
                    .filter(|&id| !backend.window_mut(id).map_or(false, |w| w.is_minimized()))
                    .collect();
                let len = active.len();
                let pos = active.iter().position(|&w| w == focused);
                if let Some(pos) = pos {
                    let at_edge = pos + 1 >= len;
                    let right_mon = *focused_mon + 1;
                    if at_edge && right_mon < n_mons {
                        // Cross to right monitor's active workspace.
                        let target_ws = monitor_ws[right_mon];
                        workspaces[cur_ws].remove_window(focused);
                        // Insert at front: entering from the left side of the right workspace.
                        workspaces[target_ws].windows_mut().insert(0, focused);
                        *focused_mon = right_mon;
                        tracing::info!(%focused, to_ws = %workspaces[target_ws].name, "MoveNext: crossed to right monitor");
                        let src_rect = monitor_rects.get(right_mon - 1).copied().unwrap_or(cur_mon_rect);
                        let tgt_rect = monitor_rects.get(right_mon).copied().unwrap_or(cur_mon_rect);
                        if config.animations.enabled && backend.system_animations_enabled() {
                            // Compute targets for both monitors while backend is immutably
                            // accessible, then run one combined loop.
                            let mut targets = compute_layout_targets(backend, &workspaces[cur_ws], src_rect, config);
                            targets.extend(compute_layout_targets(backend, &workspaces[target_ws], tgt_rect, config));
                            animate_all_targets(backend, &targets, config.animations.duration_ms, config.animations.frames);
                        } else {
                            apply_layout(backend, &workspaces[cur_ws], src_rect, config);
                            apply_layout(backend, &workspaces[target_ws], tgt_rect, config);
                        }
                        set_global_focus(focused, target_ws, backend, workspaces, monitor_ws, config);
                        if let Some(w) = backend.window_mut(focused) { let _ = w.focus(); }
                    } else {
                        // Swap with the next non-minimised window using full-list indices.
                        let next_id = active[(pos + 1) % len];
                        let windows = workspaces[cur_ws].windows_mut();
                        if let (Some(a), Some(b)) = (
                            windows.iter().position(|&w| w == focused),
                            windows.iter().position(|&w| w == next_id),
                        ) {
                            windows.swap(a, b);
                        }
                        if config.animations.enabled && backend.system_animations_enabled() {
                            apply_layout_animated(backend, &workspaces[cur_ws], cur_mon_rect, config, config.animations.duration_ms);
                        } else {
                            apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
                        }
                        update_borders(backend, &workspaces[cur_ws], config);
                    }
                }
            }
        }

        Action::MovePrev => {
            if let Some(focused) = workspaces[cur_ws].focused {
                // Only count non-minimised windows for position and edge detection.
                let active: Vec<WindowId> = workspaces[cur_ws]
                    .windows()
                    .iter()
                    .copied()
                    .filter(|&id| !backend.window_mut(id).map_or(false, |w| w.is_minimized()))
                    .collect();
                let len = active.len();
                let pos = active.iter().position(|&w| w == focused);
                if let Some(pos) = pos {
                    let at_edge = pos == 0;
                    let left_mon = focused_mon.checked_sub(1);
                    if at_edge && left_mon.is_some() {
                        let left_mon = left_mon.unwrap();
                        let target_ws = monitor_ws[left_mon];
                        workspaces[cur_ws].remove_window(focused);
                        // Push to end: entering from the right side of the left workspace.
                        workspaces[target_ws].windows_mut().push(focused);
                        *focused_mon = left_mon;
                        tracing::info!(%focused, to_ws = %workspaces[target_ws].name, "MovePrev: crossed to left monitor");
                        let src_rect = monitor_rects.get(left_mon + 1).copied().unwrap_or(cur_mon_rect);
                        let tgt_rect = monitor_rects.get(left_mon).copied().unwrap_or(cur_mon_rect);
                        if config.animations.enabled && backend.system_animations_enabled() {
                            let mut targets = compute_layout_targets(backend, &workspaces[cur_ws], src_rect, config);
                            targets.extend(compute_layout_targets(backend, &workspaces[target_ws], tgt_rect, config));
                            animate_all_targets(backend, &targets, config.animations.duration_ms, config.animations.frames);
                        } else {
                            apply_layout(backend, &workspaces[cur_ws], src_rect, config);
                            apply_layout(backend, &workspaces[target_ws], tgt_rect, config);
                        }
                        set_global_focus(focused, target_ws, backend, workspaces, monitor_ws, config);
                        if let Some(w) = backend.window_mut(focused) { let _ = w.focus(); }
                    } else {
                        // Swap with the previous non-minimised window using full-list indices.
                        let prev_id = active[if pos == 0 { len - 1 } else { pos - 1 }];
                        let windows = workspaces[cur_ws].windows_mut();
                        if let (Some(a), Some(b)) = (
                            windows.iter().position(|&w| w == focused),
                            windows.iter().position(|&w| w == prev_id),
                        ) {
                            windows.swap(a, b);
                        }
                        if config.animations.enabled && backend.system_animations_enabled() {
                            apply_layout_animated(backend, &workspaces[cur_ws], cur_mon_rect, config, config.animations.duration_ms);
                        } else {
                            apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
                        }
                        update_borders(backend, &workspaces[cur_ws], config);
                    }
                }
            }
        }

        Action::KillFocused => {
            if let Some(id) = workspaces[cur_ws].focused {
                tracing::info!(%id, "kill focused");
                if let Some(w) = backend.window_mut(id) {
                    if let Err(e) = w.close() {
                        tracing::warn!(%id, err = %e, "close failed");
                    }
                }
            }
        }

        Action::ToggleFloat => {
            if let Some(id) = workspaces[cur_ws].focused {
                if let Some(w) = backend.window_mut(id) {
                    let now = !w.is_floating();
                    w.set_floating(now);
                    tracing::debug!(%id, floating = now, "toggle float");
                }
                apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
                update_borders(backend, &workspaces[cur_ws], config);
                broadcast_yasb(backend, workspaces, monitor_ws, ws_per_mon, *focused_mon);
            }
        }

        Action::ToggleFullscreen => {
            let fs_rect = workspaces[cur_ws].focused
                .map(|id| backend.monitor_full_rect_for_window(id))
                .unwrap_or(cur_mon_rect);
            toggle_fullscreen_for(
                workspaces[cur_ws].focused,
                backend,
                &mut workspaces[cur_ws],
                cur_mon_rect,
                fs_rect,
                config,
            );
            broadcast_yasb(backend, workspaces, monitor_ws, ws_per_mon, *focused_mon);
        }

        // ── Layout ───────────────────────────────────────────────────────────
        Action::SetLayout(kind) => {
            tracing::info!(?kind, workspace = %workspaces[cur_ws].name, "set layout");
            workspaces[cur_ws].layout = kind;
            apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
            update_borders(backend, &workspaces[cur_ws], config);
            // Monocle: raise focused window to top after layout switch.
            if workspaces[cur_ws].layout == LayoutKind::Monocle {
                if let Some(fid) = workspaces[cur_ws].focused {
                    if let Some(w) = backend.window_mut(fid) { let _ = w.raise(); }
                }
            }
            broadcast_yasb(backend, workspaces, monitor_ws, ws_per_mon, *focused_mon);
        }

        // ── Workspaces ───────────────────────────────────────────────────────
        Action::SwitchWorkspace(n) => {
            let idx = (n as usize).saturating_sub(1);
            tracing::info!(
                n,
                idx,
                focused_mon = *focused_mon,
                cur_ws,
                ws_per_mon,
                n_mons,
                n_ws,
                "SwitchWorkspace"
            );
            if idx >= n_ws {
                tracing::warn!(idx, n_ws, "workspace index out of range");
                return;
            }
            // Switch only the monitor that owns this workspace.
            let mon = (idx / ws_per_mon).min(n_mons - 1);
            let current = monitor_ws[mon];
            tracing::info!(
                target_ws  = idx,
                target_mon = mon,
                current_ws = current,
                "SwitchWorkspace: monitor resolved"
            );
            if current == idx {
                tracing::info!(idx, "already on requested workspace — no-op");
                return;
            }

            // Hide every window on the outgoing workspace.
            let outgoing: Vec<WindowId> = workspaces[current].windows().to_vec();
            let animate = config.animations.enabled && backend.system_animations_enabled();

            monitor_ws[mon] = idx;
            *focused_mon = mon;

            let mr = monitor_rects.get(mon).copied().unwrap_or(cur_mon_rect);

            // Pre-show incoming at alpha=0 so the crossfade can start immediately.
            let incoming: Vec<WindowId> = workspaces[idx].windows().to_vec();
            if animate {
                for &id in &incoming {
                    if let Some(w) = backend.window_mut(id) { let _ = w.set_alpha(0); }
                }
            }
            tracing::info!(
                ws = %workspaces[idx].name,
                count = incoming.len(),
                "showing incoming workspace windows"
            );
            for &id in &incoming {
                if let Some(w) = backend.window_mut(id) {
                    let res = w.show();
                    tracing::debug!(%id, ?res, "show");
                }
            }
            apply_layout(backend, &workspaces[idx], mr, config);
            update_borders(backend, &workspaces[idx], config);

            tracing::info!(
                ws = %workspaces[current].name,
                count = outgoing.len(),
                "hiding outgoing workspace windows"
            );

            // Crossfade: fade outgoing out and incoming in simultaneously.
            if animate {
                let frames = config.animations.frames.clamp(1, 60);
                let total  = std::time::Duration::from_millis(config.animations.duration_ms as u64);
                let start  = std::time::Instant::now();
                for frame in 1..=frames {
                    let t         = frame as f32 / frames as f32;
                    let alpha_out = ((1.0 - t) * 255.0) as u8;
                    let alpha_in  = (t * 255.0) as u8;
                    for &id in &outgoing {
                        if let Some(w) = backend.window_mut(id) { let _ = w.set_alpha(alpha_out); }
                    }
                    for &id in &incoming {
                        if let Some(w) = backend.window_mut(id) { let _ = w.set_alpha(alpha_in); }
                    }
                    let frame_target = start + total.mul_f32(t);
                    let now = std::time::Instant::now();
                    if frame_target > now { std::thread::sleep(frame_target - now); }
                }
            }

            for &id in &outgoing {
                if let Some(w) = backend.window_mut(id) {
                    if config.taskbar_mode == TaskbarMode::Global {
                        let res = w.park();
                        tracing::debug!(%id, ?res, "park (global taskbar mode)");
                    } else {
                        let res = w.hide();
                        tracing::debug!(%id, ?res, "hide");
                    }
                }
            }
            // Clear layered from all: outgoing (hidden, restore for next show) and incoming.
            if animate {
                for &id in outgoing.iter().chain(incoming.iter()) {
                    if let Some(w) = backend.window_mut(id) { let _ = w.clear_alpha(); }
                }
            }

            // Restore or auto-pick focus.
            if workspaces[idx].focused.is_none() {
                let first = workspaces[idx]
                    .windows()
                    .iter()
                    .copied()
                    .find(|&id| {
                        !backend.window_mut(id).map_or(false, |w| w.is_minimized())
                    });
                workspaces[idx].focused = first;
            }
            if let Some(id) = workspaces[idx].focused {
                tracing::debug!(%id, "focusing first window on new workspace");
                set_global_focus(id, idx, backend, workspaces, monitor_ws, config);
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.focus();
                }
            }

            broadcast_yasb(backend, workspaces, monitor_ws, ws_per_mon, *focused_mon);
            tracing::info!(
                workspace = %workspaces[idx].name,
                monitor   = mon,
                "switch workspace"
            );
        }

        Action::MoveFocusedToWorkspace(n) => {
            let target = (n as usize).saturating_sub(1);
            tracing::info!(
                n,
                target,
                cur_ws,
                focused_mon = *focused_mon,
                "MoveFocusedToWorkspace"
            );
            if target >= n_ws || target == cur_ws {
                tracing::info!(target, cur_ws, n_ws, "move: out of range or same workspace — no-op");
                return;
            }

            if let Some(id) = workspaces[cur_ws].focused {
                workspaces[cur_ws].exit_fullscreen(id);
                workspaces[cur_ws].remove_window(id);

                // Is the target workspace currently visible on some monitor?
                let target_mon = monitor_ws.iter().position(|&ws| ws == target);

                if let Some(tmon) = target_mon {
                    // Target is visible — move window there and relayout both workspaces.
                    workspaces[target].add_window(id);
                    let src_mr = ws_monitor_rect(cur_ws, ws_per_mon, monitor_rects);
                    let tgt_mr = monitor_rects.get(tmon).copied().unwrap_or(src_mr);
                    apply_layout(backend, &workspaces[cur_ws], src_mr, config);
                    update_borders(backend, &workspaces[cur_ws], config);
                    apply_layout(backend, &workspaces[target], tgt_mr, config);
                    update_borders(backend, &workspaces[target], config);
                } else {
                    // Target workspace is hidden — hide (or park) the window before moving.
                    if let Some(w) = backend.window_mut(id) {
                        if config.taskbar_mode == TaskbarMode::Global {
                            let _ = w.park();
                        } else {
                            let _ = w.hide();
                            let _ = w.hide_border_overlay();
                        }
                    }
                    workspaces[target].add_window(id);
                    let src_mr = ws_monitor_rect(cur_ws, ws_per_mon, monitor_rects);
                    apply_layout(backend, &workspaces[cur_ws], src_mr, config);
                    update_borders(backend, &workspaces[cur_ws], config);
                }

                tracing::info!(%id, to = %workspaces[target].name, "move window to workspace");
                broadcast_yasb(backend, workspaces, monitor_ws, ws_per_mon, *focused_mon);
            }
        }

        // ── WM lifecycle ─────────────────────────────────────────────────────
        Action::ReloadConfig => {
            tracing::info!("reload config requested — not yet implemented");
        }

        Action::Quit => {
            tracing::info!("quit action — restoring all hidden windows before exit");
            // Show all windows that are hidden (inactive workspaces) so they are not
            // left as orphaned inaccessible processes after the WM exits.
            let all_ids: Vec<WindowId> = backend.windows().iter().map(|w| w.id()).collect();
            for id in all_ids {
                if let Some(w) = backend.window_mut(id) {
                    let _ = w.show();
                }
            }
            std::process::exit(0);
        }
    }
}

// ── Fullscreen ────────────────────────────────────────────────────────────────

fn toggle_fullscreen_for<B: Backend>(
    target: Option<WindowId>,
    backend: &mut B,
    workspace: &mut Workspace,
    monitor: Rect,
    fullscreen_rect: Rect,
    config: &Config,
) {
    let id = match target {
        Some(id) => id,
        None => return,
    };

    if workspace.is_fullscreen(id) {
        workspace.exit_fullscreen(id);
        tracing::info!(%id, "exit fullscreen");
        // Demote to topmost only if still floating; otherwise return to normal z-order.
        if let Some(w) = backend.window_mut(id) {
            let _ = w.set_topmost(w.is_floating());
        }
        if config.animations.enabled && backend.system_animations_enabled() {
            apply_layout_animated(backend, workspace, monitor, config, config.animations.duration_ms);
        } else {
            apply_layout(backend, workspace, monitor, config);
        }
    } else {
        let saved = backend
            .window_mut(id)
            .map(|w| w.geometry())
            .unwrap_or(monitor);
        workspace.enter_fullscreen(id, saved);
        tracing::info!(%id, "enter fullscreen");

        // Assert topmost BEFORE animation starts so the window is already above
        // other topmost windows (e.g. YASB) during the fly-in.
        if let Some(w) = backend.window_mut(id) {
            let _ = w.set_topmost(true);
        }

        // Animate from current position to fullscreen rect.
        if config.animations.enabled && backend.system_animations_enabled() {
            let from = backend.window_mut(id).map(|w| w.geometry()).unwrap_or(fullscreen_rect);
            if from != fullscreen_rect {
                let frames = config.animations.frames.clamp(1, 60);
                let total  = std::time::Duration::from_millis(config.animations.duration_ms as u64);
                let start  = std::time::Instant::now();
                for frame in 1..=frames {
                    let t    = ease_out(frame as f32 / frames as f32);
                    let rect = lerp_rect(from, fullscreen_rect, t);
                    if let Some(w) = backend.window_mut(id) { let _ = w.set_geometry(rect); }
                    let frame_target = start + total.mul_f32(frame as f32 / frames as f32);
                    let now = std::time::Instant::now();
                    if frame_target > now { std::thread::sleep(frame_target - now); }
                }
            }
        }

        // Final snap: atomic TOPMOST + geometry in one DWM call so the window
        // lands at the front of the topmost stack — above YASB and the taskbar.
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.enter_fullscreen_geometry(fullscreen_rect) {
                tracing::warn!(%id, err = %e, "enter_fullscreen_geometry failed");
            }
        }
    }
    update_borders(backend, workspace, config);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the monitor index whose left edge (`x`) best matches `window_x`.
fn find_monitor_idx(rects: &[Rect], window_x: i32) -> usize {
    if rects.is_empty() {
        return 0;
    }
    rects
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| (r.x - window_x).abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Find the (workspace_index, monitor_index) for a window on a *visible* workspace.
fn find_visible_ws_and_mon(
    workspaces: &[Workspace],
    monitor_ws: &[usize],
    id: WindowId,
) -> Option<(usize, usize)> {
    for (mon, &ws_idx) in monitor_ws.iter().enumerate() {
        if workspaces[ws_idx].contains(id) {
            return Some((ws_idx, mon));
        }
    }
    None
}

/// Find the workspace index for a window on *any* workspace (visible or hidden).
fn find_any_workspace(workspaces: &[Workspace], id: WindowId) -> Option<usize> {
    workspaces.iter().position(|ws| ws.contains(id))
}

fn center_of(r: Rect) -> (i32, i32) {
    (r.x + r.width as i32 / 2, r.y + r.height as i32 / 2)
}

fn dist_sq(a: (i32, i32), b: (i32, i32)) -> i64 {
    let dx = (a.0 - b.0) as i64;
    let dy = (a.1 - b.1) as i64;
    dx * dx + dy * dy
}

/// Initialise the tracing subscriber.
///
/// On Windows (daemon mode — no console) logs are written to `log_path`.
/// On other platforms or if the log file cannot be created, logs go to stderr.
fn init_tracing(log_path: &std::path::Path) {
    use std::sync::Mutex;
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env("SHELLWRIGHT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    #[cfg(target_os = "windows")]
    {
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
        {
            fmt()
                .with_env_filter(filter)
                .with_writer(Mutex::new(file))
                .with_ansi(false)
                .init();
            return;
        }
    }

    // Fallback: stderr (useful on non-Windows or if file open fails).
    let _ = log_path; // suppress unused-variable warning on non-Windows
    fmt().with_env_filter(filter).init();
}

/// Return the per-user shellwright config directory.
///
/// Windows: `%APPDATA%\shellwright`
/// Linux/macOS: `$XDG_CONFIG_HOME/shellwright` (or `~/.config/shellwright`)
fn resolve_config_dir() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").context("APPDATA not set")?;
        Ok(std::path::PathBuf::from(appdata).join("shellwright"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home).join(".config")
            });
        Ok(base.join("shellwright"))
    }
}

// ── Autostart (Windows Run registry key) ─────────────────────────────────────

/// Register shellwright to start automatically at Windows login.
///
/// Writes `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run\shellwright`
/// pointing to the current executable path.  Uses `reg.exe` (available on all
/// Windows versions) so no extra crate dependency is needed.
fn autostart_register() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let exe = std::env::current_exe().context("cannot determine exe path")?;
        let exe_str = exe.to_string_lossy();
        let output = std::process::Command::new("reg")
            .args([
                "add",
                r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                "/v", "shellwright",
                "/t", "REG_SZ",
                "/d", exe_str.as_ref(),
                "/f",
            ])
            .output()
            .context("failed to run reg.exe")?;
        if output.status.success() {
            eprintln!("shellwright: autostart registered ({exe_str})");
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("reg add failed: {err}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    eprintln!("autostart-register is a Windows-only command");
    Ok(())
}

/// Remove the shellwright autostart registry entry.
fn autostart_unregister() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let output = std::process::Command::new("reg")
            .args([
                "delete",
                r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                "/v", "shellwright",
                "/f",
            ])
            .output()
            .context("failed to run reg.exe")?;
        if output.status.success() {
            eprintln!("shellwright: autostart unregistered");
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            // "The system was unable to find the specified registry key or value."
            // is not a hard error — key simply wasn't there.
            eprintln!("shellwright: autostart-unregister: {err}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    eprintln!("autostart-unregister is a Windows-only command");
    Ok(())
}
