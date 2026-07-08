use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;
use rusqlite::Connection;
use tokio::sync::mpsc;
use tokio::task;

use crate::db;
use crate::fswork::{self, OutputOptions};
use crate::transfer;
use crate::util::human_size;

mod theme {
    use ratatui::style::{Color, Modifier, Style};

    // Lospec500: https://lospec.com/palette-list/lospec500
    pub const BG: Color = Color::Rgb(0x10, 0x12, 0x1c);
    pub const PANEL: Color = Color::Rgb(0x2c, 0x1e, 0x31);
    pub const PANEL_DARK: Color = Color::Rgb(0x1e, 0x40, 0x44);
    pub const BORDER: Color = Color::Rgb(0x5e, 0x5b, 0x8c);
    pub const BORDER_ACTIVE: Color = Color::Rgb(0x36, 0xc5, 0xf4);
    pub const TEXT: Color = Color::Rgb(0xf6, 0xe8, 0xe0);
    pub const MUTED: Color = Color::Rgb(0xb0, 0xa7, 0xb8);
    pub const ACCENT: Color = Color::Rgb(0xf3, 0xa8, 0x33);
    pub const GREEN: Color = Color::Rgb(0x5a, 0xb5, 0x52);
    pub const LIME: Color = Color::Rgb(0x9d, 0xe6, 0x4e);
    pub const CYAN: Color = Color::Rgb(0x6d, 0xea, 0xd6);
    pub const BLUE: Color = Color::Rgb(0x33, 0x88, 0xde);
    pub const PINK: Color = Color::Rgb(0xc8, 0x78, 0xaf);
    pub const RED: Color = Color::Rgb(0xec, 0x27, 0x3f);
    pub const ORANGE: Color = Color::Rgb(0xe9, 0x85, 0x37);
    pub const SELECT: Color = Color::Rgb(0x6b, 0x26, 0x43);

    pub fn base() -> Style {
        Style::default().fg(TEXT).bg(BG)
    }

    pub fn panel() -> Style {
        Style::default().fg(TEXT).bg(PANEL)
    }

    pub fn panel_dark() -> Style {
        Style::default().fg(TEXT).bg(PANEL_DARK)
    }

    pub fn active_title() -> Style {
        Style::default()
            .fg(PINK)
            .bg(PANEL)
            .add_modifier(Modifier::BOLD)
    }

    pub fn inactive_title() -> Style {
        Style::default()
            .fg(MUTED)
            .bg(PANEL)
            .add_modifier(Modifier::BOLD)
    }

    pub fn header() -> Style {
        Style::default()
            .fg(CYAN)
            .bg(PANEL)
            .add_modifier(Modifier::BOLD)
    }

    pub fn selected() -> Style {
        Style::default()
            .fg(Color::White)
            .bg(SELECT)
            .add_modifier(Modifier::BOLD)
    }

    pub fn marked() -> Style {
        Style::default().fg(LIME).bg(PANEL)
    }

    pub fn muted() -> Style {
        Style::default().fg(MUTED).bg(PANEL)
    }

    pub fn ok() -> Style {
        Style::default().fg(GREEN).bg(PANEL)
    }

    pub fn warn() -> Style {
        Style::default().fg(ORANGE).bg(PANEL)
    }

    pub fn error() -> Style {
        Style::default()
            .fg(RED)
            .bg(PANEL)
            .add_modifier(Modifier::BOLD)
    }
}

#[derive(Debug, Default)]
struct AppState {
    focus: FocusPane,
    file_view: FileView,
    selected_root: usize,
    file_offset: usize,
    plan_offset: usize,
    event_offset: usize,
    status: String,
    transfer_source_root_id: Option<String>,
    transfer_run_plan_id: Option<String>,
    last_plan: Option<PlanSnapshot>,
}

#[derive(Debug, Clone)]
struct PlanSnapshot {
    plan_id: String,
    source_root_id: String,
    status: String,
    source_name: String,
    dest_name: String,
    summary: Vec<db::TransferPlanActionSummary>,
    entries: Vec<db::TransferPlanEntryRow>,
}

#[derive(Debug)]
enum TuiMessage {
    Status(String),
    TransferFinished { plan_id: String, status: String },
}

struct InfoBarData<'a> {
    root: Option<&'a db::RootRow>,
    file: Option<&'a db::FileRow>,
    selection: Option<&'a db::SelectionSummary>,
    event: Option<&'a db::JobEventRow>,
    root_count: usize,
}

struct DetailData<'a> {
    root: Option<&'a db::RootRow>,
    summary: Option<&'a db::RootSummary>,
    selection: Option<&'a db::SelectionSummary>,
    file: Option<&'a db::FileRow>,
    selected_paths: &'a BTreeSet<String>,
    plan: Option<&'a PlanSnapshot>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FocusPane {
    #[default]
    Roots,
    Files,
    Plan,
    Events,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FileView {
    #[default]
    Basic,
    Meta,
    Hash,
    All,
}

impl FileView {
    fn next(self) -> Self {
        match self {
            Self::Basic => Self::Meta,
            Self::Meta => Self::Hash,
            Self::Hash => Self::All,
            Self::All => Self::Basic,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Meta => "meta",
            Self::Hash => "hash",
            Self::All => "all",
        }
    }
}

impl FocusPane {
    fn next(self) -> Self {
        match self {
            Self::Roots => Self::Files,
            Self::Files => Self::Plan,
            Self::Plan => Self::Events,
            Self::Events => Self::Roots,
        }
    }

    fn title(self, title: &'static str, active: Self) -> String {
        if self == active {
            format!("{title} *")
        } else {
            title.to_string()
        }
    }
}

fn panel_block(title: &'static str, active: bool) -> Block<'static> {
    let border = if active {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER
    };
    let title_style = if active {
        theme::active_title()
    } else {
        theme::inactive_title()
    };
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(theme::panel())
        .border_style(Style::default().fg(border).bg(theme::PANEL))
        .title_style(title_style)
}

fn focus_block(title: &'static str, pane: FocusPane, active: FocusPane) -> Block<'static> {
    let focused = pane == active;
    let border = if focused {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER
    };
    let title_style = if focused {
        theme::active_title()
    } else {
        theme::inactive_title()
    };
    Block::default()
        .title(pane.title(title, active))
        .borders(Borders::ALL)
        .style(theme::panel())
        .border_style(Style::default().fg(border).bg(theme::PANEL))
        .title_style(title_style)
}

fn file_status_style(status: &str) -> Style {
    match status {
        "present" => theme::panel(),
        "missing" => theme::warn(),
        "error" => theme::error(),
        _ => theme::muted(),
    }
}

fn job_status_style(status: &str) -> Style {
    match status {
        "completed" => theme::ok(),
        "created" | "running" | "canceling" => Style::default().fg(theme::BLUE).bg(theme::PANEL),
        "completed_with_errors" | "canceled" => theme::warn(),
        "failed" => theme::error(),
        _ => theme::muted(),
    }
}

pub async fn run_with_options(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(conn, db_path, &mut terminal, machine_label).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn run_loop(
    conn: &Connection,
    db_path: &Path,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    machine_label: Option<String>,
) -> anyhow::Result<()> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<TuiMessage>();
    let mut state = AppState {
        status: "ready".to_string(),
        ..AppState::default()
    };
    loop {
        while let Ok(message) = job_rx.try_recv() {
            match message {
                TuiMessage::Status(message) => state.status = message,
                TuiMessage::TransferFinished { plan_id, status } => {
                    if state.transfer_run_plan_id.as_deref() == Some(plan_id.as_str()) {
                        state.transfer_run_plan_id = None;
                    }
                    refresh_last_plan(conn, &mut state, &plan_id)?;
                    state.status = status;
                }
            }
        }
        let roots = db::roots(conn)?;
        normalize_selection(&mut state, roots.len());
        let selected = roots.get(state.selected_root);
        let files = match selected {
            Some(root) => db::recent_files_for_root(conn, &root.id, 500)?,
            None => Vec::new(),
        };
        let event_root_id = state.last_plan.as_ref().and_then(|plan| {
            (state.focus == FocusPane::Plan).then_some(plan.source_root_id.as_str())
        });
        let events = match event_root_id.or_else(|| selected.map(|root| root.id.as_str())) {
            Some(root_id) => db::recent_jobs_and_events_for_root(conn, root_id, 300)?,
            None => db::recent_jobs_and_events(conn, 100)?,
        };
        let summary = match selected {
            Some(root) => Some(db::root_summary(conn, &root.id)?),
            None => None,
        };
        let selection_summary = match selected {
            Some(root) => Some(db::selection_summary_for_root(conn, &root.id)?),
            None => None,
        };
        let selected_paths = match selected {
            Some(root) => db::selected_paths_for_root(conn, &root.id)?,
            None => BTreeSet::new(),
        };

        terminal.draw(|frame| {
            let area = frame.size();
            frame.render_widget(Block::default().style(theme::base()), area);
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(11),
                    Constraint::Length(3),
                    Constraint::Length(5),
                ])
                .split(area);
            let middle = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
                .split(vertical[1]);
            let lower = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(vertical[2]);

            render_header(frame, vertical[0]);
            render_roots(frame, middle[0], &roots, &state);
            render_files(frame, middle[1], &files, &selected_paths, &state);
            render_detail_panel(
                frame,
                lower[0],
                DetailData {
                    root: selected,
                    summary: summary.as_ref(),
                    selection: selection_summary.as_ref(),
                    file: files.get(state.file_offset),
                    selected_paths: &selected_paths,
                    plan: state.last_plan.as_ref(),
                },
            );
            render_plan_review(frame, lower[1], state.last_plan.as_ref(), &state);
            render_info_bar(
                frame,
                vertical[3],
                InfoBarData {
                    root: selected,
                    file: files.get(state.file_offset),
                    selection: selection_summary.as_ref(),
                    event: events.get(state.event_offset),
                    root_count: roots.len(),
                },
                &state,
            );
            render_events(frame, vertical[4], &events, &state);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => state.focus = state.focus.next(),
                    KeyCode::Char('v') => {
                        state.file_view = state.file_view.next();
                        state.status = format!("file fields: {}", state.file_view.label());
                    }
                    KeyCode::Down => {
                        let plan_count = state
                            .last_plan
                            .as_ref()
                            .map(|plan| plan.entries.len())
                            .unwrap_or(0);
                        move_down(
                            &mut state,
                            roots.len(),
                            files.len(),
                            plan_count,
                            events.len(),
                        );
                    }
                    KeyCode::Up => move_up(&mut state),
                    KeyCode::Char('s') => {
                        queue_selected_root(
                            conn,
                            db_path,
                            roots.get(state.selected_root),
                            "scan",
                            machine_label.as_deref(),
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('h') => {
                        queue_selected_root(
                            conn,
                            db_path,
                            roots.get(state.selected_root),
                            "hash",
                            machine_label.as_deref(),
                            job_tx.clone(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('c') => {
                        request_selected_cancel(conn, events.get(state.event_offset), &mut state)?;
                    }
                    KeyCode::Char('t') => {
                        start_transfer_plan_selection(roots.get(state.selected_root), &mut state);
                    }
                    KeyCode::Char('p') => {
                        load_latest_transfer_plan(
                            conn,
                            roots.get(state.selected_root),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('r') => {
                        run_current_transfer_plan(db_path, job_tx.clone(), &mut state);
                    }
                    KeyCode::Char('a') => {
                        decide_current_plan_entry(
                            conn,
                            &mut state,
                            "copy",
                            "review accepted for copy",
                        )?;
                    }
                    KeyCode::Char('d') => {
                        decide_current_plan_entry(
                            conn,
                            &mut state,
                            "skip",
                            "review dropped by user",
                        )?;
                    }
                    KeyCode::Enter => {
                        create_transfer_plan_from_selection(conn, &roots, &mut state)?;
                    }
                    KeyCode::Esc => {
                        cancel_transfer_plan_selection(&mut state);
                    }
                    KeyCode::Char(' ') => {
                        toggle_selected_file_mark(
                            conn,
                            selected,
                            files.get(state.file_offset),
                            &mut state,
                        )?;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "Gremlin",
            Style::default()
                .fg(theme::ACCENT)
                .bg(theme::PANEL)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  q quit | Tab panes | arrows move | Space mark | s scan | h hash | c cancel | t plan | Enter | p load | r run | a accept | d drop",
            theme::muted(),
        ),
    ]))
    .style(theme::panel())
    .block(panel_block("Lospec500", false));
    frame.render_widget(header, area);
}

fn render_roots(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    roots: &[db::RootRow],
    state: &AppState,
) {
    let items = if roots.is_empty() {
        vec![ListItem::new(
            "No roots yet\nRun `gremlin /path` or `gremlin target add /path`",
        )]
    } else {
        let mut rows = vec![ListItem::new(root_header()).style(theme::header())];
        rows.extend(roots.iter().enumerate().map(|(idx, root)| {
            let marker = if idx == state.selected_root {
                "> "
            } else {
                "  "
            };
            let transfer_marker = if state.transfer_source_root_id.as_deref() == Some(&root.id) {
                "S"
            } else {
                " "
            };
            let style = if idx == state.selected_root {
                theme::selected()
            } else if state.transfer_source_root_id.as_deref() == Some(&root.id) {
                theme::marked()
            } else {
                theme::panel()
            };
            ListItem::new(root_row(marker, transfer_marker, root)).style(style)
        }));
        rows
    };
    frame.render_widget(
        List::new(items).style(theme::panel()).block(focus_block(
            "Roots",
            FocusPane::Roots,
            state.focus,
        )),
        area,
    );
}

fn root_header() -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<6}",
        "", "T", "ROOT", "SIZE", "JOB"
    )
}

fn root_row(marker: &str, transfer_marker: &str, root: &db::RootRow) -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<6}",
        marker,
        transfer_marker,
        truncate(&root_display_name(root), 8),
        human_size(root.current_size_bytes as u64),
        truncate(&root_job_label(root), 6)
    )
}

fn root_job_label(root: &db::RootRow) -> String {
    match (
        root.latest_job_kind.as_deref(),
        root.latest_job_status.as_deref(),
        root.latest_job_phase.as_deref(),
    ) {
        (Some(kind), Some("running"), Some(phase)) => {
            format!("{}/{}", compact_job_kind(kind), compact_phase(phase))
        }
        (Some(kind), Some(status), _) => {
            format!("{}/{}", compact_job_kind(kind), compact_status(status))
        }
        (Some(kind), None, _) => kind.to_string(),
        _ => "-".to_string(),
    }
}

fn compact_job_kind(kind: &str) -> &str {
    match kind {
        "scan" => "s",
        "hash" => "h",
        "verify" => "v",
        other => other,
    }
}

fn compact_status(status: &str) -> &str {
    match status {
        "created" => "new",
        "running" => "run",
        "completed" => "done",
        "completed_with_errors" => "errs",
        "failed" => "fail",
        other => other,
    }
}

fn compact_phase(phase: &str) -> &str {
    match phase {
        "queued" => "new",
        "preparing" => "prep",
        "walking" => "walk",
        "processing" => "work",
        "finalizing" => "done",
        other => other,
    }
}

fn root_display_name(root: &db::RootRow) -> String {
    if let Some(label) = root
        .label
        .as_deref()
        .filter(|label| !label.is_empty() && *label != root.path)
    {
        return label.to_string();
    }
    root.path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(&root.path)
        .to_string()
}

fn display_name_from_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn render_detail_panel(frame: &mut ratatui::Frame<'_>, area: Rect, data: DetailData<'_>) {
    let root_lines = match (data.root, data.summary) {
        (Some(root), Some(summary)) => format!(
            "Root: {}\nPath: {}\nFiles: {} | Hashed: {} | Current: {} | Marked: {} ({})\nMachine: {} | Set: {}",
            root_display_name(root),
            root.path,
            summary.file_count,
            summary.content_count,
            human_size(root.current_size_bytes as u64),
            data.selection.map(|value| value.marked_count).unwrap_or(0),
            human_size(data.selection.map(|value| value.marked_bytes).unwrap_or(0) as u64),
            short_id(&root.machine_id),
            data.selection
                .map(|value| short_id(&value.set_id))
                .unwrap_or("-")
        ),
        _ => "Root: -\nPath: -\nMachine: - | Files: - | Hashed: - | Current size: -".to_string(),
    };
    let file_lines = if let Some(file) = data.file {
        format!(
            "File: {}\nSize: {} ({} bytes) | Status: {} | Marked: {}\nModified: {} | Content: {} | Metadata: not extracted yet",
            file.relative_path,
            human_size(file.size_bytes as u64),
            file.size_bytes,
            file.status,
            if data.selected_paths.contains(&file.relative_path) {
                "yes"
            } else {
                "no"
            },
            file.modified_at.as_deref().unwrap_or("-"),
            file.content_id.as_deref().map(short_id).unwrap_or("stat-only")
        )
    } else {
        "File: -\nSize: - | Status: - | Modified: -\nContent: - | Metadata: not extracted yet"
            .to_string()
    };
    let plan_lines = if let Some(plan) = data.plan {
        format!(
            "Plan: {} | {} | {} -> {}\n{}",
            short_id(&plan.plan_id),
            plan.status,
            truncate(&plan.source_name, 18),
            truncate(&plan.dest_name, 18),
            plan_summary_line(&plan.summary)
        )
    } else {
        "Plan: -\nPress t on a source root, choose a destination root, Enter plans marked files"
            .to_string()
    };
    let text = format!("{root_lines}\n{file_lines}\n{plan_lines}");
    frame.render_widget(
        Paragraph::new(text)
            .style(theme::panel_dark())
            .block(panel_block("Details", false)),
        area,
    );
}

fn render_info_bar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    data: InfoBarData<'_>,
    state: &AppState,
) {
    let root_name = data.root.map(root_display_name);
    let root = root_name.as_deref().unwrap_or("-");
    let file = data
        .file
        .map(|file| file.relative_path.as_str())
        .unwrap_or("-");
    let event = data
        .event
        .map(event_summary)
        .unwrap_or_else(|| "event -".to_string());
    let plan_status = state
        .last_plan
        .as_ref()
        .map(|plan| {
            format!(
                "copy {} review {}",
                plan_copy_count(plan),
                plan_review_count(plan)
            )
        })
        .unwrap_or_else(|| "-".to_string());
    let text = format!(
        "focus {:?} | roots {} | marked {} | plan {} | root {} | file {} | {} | {}",
        state.focus,
        data.root_count,
        data.selection.map(|value| value.marked_count).unwrap_or(0),
        plan_status,
        truncate(root, 24),
        truncate(file, 20),
        truncate(&event, 24),
        state.status
    );
    frame.render_widget(
        Paragraph::new(text)
            .style(theme::panel())
            .block(panel_block("Info", true)),
        area,
    );
}

fn render_files(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    files: &[db::FileRow],
    selected_paths: &BTreeSet<String>,
    state: &AppState,
) {
    let visible = files.iter().enumerate().skip(state.file_offset);
    let items = if files.is_empty() {
        vec![ListItem::new("No indexed files for this root")]
    } else {
        let mut rows = vec![ListItem::new(file_header(state.file_view)).style(theme::header())];
        rows.extend(visible.map(|(idx, file)| {
            let marker = if idx == state.file_offset { "> " } else { "  " };
            let selected = selected_paths.contains(&file.relative_path);
            let style = if idx == state.file_offset {
                theme::selected()
            } else if selected {
                theme::marked()
            } else {
                file_status_style(&file.status)
            };
            ListItem::new(file_row(marker, selected, file, state.file_view)).style(style)
        }));
        rows
    };
    frame.render_widget(
        List::new(items).style(theme::panel()).block(focus_block(
            "Files",
            FocusPane::Files,
            state.focus,
        )),
        area,
    );
}

fn file_header(view: FileView) -> String {
    match view {
        FileView::Basic => format!(
            "{:<2} {:<1} {:<24} {:>9} {:<8}",
            "", "M", "PATH", "SIZE", "STATE"
        ),
        FileView::Meta => format!(
            "{:<2} {:<1} {:<18} {:>9} {:<18}",
            "", "M", "PATH", "SIZE", "MODIFIED"
        ),
        FileView::Hash => format!("{:<2} {:<1} {:<26} {:<18}", "", "M", "PATH", "CONTENT"),
        FileView::All => format!(
            "{:<2} {:<1} {:<14} {:>8} {:<6} {:<8} {:<10}",
            "", "M", "PATH", "SIZE", "STATE", "HASH", "MODIFIED"
        ),
    }
}

fn file_row(marker: &str, selected: bool, file: &db::FileRow, view: FileView) -> String {
    let hash = file.content_id.as_deref().map(short_id).unwrap_or("stat");
    let modified = file.modified_at.as_deref().unwrap_or("-");
    let marked = if selected { "*" } else { " " };
    match view {
        FileView::Basic => format!(
            "{:<2} {:<1} {:<24} {:>9} {:<8}",
            marker,
            marked,
            truncate(&file.relative_path, 24),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 8)
        ),
        FileView::Meta => format!(
            "{:<2} {:<1} {:<18} {:>9} {:<18}",
            marker,
            marked,
            truncate(&file.relative_path, 18),
            human_size(file.size_bytes as u64),
            truncate(modified, 18)
        ),
        FileView::Hash => format!(
            "{:<2} {:<1} {:<26} {:<18}",
            marker,
            marked,
            truncate(&file.relative_path, 26),
            truncate(hash, 18)
        ),
        FileView::All => format!(
            "{:<2} {:<1} {:<14} {:>8} {:<6} {:<8} {:<10}",
            marker,
            marked,
            truncate(&file.relative_path, 14),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 6),
            truncate(hash, 8),
            truncate(modified, 10)
        ),
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        value.to_string()
    } else if width <= 1 {
        "~".to_string()
    } else {
        let mut out = value.chars().take(width - 1).collect::<String>();
        out.push('~');
        out
    }
}

fn short_id(value: &str) -> &str {
    value.get(..value.len().min(18)).unwrap_or(value)
}

fn plan_summary_line(summary: &[db::TransferPlanActionSummary]) -> String {
    if summary.is_empty() {
        return "No plan entries".to_string();
    }
    summary
        .iter()
        .map(|row| {
            format!(
                "{} {} {}",
                row.action,
                row.files,
                human_size(row.bytes as u64)
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn plan_review_count(plan: &PlanSnapshot) -> usize {
    plan.entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.action.as_str(),
                "review" | "conflict" | "verify_needed"
            )
        })
        .count()
}

fn plan_copy_count(plan: &PlanSnapshot) -> usize {
    plan.entries
        .iter()
        .filter(|entry| entry.action == "copy")
        .count()
}

fn render_plan_review(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    plan: Option<&PlanSnapshot>,
    state: &AppState,
) {
    let items =
        if let Some(plan) = plan {
            let mut rows = vec![ListItem::new(plan_entry_header()).style(theme::header())];
            rows.extend(plan.entries.iter().enumerate().skip(state.plan_offset).map(
                |(idx, entry)| {
                    let marker = if idx == state.plan_offset { "> " } else { "  " };
                    let style = if idx == state.plan_offset {
                        theme::selected()
                    } else {
                        plan_action_style(&entry.action)
                    };
                    ListItem::new(plan_entry_row(marker, entry)).style(style)
                },
            ));
            rows
        } else {
            vec![ListItem::new("No transfer plan yet")]
        };
    frame.render_widget(
        List::new(items).style(theme::panel()).block(focus_block(
            "Plan",
            FocusPane::Plan,
            state.focus,
        )),
        area,
    );
}

fn plan_entry_header() -> String {
    format!(
        "{:<2} {:<10} {:<18} {:>9} {}",
        "", "ACTION", "PATH", "SIZE", "WHY"
    )
}

fn plan_entry_row(marker: &str, entry: &db::TransferPlanEntryRow) -> String {
    format!(
        "{:<2} {:<10} {:<18} {:>9} {}",
        marker,
        truncate(&entry.action, 10),
        truncate(&entry.relative_path, 18),
        human_size(entry.size_bytes),
        truncate(&plan_entry_hint(entry), 26)
    )
}

fn plan_entry_hint(entry: &db::TransferPlanEntryRow) -> String {
    if entry.action == "review" {
        let payload: serde_json::Value =
            serde_json::from_str(&entry.metadata_json).unwrap_or(serde_json::Value::Null);
        let hash_count = payload
            .get("hash_collisions")
            .and_then(|value| value.as_array())
            .map(|value| value.len())
            .unwrap_or(0);
        let name_count = payload
            .get("filename_size_date_collisions")
            .and_then(|value| value.as_array())
            .map(|value| value.len())
            .unwrap_or(0);
        return format!("review hash={hash_count} name={name_count}");
    }
    entry.reason.clone()
}

fn plan_action_style(action: &str) -> Style {
    match action {
        "copy" => theme::ok(),
        "review" | "verify_needed" => theme::warn(),
        "conflict" | "unavailable" => theme::error(),
        "skip" => theme::muted(),
        _ => theme::panel(),
    }
}

fn render_events(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    events: &[db::JobEventRow],
    state: &AppState,
) {
    let visible = events.iter().enumerate().skip(state.event_offset);
    let items = if events.is_empty() {
        vec![ListItem::new("No jobs or events for this root")]
    } else {
        let mut rows = vec![ListItem::new(event_header()).style(theme::header())];
        rows.extend(visible.map(|(idx, row)| {
            let marker = if idx == state.event_offset {
                "> "
            } else {
                "  "
            };
            let style = if idx == state.event_offset {
                theme::selected()
            } else {
                job_status_style(&event_status(row))
            };
            ListItem::new(event_row(marker, row)).style(style)
        }));
        rows
    };
    frame.render_widget(
        List::new(items).style(theme::panel()).block(focus_block(
            "Jobs",
            FocusPane::Events,
            state.focus,
        )),
        area,
    );
}

fn event_header() -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:<24}",
        "", "JOB", "KIND", "STATUS", "PHASE", "PROGRESS"
    )
}

fn event_row(marker: &str, row: &db::JobEventRow) -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:<24}",
        marker,
        short_id(&row.job_id),
        truncate(&row.job_kind, 5),
        truncate(&event_status(row), 9),
        truncate(row.phase.as_deref().unwrap_or("-"), 10),
        truncate(&progress_count(row), 24)
    )
}

fn event_summary(row: &db::JobEventRow) -> String {
    format!(
        "{} {} #{} {} {} {}",
        row.job_kind,
        event_status(row),
        row.sequence,
        row.event_kind,
        progress_count(row),
        truncate(&row.payload_json, 28)
    )
}

fn event_status(row: &db::JobEventRow) -> String {
    if row.cancel_requested && matches!(row.status.as_str(), "created" | "running") {
        "canceling".to_string()
    } else {
        row.status.clone()
    }
}

fn progress_count(row: &db::JobEventRow) -> String {
    if let Some(progress) = byte_progress_summary(&row.payload_json) {
        return progress;
    }
    if row.files_skipped > 0 || row.errors > 0 {
        format!("{}/{}/{}", row.files_done, row.files_skipped, row.errors)
    } else {
        format!("{}/{}", row.files_done, row.files_seen)
    }
}

fn byte_progress_summary(payload_json: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    if payload.get("type")?.as_str()? != "job_progress" {
        return None;
    }
    let done = payload.get("bytes_done")?.as_u64()?;
    let total = payload.get("bytes_total")?.as_u64()?;
    if total == 0 {
        return None;
    }
    let rate = payload
        .get("bytes_per_second")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    Some(format!(
        "{} {:>3}% {:>7}/s",
        progress_bar(done, total, 8),
        ((done.saturating_mul(100)) / total).min(100),
        transfer_rate(rate)
    ))
}

fn progress_bar(done: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 {
        0
    } else {
        ((done.min(total) as usize) * width) / total as usize
    };
    format!(
        "[{}{}]",
        "#".repeat(filled),
        "-".repeat(width.saturating_sub(filled))
    )
}

fn transfer_rate(bytes_per_second: f64) -> String {
    if bytes_per_second >= 1024.0 * 1024.0 {
        format!("{:.1} MiB", bytes_per_second / 1024.0 / 1024.0)
    } else if bytes_per_second >= 1024.0 {
        format!("{:.1} KiB", bytes_per_second / 1024.0)
    } else {
        format!("{:.0} B", bytes_per_second)
    }
}

fn normalize_selection(state: &mut AppState, root_count: usize) {
    if root_count == 0 {
        state.selected_root = 0;
    } else if state.selected_root >= root_count {
        state.selected_root = root_count - 1;
    }
}

fn move_down(
    state: &mut AppState,
    root_count: usize,
    file_count: usize,
    plan_count: usize,
    event_count: usize,
) {
    match state.focus {
        FocusPane::Roots => {
            if state.selected_root + 1 < root_count {
                state.selected_root += 1;
                state.file_offset = 0;
                state.event_offset = 0;
            }
        }
        FocusPane::Files => {
            if state.file_offset + 1 < file_count {
                state.file_offset += 1;
            }
        }
        FocusPane::Plan => {
            if state.plan_offset + 1 < plan_count {
                state.plan_offset += 1;
            }
        }
        FocusPane::Events => {
            if state.event_offset + 1 < event_count {
                state.event_offset += 1;
            }
        }
    }
}

fn move_up(state: &mut AppState) {
    match state.focus {
        FocusPane::Roots => {
            if state.selected_root > 0 {
                state.selected_root -= 1;
                state.file_offset = 0;
                state.event_offset = 0;
            }
        }
        FocusPane::Files => {
            state.file_offset = state.file_offset.saturating_sub(1);
        }
        FocusPane::Plan => {
            state.plan_offset = state.plan_offset.saturating_sub(1);
        }
        FocusPane::Events => {
            state.event_offset = state.event_offset.saturating_sub(1);
        }
    }
}

fn queue_selected_root(
    conn: &Connection,
    db_path: &Path,
    root: Option<&db::RootRow>,
    kind: &str,
    machine_label: Option<&str>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        state.status = "No root selected. Add one with `gremlin /path`.".to_string();
        return Ok(());
    };
    let job_id = db::queue_file_job(conn, kind, std::path::Path::new(&root.path), machine_label)?;
    state.status = format!("started {kind} job {job_id}");
    spawn_job_runner(
        db_path.to_path_buf(),
        job_id,
        kind.to_string(),
        machine_label.map(str::to_string),
        job_tx,
    );
    Ok(())
}

fn request_selected_cancel(
    conn: &Connection,
    selected_event: Option<&db::JobEventRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(event) = selected_event else {
        state.status = "No job selected to cancel".to_string();
        return Ok(());
    };
    if db::request_job_cancel(conn, &event.job_id)? {
        let envelope = crate::events::EventEnvelope {
            event_kind: crate::events::EventKind::JobCancelRequested,
            job_id: Some(event.job_id.clone()),
            sequence: None,
            created_at: crate::util::now_rfc3339(),
            payload: crate::events::EventPayload::Job {
                kind: event.job_kind.clone(),
                path: event.current_path.clone(),
                message: Some("cancel requested from tui".to_string()),
                files_seen: Some(event.files_seen as u64),
                errors: Some(event.errors as u64),
            },
        };
        db::persist_event(conn, &envelope)?;
        state.status = format!("cancel requested for {}", event.job_id);
    } else {
        state.status = format!("job {} is not cancelable", event.job_id);
    }
    Ok(())
}

fn toggle_selected_file_mark(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    selected_file: Option<&db::FileRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.status = "No root selected".to_string();
        return Ok(());
    };
    let Some(file) = selected_file else {
        state.status = "No file selected".to_string();
        return Ok(());
    };
    let marked = db::toggle_selection_entry(conn, &root.id, &file.relative_path)?;
    state.status = if marked {
        format!("marked {}", file.relative_path)
    } else {
        format!("unmarked {}", file.relative_path)
    };
    Ok(())
}

fn start_transfer_plan_selection(root: Option<&db::RootRow>, state: &mut AppState) {
    let Some(root) = root else {
        state.status = "No source root selected".to_string();
        return;
    };
    state.transfer_source_root_id = Some(root.id.clone());
    state.focus = FocusPane::Roots;
    state.status = format!(
        "transfer source: {}; choose destination root and press Enter",
        root_display_name(root)
    );
}

fn cancel_transfer_plan_selection(state: &mut AppState) {
    if state.transfer_source_root_id.take().is_some() {
        state.status = "transfer planning canceled".to_string();
    }
}

fn create_transfer_plan_from_selection(
    conn: &Connection,
    roots: &[db::RootRow],
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(source_root_id) = state.transfer_source_root_id.clone() else {
        return Ok(());
    };
    let Some(source) = roots.iter().find(|root| root.id == source_root_id) else {
        state.transfer_source_root_id = None;
        state.status = "transfer source root is no longer visible".to_string();
        return Ok(());
    };
    let Some(dest) = roots.get(state.selected_root) else {
        state.status = "No destination root selected".to_string();
        return Ok(());
    };
    match transfer::plan_selected_files(conn, source, dest) {
        Ok(result) => {
            let summary = result.summary.clone();
            let entries = db::transfer_plan_entries(conn, &result.plan_id)?;
            state.last_plan = Some(PlanSnapshot {
                plan_id: result.plan_id.clone(),
                source_root_id: source.id.clone(),
                status: "planned".to_string(),
                source_name: root_display_name(source),
                dest_name: root_display_name(dest),
                summary,
                entries,
            });
            state.transfer_source_root_id = None;
            state.plan_offset = 0;
            state.focus = FocusPane::Plan;
            state.status = format!("planned transfer {}", result.plan_id);
        }
        Err(err) => {
            state.status = format!("transfer plan failed: {err}");
        }
    }
    Ok(())
}

fn load_latest_transfer_plan(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.status = "No root selected".to_string();
        return Ok(());
    };
    let Some(plan) = db::recent_transfer_plans(conn, 100)?
        .into_iter()
        .find(|plan| plan.source_root_id == root.id || plan.dest_root_id == root.id)
    else {
        state.status = format!("No transfer plans found for {}", root_display_name(root));
        return Ok(());
    };
    let summary = db::transfer_plan_action_summary(conn, &plan.id)?;
    let entries = db::transfer_plan_entries(conn, &plan.id)?;
    state.last_plan = Some(PlanSnapshot {
        plan_id: plan.id.clone(),
        source_root_id: plan.source_root_id.clone(),
        status: plan.status.clone(),
        source_name: display_name_from_path(&plan.source_path),
        dest_name: display_name_from_path(&plan.dest_path),
        summary,
        entries,
    });
    state.plan_offset = 0;
    state.focus = FocusPane::Plan;
    state.status = format!("loaded transfer plan {}", short_id(&plan.id));
    Ok(())
}

fn run_current_transfer_plan(
    db_path: &Path,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) {
    if let Some(plan_id) = state.transfer_run_plan_id.as_deref() {
        state.status = format!("transfer plan {} is already running", short_id(plan_id));
        return;
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to run".to_string();
        return;
    };
    let copy_entries = plan_copy_count(plan);
    if copy_entries == 0 {
        state.status = "Plan has no copy entries; review conflicts first".to_string();
        return;
    }
    let plan_id = plan.plan_id.clone();
    state.transfer_run_plan_id = Some(plan_id.clone());
    state.focus = FocusPane::Plan;
    state.status = format!(
        "running transfer {} ({} copy entries)",
        short_id(&plan_id),
        copy_entries
    );
    spawn_transfer_runner(db_path.to_path_buf(), plan_id, job_tx);
}

fn decide_current_plan_entry(
    conn: &Connection,
    state: &mut AppState,
    action: &str,
    reason: &str,
) -> anyhow::Result<()> {
    if state.focus != FocusPane::Plan {
        state.status = "Move focus to Plan before deciding entries".to_string();
        return Ok(());
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to decide".to_string();
        return Ok(());
    };
    let Some(entry) = plan.entries.get(state.plan_offset) else {
        state.status = "No plan entry selected".to_string();
        return Ok(());
    };
    if entry.action != "review" {
        state.status = format!(
            "{} is {}; only review entries can be decided",
            entry.relative_path, entry.action
        );
        return Ok(());
    }
    let plan_id = plan.plan_id.clone();
    let relative_path = entry.relative_path.clone();
    let changed = db::decide_review_transfer_plan_entry(
        conn,
        &plan_id,
        &relative_path,
        action,
        reason,
        serde_json::json!({
            "decision": action,
            "decided_at": crate::util::now_rfc3339(),
        }),
    )?;
    if !changed {
        state.status = format!("{} is no longer a review entry", relative_path);
        refresh_last_plan(conn, state, &plan_id)?;
        return Ok(());
    }
    refresh_last_plan(conn, state, &plan_id)?;
    state.status = format!("{} -> {}", relative_path, action);
    Ok(())
}

fn refresh_last_plan(conn: &Connection, state: &mut AppState, plan_id: &str) -> anyhow::Result<()> {
    if let Some(plan) = state
        .last_plan
        .as_mut()
        .filter(|plan| plan.plan_id == plan_id)
    {
        if let Some(row) = db::transfer_plan_by_id(conn, plan_id)? {
            plan.status = row.status;
        }
        plan.summary = db::transfer_plan_action_summary(conn, plan_id)?;
        plan.entries = db::transfer_plan_entries(conn, plan_id)?;
        if state.plan_offset >= plan.entries.len() {
            state.plan_offset = plan.entries.len().saturating_sub(1);
        }
    }
    Ok(())
}

fn spawn_job_runner(
    db_path: PathBuf,
    job_id: String,
    kind: String,
    machine_label: Option<String>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<()> {
            let conn = db::open_existing(&db_path)?;
            fswork::run_queued_job(
                &conn,
                &job_id,
                &db_path,
                machine_label.as_deref(),
                OutputOptions {
                    quiet: true,
                    ..OutputOptions::default()
                },
            )
        })();
        let message = match result {
            Ok(()) => format!("completed {kind} job {job_id}"),
            Err(err) => format!("failed {kind} job {job_id}: {err}"),
        };
        let _ = job_tx.send(TuiMessage::Status(message));
    });
}

fn spawn_transfer_runner(
    db_path: PathBuf,
    plan_id: String,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    task::spawn_blocking(move || {
        let result = (|| -> anyhow::Result<transfer::TransferRunResult> {
            let conn = db::open_existing(&db_path)?;
            transfer::run_transfer_plan(&conn, &plan_id, false)
        })();
        let status = match result {
            Ok(result) => format!(
                "completed transfer {}: copied {} ({}) skipped {} errors {}",
                short_id(&result.plan_id),
                result.copied,
                human_size(result.bytes_copied),
                result.skipped,
                result.errors
            ),
            Err(err) => format!("failed transfer {}: {err}", short_id(&plan_id)),
        };
        let _ = job_tx.send(TuiMessage::TransferFinished { plan_id, status });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_values() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }

    #[test]
    fn root_display_name_uses_basename_when_label_is_path() {
        let root = db::RootRow {
            id: "root_1".to_string(),
            machine_id: "machine_1".to_string(),
            path: "/tmp/archive/photos".to_string(),
            label: Some("/tmp/archive/photos".to_string()),
            current_size_bytes: 0,
            latest_job_kind: None,
            latest_job_status: None,
            latest_job_phase: None,
        };
        assert_eq!(root_display_name(&root), "photos");
    }

    #[test]
    fn formats_plan_summary_line() {
        let summary = vec![db::TransferPlanActionSummary {
            action: "copy".to_string(),
            files: 2,
            bytes: 2048,
        }];
        assert_eq!(plan_summary_line(&summary), "copy 2 2.00 KiB");
    }

    #[test]
    fn formats_byte_progress_summary() {
        let payload = serde_json::json!({
            "type": "job_progress",
            "bytes_done": 512,
            "bytes_total": 1024,
            "bytes_per_second": 1048576.0
        });
        assert_eq!(
            byte_progress_summary(&payload.to_string()).unwrap(),
            "[####----]  50% 1.0 MiB/s"
        );
    }

    #[test]
    fn formats_plan_review_hint_and_count() {
        let review = db::TransferPlanEntryRow {
            relative_path: "incoming/foo.png".to_string(),
            size_bytes: 10,
            source_content_id: Some("content_src".to_string()),
            dest_content_id: Some("content_dest".to_string()),
            action: "review".to_string(),
            reason: "collision".to_string(),
            metadata_json: serde_json::json!({
                "hash_collisions": [{"relative_path": "existing/foo.png"}],
                "filename_size_date_collisions": [{"relative_path": "other/foo.png"}]
            })
            .to_string(),
        };
        let copy = db::TransferPlanEntryRow {
            action: "copy".to_string(),
            reason: "destination path is not indexed".to_string(),
            ..review.clone()
        };
        let plan = PlanSnapshot {
            plan_id: "plan_1".to_string(),
            source_root_id: "source_root".to_string(),
            status: "planned".to_string(),
            source_name: "source".to_string(),
            dest_name: "dest".to_string(),
            summary: Vec::new(),
            entries: vec![review.clone(), copy],
        };

        assert_eq!(plan_entry_hint(&review), "review hash=1 name=1");
        assert_eq!(plan_review_count(&plan), 1);
        assert_eq!(plan_copy_count(&plan), 1);
    }
}
