//! Ratatui-based interactive terminal UI for `merlion chat`.
//!
//! Layout (top to bottom): header bar, scrollable conversation pane,
//! status line, multiline input bar. The agent loop streams `AgentEvent`s
//! over an mpsc channel; the run loop multiplexes those with crossterm
//! key events via `tokio::select!`.

mod app;
mod input;
mod render;
mod theme;

pub use app::run;
#[allow(unused_imports)]
pub use theme::Theme;
