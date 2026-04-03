use crate::config::{self, MatchMode, Policy};
use crate::db::{Db, ToolCall};
use crate::format;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io::{self, stdout};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal);

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    result
}

struct App {
    db_path: std::path::PathBuf,
    pending: Vec<ToolCall>,
    session_names: std::collections::HashMap<String, String>,
    list_state: ListState,
    mode: Mode,
    deny_reason: String,
    policy_suggestion: PolicySuggestion,
    message: Option<(String, Instant)>,
}

#[derive(Default)]
enum Mode {
    #[default]
    Normal,
    DenyPrompt,
    PolicyConfirm,
}

struct PolicySuggestion {
    tool: String,
    action: String,
    pattern: Option<String>,
    save_to_user: bool,
}

impl Default for PolicySuggestion {
    fn default() -> Self {
        Self {
            tool: String::new(),
            action: "allow".to_string(),
            pattern: None,
            save_to_user: false,
        }
    }
}

impl App {
    fn new() -> Self {
        let db_path = config::db_path();
        let mut app = App {
            db_path,
            pending: Vec::new(),
            session_names: std::collections::HashMap::new(),
            list_state: ListState::default(),
            mode: Mode::Normal,
            deny_reason: String::new(),
            policy_suggestion: PolicySuggestion::default(),
            message: None,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        if let Ok(db) = Db::open(&self.db_path) {
            if let Ok(pending) = db.get_pending_tool_calls(None) {
                self.pending = pending;
            }
            if let Ok(names) = db.get_session_names() {
                self.session_names = names;
            }
        }

        // Adjust selection if list changed
        if self.pending.is_empty() {
            self.list_state.select(None);
        } else if self.list_state.selected().is_none() {
            self.list_state.select(Some(0));
        } else if let Some(i) = self.list_state.selected() {
            if i >= self.pending.len() {
                self.list_state.select(Some(self.pending.len() - 1));
            }
        }
    }

    fn selected_tool_call(&self) -> Option<&ToolCall> {
        self.list_state.selected().and_then(|i| self.pending.get(i))
    }

    fn session_display(&self, session_id: &str) -> String {
        self.session_names
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| session_id[..8.min(session_id.len())].to_string())
    }

    fn approve_selected(&mut self) {
        if let Some(tc) = self.selected_tool_call() {
            let id = tc.id;
            if let Ok(db) = Db::open(&self.db_path) {
                if db.resolve_tool_call(id, "approved", None).unwrap_or(false) {
                    self.show_message(format!("Approved tool call #{}", id));
                }
            }
            self.refresh();
        }
    }

    fn deny_selected(&mut self) {
        if let Some(tc) = self.selected_tool_call() {
            let id = tc.id;
            let reason = if self.deny_reason.is_empty() {
                None
            } else {
                Some(self.deny_reason.as_str())
            };
            if let Ok(db) = Db::open(&self.db_path) {
                if db.resolve_tool_call(id, "denied", reason).unwrap_or(false) {
                    self.show_message(format!("Denied tool call #{}", id));
                }
            }
            self.deny_reason.clear();
            self.refresh();
        }
    }

    fn start_deny_prompt(&mut self) {
        if self.selected_tool_call().is_some() {
            self.deny_reason.clear();
            self.mode = Mode::DenyPrompt;
        }
    }

    fn start_policy_confirm(&mut self) {
        if let Some(tc) = self.selected_tool_call() {
            // Suggest a policy based on the tool call
            self.policy_suggestion = PolicySuggestion {
                tool: tc.tool_name.clone(),
                action: "allow".to_string(),
                pattern: extract_pattern_suggestion(&tc.tool_name, &tc.tool_input),
                save_to_user: false,
            };
            self.mode = Mode::PolicyConfirm;
        }
    }

    fn confirm_policy(&mut self) {
        // First approve the tool call
        self.approve_selected();

        // Then add the policy
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = if self.policy_suggestion.save_to_user {
            config::user_config_path()
        } else {
            config::project_config_path(&cwd)
        };

        let mut cfg = config::load_file(&path);
        cfg.policies
            .retain(|p| p.tool != self.policy_suggestion.tool);
        cfg.policies.push(Policy {
            tool: self.policy_suggestion.tool.clone(),
            action: self.policy_suggestion.action.clone(),
            pattern: self.policy_suggestion.pattern.clone(),
            match_mode: MatchMode::default(),
        });

        if cfg.save(&path).is_ok() {
            let scope = if self.policy_suggestion.save_to_user {
                "user"
            } else {
                "project"
            };
            self.show_message(format!(
                "Added {} policy: {} -> {}",
                scope, self.policy_suggestion.tool, self.policy_suggestion.action
            ));
        }

        self.mode = Mode::Normal;
        self.refresh();
    }

    fn show_message(&mut self, msg: String) {
        self.message = Some((msg, Instant::now()));
    }

    fn clear_old_message(&mut self) {
        if let Some((_, when)) = &self.message {
            if when.elapsed() > Duration::from_secs(3) {
                self.message = None;
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.pending.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).clamp(0, self.pending.len() as i32 - 1) as usize;
        self.list_state.select(Some(new));
    }
}

fn extract_pattern_suggestion(tool_name: &str, tool_input: &str) -> Option<String> {
    // For Bash commands, suggest a pattern based on the command
    if tool_name == "Bash" {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(tool_input) {
            if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
                // Extract the base command (first word)
                if let Some(base) = cmd.split_whitespace().next() {
                    // Escape regex special chars and create a pattern
                    let escaped = regex::escape(base);
                    return Some(format!("^{}\\b", escaped));
                }
            }
        }
    }
    None
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        // Poll for events with timeout
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match &app.mode {
                    Mode::Normal => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
                        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
                        KeyCode::Enter | KeyCode::Char('a') => app.approve_selected(),
                        KeyCode::Char('d') => app.start_deny_prompt(),
                        KeyCode::Char('p') => app.start_policy_confirm(),
                        _ => {}
                    },
                    Mode::DenyPrompt => match key.code {
                        KeyCode::Esc => {
                            app.mode = Mode::Normal;
                            app.deny_reason.clear();
                        }
                        KeyCode::Enter => {
                            app.deny_selected();
                            app.mode = Mode::Normal;
                        }
                        KeyCode::Backspace => {
                            app.deny_reason.pop();
                        }
                        KeyCode::Char(c) => {
                            app.deny_reason.push(c);
                        }
                        _ => {}
                    },
                    Mode::PolicyConfirm => match key.code {
                        KeyCode::Esc | KeyCode::Char('n') => {
                            app.mode = Mode::Normal;
                        }
                        KeyCode::Enter | KeyCode::Char('y') => {
                            app.confirm_policy();
                        }
                        KeyCode::Char('u') => {
                            app.policy_suggestion.save_to_user =
                                !app.policy_suggestion.save_to_user;
                        }
                        KeyCode::Char('c') => {
                            // Clear pattern
                            app.policy_suggestion.pattern = None;
                        }
                        _ => {}
                    },
                }
            }
        }

        // Refresh pending list periodically
        if last_refresh.elapsed() >= POLL_INTERVAL {
            app.refresh();
            app.clear_old_message();
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(5),    // List
            Constraint::Length(3), // Footer/help
        ])
        .split(f.area());

    // Header
    let header_text = format!(
        " cq tui - {} pending approval{}",
        app.pending.len(),
        if app.pending.len() == 1 { "" } else { "s" }
    );
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Cyan).bold())
        .block(Block::default().borders(Borders::BOTTOM));
    f.render_widget(header, chunks[0]);

    // Main list
    let items: Vec<ListItem> = app
        .pending
        .iter()
        .map(|tc| {
            let session_name = app.session_display(&tc.session_id);
            let input_preview = format::format_tool_input(&tc.tool_name, &tc.tool_input, 50);
            let summary_text = tc
                .summary
                .as_ref()
                .map(|s| format!(" - {}", s))
                .unwrap_or_default();

            let line = Line::from(vec![
                Span::styled(
                    format!("#{:<4} ", tc.id),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<12} ", session_name),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(
                    format!("{:<15} ", tc.tool_name),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(input_preview),
                Span::styled(summary_text, Style::default().fg(Color::DarkGray).italic()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Pending Tool Calls ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, chunks[1], &mut app.list_state);

    // Footer
    let help_text = match &app.mode {
        Mode::Normal => {
            " [Enter/a] Approve  [d] Deny  [p] Approve + Policy  [j/k] Navigate  [q] Quit"
        }
        Mode::DenyPrompt => " Type reason (optional), then [Enter] to deny, [Esc] to cancel",
        Mode::PolicyConfirm => " [y/Enter] Confirm  [n/Esc] Cancel  [u] Toggle user/project  [c] Clear pattern",
    };

    let mut footer_spans = vec![Span::raw(help_text)];

    // Show message if any
    if let Some((msg, _)) = &app.message {
        footer_spans = vec![
            Span::styled(format!(" {} ", msg), Style::default().fg(Color::Green)),
            Span::raw(" | "),
            Span::raw(help_text),
        ];
    }

    let footer = Paragraph::new(Line::from(footer_spans))
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::TOP));
    f.render_widget(footer, chunks[2]);

    // Draw modal dialogs
    match &app.mode {
        Mode::DenyPrompt => {
            draw_deny_dialog(f, app);
        }
        Mode::PolicyConfirm => {
            draw_policy_dialog(f, app);
        }
        Mode::Normal => {}
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_deny_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Deny Reason (optional) ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = if app.deny_reason.is_empty() {
        Span::styled(
            "(press Enter to deny without reason)",
            Style::default().fg(Color::DarkGray),
        )
    } else {
        Span::raw(&app.deny_reason)
    };

    let paragraph = Paragraph::new(Line::from(text)).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}

fn draw_policy_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Add Policy ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let scope = if app.policy_suggestion.save_to_user {
        "user (~/.cq/config.json)"
    } else {
        "project (.cq/config.json)"
    };

    let pattern_display = app
        .policy_suggestion
        .pattern
        .as_ref()
        .map(|p| format!("Pattern: {}", p))
        .unwrap_or_else(|| "Pattern: (none - matches all)".to_string());

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Tool:    "),
            Span::styled(
                &app.policy_suggestion.tool,
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Action:  "),
            Span::styled(
                &app.policy_suggestion.action,
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(pattern_display, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Scope:   "),
            Span::styled(scope, Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  This will auto-approve matching tool calls in the future.",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, inner);
}
