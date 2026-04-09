//! `wm-core` — platform-agnostic window manager abstractions.
//!
//! # Design rationale
//! ISO 25010 §4.2 (Portability / Adaptability) requires that platform-specific
//! concerns are isolated behind stable interfaces.  This crate defines those
//! interfaces; each platform crate (`wm-windows`, `wm-macos`, `wm-wayland`)
//! provides a concrete implementation without touching this crate.
//!
//! # Crate map
//! - [`backend`]   — [`Backend`] trait every platform must implement
//! - [`window`]    — [`Window`] trait + [`Rect`] / [`WindowId`] primitives
//! - [`layout`]    — tiling algorithms (BSP, columns, monocle, float)
//! - [`workspace`] — named groups of windows with an assigned layout
//! - [`event`]     — platform-normalised event enum
//! - [`config`]    — TOML-based user configuration (serde)
//! - [`error`]     — unified [`Error`] type

pub mod action;
pub mod backend;
pub mod config;
pub mod error;
pub mod event;
pub mod hotkey;
pub mod layout;
pub mod window;
pub mod workspace;

pub use error::{Error, Result};
