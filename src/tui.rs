use std::io;
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

use crate::db;

#[derive(Debug, Default)]
struct AppState {
    focus: FocusPane,
    file_view: FileView,
    selected_root: usize,
    file_offset: usize,
    event_offset: usize,
    status: String,
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

pub fn run_with_options(conn: &Connection, machine_label: Option<String>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(conn, &mut terminal, machine_label);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    conn: &Connection,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    machine_label: Option<String>,
) -> anyhow::Result<()> {
    let mut state = AppState {
        status: "ready".to_string(),
        ..AppState::default()
    };
    loop {
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

        terminal.draw(|frame| {
            let area = frame.size();
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(8),
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
            render_files(frame, middle[1], &files, &state);
            render_detail_panel(
                frame,
                vertical[2],
                selected,
                summary.as_ref(),
                files.get(state.file_offset),
            );
            render_info_bar(
                frame,
                vertical[3],
                selected,
                files.get(state.file_offset),
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
                            &roots,
                            state.selected_root,
                            "scan",
                            machine_label.as_deref(),
                            &mut state,
                        )?;
                    }
                    KeyCode::Char('h') => {
                        queue_selected_root(
                            conn,
                            &roots,
                            state.selected_root,
                            "hash",
                            machine_label.as_deref(),
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
        Span::raw("  q quit | Tab focus | arrows move | v fields | s scan job | h hash job"),
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
        roots
            .iter()
            .enumerate()
            .map(|(idx, root)| {
                let label = root.label.as_deref().unwrap_or(&root.path);
                let marker = if idx == state.selected_root {
                    "> "
                } else {
                    "  "
                };
                ListItem::new(format!(
                    "{marker}{label}\n  {}\n  {}",
                    human_size(root.current_size_bytes as u64),
                    root.path
                ))
            })
            .collect()
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

fn render_detail_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    selected: Option<&db::RootRow>,
    summary: Option<&db::RootSummary>,
    selected_file: Option<&db::FileRow>,
) {
    let root_lines = match (selected, summary) {
        (Some(root), Some(summary)) => format!(
            "Root: {}\nPath: {}\nMachine: {} | Files: {} | Hashed: {} | Current size: {}",
            root.label.as_deref().unwrap_or(&root.id),
            root.path,
            short_id(&root.machine_id),
            summary.file_count,
            summary.content_count,
            human_size(root.current_size_bytes as u64)
        ),
        _ => "Root: -\nPath: -\nMachine: - | Files: - | Hashed: - | Current size: -".to_string(),
    };
    let file_lines = if let Some(file) = selected_file {
        format!(
            "File: {}\nSize: {} ({} bytes) | Status: {} | Modified: {}\nContent: {} | Metadata: not extracted yet",
            file.relative_path,
            human_size(file.size_bytes as u64),
            file.size_bytes,
            file.status,
            file.modified_at.as_deref().unwrap_or("-"),
            file.content_id.as_deref().map(short_id).unwrap_or("stat-only")
        )
    } else {
        "File: -\nSize: - | Status: - | Modified: -\nContent: - | Metadata: not extracted yet"
            .to_string()
    };
    let text = format!("{root_lines}\n{file_lines}");
    frame.render_widget(
        Paragraph::new(text).block(Block::default().title("Details").borders(Borders::ALL)),
        area,
    );
}

fn render_info_bar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    selected: Option<&db::RootRow>,
    selected_file: Option<&db::FileRow>,
    state: &AppState,
) {
    let root = selected
        .map(|root| root.label.as_deref().unwrap_or(&root.path))
        .unwrap_or("-");
    let file = selected_file
        .map(|file| file.relative_path.as_str())
        .unwrap_or("-");
    let text = format!(
        "focus {:?} | selector fields {} | root {} | file {} | {}",
        state.focus,
        state.file_view.label(),
        truncate(root, 24),
        truncate(file, 28),
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
    state: &AppState,
) {
    let visible = files.iter().enumerate().skip(state.file_offset);
    let items = if files.is_empty() {
        vec![ListItem::new("No indexed files for this root")]
    } else {
        let mut rows = vec![ListItem::new(file_header(state.file_view))];
        rows.extend(visible.map(|(idx, file)| {
            let marker = if idx == state.file_offset { "> " } else { "  " };
            ListItem::new(file_row(marker, file, state.file_view))
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
        FileView::Basic => format!("{:<2} {:<26} {:>9} {:<8}", "", "PATH", "SIZE", "STATE"),
        FileView::Meta => format!("{:<2} {:<20} {:>9} {:<18}", "", "PATH", "SIZE", "MODIFIED"),
        FileView::Hash => format!("{:<2} {:<28} {:<18}", "", "PATH", "CONTENT"),
        FileView::All => format!(
            "{:<2} {:<16} {:>8} {:<6} {:<8} {:<10}",
            "", "PATH", "SIZE", "STATE", "HASH", "MODIFIED"
        ),
    }
}

fn file_row(marker: &str, file: &db::FileRow, view: FileView) -> String {
    let hash = file.content_id.as_deref().map(short_id).unwrap_or("stat");
    let modified = file.modified_at.as_deref().unwrap_or("-");
    match view {
        FileView::Basic => format!(
            "{:<2} {:<26} {:>9} {:<8}",
            marker,
            truncate(&file.relative_path, 26),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 8)
        ),
        FileView::Meta => format!(
            "{:<2} {:<20} {:>9} {:<18}",
            marker,
            truncate(&file.relative_path, 20),
            human_size(file.size_bytes as u64),
            truncate(modified, 18)
        ),
        FileView::Hash => format!(
            "{:<2} {:<28} {:<18}",
            marker,
            truncate(&file.relative_path, 28),
            truncate(hash, 18)
        ),
        FileView::All => format!(
            "{:<2} {:<16} {:>8} {:<6} {:<8} {:<10}",
            marker,
            truncate(&file.relative_path, 16),
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

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.00 KiB");
        assert_eq!(human_size(12 * 1024), "12.0 KiB");
    }

    #[test]
    fn truncates_long_values() {
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }
}

fn render_events(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    events: &[db::JobEventRow],
    state: &AppState,
) {
    let visible = events.iter().skip(state.event_offset);
    let items = if events.is_empty() {
        vec![ListItem::new("No jobs or events for this root")]
    } else {
        visible
            .map(|row| {
                ListItem::new(format!(
                    "{}  {}  {}  {}",
                    row.created_at, row.job_id, row.status, row.event_kind
                ))
            })
            .collect()
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(FocusPane::Events.title("Jobs / Events", state.focus))
                .borders(Borders::ALL),
        ),
        area,
    );
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
    roots: &[db::RootRow],
    selected_root: usize,
    kind: &str,
    machine_label: Option<&str>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = roots.get(selected_root) else {
        state.status = "No root selected. Add one with `gremlin /path`.".to_string();
        return Ok(());
    };
    let job_id = db::queue_file_job(conn, kind, std::path::Path::new(&root.path), machine_label)?;
    state.status = format!("queued {kind} job {job_id}");
    Ok(())
}
