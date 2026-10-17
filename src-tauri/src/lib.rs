//! LJ Hub library crate.
//!
//! Module map:
//! - `model`    persisted / IPC data types
//! - `store`    local JSON persistence (atomic writes)
//! - `agents`   agent registry, executable discovery, version probe
//! - `acp`      live ACP session runtime (official Rust SDK)
//! - `events`   event pipeline between ACP callbacks and the UI
//! - `git`      Git inspection through the Git CLI (read-only)
//! - `handoff`  PREPARED / VERIFIED / ACCEPTED task handoff
//! - `artifacts` important-artifact registry (path, size, SHA-256)
//! - `commands` Tauri command surface
//! - `app`      application state wiring

pub mod acp;
pub mod agents;
pub mod app;
pub mod artifacts;
pub mod commands;
pub mod events;
pub mod git;
pub mod handoff;
pub mod model;
pub mod run_driver;
pub mod runs;
pub mod scheduler;
pub mod sessions;
pub mod store;
pub mod worktree;

/// Fixtures shared by unit and integration tests. Public so the integration
/// tests in `tests/` can build real (disposable) Git repositories.
pub mod test_support;
pub mod workbuddy;

pub use app::AppState;
pub use commands::*;

pub fn run() {
    app::run();
}
