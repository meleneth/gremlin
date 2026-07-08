use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
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
            .fg(ACCENT)
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

#[derive(Default)]
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
    retarget_draft: Option<RetargetDraft>,
    pending_delete_root_id: Option<String>,
    pending_import: Option<PendingTemporaryImport>,
    last_plan: Option<PlanSnapshot>,
    temporary_browse: Option<TemporaryBrowse>,
    root_browse_dirs: BTreeMap<String, String>,
}

pub type BrowseProvider =
    Arc<dyn Fn(&str) -> anyhow::Result<Vec<InitialBrowseEntry>> + Send + Sync + 'static>;
pub type ImportProvider =
    Arc<dyn Fn(ImportMode, &str) -> anyhow::Result<ImportResult> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    No,
    Fast,
    Hash,
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    pub mode: ImportMode,
    pub root_id: String,
    pub root_path: String,
    pub files_imported: u64,
}

#[derive(Clone)]
pub struct InitialBrowse {
    pub label: String,
    pub machine_id: String,
    pub root_path: String,
    pub current_path: String,
    pub entries: Vec<InitialBrowseEntry>,
    pub browse_provider: Option<BrowseProvider>,
    pub import_provider: Option<ImportProvider>,
}

#[derive(Debug, Clone)]
pub struct InitialBrowseEntry {
    pub kind: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
}

#[derive(Clone)]
struct TemporaryBrowse {
    label: String,
    machine_id: String,
    root_path: String,
    current_path: String,
    entries: Vec<InitialBrowseEntry>,
    browse_provider: Option<BrowseProvider>,
    import_provider: Option<ImportProvider>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
struct FileViewRow {
    relative_path: String,
    size_bytes: i64,
    modified_at: Option<String>,
    content_id: Option<String>,
    status: String,
    kind: FileKind,
}

impl From<InitialBrowse> for TemporaryBrowse {
    fn from(value: InitialBrowse) -> Self {
        Self {
            label: value.label,
            machine_id: value.machine_id,
            root_path: value.root_path,
            current_path: value.current_path,
            entries: value.entries,
            browse_provider: value.browse_provider,
            import_provider: value.import_provider,
        }
    }
}

impl From<&db::FileRow> for FileViewRow {
    fn from(value: &db::FileRow) -> Self {
        Self {
            relative_path: value.relative_path.clone(),
            size_bytes: value.size_bytes,
            modified_at: value.modified_at.clone(),
            content_id: value.content_id.clone(),
            status: value.status.clone(),
            kind: FileKind::File,
        }
    }
}

impl FileViewRow {
    fn from_cached_directory_entry(entry: &db::CachedDirectoryEntry) -> Self {
        let kind = if entry.kind == "dir" {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Self {
            relative_path: entry.relative_path.clone(),
            size_bytes: entry.size_bytes,
            modified_at: entry.modified_at.clone(),
            content_id: entry.content_id.clone(),
            status: entry.status.clone().unwrap_or_else(|| {
                if kind == FileKind::Directory {
                    format!("dir:{}", entry.file_count)
                } else {
                    "present".to_string()
                }
            }),
            kind,
        }
    }

    fn from_temporary_entry(entry: &InitialBrowseEntry) -> Self {
        let kind = if entry.kind == "dir" {
            FileKind::Directory
        } else {
            FileKind::File
        };
        Self {
            relative_path: entry.name.clone(),
            size_bytes: entry.size_bytes as i64,
            modified_at: entry.modified_at.clone(),
            content_id: None,
            status: if kind == FileKind::Directory {
                "dir".to_string()
            } else {
                "remote".to_string()
            },
            kind,
        }
    }
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
    TransferFinished {
        plan_id: String,
        status: String,
    },
    ImportFinished(String),
    TemporaryTransferSourceImported {
        root_id: String,
        selected_relative_path: Option<String>,
        mark_all: bool,
        status: String,
    },
}

struct InfoBarData<'a> {
    root_name: Option<String>,
    file: Option<&'a FileViewRow>,
    selection: Option<&'a db::SelectionSummary>,
    event: Option<&'a db::JobEventRow>,
    root_count: usize,
}

struct DetailData<'a> {
    root: Option<&'a db::RootRow>,
    temporary_browse: Option<&'a TemporaryBrowse>,
    persisted_browse_dir: Option<&'a str>,
    summary: Option<&'a db::RootSummary>,
    selection: Option<&'a db::SelectionSummary>,
    file: Option<&'a FileViewRow>,
    selected_paths: &'a BTreeSet<String>,
    plan: Option<&'a PlanSnapshot>,
    transfer_progress: Option<TransferProgressSnapshot>,
}

#[derive(Debug, Clone)]
struct TransferProgressSnapshot {
    current_path: String,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    file_bytes_done: u64,
    file_bytes_total: u64,
    bytes_per_second: f64,
    errors: u64,
}

#[derive(Debug, Clone)]
struct RetargetDraft {
    plan_id: String,
    relative_path: String,
    value: String,
}

#[derive(Debug, Clone)]
struct PendingTemporaryImport {
    remote_path: String,
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
    run_with_initial_browse(conn, db_path, machine_label, None).await
}

pub async fn run_with_initial_browse(
    conn: &Connection,
    db_path: &Path,
    machine_label: Option<String>,
    initial_browse: Option<InitialBrowse>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(conn, db_path, &mut terminal, machine_label, initial_browse).await;

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
    initial_browse: Option<InitialBrowse>,
) -> anyhow::Result<()> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<TuiMessage>();
    let mut state = AppState {
        status: "ready".to_string(),
        temporary_browse: initial_browse.map(TemporaryBrowse::from),
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
                TuiMessage::ImportFinished(status) => state.status = status,
                TuiMessage::TemporaryTransferSourceImported {
                    root_id,
                    selected_relative_path,
                    mark_all,
                    status,
                } => {
                    mark_imported_transfer_source(
                        conn,
                        &root_id,
                        selected_relative_path.as_deref(),
                        mark_all,
                    )?;
                    state.transfer_source_root_id = Some(root_id);
                    state.focus = FocusPane::Roots;
                    state.status = status;
                }
            }
        }
        let roots = db::roots(conn)?;
        let root_count = visible_root_count(&state, roots.len());
        normalize_selection(&mut state, root_count);
        let selected = selected_persisted_root(&roots, &state);
        let selected_temporary = selected_temporary_browse(&state);
        let files = match (selected, selected_temporary) {
            (Some(root), _) => db::cached_directory_entries(
                conn,
                &root.id,
                current_persisted_root_dir(&state, &root.id),
            )?
            .iter()
            .map(FileViewRow::from_cached_directory_entry)
            .collect(),
            (None, Some(browse)) => browse
                .entries
                .iter()
                .map(FileViewRow::from_temporary_entry)
                .collect(),
            (None, None) => Vec::new(),
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
        let transfer_progress = latest_transfer_progress(&events);

        terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Block::default().style(theme::base()), area);
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(4),
                    Constraint::Min(5),
                    Constraint::Length(14),
                    Constraint::Length(3),
                    Constraint::Length(6),
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

            render_header(frame, vertical[0], &state, selected_temporary.is_some());
            render_roots(frame, middle[0], &roots, &state);
            render_files(frame, middle[1], &files, &selected_paths, &state);
            render_detail_panel(
                frame,
                lower[0],
                DetailData {
                    root: selected,
                    temporary_browse: selected_temporary,
                    persisted_browse_dir: selected
                        .map(|root| current_persisted_root_dir(&state, &root.id)),
                    summary: summary.as_ref(),
                    selection: selection_summary.as_ref(),
                    file: files.get(state.file_offset),
                    selected_paths: &selected_paths,
                    plan: state.last_plan.as_ref(),
                    transfer_progress: transfer_progress.clone(),
                },
            );
            render_plan_review(frame, lower[1], state.last_plan.as_ref(), &state);
            render_info_bar(
                frame,
                vertical[3],
                InfoBarData {
                    root_name: selected_root_name(selected, selected_temporary),
                    file: files.get(state.file_offset),
                    selection: selection_summary.as_ref(),
                    event: events.get(state.event_offset),
                    root_count,
                },
                &state,
            );
            render_events(frame, vertical[4], &events, &state);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if state.retarget_draft.is_some() {
                    handle_retarget_input(conn, &mut state, key.code)?;
                    continue;
                }
                if state.pending_delete_root_id.is_some() {
                    handle_delete_root_confirmation(conn, &mut state, key.code)?;
                    continue;
                }
                if state.pending_import.is_some() {
                    handle_temporary_import_choice(&mut state, key.code, job_tx.clone());
                    continue;
                }
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
                            root_count,
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
                            selected_persisted_root(&roots, &state),
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
                            selected_persisted_root(&roots, &state),
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
                        if selected_temporary_browse(&state).is_some() {
                            let file = files.get(state.file_offset).cloned();
                            start_temporary_transfer_source_import(
                                &mut state,
                                file.as_ref(),
                                job_tx.clone(),
                            );
                        } else {
                            start_transfer_plan_selection(
                                selected_persisted_root(&roots, &state),
                                &mut state,
                            );
                        }
                    }
                    KeyCode::Char('p') => {
                        load_latest_transfer_plan(
                            conn,
                            selected_persisted_root(&roots, &state),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('x') => {
                        start_delete_root_confirmation(
                            selected_persisted_root(&roots, &state),
                            &mut state,
                        );
                    }
                    KeyCode::Char('i') => {
                        let file = files.get(state.file_offset);
                        start_temporary_import_prompt(&mut state, file);
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
                    KeyCode::Char('e') => {
                        start_retarget_current_plan_entry(&mut state);
                    }
                    KeyCode::Enter => {
                        if state.focus == FocusPane::Files
                            && selected_temporary_browse(&state).is_some()
                        {
                            let file = files.get(state.file_offset).cloned();
                            open_temporary_file_entry(&mut state, file.as_ref());
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            let file = files.get(state.file_offset).cloned();
                            open_persisted_file_entry(
                                &mut state,
                                root_id.as_deref(),
                                file.as_ref(),
                            );
                        } else {
                            create_transfer_plan_from_selection(conn, &roots, &mut state)?;
                        }
                    }
                    KeyCode::Backspace => {
                        if state.focus == FocusPane::Files
                            && selected_temporary_browse(&state).is_some()
                        {
                            open_temporary_parent(&mut state);
                        } else if state.focus == FocusPane::Files {
                            let root_id = selected.map(|root| root.id.clone());
                            open_persisted_parent(&mut state, root_id.as_deref());
                        }
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

fn render_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &AppState,
    has_temporary_browse: bool,
) {
    let header = Paragraph::new(command_hint_lines(state, has_temporary_browse))
        .style(theme::panel())
        .wrap(Wrap { trim: true })
        .block(panel_block("Commands", true));
    frame.render_widget(header, area);
}

fn command_hint_lines(state: &AppState, has_temporary_browse: bool) -> Vec<Line<'static>> {
    let mode = active_command_hint(state, has_temporary_browse);
    vec![
        Line::from(vec![
            Span::styled("Global  ", theme::header()),
            Span::styled(
                "q quit  Tab focus  arrows move  c cancel job",
                theme::muted(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Here    ", theme::header()),
            Span::styled(mode, theme::panel()),
        ]),
    ]
}

fn active_command_hint(state: &AppState, has_temporary_browse: bool) -> &'static str {
    if state.retarget_draft.is_some() {
        return "type destination path  Enter apply  Esc cancel";
    }
    if state.pending_delete_root_id.is_some() {
        return "y confirm remove root from database  n/Esc cancel";
    }
    if state.pending_import.is_some() {
        return "n root only  f fast stat import  h SHA-256 hash import  Esc cancel";
    }
    if state.transfer_run_plan_id.is_some() {
        return "transfer running  c request cancel  Tab inspect panes";
    }
    if state.transfer_source_root_id.is_some() {
        return "choose destination root  Enter create plan  Esc cancel source";
    }
    match state.focus {
        FocusPane::Roots if has_temporary_browse && state.selected_root == 0 => {
            "Tab files  i import browsed path  t copy from browsed path  Backspace up from Files"
        }
        FocusPane::Roots => "Space mark in Files  s scan  h hash  t choose source  p load plan  x remove root",
        FocusPane::Files if has_temporary_browse && state.selected_root == 0 => {
            "Enter open directory  Backspace parent  i import selected/current  t copy selected/current"
        }
        FocusPane::Files => {
            "Enter open directory  Backspace parent  Space mark file/dir  t choose source  v columns"
        }
        FocusPane::Plan => "r run copy entries  a accept review  d drop review  e retarget review",
        FocusPane::Events => "c request cancel for selected job  Tab return to roots",
    }
}

fn render_roots(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    roots: &[db::RootRow],
    state: &AppState,
) {
    let root_count = visible_root_count(state, roots.len());
    let items = if root_count == 0 {
        vec![ListItem::new(
            "No roots yet\nRun `gremlin /path` or `gremlin target add /path`",
        )]
    } else {
        let mut rows = vec![ListItem::new(root_header()).style(theme::header())];
        if let Some(browse) = state.temporary_browse.as_ref() {
            let style = if state.selected_root == 0 {
                theme::selected()
            } else {
                theme::warn()
            };
            rows.push(
                ListItem::new(temporary_root_row(state.selected_root == 0, browse)).style(style),
            );
        }
        rows.extend(roots.iter().enumerate().map(|(root_idx, root)| {
            let idx = visible_index_for_persisted(state, root_idx);
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

fn temporary_root_row(selected: bool, browse: &TemporaryBrowse) -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<6}",
        if selected { "> " } else { "  " },
        "T",
        truncate(&browse.label, 8),
        human_size(
            browse
                .entries
                .iter()
                .filter(|entry| entry.kind != "dir")
                .map(|entry| entry.size_bytes)
                .sum()
        ),
        "browse"
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
    let root_lines = if let Some(browse) = data.temporary_browse {
        format!(
            "Root: {} (temporary)\nPath: {}\nFiles: {} | Directories: {} | Current: {}\nMachine: {} | Set: browse-only",
            browse.label,
            browse.current_path,
            browse.entries.iter().filter(|entry| entry.kind != "dir").count(),
            browse.entries.iter().filter(|entry| entry.kind == "dir").count(),
            human_size(
                browse
                    .entries
                    .iter()
                    .filter(|entry| entry.kind != "dir")
                    .map(|entry| entry.size_bytes)
                    .sum()
            ),
            short_id(&browse.machine_id),
        )
    } else {
        match (data.root, data.summary) {
        (Some(root), Some(summary)) => format!(
            "Root: {}\nPath: {}\nBrowse: {}\nFiles: {} | Hashed: {} | Current: {} | Marked: {} ({})\nMachine: {} | Set: {}",
            root_display_name(root),
            root.path,
            data.persisted_browse_dir.unwrap_or("."),
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
        _ => "Root: -\nPath: -\nBrowse: -\nMachine: - | Files: - | Hashed: - | Current size: -".to_string(),
        }
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
    let transfer_lines = data
        .transfer_progress
        .as_ref()
        .map(transfer_progress_lines)
        .unwrap_or_else(|| "Transfer: -".to_string());
    let text = format!("{root_lines}\n{file_lines}\n{plan_lines}\n{transfer_lines}");
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
    let root = data.root_name.as_deref().unwrap_or("-");
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
    let status = state
        .retarget_draft
        .as_ref()
        .map(|draft| {
            format!(
                "retarget {} -> {}",
                truncate(&draft.relative_path, 18),
                draft.value
            )
        })
        .unwrap_or_else(|| state.status.clone());
    let text = format!(
        "focus {:?} | roots {} | marked {} | plan {} | root {} | file {} | {} | {}",
        state.focus,
        data.root_count,
        data.selection.map(|value| value.marked_count).unwrap_or(0),
        plan_status,
        truncate(root, 24),
        truncate(file, 20),
        truncate(&event, 24),
        status
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
    files: &[FileViewRow],
    selected_paths: &BTreeSet<String>,
    state: &AppState,
) {
    let visible = files.iter().enumerate().skip(state.file_offset);
    let items = if files.is_empty() {
        let message = if selected_temporary_browse(state).is_some() {
            "No files in this remote directory"
        } else {
            "No indexed files for this root"
        };
        vec![ListItem::new(message)]
    } else {
        let mut rows = vec![ListItem::new(file_header(state.file_view)).style(theme::header())];
        rows.extend(visible.map(|(idx, file)| {
            let marker = if idx == state.file_offset { "> " } else { "  " };
            let selected = file_row_selected(file, selected_paths);
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

fn file_row_selected(file: &FileViewRow, selected_paths: &BTreeSet<String>) -> bool {
    if file.kind == FileKind::Directory {
        let prefix = format!("{}/", file.relative_path);
        selected_paths.iter().any(|path| path.starts_with(&prefix))
    } else {
        selected_paths.contains(&file.relative_path)
    }
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

fn file_row(marker: &str, selected: bool, file: &FileViewRow, view: FileView) -> String {
    let hash = file.content_id.as_deref().map(short_id).unwrap_or("stat");
    let modified = file.modified_at.as_deref().unwrap_or("-");
    let marked = if selected { "*" } else { " " };
    let path = if file.kind == FileKind::Directory {
        format!("{}/", file.relative_path)
    } else {
        file.relative_path.clone()
    };
    match view {
        FileView::Basic => format!(
            "{:<2} {:<1} {:<24} {:>9} {:<8}",
            marker,
            marked,
            truncate(&path, 24),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 8)
        ),
        FileView::Meta => format!(
            "{:<2} {:<1} {:<18} {:>9} {:<18}",
            marker,
            marked,
            truncate(&path, 18),
            human_size(file.size_bytes as u64),
            truncate(modified, 18)
        ),
        FileView::Hash => format!(
            "{:<2} {:<1} {:<26} {:<18}",
            marker,
            marked,
            truncate(&path, 26),
            truncate(hash, 18)
        ),
        FileView::All => format!(
            "{:<2} {:<1} {:<14} {:>8} {:<6} {:<8} {:<10}",
            marker,
            marked,
            truncate(&path, 14),
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
    let path = if entry.dest_relative_path == entry.relative_path {
        entry.relative_path.clone()
    } else {
        format!("{} -> {}", entry.relative_path, entry.dest_relative_path)
    };
    format!(
        "{:<2} {:<10} {:<18} {:>9} {}",
        marker,
        truncate(&entry.action, 10),
        truncate(&path, 18),
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

fn latest_transfer_progress(events: &[db::JobEventRow]) -> Option<TransferProgressSnapshot> {
    events
        .iter()
        .filter(|row| row.job_kind == "transfer_copy")
        .find_map(|row| transfer_progress_snapshot(&row.payload_json))
}

fn transfer_progress_snapshot(payload_json: &str) -> Option<TransferProgressSnapshot> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    if payload.get("type")?.as_str()? != "job_progress" {
        return None;
    }
    Some(TransferProgressSnapshot {
        current_path: payload
            .get("current_path")
            .and_then(|value| value.as_str())
            .unwrap_or("-")
            .to_string(),
        files_done: payload
            .get("files_done")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        files_total: payload
            .get("files_total")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        bytes_done: payload.get("bytes_done")?.as_u64()?,
        bytes_total: payload.get("bytes_total")?.as_u64()?,
        file_bytes_done: payload
            .get("file_bytes_done")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        file_bytes_total: payload
            .get("file_bytes_total")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        bytes_per_second: payload
            .get("bytes_per_second")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0),
        errors: payload
            .get("errors")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    })
}

fn transfer_progress_lines(progress: &TransferProgressSnapshot) -> String {
    let overall_percent = progress_percent(progress.bytes_done, progress.bytes_total);
    let file_percent = progress_percent(progress.file_bytes_done, progress.file_bytes_total);
    format!(
        "Overall {} {:>3}% {}/{} @ {}/s\nCurrent {} {:>3}% {}/{}\nNow: {} | files {}/{} | errors {}",
        progress_bar(progress.bytes_done, progress.bytes_total, DETAIL_PROGRESS_WIDTH),
        overall_percent,
        human_size(progress.bytes_done),
        human_size(progress.bytes_total),
        transfer_rate(progress.bytes_per_second),
        progress_bar(
            progress.file_bytes_done,
            progress.file_bytes_total,
            DETAIL_PROGRESS_WIDTH
        ),
        file_percent,
        human_size(progress.file_bytes_done),
        human_size(progress.file_bytes_total),
        truncate(&progress.current_path, 36),
        progress.files_done,
        progress.files_total,
        progress.errors
    )
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
        progress_bar(done, total, EVENT_PROGRESS_WIDTH),
        ((done.saturating_mul(100)) / total).min(100),
        transfer_rate(rate)
    ))
}

fn progress_percent(done: u64, total: u64) -> u64 {
    done.min(total)
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100)
}

const DETAIL_PROGRESS_WIDTH: usize = 28;
const EVENT_PROGRESS_WIDTH: usize = 14;
const PARTIAL_BLOCKS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

fn progress_bar(done: u64, total: u64, width: usize) -> String {
    if width == 0 {
        return "[]".to_string();
    }
    let clamped_done = done.min(total);
    let eighths_total = if total == 0 {
        0
    } else {
        ((clamped_done as u128) * (width as u128) * 8) / (total as u128)
    };
    let full = (eighths_total / 8).min(width as u128) as usize;
    let partial = (eighths_total % 8) as usize;
    let mut bar = String::with_capacity(width + 2);
    bar.push('▕');
    bar.push_str(&"█".repeat(full));
    if full < width {
        bar.push_str(PARTIAL_BLOCKS[partial]);
        let empty = width.saturating_sub(full + usize::from(partial > 0));
        bar.push_str(&"░".repeat(empty));
    }
    bar.push('▏');
    bar
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

fn visible_root_count(state: &AppState, persisted_count: usize) -> usize {
    persisted_count + usize::from(state.temporary_browse.is_some())
}

fn visible_index_for_persisted(state: &AppState, persisted_idx: usize) -> usize {
    persisted_idx + usize::from(state.temporary_browse.is_some())
}

fn persisted_index_for_visible(state: &AppState) -> Option<usize> {
    if state.temporary_browse.is_some() {
        state.selected_root.checked_sub(1)
    } else {
        Some(state.selected_root)
    }
}

fn selected_persisted_root<'a>(
    roots: &'a [db::RootRow],
    state: &AppState,
) -> Option<&'a db::RootRow> {
    roots.get(persisted_index_for_visible(state)?)
}

fn selected_temporary_browse(state: &AppState) -> Option<&TemporaryBrowse> {
    state
        .temporary_browse
        .as_ref()
        .filter(|_| state.selected_root == 0)
}

fn selected_root_name(
    root: Option<&db::RootRow>,
    browse: Option<&TemporaryBrowse>,
) -> Option<String> {
    browse
        .map(|browse| format!("{}:{}", browse.label, browse.current_path))
        .or_else(|| root.map(root_display_name))
}

fn current_persisted_root_dir<'a>(state: &'a AppState, root_id: &str) -> &'a str {
    state
        .root_browse_dirs
        .get(root_id)
        .map(String::as_str)
        .unwrap_or(".")
}

fn open_persisted_file_entry(
    state: &mut AppState,
    root_id: Option<&str>,
    selected_file: Option<&FileViewRow>,
) {
    let Some(root_id) = root_id else {
        state.status = "No persisted root selected".to_string();
        return;
    };
    let Some(file) = selected_file else {
        state.status = "No indexed entry selected".to_string();
        return;
    };
    if file.kind != FileKind::Directory {
        state.status = format!("selected indexed file {}", file.relative_path);
        return;
    }
    state
        .root_browse_dirs
        .insert(root_id.to_string(), file.relative_path.clone());
    state.file_offset = 0;
    state.status = format!("browsing {}", file.relative_path);
}

fn open_persisted_parent(state: &mut AppState, root_id: Option<&str>) {
    let Some(root_id) = root_id else {
        state.status = "No persisted root selected".to_string();
        return;
    };
    let current = current_persisted_root_dir(state, root_id).to_string();
    if current == "." {
        state.status = "Already at root".to_string();
        return;
    }
    let parent = current
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "." } else { parent })
        .unwrap_or(".");
    if parent == "." {
        state.root_browse_dirs.remove(root_id);
    } else {
        state
            .root_browse_dirs
            .insert(root_id.to_string(), parent.to_string());
    }
    state.file_offset = 0;
    state.status = if parent == "." {
        "browsing root".to_string()
    } else {
        format!("browsing {parent}")
    };
}

fn open_temporary_file_entry(state: &mut AppState, selected_file: Option<&FileViewRow>) {
    let Some(file) = selected_file else {
        state.status = "No remote entry selected".to_string();
        return;
    };
    if file.kind != FileKind::Directory {
        state.status = format!("selected remote file {}", file.relative_path);
        return;
    }
    let Some(current) = state
        .temporary_browse
        .as_ref()
        .map(|browse| browse.current_path.clone())
    else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    let next_path = remote_child_path(&current, &file.relative_path);
    open_temporary_path(state, next_path);
}

fn open_temporary_parent(state: &mut AppState) {
    let Some(browse) = state.temporary_browse.as_ref() else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    if browse.current_path == browse.root_path {
        state.status = "Already at temporary root".to_string();
        return;
    }
    let Some(parent) = remote_parent_path(&browse.current_path, &browse.root_path) else {
        state.status = "Already at temporary root".to_string();
        return;
    };
    open_temporary_path(state, parent);
}

fn open_temporary_path(state: &mut AppState, next_path: String) {
    let Some(provider) = state
        .temporary_browse
        .as_ref()
        .and_then(|browse| browse.browse_provider.clone())
    else {
        state.status = "Remote browsing is unavailable for this temporary root".to_string();
        return;
    };
    match provider(&next_path) {
        Ok(entries) => {
            if let Some(browse) = state.temporary_browse.as_mut() {
                browse.current_path = next_path.clone();
                browse.entries = entries;
            }
            state.file_offset = 0;
            state.status = format!("browsing {next_path}");
        }
        Err(err) => {
            state.status = format!("remote browse failed: {err}");
        }
    }
}

fn remote_child_path(root_path: &str, child_path: &str) -> String {
    let child = child_path.trim().trim_matches('/');
    if child.is_empty() || child == "." {
        return root_path.to_string();
    }
    if root_path == "~" {
        format!("~/{child}")
    } else {
        format!("{}/{}", root_path.trim_end_matches('/'), child)
    }
}

fn remote_parent_path(current_path: &str, root_path: &str) -> Option<String> {
    if current_path == root_path {
        return None;
    }
    if current_path.starts_with("~/") {
        let parent = current_path.rsplit_once('/').map(|(parent, _)| parent)?;
        return Some(if parent == "~" || parent.is_empty() {
            "~".to_string()
        } else {
            parent.to_string()
        });
    }
    let parent = current_path.trim_end_matches('/').rsplit_once('/')?.0;
    let parent = if parent.is_empty() { "/" } else { parent };
    if root_path != "/" && !parent.starts_with(root_path.trim_end_matches('/')) {
        Some(root_path.to_string())
    } else {
        Some(parent.to_string())
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
    selected_file: Option<&FileViewRow>,
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
    if file.kind == FileKind::Directory {
        let change = db::toggle_selection_directory(conn, &root.id, &file.relative_path)?;
        state.status = if change.files_changed == 0 {
            format!("{} has no indexed files to mark", file.relative_path)
        } else if change.selected {
            format!(
                "marked {} files under {} ({})",
                change.files_changed,
                file.relative_path,
                human_size(change.bytes_changed)
            )
        } else {
            format!(
                "unmarked {} files under {} ({})",
                change.files_changed,
                file.relative_path,
                human_size(change.bytes_changed)
            )
        };
        return Ok(());
    }
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

fn start_temporary_transfer_source_import(
    state: &mut AppState,
    selected_file: Option<&FileViewRow>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let Some(browse) = selected_temporary_browse(state) else {
        state.status = "Select a temporary SSH browse root first".to_string();
        return;
    };
    let Some(provider) = browse.import_provider.clone() else {
        state.status = "Import is unavailable for this temporary root".to_string();
        return;
    };
    let target = temporary_transfer_import_target(state.focus, browse, selected_file);
    state.status = format!("importing transfer source {} (fast)", target.remote_path);
    task::spawn_blocking(move || {
        let message = match provider(ImportMode::Fast, &target.remote_path) {
            Ok(result) => {
                if result.files_imported == 0 {
                    TuiMessage::Status(format!(
                        "imported {} but found no files to mark for transfer",
                        result.root_path
                    ))
                } else {
                    TuiMessage::TemporaryTransferSourceImported {
                        root_id: result.root_id,
                        selected_relative_path: target.selected_relative_path,
                        mark_all: target.mark_all,
                        status: format!(
                            "transfer source imported {}; choose destination root and press Enter",
                            result.root_path
                        ),
                    }
                }
            }
            Err(err) => TuiMessage::Status(format!("transfer source import failed: {err}")),
        };
        let _ = job_tx.send(message);
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemporaryTransferImportTarget {
    remote_path: String,
    selected_relative_path: Option<String>,
    mark_all: bool,
}

fn temporary_transfer_import_target(
    focus: FocusPane,
    browse: &TemporaryBrowse,
    selected_file: Option<&FileViewRow>,
) -> TemporaryTransferImportTarget {
    let selected_entry = selected_file.filter(|_| focus == FocusPane::Files);
    let remote_path = selected_entry
        .map(|file| remote_child_path(&browse.current_path, &file.relative_path))
        .unwrap_or_else(|| browse.current_path.clone());
    let selected_relative_path = selected_entry
        .filter(|file| file.kind == FileKind::File)
        .map(|file| file.relative_path.clone());
    TemporaryTransferImportTarget {
        remote_path,
        selected_relative_path,
        mark_all: selected_entry
            .map(|file| file.kind == FileKind::Directory)
            .unwrap_or(true),
    }
}

fn mark_imported_transfer_source(
    conn: &Connection,
    root_id: &str,
    selected_relative_path: Option<&str>,
    mark_all: bool,
) -> anyhow::Result<()> {
    let mut already_selected = db::selected_paths_for_root(conn, root_id)?;
    if let Some(path) = selected_relative_path {
        if !already_selected.contains(path) {
            db::toggle_selection_entry(conn, root_id, path)?;
        }
        return Ok(());
    }
    if mark_all {
        for file in db::recent_files_for_root(conn, root_id, i64::MAX)? {
            if already_selected.insert(file.relative_path.clone()) {
                db::toggle_selection_entry(conn, root_id, &file.relative_path)?;
            }
        }
    }
    Ok(())
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

fn start_delete_root_confirmation(selected_root: Option<&db::RootRow>, state: &mut AppState) {
    let Some(root) = selected_root else {
        state.status = "No persisted root selected to remove".to_string();
        return;
    };
    state.pending_delete_root_id = Some(root.id.clone());
    state.status = format!(
        "Remove root {} from database? y confirms, n/Esc cancels; files stay on disk",
        root_display_name(root)
    );
}

fn handle_delete_root_confirmation(
    conn: &Connection,
    state: &mut AppState,
    code: KeyCode,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(root_id) = state.pending_delete_root_id.take() else {
                return Ok(());
            };
            match db::delete_root(conn, &root_id)? {
                Some(summary) => {
                    state.selected_root = state.selected_root.saturating_sub(1);
                    state.file_offset = 0;
                    state.event_offset = 0;
                    state.transfer_source_root_id = None;
                    state.last_plan = None;
                    state.status = format!(
                        "removed root {} ({} observations, {} plans); files untouched",
                        short_id(&summary.root_id),
                        summary.path_observations,
                        summary.transfer_plans
                    );
                }
                None => {
                    state.status = format!("root {} was already gone", short_id(&root_id));
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.pending_delete_root_id = None;
            state.status = "root removal canceled".to_string();
        }
        _ => {
            state.status = "Confirm root removal with y, or cancel with n/Esc".to_string();
        }
    }
    Ok(())
}

fn start_temporary_import_prompt(state: &mut AppState, selected_file: Option<&FileViewRow>) {
    let Some(browse) = selected_temporary_browse(state) else {
        state.status = "Select a temporary SSH browse root to import".to_string();
        return;
    };
    if browse.import_provider.is_none() {
        state.status = "Import is unavailable for this temporary root".to_string();
        return;
    }
    let selected_entry = selected_file.filter(|_| state.focus == FocusPane::Files);
    let remote_path = selected_entry
        .map(|file| remote_child_path(&browse.current_path, &file.relative_path))
        .unwrap_or_else(|| browse.current_path.clone());
    let target_kind = selected_entry
        .map(|file| {
            if file.kind == FileKind::Directory {
                "directory"
            } else {
                "file"
            }
        })
        .unwrap_or("directory");
    state.pending_import = Some(PendingTemporaryImport {
        remote_path: remote_path.clone(),
    });
    state.status = format!(
        "Import remote {target_kind} {remote_path}? n=root only, f=fast recursive stat, h=remote hash, Esc cancels"
    );
}

fn handle_temporary_import_choice(
    state: &mut AppState,
    code: KeyCode,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let mode = match code {
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ImportMode::No),
        KeyCode::Char('f') | KeyCode::Char('F') => Some(ImportMode::Fast),
        KeyCode::Char('h') | KeyCode::Char('H') => Some(ImportMode::Hash),
        KeyCode::Esc => {
            state.pending_import = None;
            state.status = "import canceled".to_string();
            return;
        }
        _ => {
            state.status = "Choose n root-only, f fast stat, h remote hash, or Esc".to_string();
            return;
        }
    };
    let Some(mode) = mode else {
        return;
    };
    let Some(pending) = state.pending_import.take() else {
        state.status = "No pending import".to_string();
        return;
    };
    let Some(provider) =
        selected_temporary_browse(state).and_then(|browse| browse.import_provider.clone())
    else {
        state.status = "No temporary root selected".to_string();
        return;
    };
    let remote_path = pending.remote_path;
    state.status = format!("importing {remote_path} ({})", import_mode_label(mode));
    task::spawn_blocking(move || {
        let status = match provider(mode, &remote_path) {
            Ok(result) => format!(
                "imported {} as root {} ({}, {} files)",
                result.root_path,
                short_id(&result.root_id),
                import_mode_label(result.mode),
                result.files_imported
            ),
            Err(err) => format!("import failed: {err}"),
        };
        let _ = job_tx.send(TuiMessage::ImportFinished(status));
    });
}

fn import_mode_label(mode: ImportMode) -> &'static str {
    match mode {
        ImportMode::No => "root-only",
        ImportMode::Fast => "fast",
        ImportMode::Hash => "hash",
    }
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

fn start_retarget_current_plan_entry(state: &mut AppState) {
    if state.focus != FocusPane::Plan {
        state.status = "Move focus to Plan before retargeting entries".to_string();
        return;
    }
    let Some(plan) = state.last_plan.as_ref() else {
        state.status = "No transfer plan to retarget".to_string();
        return;
    };
    let Some(entry) = plan.entries.get(state.plan_offset) else {
        state.status = "No plan entry selected".to_string();
        return;
    };
    if entry.action != "review" {
        state.status = format!(
            "{} is {}; only review entries can be retargeted",
            entry.relative_path, entry.action
        );
        return;
    }
    state.retarget_draft = Some(RetargetDraft {
        plan_id: plan.plan_id.clone(),
        relative_path: entry.relative_path.clone(),
        value: entry.dest_relative_path.clone(),
    });
    state.status = "Edit destination path, Enter applies, Esc cancels".to_string();
}

fn handle_retarget_input(
    conn: &Connection,
    state: &mut AppState,
    code: KeyCode,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Esc => {
            state.retarget_draft = None;
            state.status = "retarget canceled".to_string();
        }
        KeyCode::Enter => {
            let Some(draft) = state.retarget_draft.take() else {
                return Ok(());
            };
            let dest = draft.value.trim().to_string();
            if dest.is_empty() {
                state.status = "Destination path cannot be empty".to_string();
                state.retarget_draft = Some(RetargetDraft {
                    value: dest,
                    ..draft
                });
                return Ok(());
            }
            match db::retarget_review_transfer_plan_entry(
                conn,
                &draft.plan_id,
                &draft.relative_path,
                &dest,
            ) {
                Ok(true) => {
                    refresh_last_plan(conn, state, &draft.plan_id)?;
                    state.status = format!("{} -> {}", draft.relative_path, dest);
                }
                Ok(false) => {
                    refresh_last_plan(conn, state, &draft.plan_id)?;
                    state.status = format!("{} is no longer a review entry", draft.relative_path);
                }
                Err(err) => {
                    state.status = format!("retarget failed: {err}");
                    state.retarget_draft = Some(RetargetDraft {
                        value: dest,
                        ..draft
                    });
                }
            }
        }
        KeyCode::Backspace => {
            if let Some(draft) = state.retarget_draft.as_mut() {
                draft.value.pop();
            }
        }
        KeyCode::Char(value) if !value.is_control() => {
            if let Some(draft) = state.retarget_draft.as_mut() {
                draft.value.push(value);
            }
        }
        _ => {}
    }
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
            Ok(result) if result.canceled => {
                format!(
                    "canceled transfer {}: copied {} ({}) skipped {} errors {}",
                    short_id(&result.plan_id),
                    result.copied,
                    human_size(result.bytes_copied),
                    result.skipped,
                    result.errors
                )
            }
            Ok(result) => {
                format!(
                    "completed transfer {}: copied {} ({}) skipped {} errors {}",
                    short_id(&result.plan_id),
                    result.copied,
                    human_size(result.bytes_copied),
                    result.skipped,
                    result.errors
                )
            }
            Err(err) => format!("failed transfer {}: {err}", short_id(&plan_id)),
        };
        let _ = job_tx.send(TuiMessage::TransferFinished { plan_id, status });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

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
    fn temporary_browse_enter_directory_loads_child_entries() {
        let requested_paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let provider_paths = requested_paths.clone();
        let provider: BrowseProvider = Arc::new(move |path| {
            provider_paths.lock().unwrap().push(path.to_string());
            Ok(vec![InitialBrowseEntry {
                kind: "file".to_string(),
                name: "inside.txt".to_string(),
                size_bytes: 5,
                modified_at: None,
            }])
        });
        let mut state = AppState {
            temporary_browse: Some(TemporaryBrowse {
                label: "nas01:".to_string(),
                machine_id: "machine_remote".to_string(),
                root_path: "~".to_string(),
                current_path: "~".to_string(),
                entries: vec![InitialBrowseEntry {
                    kind: "dir".to_string(),
                    name: "photos".to_string(),
                    size_bytes: 0,
                    modified_at: None,
                }],
                browse_provider: Some(provider),
                import_provider: None,
            }),
            ..AppState::default()
        };
        let selected =
            FileViewRow::from_temporary_entry(&state.temporary_browse.as_ref().unwrap().entries[0]);

        open_temporary_file_entry(&mut state, Some(&selected));

        let browse = state.temporary_browse.as_ref().unwrap();
        assert_eq!(browse.current_path, "~/photos");
        assert_eq!(browse.entries.len(), 1);
        assert_eq!(browse.entries[0].name, "inside.txt");
        assert_eq!(requested_paths.lock().unwrap().as_slice(), ["~/photos"]);
    }

    #[test]
    fn temporary_import_prompt_targets_selected_file() {
        let mut state = AppState {
            focus: FocusPane::Files,
            temporary_browse: Some(TemporaryBrowse {
                label: "nas01:".to_string(),
                machine_id: "machine_remote".to_string(),
                root_path: "~".to_string(),
                current_path: "~/photos".to_string(),
                entries: vec![InitialBrowseEntry {
                    kind: "file".to_string(),
                    name: "image.png".to_string(),
                    size_bytes: 10,
                    modified_at: None,
                }],
                browse_provider: None,
                import_provider: Some(Arc::new(|_, _| unreachable!())),
            }),
            ..AppState::default()
        };
        let selected =
            FileViewRow::from_temporary_entry(&state.temporary_browse.as_ref().unwrap().entries[0]);

        start_temporary_import_prompt(&mut state, Some(&selected));

        assert_eq!(
            state.pending_import.as_ref().unwrap().remote_path,
            "~/photos/image.png"
        );
        assert!(state.status.contains("remote file ~/photos/image.png"));
    }

    #[test]
    fn temporary_import_prompt_defaults_to_current_directory() {
        let mut state = AppState {
            focus: FocusPane::Roots,
            temporary_browse: Some(TemporaryBrowse {
                label: "nas01:".to_string(),
                machine_id: "machine_remote".to_string(),
                root_path: "~".to_string(),
                current_path: "~/photos".to_string(),
                entries: Vec::new(),
                browse_provider: None,
                import_provider: Some(Arc::new(|_, _| unreachable!())),
            }),
            ..AppState::default()
        };

        start_temporary_import_prompt(&mut state, None);

        assert_eq!(
            state.pending_import.as_ref().unwrap().remote_path,
            "~/photos"
        );
        assert!(state.status.contains("remote directory ~/photos"));
    }

    #[test]
    fn command_hints_prioritize_modal_prompts() {
        let state = AppState {
            pending_import: Some(PendingTemporaryImport {
                remote_path: "~/photos".to_string(),
            }),
            ..AppState::default()
        };

        assert_eq!(
            active_command_hint(&state, true),
            "n root only  f fast stat import  h SHA-256 hash import  Esc cancel"
        );
    }

    #[test]
    fn command_hints_explain_temporary_file_browse_actions() {
        let state = AppState {
            focus: FocusPane::Files,
            selected_root: 0,
            temporary_browse: Some(TemporaryBrowse {
                label: "nas01:".to_string(),
                machine_id: "machine_remote".to_string(),
                root_path: "~".to_string(),
                current_path: "~/photos".to_string(),
                entries: Vec::new(),
                browse_provider: None,
                import_provider: None,
            }),
            ..AppState::default()
        };

        assert_eq!(
            active_command_hint(&state, true),
            "Enter open directory  Backspace parent  i import selected/current  t copy selected/current"
        );
    }

    #[test]
    fn command_hints_explain_destination_selection() {
        let state = AppState {
            transfer_source_root_id: Some("root_1".to_string()),
            ..AppState::default()
        };

        assert_eq!(
            active_command_hint(&state, false),
            "choose destination root  Enter create plan  Esc cancel source"
        );
    }

    #[test]
    fn persisted_root_enter_and_backspace_navigate_directories() {
        let mut state = AppState::default();
        let dir = FileViewRow {
            relative_path: "photos/2026".to_string(),
            size_bytes: 10,
            modified_at: None,
            content_id: None,
            status: "dir:1".to_string(),
            kind: FileKind::Directory,
        };

        open_persisted_file_entry(&mut state, Some("root_1"), Some(&dir));
        assert_eq!(current_persisted_root_dir(&state, "root_1"), "photos/2026");
        assert_eq!(state.file_offset, 0);

        open_persisted_parent(&mut state, Some("root_1"));
        assert_eq!(current_persisted_root_dir(&state, "root_1"), "photos");

        open_persisted_parent(&mut state, Some("root_1"));
        assert_eq!(current_persisted_root_dir(&state, "root_1"), ".");
    }

    #[test]
    fn directory_rows_mark_descendant_files() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = db::ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        for path in ["photos/a.png", "photos/nested/b.png"] {
            db::insert_path_observation(
                &conn,
                db::PathObservationInput {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    relative_path: path,
                    basename: path.rsplit('/').next().unwrap(),
                    parent_path: ".",
                    size_bytes: 1,
                    modified_at: None,
                    content_id: None,
                },
            )
            .unwrap();
        }
        let root = db::root_by_id(&conn, &root_id).unwrap().unwrap();
        let dir = FileViewRow {
            relative_path: "photos".to_string(),
            size_bytes: 2,
            modified_at: None,
            content_id: None,
            status: "dir:2".to_string(),
            kind: FileKind::Directory,
        };
        let mut state = AppState::default();

        toggle_selected_file_mark(&conn, Some(&root), Some(&dir), &mut state).unwrap();

        assert_eq!(
            db::selected_paths_for_root(&conn, &root_id).unwrap(),
            BTreeSet::from([
                "photos/a.png".to_string(),
                "photos/nested/b.png".to_string()
            ])
        );
        assert!(state.status.contains("marked 2 files under photos"));
        assert!(file_row_selected(
            &dir,
            &db::selected_paths_for_root(&conn, &root_id).unwrap()
        ));
    }

    #[test]
    fn temporary_transfer_source_targets_selected_file() {
        let browse = TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~/photos".to_string(),
            entries: Vec::new(),
            browse_provider: None,
            import_provider: None,
        };
        let selected = FileViewRow {
            relative_path: "image.png".to_string(),
            size_bytes: 10,
            modified_at: None,
            content_id: None,
            status: "remote".to_string(),
            kind: FileKind::File,
        };

        let target = temporary_transfer_import_target(FocusPane::Files, &browse, Some(&selected));

        assert_eq!(target.remote_path, "~/photos/image.png");
        assert_eq!(target.selected_relative_path.as_deref(), Some("image.png"));
        assert!(!target.mark_all);
    }

    #[test]
    fn temporary_transfer_source_marks_all_for_directory_target() {
        let browse = TemporaryBrowse {
            label: "nas01:".to_string(),
            machine_id: "machine_remote".to_string(),
            root_path: "~".to_string(),
            current_path: "~/photos".to_string(),
            entries: Vec::new(),
            browse_provider: None,
            import_provider: None,
        };
        let selected = FileViewRow {
            relative_path: "albums".to_string(),
            size_bytes: 0,
            modified_at: None,
            content_id: None,
            status: "dir".to_string(),
            kind: FileKind::Directory,
        };

        let target = temporary_transfer_import_target(FocusPane::Files, &browse, Some(&selected));

        assert_eq!(target.remote_path, "~/photos/albums");
        assert_eq!(target.selected_relative_path, None);
        assert!(target.mark_all);
    }

    #[test]
    fn mark_imported_transfer_source_marks_selected_or_all_paths() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
        let root_id = db::ensure_root(&conn, &machine_id, "/srv/photos").unwrap();
        for path in ["a.png", "b.png"] {
            db::insert_path_observation(
                &conn,
                db::PathObservationInput {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    relative_path: path,
                    basename: path,
                    parent_path: ".",
                    size_bytes: 1,
                    modified_at: None,
                    content_id: None,
                },
            )
            .unwrap();
        }

        mark_imported_transfer_source(&conn, &root_id, Some("a.png"), false).unwrap();
        assert_eq!(
            db::selected_paths_for_root(&conn, &root_id).unwrap(),
            BTreeSet::from(["a.png".to_string()])
        );

        mark_imported_transfer_source(&conn, &root_id, None, true).unwrap();
        assert_eq!(
            db::selected_paths_for_root(&conn, &root_id).unwrap(),
            BTreeSet::from(["a.png".to_string(), "b.png".to_string()])
        );
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
            "▕███████░░░░░░░▏  50% 1.0 MiB/s"
        );
    }

    #[test]
    fn progress_bar_uses_partial_blocks_at_static_width() {
        assert_eq!(progress_bar(1, 4, 4), "▕█░░░▏");
        assert_eq!(progress_bar(1, 8, 4), "▕▌░░░▏");
        assert_eq!(progress_bar(4, 4, 4), "▕████▏");
    }

    #[test]
    fn formats_transfer_progress_detail() {
        let payload = serde_json::json!({
            "type": "job_progress",
            "current_path": "incoming/photos/foo.png",
            "files_done": 2,
            "files_total": 4,
            "bytes_done": 512,
            "bytes_total": 1024,
            "file_bytes_done": 128,
            "file_bytes_total": 256,
            "bytes_per_second": 2.0 * 1024.0 * 1024.0,
            "errors": 1
        });
        let progress = transfer_progress_snapshot(&payload.to_string()).unwrap();
        let lines = transfer_progress_lines(&progress);

        assert!(lines.contains("Overall ▕██████████████░░░░░░░░░░░░░░▏  50%"));
        assert!(lines.contains("@ 2.0 MiB/s"));
        assert!(lines.contains("Current ▕██████████████░░░░░░░░░░░░░░▏  50%"));
        assert!(lines.contains("files 2/4 | errors 1"));
    }

    #[test]
    fn finds_latest_transfer_progress_event() {
        let complete = db::JobEventRow {
            job_id: "job_1".to_string(),
            job_kind: "transfer_copy".to_string(),
            status: "completed".to_string(),
            phase: Some("copying".to_string()),
            current_path: None,
            files_seen: 1,
            files_done: 1,
            files_skipped: 0,
            errors: 0,
            cancel_requested: false,
            sequence: 2,
            event_kind: "job_completed".to_string(),
            payload_json: serde_json::json!({"type": "job", "message": "completed"}).to_string(),
        };
        let progress = db::JobEventRow {
            sequence: 1,
            event_kind: "job_progress".to_string(),
            payload_json: serde_json::json!({
                "type": "job_progress",
                "current_path": "a.bin",
                "files_done": 0,
                "files_total": 1,
                "bytes_done": 5,
                "bytes_total": 10,
                "file_bytes_done": 5,
                "file_bytes_total": 10,
                "bytes_per_second": 512.0,
                "errors": 0
            })
            .to_string(),
            ..complete.clone()
        };

        let found = latest_transfer_progress(&[complete, progress]).unwrap();
        assert_eq!(found.current_path, "a.bin");
        assert_eq!(found.bytes_done, 5);
    }

    #[test]
    fn formats_plan_review_hint_and_count() {
        let review = db::TransferPlanEntryRow {
            relative_path: "incoming/foo.png".to_string(),
            dest_relative_path: "incoming/foo.png".to_string(),
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
