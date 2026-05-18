use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, RenderedTurn};

pub fn draw(f: &mut Frame, app: &App) {
    let input_height = input_height(&app.input).clamp(1, 8);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),                     // header
            Constraint::Min(1),                        // conversation
            Constraint::Length(1),                     // status
            Constraint::Length(input_height + 2),      // input (+2 for borders)
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_conversation(f, chunks[1], app);
    draw_status(f, chunks[2], app);
    draw_input(f, chunks[3], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![Span::styled(
        format!(
            "merlion · {} · session {} · {} skills · {} memories",
            app.model, app.session_id_short, app.skill_count, app.memory_count
        ),
        Style::default().add_modifier(Modifier::DIM),
    )]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let style = if app.status.starts_with("running tool")
        || app.status.starts_with("thinking")
    {
        Style::default().fg(Color::Yellow)
    } else if app.status.contains("exhausted") || app.status.contains("error") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut spans = vec![Span::styled(format!("· {}", app.status), style)];
    let usage = &app.usage;
    if usage.prompt_tokens.is_some()
        || usage.completion_tokens.is_some()
        || usage.total_tokens.is_some()
    {
        let p = usage.prompt_tokens.unwrap_or(0);
        let c = usage.completion_tokens.unwrap_or(0);
        let t = usage.total_tokens.unwrap_or(p + c);
        spans.push(Span::styled(format!("   · tokens: {p} in / {c} out / {t} total"), dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_conversation(f: &mut Frame, area: Rect, app: &App) {
    let lines = render_turns(&app.messages);
    let total = lines.len() as u16;
    // Convert lines to Text for the Paragraph widget.
    let text = Text::from(lines);
    // Inner viewport height (no borders).
    let visible = area.height;
    // Scroll value: distance from top of `text` to top of viewport.
    // When pinned to bottom: scroll_y = total - visible.
    let max_scroll_top = total.saturating_sub(visible);
    let scroll_top = max_scroll_top.saturating_sub(app.scroll);

    let para = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll_top, 0));
    f.render_widget(para, area);
}

fn render_turns(turns: &[RenderedTurn]) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for turn in turns {
        match turn {
            RenderedTurn::UserText(text) => {
                let mut iter = text.lines();
                if let Some(first) = iter.next() {
                    out.push(Line::from(vec![
                        Span::styled(
                            "you › ",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(first.to_string()),
                    ]));
                }
                for rest in iter {
                    out.push(Line::from(Span::raw(format!("      {rest}"))));
                }
                out.push(Line::from(""));
            }
            RenderedTurn::AssistantText(text) => {
                if text.is_empty() {
                    continue;
                }
                let mut iter = text.lines();
                if let Some(first) = iter.next() {
                    out.push(Line::from(vec![
                        Span::styled(
                            "merlion › ",
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(first.to_string()),
                    ]));
                }
                for rest in iter {
                    out.push(Line::from(Span::raw(format!("          {rest}"))));
                }
                out.push(Line::from(""));
            }
            RenderedTurn::ToolCall {
                name,
                args,
                content,
                is_error,
                finished,
            } => {
                let args_preview = preview_args(args);
                out.push(Line::from(Span::styled(
                    format!("· tool {name} {args_preview}"),
                    Style::default().add_modifier(Modifier::DIM),
                )));
                if *finished {
                    let head: String =
                        content.lines().next().unwrap_or("").chars().take(120).collect();
                    let tag = if *is_error { "ERR" } else { "ok" };
                    let style = if *is_error {
                        Style::default().fg(Color::Red).add_modifier(Modifier::DIM)
                    } else {
                        Style::default().add_modifier(Modifier::DIM)
                    };
                    out.push(Line::from(Span::styled(
                        format!("  ↪ {tag}: {head}"),
                        style,
                    )));
                } else {
                    out.push(Line::from(Span::styled(
                        "  ↪ …",
                        Style::default().add_modifier(Modifier::DIM),
                    )));
                }
            }
            RenderedTurn::Info(text) => {
                for l in text.lines() {
                    out.push(Line::from(Span::styled(
                        l.to_string(),
                        Style::default().add_modifier(Modifier::DIM),
                    )));
                }
            }
        }
    }
    out
}

fn preview_args(v: &serde_json::Value) -> String {
    let s = v.to_string();
    let max = 80;
    if s.chars().count() <= max {
        s
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

fn input_height(input: &str) -> u16 {
    let lines = input.split('\n').count() as u16;
    lines.max(1)
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().add_modifier(Modifier::DIM))
        .title(Span::styled(
            " input ",
            Style::default().add_modifier(Modifier::DIM),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = if app.input.is_empty() {
        Text::from(Line::from(Span::styled(
            "type a message — Enter sends, Ctrl+J for newline",
            Style::default().add_modifier(Modifier::DIM),
        )))
    } else {
        let mut lines: Vec<Line> = Vec::new();
        for l in app.input.split('\n') {
            lines.push(Line::from(l.to_string()));
        }
        Text::from(lines)
    };
    let para = Paragraph::new(text).wrap(Wrap { trim: false });
    f.render_widget(para, inner);

    // Place a visible cursor at the current byte position. We approximate
    // grapheme width by counting chars on the current line.
    let (row, col) = cursor_rowcol(&app.input, app.cursor);
    if inner.width > 0 && inner.height > 0 {
        let x = inner.x + col.min(inner.width.saturating_sub(1) as usize) as u16;
        let y = inner.y + row.min(inner.height.saturating_sub(1) as usize) as u16;
        f.set_cursor_position((x, y));
    }
}

fn cursor_rowcol(s: &str, byte_pos: usize) -> (usize, usize) {
    let before = &s[..byte_pos.min(s.len())];
    let mut row = 0usize;
    let mut col = 0usize;
    for c in before.chars() {
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (row, col)
}
