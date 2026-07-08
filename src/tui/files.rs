use super::*;
pub(super) fn render_files(
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

pub(super) fn file_row_selected(file: &FileViewRow, selected_paths: &BTreeSet<String>) -> bool {
    if file.kind == FileKind::Directory {
        let prefix = format!("{}/", file.relative_path);
        selected_paths.iter().any(|path| path.starts_with(&prefix))
    } else {
        selected_paths.contains(&file.relative_path)
    }
}

pub(super) fn file_header(view: FileView) -> String {
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

pub(super) fn file_row(marker: &str, selected: bool, file: &FileViewRow, view: FileView) -> String {
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

pub(super) fn truncate(value: &str, width: usize) -> String {
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

pub(super) fn short_id(value: &str) -> &str {
    value.get(..value.len().min(18)).unwrap_or(value)
}
