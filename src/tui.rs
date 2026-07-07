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

#[derive(Debug, Default)]
struct AppState {
    focus: FocusPane,
    file_view: FileView,
    selected_root: usize,
    file_offset: usize,
    event_offset: usize,
    status: String,
    transfer_source_root_id: Option<String>,
    last_plan: Option<PlanSnapshot>,
}

#[derive(Debug, Clone)]
struct PlanSnapshot {
    plan_id: String,
    source_name: String,
    dest_name: String,
    summary: Vec<db::TransferPlanActionSummary>,
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
            Self::Files => Self::Events,
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
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<String>();
    let mut state = AppState {
        status: "ready".to_string(),
        ..AppState::default()
    };
    loop {
        while let Ok(message) = job_rx.try_recv() {
            state.status = message;
        }
        let roots = db::roots(conn)?;
        normalize_selection(&mut state, roots.len());
        let selected = roots.get(state.selected_root);
        let files = match selected {
            Some(root) => db::recent_files_for_root(conn, &root.id, 500)?,
            None => Vec::new(),
        };
        let events = match selected {
            Some(root) => db::recent_jobs_and_events_for_root(conn, &root.id, 300)?,
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

            render_header(frame, vertical[0]);
            render_roots(frame, middle[0], &roots, &state);
            render_files(frame, middle[1], &files, &selected_paths, &state);
            render_detail_panel(
                frame,
                vertical[2],
                DetailData {
                    root: selected,
                    summary: summary.as_ref(),
                    selection: selection_summary.as_ref(),
                    file: files.get(state.file_offset),
                    selected_paths: &selected_paths,
                    plan: state.last_plan.as_ref(),
                },
            );
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
                    KeyCode::Down => move_down(&mut state, roots.len(), files.len(), events.len()),
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
        Span::styled("Gremlin", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(
            "  q quit | Tab | arrows | Space mark | s scan | h hash | c cancel | t plan | Enter",
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
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
        let mut rows = vec![ListItem::new(root_header())];
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
            ListItem::new(root_row(marker, transfer_marker, root))
        }));
        rows
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(FocusPane::Roots.title("Roots", state.focus))
                .borders(Borders::ALL),
        ),
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
            "Plan: {} | {} -> {}\n{}",
            short_id(&plan.plan_id),
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
        Paragraph::new(text).block(Block::default().title("Details").borders(Borders::ALL)),
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
    let text = format!(
        "focus {:?} | roots {} | marked {} | root {} | file {} | {} | {}",
        state.focus,
        data.root_count,
        data.selection.map(|value| value.marked_count).unwrap_or(0),
        truncate(root, 24),
        truncate(file, 20),
        truncate(&event, 24),
        state.status
    );
    frame.render_widget(
        Paragraph::new(text).block(Block::default().title("Info").borders(Borders::ALL)),
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
        let mut rows = vec![ListItem::new(file_header(state.file_view))];
        rows.extend(visible.map(|(idx, file)| {
            let marker = if idx == state.file_offset { "> " } else { "  " };
            let selected = selected_paths.contains(&file.relative_path);
            ListItem::new(file_row(marker, selected, file, state.file_view))
        }));
        rows
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(FocusPane::Files.title("Files", state.focus))
                .borders(Borders::ALL),
        ),
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
        let mut rows = vec![ListItem::new(event_header())];
        rows.extend(visible.map(|(idx, row)| {
            let marker = if idx == state.event_offset {
                "> "
            } else {
                "  "
            };
            ListItem::new(event_row(marker, row))
        }));
        rows
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(FocusPane::Events.title("Jobs", state.focus))
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn event_header() -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:>9}",
        "", "JOB", "KIND", "STATUS", "PHASE", "DONE"
    )
}

fn event_row(marker: &str, row: &db::JobEventRow) -> String {
    format!(
        "{:<2} {:<18} {:<5} {:<9} {:<10} {:>9}",
        marker,
        short_id(&row.job_id),
        truncate(&row.job_kind, 5),
        truncate(&event_status(row), 9),
        truncate(row.phase.as_deref().unwrap_or("-"), 10),
        progress_count(row)
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
    if row.files_skipped > 0 || row.errors > 0 {
        format!("{}/{}/{}", row.files_done, row.files_skipped, row.errors)
    } else {
        format!("{}/{}", row.files_done, row.files_seen)
    }
}

fn normalize_selection(state: &mut AppState, root_count: usize) {
    if root_count == 0 {
        state.selected_root = 0;
    } else if state.selected_root >= root_count {
        state.selected_root = root_count - 1;
    }
}

fn move_down(state: &mut AppState, root_count: usize, file_count: usize, event_count: usize) {
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
    job_tx: mpsc::UnboundedSender<String>,
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
            state.last_plan = Some(PlanSnapshot {
                plan_id: result.plan_id.clone(),
                source_name: root_display_name(source),
                dest_name: root_display_name(dest),
                summary,
            });
            state.transfer_source_root_id = None;
            state.status = format!("planned transfer {}", result.plan_id);
        }
        Err(err) => {
            state.status = format!("transfer plan failed: {err}");
        }
    }
    Ok(())
}

fn spawn_job_runner(
    db_path: PathBuf,
    job_id: String,
    kind: String,
    machine_label: Option<String>,
    job_tx: mpsc::UnboundedSender<String>,
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
        let _ = job_tx.send(message);
    });
}
