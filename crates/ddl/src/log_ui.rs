use std::collections::BTreeMap;
use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::error::Result;
use crate::model::{CheckpointKind, CheckpointRecord, RunRecord, TimelineRecord};
use crate::presentation::{
    RecoveryCapability, format_absolute_time, format_relative_time, format_runtime,
    latest_action_label, recovery_capability, session_status_label, session_title,
    tool_event_label, tool_event_preview,
};
use crate::store::DaedalusStore;

pub enum LogUiExit {
    Quit,
    Rewind(String),
}

const LIST_HIGHLIGHT_SYMBOL: &str = "› ";

pub fn run_log_ui(store: &DaedalusStore) -> Result<LogUiExit> {
    let mut session = TerminalSession::new()?;
    let mut app = LogUiApp::load(store)?;

    loop {
        session.terminal.draw(|frame| app.draw(frame))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match app.handle_key(key, store)? {
            Some(outcome) => return Ok(outcome),
            None => {}
        }
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Timelines,
    Checkpoints,
    Diff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffCompareMode {
    Parent,
    Workspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffFocus {
    Files,
    Patch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingActionKind {
    Restore,
    Rewind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryFocus {
    Head,
    Actions,
}

#[derive(Clone)]
struct PendingAction {
    kind: PendingActionKind,
    checkpoint_id: String,
}

struct LogUiApp {
    screen: Screen,
    timelines: Vec<TimelineSummary>,
    timeline_state: ListState,
    recovery_focus: RecoveryFocus,
    checkpoint_state: ListState,
    file_state: ListState,
    diff_focus: DiffFocus,
    compare_mode: DiffCompareMode,
    diff_panel: DiffPanel,
    modal: Option<PendingAction>,
    status_message: Option<String>,
}

impl LogUiApp {
    fn load(store: &DaedalusStore) -> Result<Self> {
        let timelines = load_timeline_summaries(store)?;
        let mut timeline_state = ListState::default();
        if !timelines.is_empty() {
            timeline_state.select(Some(0));
        }

        Ok(Self {
            screen: Screen::Timelines,
            timelines,
            timeline_state,
            recovery_focus: RecoveryFocus::Actions,
            checkpoint_state: ListState::default(),
            file_state: ListState::default(),
            diff_focus: DiffFocus::Files,
            compare_mode: DiffCompareMode::Parent,
            diff_panel: DiffPanel::empty("Select a checkpoint to inspect its diff".to_string()),
            modal: None,
            status_message: None,
        })
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < 72 || area.height < 18 {
            self.draw_small_terminal(frame, area);
            return;
        }

        match self.screen {
            Screen::Timelines => self.draw_timelines(frame, area),
            Screen::Checkpoints => self.draw_checkpoints(frame, area),
            Screen::Diff => self.draw_diff(frame, area),
        }

        if self.modal.is_some() {
            self.draw_modal(frame, area);
        }
    }

    fn handle_key(&mut self, key: KeyEvent, store: &DaedalusStore) -> Result<Option<LogUiExit>> {
        if self.modal.is_some() {
            return self.handle_modal_key(key, store);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(Some(LogUiExit::Quit));
        }

        match self.screen {
            Screen::Timelines => self.handle_timelines_key(key),
            Screen::Checkpoints => self.handle_checkpoints_key(key, store),
            Screen::Diff => self.handle_diff_key(key, store),
        }
    }

    fn handle_timelines_key(&mut self, key: KeyEvent) -> Result<Option<LogUiExit>> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Ok(Some(LogUiExit::Quit)),
            KeyCode::Down => {
                move_selection(&mut self.timeline_state, self.timelines.len(), 1);
                Ok(None)
            }
            KeyCode::Up => {
                move_selection(&mut self.timeline_state, self.timelines.len(), -1);
                Ok(None)
            }
            KeyCode::PageDown => {
                move_selection(&mut self.timeline_state, self.timelines.len(), 8);
                Ok(None)
            }
            KeyCode::PageUp => {
                move_selection(&mut self.timeline_state, self.timelines.len(), -8);
                Ok(None)
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if self.selected_timeline().is_some() {
                    self.enter_checkpoints();
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn handle_checkpoints_key(
        &mut self,
        key: KeyEvent,
        store: &DaedalusStore,
    ) -> Result<Option<LogUiExit>> {
        let has_head = self
            .selected_timeline()
            .and_then(|item| item.head.as_ref())
            .is_some();
        let checkpoints_len = self
            .selected_timeline()
            .map(|item| item.checkpoints.len())
            .unwrap_or(0);

        match key.code {
            KeyCode::Char('q') => Ok(Some(LogUiExit::Quit)),
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                self.screen = Screen::Timelines;
                self.status_message = None;
                Ok(None)
            }
            KeyCode::Down => {
                match self.recovery_focus {
                    RecoveryFocus::Head if checkpoints_len > 0 => {
                        self.recovery_focus = RecoveryFocus::Actions;
                        if self.checkpoint_state.selected().is_none() {
                            self.checkpoint_state.select(Some(0));
                        }
                    }
                    RecoveryFocus::Actions => {
                        move_selection(&mut self.checkpoint_state, checkpoints_len, 1);
                    }
                    RecoveryFocus::Head => {}
                }
                Ok(None)
            }
            KeyCode::Up => {
                match self.recovery_focus {
                    RecoveryFocus::Actions
                        if self.checkpoint_state.selected() == Some(0) && has_head =>
                    {
                        self.recovery_focus = RecoveryFocus::Head;
                    }
                    RecoveryFocus::Actions => {
                        move_selection(&mut self.checkpoint_state, checkpoints_len, -1);
                    }
                    RecoveryFocus::Head => {}
                }
                Ok(None)
            }
            KeyCode::PageDown => {
                match self.recovery_focus {
                    RecoveryFocus::Head if checkpoints_len > 0 => {
                        self.recovery_focus = RecoveryFocus::Actions;
                        self.checkpoint_state.select(Some(0));
                    }
                    RecoveryFocus::Actions => {
                        move_selection(&mut self.checkpoint_state, checkpoints_len, 8);
                    }
                    RecoveryFocus::Head => {}
                }
                Ok(None)
            }
            KeyCode::PageUp => {
                match self.recovery_focus {
                    RecoveryFocus::Actions if has_head => {
                        self.recovery_focus = RecoveryFocus::Head;
                    }
                    RecoveryFocus::Actions => {
                        move_selection(&mut self.checkpoint_state, checkpoints_len, -8);
                    }
                    RecoveryFocus::Head => {}
                }
                Ok(None)
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.enter_diff(store)?;
                Ok(None)
            }
            KeyCode::Char('r') => {
                self.begin_action(PendingActionKind::Restore);
                Ok(None)
            }
            KeyCode::Char('w') => {
                self.begin_action(PendingActionKind::Rewind);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn handle_diff_key(
        &mut self,
        key: KeyEvent,
        store: &DaedalusStore,
    ) -> Result<Option<LogUiExit>> {
        match key.code {
            KeyCode::Char('q') => Ok(Some(LogUiExit::Quit)),
            KeyCode::Esc | KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                self.screen = Screen::Checkpoints;
                self.status_message = None;
                Ok(None)
            }
            KeyCode::Tab => {
                self.diff_focus = match self.diff_focus {
                    DiffFocus::Files => DiffFocus::Patch,
                    DiffFocus::Patch => DiffFocus::Files,
                };
                Ok(None)
            }
            KeyCode::Char('c') => {
                self.compare_mode = match self.compare_mode {
                    DiffCompareMode::Parent => DiffCompareMode::Workspace,
                    DiffCompareMode::Workspace => DiffCompareMode::Parent,
                };
                self.reload_diff(store)?;
                Ok(None)
            }
            KeyCode::Char('r') => {
                self.begin_action(PendingActionKind::Restore);
                Ok(None)
            }
            KeyCode::Char('w') => {
                self.begin_action(PendingActionKind::Rewind);
                Ok(None)
            }
            KeyCode::Down => {
                self.move_diff_focus(1);
                Ok(None)
            }
            KeyCode::Up => {
                self.move_diff_focus(-1);
                Ok(None)
            }
            KeyCode::PageDown => {
                self.page_diff(1);
                Ok(None)
            }
            KeyCode::PageUp => {
                self.page_diff(-1);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn handle_modal_key(
        &mut self,
        key: KeyEvent,
        store: &DaedalusStore,
    ) -> Result<Option<LogUiExit>> {
        let Some(modal) = self.modal.as_ref() else {
            return Ok(None);
        };

        match key.code {
            KeyCode::Esc => {
                self.modal = None;
                Ok(None)
            }
            KeyCode::Enter => {
                let action = modal.clone();
                let outcome = match action.kind {
                    PendingActionKind::Restore => {
                        store.restore(&action.checkpoint_id)?;
                        self.status_message =
                            Some(format!("restored workspace to {}", action.checkpoint_id));
                        self.refresh(store)?;
                        None
                    }
                    PendingActionKind::Rewind => {
                        self.modal = None;
                        return Ok(Some(LogUiExit::Rewind(action.checkpoint_id)));
                    }
                };
                self.modal = None;
                Ok(outcome)
            }
            _ => Ok(None),
        }
    }

    fn draw_small_terminal(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(" daedalus recovery ")
            .borders(Borders::ALL)
            .border_type(BorderType::Thick);
        let message = Paragraph::new(Text::from(vec![
            Line::from("Terminal is too small for the interactive log view."),
            Line::from("Resize to at least 72x18 or pipe `ddl log` for plain text output."),
            Line::from(""),
            Line::from("[q] quit"),
        ]))
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
        frame.render_widget(message, area);
    }

    fn draw_timelines(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(3)])
            .split(area);
        let row_width = list_row_width(sections[0], LIST_HIGHLIGHT_SYMBOL);

        let items = if self.timelines.is_empty() {
            vec![ListItem::new(Line::from(
                "No sessions recorded yet. Run `ddl run -- claude ...` or `ddl shell -- ...` first.",
            ))]
        } else {
            self.timelines
                .iter()
                .map(|timeline| ListItem::new(timeline_row(timeline, row_width)))
                .collect::<Vec<_>>()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Recent Sessions ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(LIST_HIGHLIGHT_SYMBOL);
        frame.render_stateful_widget(list, sections[0], &mut self.timeline_state);
        frame.render_widget(
            footer_paragraph(
                "Browse recent sessions",
                self.status_message.as_deref(),
                "[up/down] move  [enter] session history  [q] quit",
            ),
            sections[1],
        );
    }

    fn draw_checkpoints(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(timeline) = self.selected_timeline() else {
            self.draw_timelines(frame, area);
            return;
        };
        let sections = if timeline.head.is_some() {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),
                    Constraint::Length(4),
                    Constraint::Min(8),
                    Constraint::Length(3),
                ])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),
                    Constraint::Min(8),
                    Constraint::Length(3),
                ])
                .split(area)
        };
        let actions_section = if timeline.head.is_some() { 2 } else { 1 };
        let footer_section = if timeline.head.is_some() { 3 } else { 2 };
        let row_width = list_row_width(sections[actions_section], LIST_HIGHLIGHT_SYMBOL);

        let header = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    timeline.title.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    session_status_label(&timeline.run.status),
                    status_style(&timeline.run),
                ),
                Span::raw("  "),
                Span::styled(
                    timeline.capability.label(),
                    recovery_style(timeline.capability),
                ),
            ]),
            Line::from(format!(
                "Started {}  |  Runtime {}  |  {} protected actions",
                format_absolute_time(timeline.timeline.created_at),
                timeline.runtime,
                timeline.checkpoints.len()
            )),
            Line::from(format!(
                "Latest protected action: {}",
                timeline.latest_action
            )),
        ]))
        .block(
            Block::default()
                .title(" Session ")
                .borders(Borders::ALL)
                .border_type(BorderType::Thick),
        );
        frame.render_widget(header, sections[0]);

        if let Some(head) = &timeline.head {
            frame.render_widget(
                session_head_panel(
                    head,
                    panel_row_width(sections[1]),
                    self.recovery_focus == RecoveryFocus::Head,
                ),
                sections[1],
            );
        }

        let items = if timeline.checkpoints.is_empty() {
            vec![ListItem::new(Line::from(
                "No protected actions recorded in this session yet.",
            ))]
        } else {
            timeline
                .checkpoints
                .iter()
                .map(|checkpoint| ListItem::new(checkpoint_row(checkpoint, row_width)))
                .collect::<Vec<_>>()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .title(" Protected Actions ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(LIST_HIGHLIGHT_SYMBOL);
        frame.render_stateful_widget(list, sections[actions_section], &mut self.checkpoint_state);
        frame.render_widget(
            footer_paragraph(
                "Inspect protected actions before recovering",
                self.status_message.as_deref(),
                "[up/down] move  [enter] diff  [r] restore  [w] rewind action  [esc] back  [q] quit",
            ),
            sections[footer_section],
        );
    }

    fn draw_diff(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(timeline) = self.selected_timeline() else {
            self.draw_timelines(frame, area);
            return;
        };
        let Some(checkpoint) = self.selected_target() else {
            self.draw_checkpoints(frame, area);
            return;
        };

        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
            .split(sections[1]);

        let header = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    checkpoint.label.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    checkpoint.capability.label(),
                    recovery_style(checkpoint.capability),
                ),
                Span::raw("  "),
                Span::styled(
                    format_relative_time(checkpoint.checkpoint.created_at),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(format!(
                "{}  |  {}",
                timeline.title, self.diff_panel.compare_label
            )),
            Line::from(checkpoint.preview.clone().unwrap_or_else(|| {
                "Review the file changes before you restore or rewind.".to_string()
            })),
        ]))
        .block(
            Block::default()
                .title(" Diff ")
                .borders(Borders::ALL)
                .border_type(BorderType::Thick),
        );
        frame.render_widget(header, sections[0]);

        let file_items = if self.diff_panel.files.is_empty() {
            vec![ListItem::new(Line::from(
                "No file changes in this comparison.",
            ))]
        } else {
            self.diff_panel
                .files
                .iter()
                .map(|file| {
                    ListItem::new(Line::from(vec![
                        Span::styled(file.path.clone(), Style::default().fg(Color::White)),
                        Span::raw(format!("  ({})", file.summary)),
                    ]))
                })
                .collect::<Vec<_>>()
        };
        let file_block = Block::default()
            .title(if self.diff_focus == DiffFocus::Files {
                " Files [focus] "
            } else {
                " Files "
            })
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let files = List::new(file_items)
            .block(file_block)
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");
        frame.render_stateful_widget(files, body[0], &mut self.file_state);

        let patch_block = Block::default()
            .title(if self.diff_focus == DiffFocus::Patch {
                " Patch [focus] "
            } else {
                " Patch "
            })
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let patch = Paragraph::new(self.patch_text())
            .block(patch_block)
            .wrap(Wrap { trim: false })
            .scroll((self.diff_panel.patch_scroll, 0));
        frame.render_widget(patch, body[1]);

        frame.render_widget(
            footer_paragraph(
                "Diff is the recovery decision point",
                self.status_message.as_deref(),
                "[tab] switch pane  [up/down] scroll  [c] compare mode  [r] restore  [w] rewind  [esc] back  [q] quit",
            ),
            sections[2],
        );
    }

    fn draw_modal(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(modal) = &self.modal else {
            return;
        };

        let modal_area = centered_rect(70, 32, area);
        frame.render_widget(Clear, modal_area);

        let mut lines = vec![
            Line::from(Span::styled(
                pending_action_title(modal.kind),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];

        if let Some(checkpoint) = self.selected_target() {
            lines.push(Line::from(checkpoint.label.clone()));
        }

        match modal.kind {
            PendingActionKind::Restore => {
                lines.push(Line::from(
                    "This restores the workspace snapshot and keeps you in session history.",
                ));
            }
            PendingActionKind::Rewind => {
                lines.push(Line::from(
                    "This restores the snapshot and exits into the resumed runtime.",
                ));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from("[enter] confirm  [esc] cancel"));

        let paragraph = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .title(" Confirm Action ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, modal_area);
    }

    fn enter_checkpoints(&mut self) {
        self.screen = Screen::Checkpoints;
        self.checkpoint_state = ListState::default();
        self.recovery_focus = RecoveryFocus::Actions;
        if let Some(timeline) = self.selected_timeline() {
            if timeline.head.is_some() {
                self.recovery_focus = RecoveryFocus::Head;
            } else if !timeline.checkpoints.is_empty() {
                self.checkpoint_state.select(Some(0));
            }
        }
        self.status_message = None;
    }

    fn enter_diff(&mut self, store: &DaedalusStore) -> Result<()> {
        if self.selected_target().is_none() {
            return Ok(());
        }

        self.screen = Screen::Diff;
        self.compare_mode = if self
            .selected_target()
            .and_then(|checkpoint| checkpoint.checkpoint.parent_checkpoint_id.as_ref())
            .is_some()
        {
            DiffCompareMode::Parent
        } else {
            DiffCompareMode::Workspace
        };
        self.diff_focus = DiffFocus::Files;
        self.reload_diff(store)
    }

    fn reload_diff(&mut self, store: &DaedalusStore) -> Result<()> {
        let Some(checkpoint) = self.selected_target() else {
            return Ok(());
        };

        self.diff_panel = build_diff_panel(store, checkpoint, self.compare_mode)?;
        self.file_state = ListState::default();
        if !self.diff_panel.files.is_empty() {
            self.file_state.select(Some(0));
        }
        Ok(())
    }

    fn refresh(&mut self, store: &DaedalusStore) -> Result<()> {
        let selected_timeline_id = self
            .selected_timeline()
            .map(|item| item.timeline.id.clone());
        let selected_checkpoint_id = self
            .selected_target()
            .map(|item| item.checkpoint.id.clone());

        self.timelines = load_timeline_summaries(store)?;
        self.timeline_state = ListState::default();
        select_by_id(
            &mut self.timeline_state,
            &self.timelines,
            selected_timeline_id.as_deref(),
            |item| item.timeline.id.as_str(),
        );

        if self.screen != Screen::Timelines {
            self.checkpoint_state = ListState::default();
            let selected_timeline_index = self.timeline_state.selected();
            if let Some(index) = selected_timeline_index {
                let timeline = &self.timelines[index];
                if timeline.head.as_ref().is_some_and(|item| {
                    Some(item.checkpoint.id.as_str()) == selected_checkpoint_id.as_deref()
                }) {
                    self.recovery_focus = RecoveryFocus::Head;
                } else {
                    self.recovery_focus = RecoveryFocus::Actions;
                    select_by_id(
                        &mut self.checkpoint_state,
                        &timeline.checkpoints,
                        selected_checkpoint_id.as_deref(),
                        |item| item.checkpoint.id.as_str(),
                    );
                    if self.checkpoint_state.selected().is_none() && timeline.head.is_some() {
                        self.recovery_focus = RecoveryFocus::Head;
                    }
                }
                if timeline.checkpoints.is_empty() && timeline.head.is_none() {
                    self.screen = Screen::Checkpoints;
                }
            } else {
                self.screen = Screen::Timelines;
            }
        }

        if self.screen == Screen::Diff {
            self.reload_diff(store)?;
        }

        Ok(())
    }

    fn begin_action(&mut self, kind: PendingActionKind) {
        let Some(checkpoint) = self.selected_target() else {
            return;
        };

        if kind == PendingActionKind::Rewind
            && checkpoint.capability != RecoveryCapability::Rewindable
        {
            self.status_message = Some("rewind is unavailable for this checkpoint".to_string());
            return;
        }

        if kind == PendingActionKind::Restore
            && checkpoint.capability == RecoveryCapability::Unavailable
        {
            self.status_message = Some("restore is unavailable for this checkpoint".to_string());
            return;
        }

        self.modal = Some(PendingAction {
            kind,
            checkpoint_id: checkpoint.checkpoint.id.clone(),
        });
    }

    fn move_diff_focus(&mut self, delta: isize) {
        match self.diff_focus {
            DiffFocus::Files => {
                move_selection(&mut self.file_state, self.diff_panel.files.len(), delta);
                self.diff_panel.patch_scroll = 0;
            }
            DiffFocus::Patch => {
                self.diff_panel.patch_scroll =
                    scroll_amount(self.diff_panel.patch_scroll, delta, 1);
            }
        }
    }

    fn page_diff(&mut self, direction: isize) {
        match self.diff_focus {
            DiffFocus::Files => {
                move_selection(
                    &mut self.file_state,
                    self.diff_panel.files.len(),
                    direction * 8,
                );
                self.diff_panel.patch_scroll = 0;
            }
            DiffFocus::Patch => {
                self.diff_panel.patch_scroll =
                    scroll_amount(self.diff_panel.patch_scroll, direction, 12);
            }
        }
    }

    fn patch_text(&self) -> Text<'_> {
        let lines = self.selected_patch_lines();
        if lines.is_empty() {
            return Text::from(vec![Line::from("No patch content for this comparison.")]);
        }

        Text::from(
            lines
                .iter()
                .map(|line| Line::from(style_diff_line(line)))
                .collect::<Vec<_>>(),
        )
    }

    fn selected_patch_lines(&self) -> &[String] {
        let Some(index) = self.file_state.selected() else {
            return &self.diff_panel.patch_lines;
        };
        let Some(file) = self.diff_panel.files.get(index) else {
            return &self.diff_panel.patch_lines;
        };
        &self.diff_panel.patch_lines[file.start_line..file.end_line]
    }

    fn selected_timeline(&self) -> Option<&TimelineSummary> {
        self.timeline_state
            .selected()
            .and_then(|index| self.timelines.get(index))
    }

    fn selected_target(&self) -> Option<&CheckpointSummary> {
        let timeline = self.selected_timeline()?;
        match self.recovery_focus {
            RecoveryFocus::Head => timeline.head.as_ref(),
            RecoveryFocus::Actions => self
                .checkpoint_state
                .selected()
                .and_then(|index| timeline.checkpoints.get(index))
                .or_else(|| timeline.head.as_ref()),
        }
    }
}

#[derive(Clone)]
struct TimelineSummary {
    timeline: TimelineRecord,
    run: RunRecord,
    title: String,
    runtime: String,
    head: Option<CheckpointSummary>,
    checkpoints: Vec<CheckpointSummary>,
    latest_action: String,
    capability: RecoveryCapability,
}

#[derive(Clone)]
struct CheckpointSummary {
    checkpoint: CheckpointRecord,
    label: String,
    preview: Option<String>,
    capability: RecoveryCapability,
}

struct DiffPanel {
    compare_label: String,
    files: Vec<DiffFile>,
    patch_lines: Vec<String>,
    patch_scroll: u16,
}

impl DiffPanel {
    fn empty(compare_label: String) -> Self {
        Self {
            compare_label,
            files: Vec::new(),
            patch_lines: Vec::new(),
            patch_scroll: 0,
        }
    }
}

struct DiffFile {
    path: String,
    summary: String,
    start_line: usize,
    end_line: usize,
}

fn load_timeline_summaries(store: &DaedalusStore) -> Result<Vec<TimelineSummary>> {
    let timelines = store.list_timelines()?;
    let checkpoints = store.list_checkpoints()?;

    let mut grouped = BTreeMap::<String, Vec<CheckpointRecord>>::new();
    for checkpoint in checkpoints {
        grouped
            .entry(checkpoint.timeline_id.clone())
            .or_default()
            .push(checkpoint);
    }

    let mut items = Vec::new();
    for timeline in timelines.into_iter().rev() {
        let run = store.read_run(&timeline.run_id)?;
        let mut timeline_checkpoints = grouped.remove(&timeline.id).unwrap_or_default();
        timeline_checkpoints.sort_by_key(|item| std::cmp::Reverse(item.created_at));

        let mut head = None;
        let mut checkpoints = Vec::new();
        for checkpoint in timeline_checkpoints {
            let summary = CheckpointSummary {
                label: tool_event_label(&checkpoint),
                preview: tool_event_preview(&checkpoint),
                capability: recovery_capability(&checkpoint),
                checkpoint,
            };
            if summary.checkpoint.kind == CheckpointKind::SessionHead {
                let is_selected_head =
                    run.head_checkpoint_id.as_deref() == Some(summary.checkpoint.id.as_str());
                if head.is_none() || is_selected_head {
                    head = Some(summary);
                }
            } else {
                checkpoints.push(summary);
            }
        }

        let latest_action = if let Some(checkpoint) = checkpoints.first() {
            latest_action_label(Some(&checkpoint.checkpoint))
        } else if head.is_some() {
            "Session head available".to_string()
        } else {
            latest_action_label(None)
        };
        let capability = checkpoints
            .first()
            .map(|item| item.capability)
            .or_else(|| head.as_ref().map(|item| item.capability))
            .unwrap_or(RecoveryCapability::Unavailable);
        let title = session_title(&timeline, &run);
        let end_timestamp = head
            .as_ref()
            .map(|item| item.checkpoint.created_at)
            .or_else(|| checkpoints.first().map(|item| item.checkpoint.created_at));
        let runtime = format_runtime(timeline.created_at, end_timestamp);

        items.push(TimelineSummary {
            timeline,
            run,
            title,
            runtime,
            head,
            checkpoints,
            latest_action,
            capability,
        });
    }

    Ok(items)
}

fn build_diff_panel(
    store: &DaedalusStore,
    checkpoint: &CheckpointSummary,
    compare_mode: DiffCompareMode,
) -> Result<DiffPanel> {
    let (compare_label, patch) = match compare_mode {
        DiffCompareMode::Parent => match checkpoint.checkpoint.parent_checkpoint_id.as_deref() {
            Some(parent_id) => (
                if checkpoint.checkpoint.kind == CheckpointKind::SessionHead {
                    "Compared with latest protected action".to_string()
                } else {
                    "Compared with previous protected action".to_string()
                },
                store.diff(parent_id, &checkpoint.checkpoint.id)?,
            ),
            None => (
                "Compared with current workspace".to_string(),
                store.diff_workspace(&checkpoint.checkpoint.id)?,
            ),
        },
        DiffCompareMode::Workspace => (
            "Compared with current workspace".to_string(),
            store.diff_workspace(&checkpoint.checkpoint.id)?,
        ),
    };

    let patch_lines = if patch.trim().is_empty() {
        vec!["No textual differences in this comparison.".to_string()]
    } else {
        patch.lines().map(ToOwned::to_owned).collect::<Vec<_>>()
    };

    Ok(DiffPanel {
        files: parse_diff_files(&patch_lines),
        patch_lines,
        compare_label,
        patch_scroll: 0,
    })
}

fn parse_diff_files(lines: &[String]) -> Vec<DiffFile> {
    let mut items = Vec::new();
    let mut start = None;
    let mut path = String::new();

    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git ") {
            if let Some(start_line) = start.replace(index) {
                items.push(DiffFile {
                    path: std::mem::take(&mut path),
                    summary: summarize_diff(&lines[start_line..index]),
                    start_line,
                    end_line: index,
                });
            }
            path = parse_diff_path(line);
        }
    }

    if let Some(start_line) = start {
        items.push(DiffFile {
            path,
            summary: summarize_diff(&lines[start_line..]),
            start_line,
            end_line: lines.len(),
        });
    }

    items
}

fn parse_diff_path(line: &str) -> String {
    let mut parts = line.split_whitespace();
    let _ = parts.next();
    let _ = parts.next();
    let _ = parts.next();
    let b_path = parts.next().unwrap_or("b/unknown");
    b_path
        .trim_start_matches("b/")
        .trim_start_matches("a/")
        .to_string()
}

fn summarize_diff(lines: &[String]) -> String {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in lines {
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }

    match (added, removed) {
        (0, 0) => "metadata".to_string(),
        (0, removed) => format!("-{removed}"),
        (added, 0) => format!("+{added}"),
        (added, removed) => format!("+{added} -{removed}"),
    }
}

fn timeline_row(timeline: &TimelineSummary, width: usize) -> Text<'static> {
    let status = session_status_label(&timeline.run.status);
    let capability = timeline.capability.label();
    let started = format_absolute_time(timeline.timeline.created_at);
    let checkpoint_count = format!("{} protected actions", timeline.checkpoints.len());
    let title_width = line_budget(width, text_width(status) + text_width(capability) + 4, 24);
    let latest_action_width = line_budget(
        width,
        text_width(&started) + text_width(&checkpoint_count) + 10,
        24,
    );

    Text::from(vec![
        Line::from(vec![
            Span::styled(
                truncate(&timeline.title, title_width),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(status, status_style(&timeline.run)),
            Span::raw("  "),
            Span::styled(capability, recovery_style(timeline.capability)),
        ]),
        Line::from(vec![
            Span::styled(started, Style::default().fg(Color::DarkGray)),
            Span::raw("  |  "),
            Span::raw(checkpoint_count),
            Span::raw("  |  "),
            Span::raw(truncate(&timeline.latest_action, latest_action_width)),
        ]),
    ])
}

fn checkpoint_row(checkpoint: &CheckpointSummary, width: usize) -> Text<'static> {
    let preview = checkpoint
        .preview
        .clone()
        .unwrap_or_else(|| "Protected action metadata unavailable.".to_string());
    let capability = checkpoint.capability.label();
    let relative_time = format_relative_time(checkpoint.checkpoint.created_at);
    let absolute_time = format_absolute_time(checkpoint.checkpoint.created_at);
    let label_width = line_budget(
        width,
        text_width(capability) + text_width(&relative_time) + 4,
        24,
    );
    let preview_width = line_budget(width, text_width(&absolute_time) + 5, 24);

    Text::from(vec![
        Line::from(vec![
            Span::styled(
                truncate(&checkpoint.label, label_width),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(capability, recovery_style(checkpoint.capability)),
            Span::raw("  "),
            Span::styled(relative_time, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled(
                truncate(&preview, preview_width),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  |  "),
            Span::raw(absolute_time),
        ]),
    ])
}

fn session_head_panel(
    checkpoint: &CheckpointSummary,
    width: usize,
    focused: bool,
) -> Paragraph<'static> {
    let block = Block::default()
        .title(if focused {
            " Session Head [focus] "
        } else {
            " Session Head "
        })
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        });
    Paragraph::new(checkpoint_row(checkpoint, width))
        .block(block)
        .wrap(Wrap { trim: false })
}

fn footer_paragraph<'a>(
    headline: &'a str,
    status: Option<&'a str>,
    help: &'a str,
) -> Paragraph<'a> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            headline,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(help, Style::default().fg(Color::DarkGray)),
    ])];

    if let Some(status) = status {
        lines.push(Line::from(Span::styled(
            status,
            Style::default().fg(Color::Green),
        )));
    }

    Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::ALL))
}

fn status_style(run: &RunRecord) -> Style {
    match run.status {
        crate::model::RunStatus::Running => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        crate::model::RunStatus::Succeeded => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        crate::model::RunStatus::Failed => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        _ => Style::default().fg(Color::White),
    }
}

fn recovery_style(recovery: RecoveryCapability) -> Style {
    match recovery {
        RecoveryCapability::Rewindable => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        RecoveryCapability::RestoreOnly => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        RecoveryCapability::Unavailable => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }
}

fn truncate(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }

    let mut output = value
        .chars()
        .take(max_len.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn list_row_width(area: Rect, highlight_symbol: &str) -> usize {
    usize::from(area.width.saturating_sub(2))
        .saturating_sub(text_width(highlight_symbol))
        .saturating_sub(1)
}

fn panel_row_width(area: Rect) -> usize {
    usize::from(area.width.saturating_sub(3))
}

fn line_budget(total_width: usize, fixed_width: usize, min_width: usize) -> usize {
    if total_width == 0 {
        return 0;
    }

    let minimum = min_width.min(total_width);
    total_width.saturating_sub(fixed_width).max(minimum)
}

fn text_width(value: &str) -> usize {
    value.chars().count()
}

fn style_diff_line(line: &str) -> Span<'static> {
    let style = if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if line.starts_with("diff --git") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Span::styled(line.to_string(), style)
}

fn pending_action_title(kind: PendingActionKind) -> &'static str {
    match kind {
        PendingActionKind::Restore => "Restore workspace",
        PendingActionKind::Rewind => "Rewind from action",
    }
}

fn move_selection(state: &mut ListState, len: usize, delta: isize) {
    if len == 0 {
        state.select(None);
        return;
    }

    let current = state.selected().unwrap_or(0) as isize;
    let max_index = (len - 1) as isize;
    let next = (current + delta).clamp(0, max_index) as usize;
    state.select(Some(next));
}

fn scroll_amount(current: u16, direction: isize, page: u16) -> u16 {
    if direction.is_negative() {
        current.saturating_sub(page)
    } else {
        current.saturating_add(page)
    }
}

fn centered_rect(horizontal: u16, vertical: u16, area: Rect) -> Rect {
    let vertical_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - vertical) / 2),
            Constraint::Percentage(vertical),
            Constraint::Percentage((100 - vertical) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - horizontal) / 2),
            Constraint::Percentage(horizontal),
            Constraint::Percentage((100 - horizontal) / 2),
        ])
        .split(vertical_layout[1])[1]
}

fn select_by_id<T, F>(state: &mut ListState, items: &[T], selected_id: Option<&str>, id_fn: F)
where
    F: Fn(&T) -> &str,
{
    let selected = selected_id
        .and_then(|target| items.iter().position(|item| id_fn(item) == target))
        .or_else(|| (!items.is_empty()).then_some(0));
    state.select(selected);
}

#[cfg(test)]
mod tests {
    use super::{line_budget, parse_diff_files};
    use crate::model::{CheckpointKind, CheckpointRecord, Resumability, RuntimeFingerprint};
    use crate::presentation::{RecoveryCapability, recovery_capability};

    #[test]
    fn parse_diff_files_extracts_per_file_sections() {
        let lines = vec![
            "diff --git a/src/main.rs b/src/main.rs".to_string(),
            "--- a/src/main.rs".to_string(),
            "+++ b/src/main.rs".to_string(),
            "@@ -1 +1 @@".to_string(),
            "-old".to_string(),
            "+new".to_string(),
            "diff --git a/README.md b/README.md".to_string(),
            "--- a/README.md".to_string(),
            "+++ b/README.md".to_string(),
            "+hello".to_string(),
        ];

        let files = parse_diff_files(&lines);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].summary, "+1 -1");
        assert_eq!(files[1].path, "README.md");
        assert_eq!(files[1].summary, "+1");
    }

    #[test]
    fn recovery_capability_treats_partial_claude_checkpoints_as_restore_only() {
        let checkpoint = CheckpointRecord {
            id: "cp_test".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            kind: CheckpointKind::ProtectedAction,
            parent_checkpoint_id: None,
            reason: "before-shell".to_string(),
            snapshot_rel_path: "snapshots/cp_test".to_string(),
            shadow_commit: "deadbeef".to_string(),
            created_at: 1,
            resumability: Resumability::Partial,
            trigger_tool_type: Some("bash".to_string()),
            trigger_command: Some("rm -rf tmp".to_string()),
            runtime_name: Some("claude".to_string()),
            claude_session_id: None,
            claude_rewind_rel_path: None,
            fingerprint: RuntimeFingerprint {
                cwd: ".".to_string(),
                repo_root: ".".to_string(),
                git_head: "deadbeef".to_string(),
                git_branch: "main".to_string(),
                git_dirty: false,
                git_version: "git version".to_string(),
            },
        };

        assert_eq!(
            recovery_capability(&checkpoint),
            RecoveryCapability::RestoreOnly
        );
    }

    #[test]
    fn line_budget_expands_when_space_is_available() {
        assert_eq!(line_budget(80, 20, 24), 60);
    }

    #[test]
    fn line_budget_preserves_a_minimum_budget_when_space_is_tight() {
        assert_eq!(line_budget(30, 20, 24), 24);
    }
}
