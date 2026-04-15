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
    let monitor_rects = backend.monitor_rects();
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
                tracing::info!("quit — shutting down");
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
                }
            }

            Event::WindowFocused(id) => {
                tracing::debug!(%id, "focused");
                if let Some((ws_idx, mon)) =
                    find_visible_ws_and_mon(&workspaces, &monitor_ws, id)
                {
                    focused_mon = mon;
                    workspaces[ws_idx].focused = Some(id);
                    update_borders(&mut backend, &workspaces[ws_idx], &config);
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
        let rect = apply_padding(monitor, config.padding);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.set_geometry(rect) {
                tracing::warn!(%id, err = %e, "set_geometry (fullscreen) failed");
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

fn update_borders<B: Backend>(backend: &mut B, workspace: &Workspace, config: &Config) {
    let active_color   = parse_hex_color(&config.border_active);
    let inactive_color = parse_hex_color(&config.border_inactive);

    for &id in workspace.windows() {
        let is_full      = workspace.is_fullscreen(id);
        let is_minimized = backend.window_mut(id).map_or(false, |w| w.is_minimized());

        if is_full || is_minimized {
            if let Some(w) = backend.window_mut(id) {
                let _ = w.hide_border_overlay();
            }
            continue;
        }

        let color = if workspace.focused == Some(id) { active_color } else { inactive_color };
        if let Some(w) = backend.window_mut(id) {
            let _ = w.set_border_overlay(color, config.border_width, config.border_radius);
            let _ = w.set_border_color(color);
        }
    }
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
    let json = build_yasb_json(workspaces, monitor_ws, ws_per_mon, focused_mon);
    backend.broadcast_state(&json);
}

/// Build a komorebi-compatible JSON state line for YASB.
fn build_yasb_json(
    workspaces: &[Workspace],
    monitor_ws: &[usize],
    ws_per_mon: usize,
    focused_mon: usize,
) -> String {
    let n_mons = monitor_ws.len();
    let n_ws   = workspaces.len();

    let mut monitors_json = String::new();
    for mon_idx in 0..n_mons {
        if mon_idx > 0 {
            monitors_json.push(',');
        }

        let ws_start    = mon_idx * ws_per_mon;
        let ws_end      = (ws_start + ws_per_mon).min(n_ws);
        let active_ws   = monitor_ws[mon_idx];
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
            let win_elements: String = (0..n_windows)
                .map(|i| format!(r#"{{"id":{}}}"#, i))
                .collect::<Vec<_>>()
                .join(",");
            ws_arr.push_str(&format!(
                r#"{{"name":"{}","index":{},"windows":{{"elements":[{}],"focused":0}}}}"#,
                json_escape(&ws.name),
                local_idx,
                win_elements,
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

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
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
            workspaces[next_ws].focused = Some(next_id);
            *focused_mon = next_mon;
            if let Some(w) = backend.window_mut(next_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %next_id, err = %e, "focus failed");
                }
            }
            for m in 0..n_mons {
                update_borders(backend, &workspaces[monitor_ws[m]], config);
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
            workspaces[prev_ws].focused = Some(prev_id);
            *focused_mon = prev_mon;
            if let Some(w) = backend.window_mut(prev_id) {
                if let Err(e) = w.focus() {
                    tracing::warn!(id = %prev_id, err = %e, "focus failed");
                }
            }
            for m in 0..n_mons {
                update_borders(backend, &workspaces[monitor_ws[m]], config);
            }
        }

        // ── Window manipulation ───────────────────────────────────────────────
        Action::MoveNext => {
            if let Some(focused) = workspaces[cur_ws].focused {
                let len = workspaces[cur_ws].windows().len();
                let pos = workspaces[cur_ws].windows().iter().position(|&w| w == focused);
                if let Some(pos) = pos {
                    let at_edge = pos + 1 >= len;
                    let right_mon = *focused_mon + 1;
                    if at_edge && right_mon < n_mons {
                        // Cross to right monitor's active workspace.
                        let target_ws = monitor_ws[right_mon];
                        workspaces[cur_ws].remove_window(focused);
                        // Append to end so window sits at the right edge of the new
                        // workspace — next MoveNext crosses immediately (one press
                        // per monitor boundary).
                        workspaces[target_ws].windows_mut().push(focused);
                        workspaces[target_ws].focused = Some(focused);
                        *focused_mon = right_mon;
                        tracing::info!(%focused, to_ws = %workspaces[target_ws].name, "MoveNext: crossed to right monitor");
                        let src_rect = monitor_rects.get(*focused_mon - 1).copied().unwrap_or(cur_mon_rect);
                        apply_layout(backend, &workspaces[cur_ws], src_rect, config);
                        update_borders(backend, &workspaces[cur_ws], config);
                        let tgt_rect = monitor_rects.get(right_mon).copied().unwrap_or(cur_mon_rect);
                        apply_layout(backend, &workspaces[target_ws], tgt_rect, config);
                        update_borders(backend, &workspaces[target_ws], config);
                        if let Some(w) = backend.window_mut(focused) { let _ = w.focus(); }
                    } else {
                        let next = (pos + 1) % len;
                        workspaces[cur_ws].windows_mut().swap(pos, next);
                        apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
                        update_borders(backend, &workspaces[cur_ws], config);
                    }
                }
            }
        }

        Action::MovePrev => {
            if let Some(focused) = workspaces[cur_ws].focused {
                let len = workspaces[cur_ws].windows().len();
                let pos = workspaces[cur_ws].windows().iter().position(|&w| w == focused);
                if let Some(pos) = pos {
                    let at_edge = pos == 0;
                    let left_mon = focused_mon.checked_sub(1);
                    if at_edge && left_mon.is_some() {
                        let left_mon = left_mon.unwrap();
                        let target_ws = monitor_ws[left_mon];
                        workspaces[cur_ws].remove_window(focused);
                        // Insert at front so window sits at the left edge of the new
                        // workspace — next MovePrev crosses immediately (one press
                        // per monitor boundary).
                        workspaces[target_ws].windows_mut().insert(0, focused);
                        workspaces[target_ws].focused = Some(focused);
                        *focused_mon = left_mon;
                        tracing::info!(%focused, to_ws = %workspaces[target_ws].name, "MovePrev: crossed to left monitor");
                        let src_rect = monitor_rects.get(left_mon + 1).copied().unwrap_or(cur_mon_rect);
                        apply_layout(backend, &workspaces[cur_ws], src_rect, config);
                        update_borders(backend, &workspaces[cur_ws], config);
                        let tgt_rect = monitor_rects.get(left_mon).copied().unwrap_or(cur_mon_rect);
                        apply_layout(backend, &workspaces[target_ws], tgt_rect, config);
                        update_borders(backend, &workspaces[target_ws], config);
                        if let Some(w) = backend.window_mut(focused) { let _ = w.focus(); }
                    } else {
                        let prev = if pos == 0 { len - 1 } else { pos - 1 };
                        workspaces[cur_ws].windows_mut().swap(pos, prev);
                        apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
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
            }
        }

        Action::ToggleFullscreen => {
            toggle_fullscreen_for(
                workspaces[cur_ws].focused,
                backend,
                &mut workspaces[cur_ws],
                cur_mon_rect,
                config,
            );
        }

        // ── Layout ───────────────────────────────────────────────────────────
        Action::SetLayout(kind) => {
            tracing::info!(?kind, workspace = %workspaces[cur_ws].name, "set layout");
            workspaces[cur_ws].layout = kind;
            apply_layout(backend, &workspaces[cur_ws], cur_mon_rect, config);
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
            tracing::info!(
                ws = %workspaces[current].name,
                count = outgoing.len(),
                "hiding outgoing workspace windows"
            );
            for id in outgoing {
                if let Some(w) = backend.window_mut(id) {
                    let res = w.hide();
                    tracing::debug!(%id, ?res, "hide");
                }
            }

            monitor_ws[mon] = idx;
            *focused_mon = mon;

            // Show and re-tile the incoming workspace.
            let incoming: Vec<WindowId> = workspaces[idx].windows().to_vec();
            tracing::info!(
                ws = %workspaces[idx].name,
                count = incoming.len(),
                "showing incoming workspace windows"
            );
            for id in incoming {
                if let Some(w) = backend.window_mut(id) {
                    let res = w.show();
                    tracing::debug!(%id, ?res, "show");
                }
            }
            let mr = monitor_rects.get(mon).copied().unwrap_or(cur_mon_rect);
            apply_layout(backend, &workspaces[idx], mr, config);
            update_borders(backend, &workspaces[idx], config);

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
                    // Target workspace is hidden — hide the window before moving.
                    if let Some(w) = backend.window_mut(id) {
                        let _ = w.hide();
                        let _ = w.hide_border_overlay();
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
            tracing::info!("quit action");
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
    config: &Config,
) {
    let id = match target {
        Some(id) => id,
        None => return,
    };

    if workspace.is_fullscreen(id) {
        workspace.exit_fullscreen(id);
        tracing::info!(%id, "exit fullscreen");
        apply_layout(backend, workspace, monitor, config);
    } else {
        let saved = backend
            .window_mut(id)
            .map(|w| w.geometry())
            .unwrap_or(monitor);
        workspace.enter_fullscreen(id, saved);
        tracing::info!(%id, "enter fullscreen");

        let area = apply_padding(monitor, config.padding);
        if let Some(w) = backend.window_mut(id) {
            if let Err(e) = w.set_geometry(area) {
                tracing::warn!(%id, err = %e, "set_geometry (enter fullscreen) failed");
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

fn center_of(r: Rect) -> (i32, i32) {
    (r.x + r.width as i32 / 2, r.y + r.height as i32 / 2)
}

fn dist_sq(a: (i32, i32), b: (i32, i32)) -> i64 {
    let dx = (a.0 - b.0) as i64;
    let dy = (a.1 - b.1) as i64;
    dx * dx + dy * dy
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    fmt()
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
