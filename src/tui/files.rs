use super::*;
pub(super) struct FilesPane<'a> {
    pub(super) files: &'a [FileViewRow],
    pub(super) selected_paths: &'a BTreeSet<String>,
    pub(super) state: &'a AppState,
}

impl Widget for FilesPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible = self.files.iter().enumerate().skip(self.state.file_offset);
        let items = if self.files.is_empty() {
            let message = if !self.state.file_filter.is_empty() {
                "No files match the active filter"
            } else if selected_temporary_browse(self.state).is_some() {
                "No files in this remote directory"
            } else if let Some(progress) = self.state.active_import_progress.as_ref() {
                return Paragraph::new(format!(
                    "Import in progress: {}\n{} processed | {} queued\n{}",
                    progress.phase,
                    progress.files_imported,
                    progress.files_queued,
                    progress
                        .current_path
                        .as_deref()
                        .unwrap_or("waiting for first file")
                ))
                .style(theme::panel())
                .wrap(Wrap { trim: true })
                .block(focus_block(
                    files_title(self.state),
                    FocusPane::Files,
                    self.state.focus,
                ))
                .render(area, buf);
            } else {
                "No indexed files for this root"
            };
            vec![ListItem::new(message)]
        } else {
            let mut rows =
                vec![ListItem::new(file_header(self.state.file_view)).style(theme::header())];
            rows.push(ListItem::new(file_legend()).style(theme::muted()));
            rows.extend(visible.map(|(idx, file)| {
                let marker = if idx == self.state.file_offset {
                    "> "
                } else {
                    "  "
                };
                let selected = file_row_selected(file, self.selected_paths);
                let style = file_row_style(file, selected, idx == self.state.file_offset);
                ListItem::new(file_row(marker, selected, file, self.state.file_view)).style(style)
            }));
            rows
        };
        List::new(items)
            .style(theme::panel())
            .block(focus_block(
                files_title(self.state),
                FocusPane::Files,
                self.state.focus,
            ))
            .render(area, buf);
    }
}

pub(super) fn file_legend() -> &'static str {
    "◇ remote  ◌ indexed  ◆ hash  ◉ local  ! changed  × missing  ▸ dir"
}

pub(super) fn files_title(state: &AppState) -> String {
    if state.file_filter.is_empty() {
        "Files".to_string()
    } else if state.file_filter_editing {
        format!("Files /{}", state.file_filter)
    } else {
        format!("Files filter:{}", state.file_filter)
    }
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
            "{:<2} {:>3} {:<1} {:<40} {:>9} {:<8} {:<10}",
            "", "#", "E", "PATH", "SIZE", "STATE", "SHA/ID"
        ),
        FileView::Meta => format!(
            "{:<2} {:>3} {:<1} {:<34} {:>9} {:<18}",
            "", "#", "E", "PATH", "SIZE", "MODIFIED"
        ),
        FileView::Hash => format!(
            "{:<2} {:>3} {:<1} {:<40} {:<18}",
            "", "#", "E", "PATH", "SHA-256 / ID"
        ),
        FileView::All => format!(
            "{:<2} {:>3} {:<1} {:<28} {:>8} {:<6} {:<10} {:<10}",
            "", "#", "E", "PATH", "SIZE", "STATE", "SHA/ID", "MODIFIED"
        ),
    }
}

pub(super) fn file_row(marker: &str, selected: bool, file: &FileViewRow, view: FileView) -> String {
    let hash = file_hash_label(file);
    let modified = file.modified_at.as_deref().unwrap_or("-");
    let evidence = file_evidence_label(file, selected);
    let occurrences = file_occurrence_label(file);
    let path = if file.kind == FileKind::Directory {
        format!("{}/", file.relative_path)
    } else {
        file.relative_path.clone()
    };
    match view {
        FileView::Basic => format!(
            "{:<2} {:>3} {:<1} {:<40} {:>9} {:<8} {:<10}",
            marker,
            occurrences,
            evidence,
            truncate(&path, 40),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 8),
            truncate(hash, 10)
        ),
        FileView::Meta => format!(
            "{:<2} {:>3} {:<1} {:<34} {:>9} {:<18}",
            marker,
            occurrences,
            evidence,
            truncate(&path, 34),
            human_size(file.size_bytes as u64),
            truncate(modified, 18)
        ),
        FileView::Hash => format!(
            "{:<2} {:>3} {:<1} {:<40} {:<18}",
            marker,
            occurrences,
            evidence,
            truncate(&path, 40),
            truncate(hash, 18)
        ),
        FileView::All => format!(
            "{:<2} {:>3} {:<1} {:<28} {:>8} {:<6} {:<10} {:<10}",
            marker,
            occurrences,
            evidence,
            truncate(&path, 28),
            human_size(file.size_bytes as u64),
            truncate(&file.status, 6),
            truncate(hash, 10),
            truncate(modified, 10)
        ),
    }
}

pub(super) fn file_row_style(file: &FileViewRow, selected: bool, focused: bool) -> Style {
    if focused {
        return theme::selected();
    }
    if selected {
        return theme::marked();
    }
    match file.index_state {
        FileIndexState::RemoteUnindexed => theme::remote_file(),
        FileIndexState::Indexed => theme::indexed_file(),
        FileIndexState::Available => theme::available_file(),
        FileIndexState::RemoteChanged => theme::changed_file(),
        FileIndexState::RemoteMissing => theme::missing_file(),
    }
}

fn file_evidence_label(file: &FileViewRow, selected: bool) -> String {
    if selected {
        return "*".to_string();
    }
    if file.kind == FileKind::Directory {
        return "▸".to_string();
    }
    match file.index_state {
        FileIndexState::RemoteUnindexed => "◇".to_string(),
        FileIndexState::Available => "◉".to_string(),
        FileIndexState::RemoteChanged => "!".to_string(),
        FileIndexState::RemoteMissing => "×".to_string(),
        FileIndexState::Indexed if file.content_id.is_some() => "◆".to_string(),
        FileIndexState::Indexed => "◌".to_string(),
    }
}

fn file_hash_label(file: &FileViewRow) -> &str {
    file.sha256
        .as_deref()
        .or(file.content_id.as_deref())
        .map(short_id)
        .unwrap_or("stat")
}

fn file_occurrence_label(file: &FileViewRow) -> String {
    if file.kind == FileKind::Directory {
        "-".to_string()
    } else {
        file.occurrence_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".to_string())
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
