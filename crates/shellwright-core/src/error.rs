//! Unified error type for `wm-core`.
//!
//! All library code returns [`Result<T>`]; panics are reserved for
//! unrecoverable programmer errors (e.g. violated invariants in `debug_assert`).
//! This aligns with IEEE 1012 (V&V) and ISO 25010 reliability goals.

/// Every failure mode the window manager can encounter.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("window not found: {id}")]
    WindowNotFound { id: u64 },

    #[error("layout error: {0}")]
    Layout(String),

    /// Config parse / IO failures.  CWE-20: input from untrusted files is
    /// validated at this boundary before entering the rest of the system.
    #[error("config error: {0}")]
    Config(String),

    #[error("backend error: {0}")]
    Backend(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
