//! [`WaylandCompositor`] — Smithay-based implementation of
//! [`shellwright_core::backend::Backend`].
//!
//! # Architecture
//! Unlike the Windows and macOS backends this crate *is* the Wayland compositor.
//! It owns the Wayland socket, the render pipeline, and all input handling.
//!
//! # Roadmap
//! 1. Bare compositor — open DRM device, create Wayland socket, accept one client
//! 2. xdg-shell — window creation / configure / destroy lifecycle
//! 3. wlr-layer-shell — reserve space for external bars (waybar, eww, etc.)
//! 4. Input — libinput via udev; xkbcommon keymap; keybinding dispatch
//! 5. Multi-output (CRTC enumeration, per-output workspaces)
//! 6. XWayland bridge

use shellwright_core::{
    backend::Backend,
    error::Error,
    event::Event,
    hotkey::BindingMap,
    window::{Rect, Window, WindowId},
    Result,
};

// ── Window ────────────────────────────────────────────────────────────────────

pub struct WaylandWindow {
    id: WindowId,
    title: String,
    geometry: Rect,
    floating: bool,
}

impl Window for WaylandWindow {
    fn id(&self) -> WindowId { self.id }
    fn title(&self) -> &str { &self.title }
    fn geometry(&self) -> Rect { self.geometry }

    fn set_geometry(&mut self, rect: Rect) -> Result<()> {
        // TODO: Send xdg_toplevel.configure(width, height) and commit the
        //       pending surface state so the client redraws at the new size.
        //       Also update the compositor-side surface geometry for damage tracking.
        tracing::debug!(id = %self.id, ?rect, "set_geometry");
        self.geometry = rect;
        Ok(())
    }

    fn focus(&mut self) -> Result<()> {
        // TODO: wl_keyboard.enter(serial, surface, []) on the seat's keyboard
        //       so the client receives keyboard events.
        //       Also raise the surface in z-order if using a software stacking model.
        tracing::debug!(id = %self.id, "focus");
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        // TODO: xdg_toplevel.close() request — asks the client to close itself
        //       gracefully.  The compositor should not destroy the surface until
        //       the client ACKs with xdg_surface.destroy.
        tracing::debug!(id = %self.id, "close");
        Ok(())
    }

    fn is_floating(&self) -> bool { self.floating }
    fn set_floating(&mut self, floating: bool) { self.floating = floating; }

    fn hide(&mut self) -> Result<()> {
        // As the compositor we control rendering: simply stop including this
        // surface in damage/repaint calculations and don't forward input to it.
        // TODO: Mark surface as invisible in compositor state; skip in repaint loop.
        // TODO: Send wl_surface.leave(output) so the client knows it's off-screen.
        tracing::debug!(id = %self.id, "hide");
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        // TODO: Mark surface as visible in compositor state; include in repaint loop.
        // TODO: Send wl_surface.enter(output) so the client knows it's on-screen.
        tracing::debug!(id = %self.id, "show");
        Ok(())
    }
}

// ── Compositor ────────────────────────────────────────────────────────────────

pub struct WaylandCompositor {
    windows: Vec<WaylandWindow>,
    bindings: BindingMap,
    // TODO: calloop::EventLoop<'static, CompositorState>
    // TODO: smithay::wayland::display::Display<CompositorState>
    // TODO: DRM/KMS device handle (smithay::backend::drm::DrmDevice)
    // TODO: output geometry (from CRTC / mode query)
}

impl WaylandCompositor {
    pub fn new(bindings: BindingMap) -> Result<Self> {
        // TODO: open /dev/dri/card* — DrmDevice::new with DrmAccessMode::Master
        // TODO: query CRTC connectors and active mode for initial output geometry
        // TODO: Display::new() → create Wayland socket at $XDG_RUNTIME_DIR/wayland-N
        // TODO: register xdg-wm-base (xdg-shell) handler
        // TODO: register zwlr-layer-shell-v1 handler (for waybar/eww)
        // TODO: register wl-seat; create xkbcommon context + keymap;
        //       pre-fetch modifier indices (Alt, Ctrl, Shift, Super/Logo)
        tracing::info!("initialising Wayland compositor");
        Ok(Self { windows: Vec::new(), bindings })
    }
}

impl Backend for WaylandCompositor {
    type W = WaylandWindow;

    fn windows(&self) -> Vec<&WaylandWindow> {
        self.windows.iter().collect()
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut WaylandWindow> {
        self.windows.iter_mut().find(|w| w.id() == id)
    }

    fn next_event(&mut self) -> Result<Event> {
        // TODO: event_loop.dispatch(timeout, &mut state)
        //
        // In the smithay InputHandler (keyboard key_action):
        //   let keysym   = keyboard.get_one_sym_raw(keycode);
        //   let key_name = input::keysym_to_key_name(keysym);
        //   let mods     = input::xkb_mods_to_modifiers(active, idx_alt, idx_ctrl, idx_shift, idx_super);
        //   if let Some(ev) = input::check_binding(&self.bindings, mods, key_name) {
        //       return ev; // consumed — do NOT forward key to focused client
        //   }
        //
        // In the xdg-shell new_toplevel handler:
        //   return Event::WindowCreated(assign_next_id(surface));
        //
        // In the xdg-shell destroy handler:
        //   return Event::WindowDestroyed(id);
        Err(Error::Backend("Wayland event loop not yet implemented".into()))
    }

    fn flush(&mut self) -> Result<()> {
        // TODO: display.flush_clients(&mut state)
        Ok(())
    }

    fn monitor_rect(&self) -> Rect {
        // TODO: query active CRTC mode → (width, height)
        //       subtract wlr-layer-shell reserved margins (top/bottom/left/right)
        //       to get the true tiling area.
        Rect::new(0, 0, 1920, 1080)
    }
}
