use std::{
    fs,
    io::{self, Stdout},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use serde::{Deserialize, Serialize};

const BG: Color = Color::Rgb(11, 15, 20);
const PANEL: Color = Color::Rgb(20, 26, 34);
const PANEL_ALT: Color = Color::Rgb(24, 32, 42);
const BORDER: Color = Color::Rgb(58, 71, 87);
const MUTED: Color = Color::Rgb(142, 155, 170);
const TEXT: Color = Color::Rgb(229, 234, 239);
const ACCENT: Color = Color::Rgb(242, 191, 83);
const ACCENT_ALT: Color = Color::Rgb(71, 176, 165);
const SUCCESS: Color = Color::Rgb(111, 197, 122);

type TerminalUi = Terminal<CrosstermBackend<Stdout>>;

fn main() -> Result<()> {
    let mut terminal = setup_terminal()?;
    let result = (|| -> Result<()> {
        let mut app = App::load()?;
        app.run(&mut terminal)
    })();
    let restore_result = restore_terminal(&mut terminal);
    result.and(restore_result)
}

fn setup_terminal() -> Result<TerminalUi> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal backend")?;
    terminal.clear().context("failed to clear terminal")?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut TerminalUi) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to show cursor")?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Done,
}

impl Filter {
    fn title(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Done => "Done",
        }
    }

    fn matches(self, task: &Task) -> bool {
        match self {
            Self::All => true,
            Self::Active => !task.completed,
            Self::Done => task.completed,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Active,
            Self::Active => Self::Done,
            Self::Done => Self::All,
        }
    }

    fn tab_index(self) -> usize {
        match self {
            Self::All => 0,
            Self::Active => 1,
            Self::Done => 2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Task {
    id: u64,
    title: String,
    completed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredState {
    next_id: u64,
    tasks: Vec<Task>,
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            next_id: 1,
            tasks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
enum DraftMode {
    Add,
    Edit(u64),
}

#[derive(Clone, Debug)]
struct Draft {
    mode: DraftMode,
    value: String,
}

#[derive(Debug)]
struct App {
    data: StoredState,
    filter: Filter,
    selected_id: Option<u64>,
    draft: Option<Draft>,
    confirm_clear_data: bool,
    should_quit: bool,
    status: String,
}

impl App {
    fn load() -> Result<Self> {
        let path = storage_path();
        let data = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            StoredState::default()
        };

        let mut app = Self {
            data,
            filter: Filter::All,
            selected_id: None,
            draft: None,
            confirm_clear_data: false,
            should_quit: false,
            status: format!("autosave -> {}", path.display()),
        };
        app.ensure_selection();
        Ok(app)
    }

    fn run(&mut self, terminal: &mut TerminalUi) -> Result<()> {
        loop {
            terminal
                .draw(|frame| render(frame, self))
                .context("failed to render terminal UI")?;

            if self.should_quit {
                return Ok(());
            }

            if !event::poll(Duration::from_millis(200)).context("failed to poll input")? {
                continue;
            }

            match event::read().context("failed to read input event")? {
                Event::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    self.handle_key(key)?
                }
                _ => {}
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.draft.is_some() {
            self.handle_draft_key(key)?;
            return Ok(());
        }

        if self.confirm_clear_data {
            self.handle_clear_confirm_key(key)?;
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_selected()?,
            KeyCode::Char('a') => self.start_add(),
            KeyCode::Char('e') => self.start_edit_selected(),
            KeyCode::Char('d') | KeyCode::Delete => self.delete_selected()?,
            KeyCode::Char('x') => self.purge_completed()?,
            KeyCode::Char('X') => self.start_clear_data_confirmation(),
            KeyCode::Char('1') => self.set_filter(Filter::All),
            KeyCode::Char('2') => self.set_filter(Filter::Active),
            KeyCode::Char('3') => self.set_filter(Filter::Done),
            KeyCode::Tab | KeyCode::Char('/') => self.set_filter(self.filter.next()),
            _ => {}
        }

        Ok(())
    }

    fn handle_clear_confirm_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.confirm_clear_data = false;
                self.status = "full reset cancelled".to_string();
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => self.clear_all_data()?,
            _ => {}
        }

        Ok(())
    }

    fn handle_draft_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(draft) = self.draft.as_mut() else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => {
                self.status = "edit cancelled".to_string();
                self.draft = None;
            }
            KeyCode::Enter => {
                let value = draft.value.trim().to_string();
                let mode = draft.mode.clone();
                self.draft = None;
                if value.is_empty() {
                    self.status = "task title cannot be empty".to_string();
                } else {
                    match mode {
                        DraftMode::Add => self.create_task(value)?,
                        DraftMode::Edit(id) => self.rename_task(id, value)?,
                    }
                }
            }
            KeyCode::Backspace => {
                draft.value.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                draft.value.clear();
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                draft.value.push(ch);
            }
            _ => {}
        }

        Ok(())
    }

    fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.ensure_selection();
        self.status = format!("filter -> {}", filter.title());
    }

    fn visible_ids(&self) -> Vec<u64> {
        self.data
            .tasks
            .iter()
            .filter(|task| self.filter.matches(task))
            .map(|task| task.id)
            .collect()
    }

    fn selected_task(&self) -> Option<&Task> {
        let id = self.selected_id?;
        self.data.tasks.iter().find(|task| task.id == id)
    }

    fn selected_index_in_visible(&self) -> Option<usize> {
        let id = self.selected_id?;
        self.visible_ids().iter().position(|visible_id| *visible_id == id)
    }

    fn task_index(&self, id: u64) -> Option<usize> {
        self.data.tasks.iter().position(|task| task.id == id)
    }

    fn ensure_selection(&mut self) {
        let visible = self.visible_ids();
        if visible.is_empty() {
            self.selected_id = None;
            return;
        }

        if self
            .selected_id
            .is_some_and(|id| visible.iter().any(|visible_id| *visible_id == id))
        {
            return;
        }

        self.selected_id = visible.first().copied();
    }

    fn select_first(&mut self) {
        self.selected_id = self.visible_ids().first().copied();
    }

    fn select_last(&mut self) {
        self.selected_id = self.visible_ids().last().copied();
    }

    fn select_next(&mut self) {
        let visible = self.visible_ids();
        if visible.is_empty() {
            self.selected_id = None;
            return;
        }

        let current = self
            .selected_id
            .and_then(|id| visible.iter().position(|visible_id| *visible_id == id))
            .unwrap_or(0);

        let next = (current + 1) % visible.len();
        self.selected_id = visible.get(next).copied();
    }

    fn select_previous(&mut self) {
        let visible = self.visible_ids();
        if visible.is_empty() {
            self.selected_id = None;
            return;
        }

        let current = self
            .selected_id
            .and_then(|id| visible.iter().position(|visible_id| *visible_id == id))
            .unwrap_or(0);

        let previous = if current == 0 {
            visible.len() - 1
        } else {
            current - 1
        };
        self.selected_id = visible.get(previous).copied();
    }

    fn start_add(&mut self) {
        self.draft = Some(Draft {
            mode: DraftMode::Add,
            value: String::new(),
        });
        self.status = "new task".to_string();
    }

    fn start_edit_selected(&mut self) {
        let Some(task) = self.selected_task() else {
            self.status = "nothing selected".to_string();
            return;
        };

        self.draft = Some(Draft {
            mode: DraftMode::Edit(task.id),
            value: task.title.clone(),
        });
        self.status = "rename task".to_string();
    }

    fn start_clear_data_confirmation(&mut self) {
        self.confirm_clear_data = true;
        self.status = "confirm full local reset".to_string();
    }

    fn create_task(&mut self, title: String) -> Result<()> {
        let task = Task {
            id: self.data.next_id,
            title,
            completed: false,
        };

        self.data.next_id += 1;
        if self.filter == Filter::Done {
            self.filter = Filter::Active;
        }
        self.selected_id = Some(task.id);
        self.data.tasks.push(task);
        self.ensure_selection();
        self.persist()?;
        self.status = "task created".to_string();
        Ok(())
    }

    fn rename_task(&mut self, id: u64, title: String) -> Result<()> {
        let Some(index) = self.task_index(id) else {
            self.status = "task not found".to_string();
            return Ok(());
        };

        self.data.tasks[index].title = title;
        self.selected_id = Some(id);
        self.persist()?;
        self.status = "task updated".to_string();
        Ok(())
    }

    fn toggle_selected(&mut self) -> Result<()> {
        let Some(id) = self.selected_id else {
            self.status = "nothing selected".to_string();
            return Ok(());
        };
        let Some(index) = self.task_index(id) else {
            self.status = "task not found".to_string();
            return Ok(());
        };

        self.data.tasks[index].completed = !self.data.tasks[index].completed;
        self.persist()?;
        self.ensure_selection();
        self.status = if self.data.tasks[index].completed {
            "marked as done".to_string()
        } else {
            "moved back to active".to_string()
        };
        Ok(())
    }

    fn delete_selected(&mut self) -> Result<()> {
        let Some(id) = self.selected_id else {
            self.status = "nothing selected".to_string();
            return Ok(());
        };
        let Some(index) = self.task_index(id) else {
            self.status = "task not found".to_string();
            return Ok(());
        };

        self.data.tasks.remove(index);
        self.ensure_selection();
        self.persist()?;
        self.status = "task removed".to_string();
        Ok(())
    }

    fn purge_completed(&mut self) -> Result<()> {
        let before = self.data.tasks.len();
        self.data.tasks.retain(|task| !task.completed);
        let removed = before.saturating_sub(self.data.tasks.len());
        self.ensure_selection();
        self.persist()?;
        self.status = if removed == 0 {
            "nothing to clean".to_string()
        } else {
            format!("removed {removed} completed task(s)")
        };
        Ok(())
    }

    fn clear_all_data(&mut self) -> Result<()> {
        let path = storage_path();
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
        }

        if let Some(parent) = path.parent().filter(|parent| parent.exists()) {
            let mut entries = fs::read_dir(parent)
                .with_context(|| format!("failed to inspect {}", parent.display()))?;
            if entries.next().transpose()?.is_none() {
                fs::remove_dir(parent)
                    .with_context(|| format!("failed to remove {}", parent.display()))?;
            }
        }

        self.data = StoredState::default();
        self.filter = Filter::All;
        self.selected_id = None;
        self.draft = None;
        self.confirm_clear_data = false;
        self.status = "all local data removed".to_string();
        Ok(())
    }

    fn counts(&self) -> (usize, usize, usize) {
        let total = self.data.tasks.len();
        let done = self.data.tasks.iter().filter(|task| task.completed).count();
        let active = total.saturating_sub(done);
        (total, active, done)
    }

    fn persist(&self) -> Result<()> {
        let path = storage_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(&self.data).context("failed to serialize tasks")?;
        fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}

fn storage_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".todo_rust")
        .join("tasks.json")
}

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(12),
            Constraint::Length(4),
        ])
        .split(area);

    render_header(frame, layout[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(layout[1]);
    render_tasks_panel(frame, body[0], app);
    render_focus_panel(frame, body[1], app);
    render_footer(frame, layout[2], app);

    if app.draft.is_some() {
        render_draft_modal(frame, app);
    } else if app.confirm_clear_data {
        render_clear_data_modal(frame);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let (total, active, done) = app.counts();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL));

    let title = Line::from(vec![
        Span::styled(" todo_rust ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled("beautiful console flow", Style::default().fg(MUTED)),
    ]);
    let stats = Line::from(vec![
        pill("Total", total, ACCENT),
        Span::raw("  "),
        pill("Active", active, ACCENT_ALT),
        Span::raw("  "),
        pill("Done", done, SUCCESS),
        Span::raw("  "),
        Span::styled(
            format!("Filter {}", app.filter.title()),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
    ]);

    let paragraph = Paragraph::new(vec![title, Line::default(), stats])
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn render_tasks_panel(frame: &mut Frame, area: Rect, app: &App) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_ALT))
        .title(Span::styled(
            " Queue ",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(outer, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(inner);

    let tabs = Tabs::new(["All", "Active", "Done"])
        .select(app.filter.tab_index())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL)),
        )
        .style(Style::default().fg(MUTED))
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider(Span::raw(" "));
    frame.render_widget(tabs, layout[0]);

    let visible_ids = app.visible_ids();
    if visible_ids.is_empty() {
        let empty = Paragraph::new(vec![
            Line::styled("No tasks in this view", Style::default().fg(TEXT)),
            Line::default(),
            Line::styled(
                "Press 'a' to add a new task and start filling the list.",
                Style::default().fg(MUTED),
            ),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL)),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        frame.render_widget(empty, layout[1]);
        return;
    }

    let items: Vec<ListItem> = visible_ids
        .iter()
        .filter_map(|id| app.data.tasks.iter().find(|task| task.id == *id))
        .map(|task| {
            let marker = if task.completed { "●" } else { "○" };
            let marker_color = if task.completed { SUCCESS } else { ACCENT_ALT };
            let title_style = if task.completed {
                Style::default()
                    .fg(MUTED)
                    .add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default().fg(TEXT)
            };

            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(marker_color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(task.title.clone(), title_style),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(app.selected_index_in_visible());

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(39, 52, 68))
                .fg(TEXT)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    frame.render_stateful_widget(list, layout[1], &mut state);
}

fn render_focus_panel(frame: &mut Frame, area: Rect, app: &App) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_ALT))
        .title(Span::styled(
            " Focus ",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(outer, area);

    let inner = area.inner(Margin {
        vertical: 1,
        horizontal: 1,
    });
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Min(7),
        ])
        .split(inner);

    let focus_text = if let Some(task) = app.selected_task() {
        let state_style = if task.completed {
            Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        };

        vec![
            Line::from(vec![
                Span::styled(
                    if task.completed { "Completed" } else { "In progress" },
                    state_style,
                ),
                Span::raw("  "),
                Span::styled(format!("#{}", task.id), Style::default().fg(MUTED)),
            ]),
            Line::default(),
            Line::styled(
                task.title.clone(),
                Style::default()
                    .fg(TEXT)
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
            Line::default(),
            Line::styled(
                if task.completed {
                    "Press Enter to reopen this task."
                } else {
                    "Press Enter to mark this task as done."
                },
                Style::default().fg(MUTED),
            ),
        ]
    } else {
        vec![
            Line::styled("Nothing selected", Style::default().fg(TEXT)),
            Line::default(),
            Line::styled(
                "Add your first task with 'a' or switch the filter to another view.",
                Style::default().fg(MUTED),
            ),
        ]
    };

    let focus = Paragraph::new(focus_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(focus, layout[0]);

    let (total, active, done) = app.counts();
    let progress = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64
    };
    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL))
                .title(Span::styled(
                    " Progress ",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
        )
        .gauge_style(Style::default().fg(ACCENT_ALT).bg(Color::Rgb(33, 43, 56)))
        .ratio(progress)
        .label(format!("{done}/{total} done"));
    frame.render_widget(gauge, layout[1]);

    let guide = Paragraph::new(vec![
        Line::styled("Quick flow", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Line::default(),
        Line::styled("a new   e rename   d delete", Style::default().fg(TEXT)),
        Line::styled("Space toggle   x clear done", Style::default().fg(TEXT)),
        Line::styled("Shift+X wipe data", Style::default().fg(TEXT)),
        Line::styled("1 2 3 filter   q quit", Style::default().fg(TEXT)),
        Line::default(),
        Line::styled(
            format!("{} active tasks waiting", active),
            Style::default().fg(MUTED),
        ),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(PANEL)),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(guide, layout[2]);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let mode_label = if let Some(draft) = &app.draft {
        match draft.mode {
            DraftMode::Add => "INPUT: NEW TASK",
            DraftMode::Edit(_) => "INPUT: RENAME TASK",
        }
    } else {
        "NORMAL"
    };

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(mode_label, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(&app.status, Style::default().fg(TEXT)),
        ]),
        Line::styled(
            "j/k navigate  enter toggle  a add  e edit  x done  Shift+X reset  q quit",
            Style::default().fg(MUTED),
        ),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(PANEL)),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(footer, area);
}

fn render_draft_modal(frame: &mut Frame, app: &App) {
    let Some(draft) = &app.draft else {
        return;
    };

    let popup = centered_rect(70, 8, frame.area());
    frame.render_widget(Clear, popup);

    let title = match draft.mode {
        DraftMode::Add => " New Task ",
        DraftMode::Edit(_) => " Rename Task ",
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL))
        .title(Span::styled(
            title,
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(block, popup);

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(3), Constraint::Length(1)])
        .split(inner);

    frame.render_widget(
        Paragraph::new("Type the task title. Enter saves, Esc cancels.")
            .style(Style::default().fg(MUTED)),
        layout[0],
    );

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_ALT));
    let input = Paragraph::new(draft.value.as_str())
        .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
        .block(input_block);
    frame.render_widget(input, layout[1]);

    let input_width = layout[1].width.saturating_sub(3);
    let cursor_offset = draft.value.chars().count().min(input_width as usize) as u16;
    frame.set_cursor_position((layout[1].x + 1 + cursor_offset, layout[1].y + 1));

    frame.render_widget(
        Paragraph::new("Tip: Ctrl+U clears the current input.")
            .style(Style::default().fg(MUTED)),
        layout[2],
    );
}

fn render_clear_data_modal(frame: &mut Frame) {
    let popup = centered_rect(72, 8, frame.area());
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL))
        .title(Span::styled(
            " Delete Local Data ",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(block, popup);

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let content = Paragraph::new(vec![
        Line::styled(
            "This will remove all tasks and delete ~/.todo_rust/tasks.json.",
            Style::default().fg(TEXT),
        ),
        Line::default(),
        Line::styled("Press Y to confirm or Esc to cancel.", Style::default().fg(MUTED)),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(content, inner);
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let width = area
        .width
        .saturating_mul(width_percent)
        .saturating_div(100)
        .clamp(24, area.width.max(24));
    let height = height.clamp(3, area.height.max(3));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn pill(label: &str, value: usize, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} {value} "),
        Style::default()
            .fg(BG)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}
