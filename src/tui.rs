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

pub fn run(conn: &Connection) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(conn, &mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    conn: &Connection,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    let mut focus = 0_usize;
    loop {
        let roots = db::roots(conn)?;
        let files = db::recent_files(conn, 50)?;
        let events = db::recent_jobs_and_events(conn, 50)?;

        terminal.draw(|frame| {
            let area = frame.size();
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(8),
                ])
                .split(area);
            let middle = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(vertical[1]);

            let header = Paragraph::new(Line::from(vec![
                Span::styled("Gremlin", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  local file evidence database  |  q quit  Tab focus"),
            ]))
            .block(Block::default().borders(Borders::ALL));
            frame.render_widget(header, vertical[0]);

            let root_items = roots
                .iter()
                .map(|root| {
                    let label = root.label.as_deref().unwrap_or(&root.path);
                    ListItem::new(format!("{label}\n{}\n{}", root.path, root.id))
                })
                .collect::<Vec<_>>();
            let roots_title = if focus == 0 { "Roots *" } else { "Roots" };
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
            let files_title = if focus == 1 {
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
            let events_title = if focus == 2 {
                "Recent Jobs/Events *"
            } else {
                "Recent Jobs/Events"
            };
            frame.render_widget(
                List::new(event_items)
                    .block(Block::default().title(events_title).borders(Borders::ALL)),
                vertical[2],
            );
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => focus = (focus + 1) % 3,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
