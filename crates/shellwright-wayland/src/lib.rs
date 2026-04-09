//! Wayland compositor backend built on [Smithay](https://github.com/Smithay/smithay).
//!
//! # Architecture difference
//! Unlike the Win32 and macOS backends — which act as *clients* on top of an
//! existing compositor — this crate *is* the compositor.  It owns:
//!
//! - The Wayland socket clients connect to.
//! - The render loop (DRM/KMS or wlroots-compatible backends).
//! - Input handling (libinput via udev).
//! - Protocol implementations: `xdg-shell`, `wlr-layer-shell`, `xdg-output`.
//!
//! The event loop is driven by [`calloop`]; Smithay wires its handlers into it.
//!
//! # Roadmap
//! 1. Bare compositor that can display a single client (xterm / foot).
//! 2. `xdg-shell` popup / decoration support.
//! 3. Multi-output (monitor) handling.
//! 4. XWayland for legacy X11 application support.

mod compositor;
pub mod input;

pub use compositor::WaylandCompositor;
