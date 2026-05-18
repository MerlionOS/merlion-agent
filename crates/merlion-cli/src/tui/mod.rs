//! Ratatui-based interactive terminal UI for `merlion chat`.
//!
//! Layout (top to bottom): header bar, scrollable conversation pane,
//! status line, multiline input bar. The agent loop streams `AgentEvent`s
//! over an mpsc channel; the run loop multiplexes those with crossterm
//! key events via `tokio::select!`.

mod app;
mod input;
mod render;
#[allow(dead_code)] // Wired up by render in a follow-up; surfaced here so
                    // settings work can land independently.
mod theme;

pub use app::run;
