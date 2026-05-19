use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::future::FutureExt;
use futures::StreamExt;
use merlion_config::Config;
use merlion_core::{Agent, AgentEvent, Curator, Message, Usage};
use merlion_memory::MemoryStore;
use merlion_session::SessionDB;
use merlion_skills::SkillSet;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use super::render;
use super::theme::Theme;

/// One renderable item in the conversation pane.
pub enum RenderedTurn {
    UserText(String),
    /// Accumulated streaming text from the assistant. Mutated by AssistantDelta.
    AssistantText(String),
    ToolCall {
        name: String,
        args: serde_json::Value,
        content: String,
        is_error: bool,
        finished: bool,
    },
    Info(String),
}

pub struct App {
    pub messages: Vec<RenderedTurn>,
    pub status: String,
    pub input: String,
    /// Caret position within `input`, byte index.
    pub cursor: usize,
    pub input_history: VecDeque<String>,
    pub history_pos: Option<usize>,
    /// Stash of in-progress input when the user pages into history.
    pub history_stash: Option<String>,
    pub agent_running: bool,
    pub interrupt_count: u8,
    /// Number of lines scrolled *up* from the bottom. 0 = pinned to bottom.
    pub scroll: u16,
    pub user_scrolled: bool,

    pub model: String,
    pub session_id_short: String,
    pub skill_count: usize,
    pub memory_count: usize,

    /// Cumulative token usage across the session (sum of provider-reported
    /// per-turn usages). Rendered in the status line.
    pub usage: Usage,

    pub theme: Theme,

    pub should_quit: bool,
}

impl App {
    pub fn new(
        model: String,
        session_id_short: String,
        skill_count: usize,
        memory_count: usize,
    ) -> Self {
        Self {
            messages: Vec::new(),
            status: "idle".into(),
            input: String::new(),
            cursor: 0,
            input_history: VecDeque::with_capacity(128),
            history_pos: None,
            history_stash: None,
            agent_running: false,
            interrupt_count: 0,
            scroll: 0,
            user_scrolled: false,
            model,
            session_id_short,
            skill_count,
            memory_count,
            usage: Usage::default(),
            theme: Theme::load(),
            should_quit: false,
        }
    }

    fn push_history(&mut self, line: String) {
        if self
            .input_history
            .back()
            .map(|s| s == &line)
            .unwrap_or(false)
        {
            return;
        }
        if self.input_history.len() >= 128 {
            self.input_history.pop_front();
        }
        self.input_history.push_back(line);
    }

    fn navigate_history(&mut self, dir: i32) {
        if self.input_history.is_empty() {
            return;
        }
        let len = self.input_history.len();
        let new_pos = match (self.history_pos, dir) {
            (None, -1) => {
                self.history_stash = Some(self.input.clone());
                Some(len - 1)
            }
            (None, 1) => None,
            (Some(i), -1) => Some(i.saturating_sub(1)),
            (Some(i), 1) => {
                if i + 1 >= len {
                    None
                } else {
                    Some(i + 1)
                }
            }
            _ => self.history_pos,
        };
        self.history_pos = new_pos;
        self.input = match new_pos {
            Some(i) => self.input_history[i].clone(),
            None => self.history_stash.take().unwrap_or_default(),
        };
        self.cursor = self.input.len();
    }

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.history_pos = None;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut new_cursor = self.cursor - 1;
        while !self.input.is_char_boundary(new_cursor) {
            new_cursor -= 1;
        }
        self.input.replace_range(new_cursor..self.cursor, "");
        self.cursor = new_cursor;
        self.history_pos = None;
    }

    fn pin_to_bottom(&mut self) {
        if !self.user_scrolled {
            self.scroll = 0;
        }
    }
}

/// What the keyboard handler decides to do next.
enum KeyOutcome {
    None,
    Quit,
    Submit(String),
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    _cfg: &Config,
    agent: &Agent,
    messages: &mut Vec<Message>,
    session_id: &str,
    skills: &SkillSet,
    memory: &Arc<MemoryStore>,
    db: &SessionDB,
    curator: &mut Curator,
) -> Result<()> {
    // The console approver fights for stdin with the TUI's raw-mode capture.
    // For MVP we bypass it. TODO: in-TUI approval prompt.
    std::env::set_var("MERLION_AUTO_APPROVE", "1");

    let mut terminal = setup_terminal()?;
    let result = run_loop(
        &mut terminal,
        agent,
        messages,
        session_id,
        skills,
        memory,
        db,
        curator,
    )
    .await;
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: &Agent,
    messages: &mut Vec<Message>,
    session_id: &str,
    skills: &SkillSet,
    memory: &Arc<MemoryStore>,
    db: &SessionDB,
    curator: &mut Curator,
) -> Result<()> {
    let memory_count = memory.list().map(|v| v.len()).unwrap_or(0);
    let mut app = App::new(
        agent.options().model.clone(),
        session_id.chars().take(8).collect(),
        skills.len(),
        memory_count,
    );
    app.messages.push(RenderedTurn::Info(format!(
        "merlion — model {} · session {} · {} skills · {} memories",
        agent.options().model,
        &app.session_id_short,
        app.skill_count,
        app.memory_count,
    )));
    app.messages.push(RenderedTurn::Info(
        "Enter sends · Ctrl+J newline · Up/Down history · PgUp/PgDn scroll · Ctrl+C interrupt / exit"
            .into(),
    ));

    let mut events = EventStream::new();

    loop {
        terminal.draw(|f| render::draw(f, &app))?;
        if app.should_quit {
            break;
        }

        // Phase 1: idle — wait for a keystroke.
        let key_event = events.next().await;
        let Some(ev) = key_event else { break };
        let ev = ev?;
        let outcome = match ev {
            Event::Key(key) => handle_key_idle(&mut app, key, skills),
            _ => KeyOutcome::None,
        };
        match outcome {
            KeyOutcome::None => continue,
            KeyOutcome::Quit => break,
            KeyOutcome::Submit(text) => {
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                let user_text =
                    match resolve_input(&mut app, &trimmed, agent, skills, messages, session_id) {
                        SubmitResolution::Local => {
                            terminal.draw(|f| render::draw(f, &app))?;
                            continue;
                        }
                        SubmitResolution::Quit => break,
                        SubmitResolution::Send(t) => t,
                    };

                curator.record_user_turn();
                let user_text = if let Some(nudge) = curator.nudge_if_due() {
                    format!("<system-reminder>{nudge}</system-reminder>\n\n{user_text}")
                } else {
                    user_text
                };

                let user_msg = Message::user(user_text.clone());
                db.append_message(session_id, &user_msg)?;
                messages.push(user_msg);
                app.messages.push(RenderedTurn::UserText(user_text));
                app.messages
                    .push(RenderedTurn::AssistantText(String::new()));
                app.status = "thinking…".into();
                app.agent_running = true;
                app.pin_to_bottom();

                // Phase 2: drive the agent + event stream concurrently.
                run_agent(
                    terminal,
                    &mut app,
                    &mut events,
                    agent,
                    messages,
                    session_id,
                    db,
                )
                .await?;
                if app.should_quit {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Wait for input while no agent is running. Mutates `app` for editing keys
/// and returns a `KeyOutcome` for actions the run loop needs to react to.
fn handle_key_idle(app: &mut App, key: KeyEvent, skills: &SkillSet) -> KeyOutcome {
    if key.kind == KeyEventKind::Release {
        return KeyOutcome::None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && key.code == KeyCode::Char('c') {
        if app.input.is_empty() {
            app.interrupt_count += 1;
            if app.interrupt_count >= 2 {
                return KeyOutcome::Quit;
            }
            app.status = "press Ctrl+C again to exit".into();
            return KeyOutcome::None;
        }
        app.input.clear();
        app.cursor = 0;
        app.interrupt_count = 0;
        return KeyOutcome::None;
    }
    app.interrupt_count = 0;

    if ctrl && key.code == KeyCode::Char('d') && app.input.is_empty() {
        return KeyOutcome::Quit;
    }

    match key.code {
        KeyCode::Enter => {
            if ctrl {
                app.insert_char('\n');
                KeyOutcome::None
            } else {
                let text = std::mem::take(&mut app.input);
                app.cursor = 0;
                app.push_history(text.clone());
                app.history_pos = None;
                KeyOutcome::Submit(text)
            }
        }
        KeyCode::Char('j') if ctrl => {
            app.insert_char('\n');
            KeyOutcome::None
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
            KeyOutcome::None
        }
        KeyCode::Backspace => {
            app.backspace();
            KeyOutcome::None
        }
        KeyCode::Left => {
            if app.cursor > 0 {
                let mut new = app.cursor - 1;
                while !app.input.is_char_boundary(new) {
                    new -= 1;
                }
                app.cursor = new;
            }
            KeyOutcome::None
        }
        KeyCode::Right => {
            if app.cursor < app.input.len() {
                let mut new = app.cursor + 1;
                while new < app.input.len() && !app.input.is_char_boundary(new) {
                    new += 1;
                }
                app.cursor = new;
            }
            KeyOutcome::None
        }
        KeyCode::Home => {
            app.cursor = 0;
            KeyOutcome::None
        }
        KeyCode::End => {
            app.cursor = app.input.len();
            KeyOutcome::None
        }
        KeyCode::Up => {
            app.navigate_history(-1);
            KeyOutcome::None
        }
        KeyCode::Down => {
            app.navigate_history(1);
            KeyOutcome::None
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_add(10);
            app.user_scrolled = true;
            KeyOutcome::None
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(10);
            if app.scroll == 0 {
                app.user_scrolled = false;
            }
            KeyOutcome::None
        }
        KeyCode::Tab => {
            // Skill autocomplete: if the input is `/<prefix>` (no spaces yet),
            // expand to the longest unique skill whose name starts with prefix.
            if let Some(rest) = app.input.strip_prefix('/') {
                if !rest.contains(char::is_whitespace) {
                    let matches: Vec<&str> = skills
                        .names()
                        .into_iter()
                        .filter(|n| n.starts_with(rest))
                        .collect();
                    match matches.as_slice() {
                        [single] => {
                            app.input = format!("/{single} ");
                            app.cursor = app.input.chars().count();
                        }
                        [] => {}
                        many => {
                            // Multiple matches → complete to the common prefix
                            // and stash the candidate list into the status line
                            // so the user can see what's available.
                            let lcp = longest_common_prefix(many);
                            if lcp.len() > rest.len() {
                                app.input = format!("/{lcp}");
                                app.cursor = app.input.chars().count();
                            }
                            app.status = format!(
                                "skills: {}",
                                many.iter().copied().collect::<Vec<_>>().join(", ")
                            );
                        }
                    }
                }
            }
            KeyOutcome::None
        }
        KeyCode::Esc => {
            if app.user_scrolled {
                app.scroll = 0;
                app.user_scrolled = false;
            }
            KeyOutcome::None
        }
        _ => KeyOutcome::None,
    }
}

enum SubmitResolution {
    Local,
    Quit,
    Send(String),
}

fn resolve_input(
    app: &mut App,
    trimmed: &str,
    agent: &Agent,
    skills: &SkillSet,
    messages: &[Message],
    session_id: &str,
) -> SubmitResolution {
    match trimmed {
        "/exit" | "/quit" => return SubmitResolution::Quit,
        "/help" => {
            app.messages.push(RenderedTurn::Info(
                "/exit — leave · /new — fresh session · /share — mint join key · \
                 /usage — message count · /model — active model · \
                 /skills — list skills · /<skill-name> — invoke skill"
                    .into(),
            ));
            app.pin_to_bottom();
            return SubmitResolution::Local;
        }
        "/usage" => {
            app.messages
                .push(RenderedTurn::Info(format!("messages: {}", messages.len())));
            app.pin_to_bottom();
            return SubmitResolution::Local;
        }
        "/model" => {
            app.messages
                .push(RenderedTurn::Info(agent.options().model.clone()));
            app.pin_to_bottom();
            return SubmitResolution::Local;
        }
        "/skills" => {
            app.messages.push(RenderedTurn::Info(skills.help_index()));
            app.pin_to_bottom();
            return SubmitResolution::Local;
        }
        "/share" => {
            let path = merlion_gateway::JoinKeyStore::default_path();
            let mut store = merlion_gateway::JoinKeyStore::load(&path).unwrap_or_default();
            store.gc();
            let key = store.mint(session_id.to_string(), 600);
            let save_note = match store.save(&path) {
                Ok(()) => String::new(),
                Err(e) => format!(" (warning: failed to persist: {e})"),
            };
            app.messages.push(RenderedTurn::Info(format!(
                "Join key: {key} — send `/join {key}` from Telegram/Discord/Slack within \
                 10 minutes to continue this conversation on that platform.{save_note}"
            )));
            app.pin_to_bottom();
            return SubmitResolution::Local;
        }
        _ => {}
    }

    if let Some(rest) = trimmed.strip_prefix('/') {
        let (name, extra) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        match skills.get(name) {
            Some(skill) => {
                app.messages.push(RenderedTurn::Info(format!(
                    "(invoking skill `{}`)",
                    skill.name
                )));
                let extra = extra.trim();
                let body = if extra.is_empty() {
                    skill.body.clone()
                } else {
                    format!("{}\n\n---\nUser-supplied arguments: {extra}", skill.body)
                };
                SubmitResolution::Send(body)
            }
            None => {
                app.messages.push(RenderedTurn::Info(format!(
                    "unknown slash command: /{name} (try /help)"
                )));
                app.pin_to_bottom();
                SubmitResolution::Local
            }
        }
    } else {
        SubmitResolution::Send(trimmed.to_string())
    }
}

async fn run_agent(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    events: &mut EventStream,
    agent: &Agent,
    messages: &mut Vec<Message>,
    session_id: &str,
    db: &SessionDB,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    let mut snapshot = messages.clone();
    let start_len = messages.len();
    let mut run_fut = Box::pin(agent.run(&mut snapshot, tx).fuse());
    let mut run_result: Option<merlion_core::Result<()>> = None;
    let mut interrupted = false;

    loop {
        terminal.draw(|f| render::draw(f, app))?;

        tokio::select! {
            biased;
            res = &mut run_fut, if run_result.is_none() => {
                run_result = Some(res);
                // Drain any remaining events before exiting.
            }
            maybe_ev = rx.recv() => {
                match maybe_ev {
                    Some(ev) => handle_agent_event(app, ev),
                    None => break,
                }
            }
            maybe_in = events.next() => {
                match maybe_in {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Release
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c')
                        {
                            interrupted = true;
                            break;
                        }
                        handle_running_key(app, key);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                }
            }
        }

        if run_result.is_some() && rx.is_empty() {
            break;
        }
    }

    if !interrupted && run_result.is_none() {
        // Make sure the agent future has truly completed before we touch
        // `snapshot` again (the future holds a &mut into it).
        let res = (&mut run_fut).await;
        run_result = Some(res);
    }
    // Drop the future to release its &mut borrow of `snapshot`.
    drop(run_fut);

    while let Ok(ev) = rx.try_recv() {
        handle_agent_event(app, ev);
    }

    for m in snapshot.iter().skip(start_len) {
        db.append_message(session_id, m)?;
    }
    *messages = snapshot;

    if interrupted {
        app.messages
            .push(RenderedTurn::Info("[interrupted]".into()));
    } else if let Some(Err(e)) = run_result {
        app.messages.push(RenderedTurn::Info(format!("error: {e}")));
    }
    app.agent_running = false;
    app.status = "idle".into();
    Ok(())
}

/// Keys handled while the agent is running. Most editing is disabled; we
/// only support scrolling. (Ctrl+C is handled in the run_agent select! loop
/// directly so the surrounding future can be cancelled.)
fn handle_running_key(app: &mut App, key: KeyEvent) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    match key.code {
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_add(10);
            app.user_scrolled = true;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(10);
            if app.scroll == 0 {
                app.user_scrolled = false;
            }
        }
        KeyCode::Esc => {
            if app.user_scrolled {
                app.scroll = 0;
                app.user_scrolled = false;
            }
        }
        _ => {}
    }
}

pub fn handle_agent_event(app: &mut App, ev: AgentEvent) {
    match ev {
        AgentEvent::AssistantDelta(s) => {
            if let Some(RenderedTurn::AssistantText(buf)) = app.messages.last_mut() {
                buf.push_str(&s);
            } else {
                app.messages.push(RenderedTurn::AssistantText(s));
            }
            app.pin_to_bottom();
        }
        AgentEvent::AssistantMessage(_) => {}
        AgentEvent::ToolCallStart {
            name, arguments, ..
        } => {
            app.status = format!("running tool: {name}");
            // Drop trailing empty assistant buffer.
            if let Some(RenderedTurn::AssistantText(buf)) = app.messages.last() {
                if buf.is_empty() {
                    app.messages.pop();
                }
            }
            app.messages.push(RenderedTurn::ToolCall {
                name,
                args: arguments,
                content: String::new(),
                is_error: false,
                finished: false,
            });
            app.pin_to_bottom();
        }
        AgentEvent::ToolCallFinish {
            name,
            content,
            is_error,
            ..
        } => {
            for turn in app.messages.iter_mut().rev() {
                if let RenderedTurn::ToolCall {
                    name: n,
                    finished,
                    content: c,
                    is_error: ie,
                    ..
                } = turn
                {
                    if !*finished && *n == name {
                        *c = content.clone();
                        *ie = is_error;
                        *finished = true;
                        break;
                    }
                }
            }
            app.status = "thinking…".into();
            app.messages
                .push(RenderedTurn::AssistantText(String::new()));
            app.pin_to_bottom();
        }
        AgentEvent::IterationBudgetExhausted => {
            app.status = "iteration budget exhausted".into();
            app.messages
                .push(RenderedTurn::Info("[iteration budget exhausted]".into()));
            app.pin_to_bottom();
        }
        AgentEvent::Done => {
            if let Some(RenderedTurn::AssistantText(buf)) = app.messages.last() {
                if buf.is_empty() {
                    app.messages.pop();
                }
            }
            app.status = "idle".into();
            app.pin_to_bottom();
        }
        AgentEvent::Usage(u) => {
            app.usage.merge(&u);
        }
    }
}

/// Longest common prefix among a slice of strings. Returns "" for empty input.
fn longest_common_prefix(strs: &[&str]) -> String {
    let Some(first) = strs.first() else {
        return String::new();
    };
    let mut prefix: String = first.to_string();
    for s in &strs[1..] {
        while !s.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    prefix
}

#[cfg(test)]
mod tab_tests {
    use super::longest_common_prefix;

    #[test]
    fn lcp_single_returns_self() {
        assert_eq!(longest_common_prefix(&["review"]), "review");
    }

    #[test]
    fn lcp_two_branches() {
        assert_eq!(longest_common_prefix(&["review", "rebase"]), "re");
    }

    #[test]
    fn lcp_no_overlap_returns_empty() {
        assert_eq!(longest_common_prefix(&["foo", "bar"]), "");
    }
}
