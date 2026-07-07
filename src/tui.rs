use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;
use rusqlite::Connection;

use crate::db;

#[derive(Debug, Default)]
struct AppState {
    focus: usize,
    selected_root: usize,
    status: String,
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
        status: "s queues scan job, h queues hash job for selected root".to_string(),
        ..AppState::default()
    };
    loop {
        let roots = db::roots(conn)?;
        let files = db::recent_files(conn, 50)?;
        let events = db::recent_jobs_and_events(conn, 50)?;
        if !roots.is_empty() && state.selected_root >= roots.len() {
            state.selected_root = roots.len() - 1;
        }

        terminal.draw(|frame| {
            let area = frame.size();
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(3),
                    Constraint::Length(8),
                ])
                .split(area);
            let middle = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(vertical[1]);

            let header = Paragraph::new(Line::from(vec![
                Span::styled("Gremlin", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(
                    "  local file evidence database  |  q quit  Tab focus  s scan job  h hash job",
                ),
            ]))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(header, vertical[0]);

            let root_items = roots
                .iter()
                .enumerate()
                .map(|(idx, root)| {
                    let label = root.label.as_deref().unwrap_or(&root.path);
                    let marker = if idx == state.selected_root {
                        "> "
                    } else {
                        "  "
                    };
                    ListItem::new(format!("{marker}{label}\n  {}\n  {}", root.path, root.id))
                })
                .collect::<Vec<_>>();
            let roots_title = if state.focus == 0 { "Roots *" } else { "Roots" };
            frame.render_widget(
                List::new(root_items)
                    .block(Block::default().title(roots_title).borders(Borders::ALL)),
                middle[0],
            );

            let file_items = files
                .iter()
                .map(|file| {
                    ListItem::new(format!(
                        "{}  {} bytes  {}",
                        file.relative_path, file.size_bytes, file.status
                    ))
                })
                .collect::<Vec<_>>();
            let files_title = if state.focus == 1 {
                "Recent Files *"
            } else {
                "Recent Files"
            };
            frame.render_widget(
                List::new(file_items)
                    .block(Block::default().title(files_title).borders(Borders::ALL)),
                middle[1],
            );

            let event_items = events
                .iter()
                .map(|row| {
                    ListItem::new(format!(
                        "{}  {}  {}  {}",
                        row.created_at, row.job_id, row.status, row.event_kind
                    ))
                })
                .collect::<Vec<_>>();
            let status = Paragraph::new(state.status.as_str())
                .block(Block::default().title("Job Control").borders(Borders::ALL));
            frame.render_widget(status, vertical[2]);

            let events_title = if state.focus == 2 {
                "Recent Jobs/Events *"
            } else {
                "Recent Jobs/Events"
            };
            frame.render_widget(
                List::new(event_items)
                    .block(Block::default().title(events_title).borders(Borders::ALL)),
                vertical[3],
            );
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => state.focus = (state.focus + 1) % 3,
                    KeyCode::Down => {
                        if state.focus == 0 && state.selected_root + 1 < roots.len() {
                            state.selected_root += 1;
                        }
                    }
                    KeyCode::Up => {
                        if state.focus == 0 && state.selected_root > 0 {
                            state.selected_root -= 1;
                        }
                    }
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

fn queue_selected_root(
    conn: &Connection,
    roots: &[db::RootRow],
    selected_root: usize,
    kind: &str,
    machine_label: Option<&str>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = roots.get(selected_root) else {
        state.status = "no root selected; run scan or create a job from the CLI first".to_string();
        return Ok(());
    };
    let job_id = db::queue_file_job(conn, kind, std::path::Path::new(&root.path), machine_label)?;
    state.status = format!("queued {kind} job {job_id}");
    Ok(())
}
