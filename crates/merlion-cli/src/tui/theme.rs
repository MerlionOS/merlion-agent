//! TUI color theme.
//!
//! Driven by the `MERLION_THEME` env var: `dark` (default) or `light`.
//! Could later be extended to full per-element customization via
//! `~/.merlion/theme.yaml`; for now the env var is the only knob.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn from_env() -> Self {
        Self::parse(std::env::var("MERLION_THEME").ok().as_deref())
    }

    /// Pure parser — split out from `from_env` so tests can exercise the
    /// matching logic without touching process-global env vars. Earlier
    /// tests used `set_var` + `remove_var` and raced under `cargo test`'s
    /// default parallel threading on macOS.
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("light") => ThemeKind::Light,
            _ => ThemeKind::Dark,
        }
    }
}

pub struct Theme {
    pub user_text: Style,
    pub assistant_text: Style,
    pub tool_call: Style,
    pub tool_ok: Style,
    pub tool_err: Style,
    pub info: Style,
    pub status_idle: Style,
    pub status_busy: Style,
    pub header: Style,
    pub input_prompt: Style,
}

impl Theme {
    pub fn load() -> Self {
        match ThemeKind::from_env() {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    fn dark() -> Self {
        Self {
            user_text: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            assistant_text: Style::default().fg(Color::White),
            tool_call: Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
            tool_ok: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
            tool_err: Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
            info: Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
            status_idle: Style::default().fg(Color::DarkGray),
            status_busy: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            header: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            input_prompt: Style::default().fg(Color::Cyan),
        }
    }

    fn light() -> Self {
        Self {
            user_text: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            assistant_text: Style::default().fg(Color::Black),
            tool_call: Style::default().fg(Color::Gray),
            tool_ok: Style::default().fg(Color::Green),
            tool_err: Style::default().fg(Color::Red),
            info: Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::ITALIC),
            status_idle: Style::default().fg(Color::Gray),
            status_busy: Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
            header: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            input_prompt: Style::default().fg(Color::Blue),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::load()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_default_when_value_unset() {
        assert_eq!(ThemeKind::parse(None), ThemeKind::Dark);
    }

    #[test]
    fn dark_is_default_for_unknown_value() {
        assert_eq!(ThemeKind::parse(Some("solarized")), ThemeKind::Dark);
        assert_eq!(ThemeKind::parse(Some("")), ThemeKind::Dark);
    }

    #[test]
    fn light_kicks_in_when_value_is_light() {
        assert_eq!(ThemeKind::parse(Some("light")), ThemeKind::Light);
    }
}
