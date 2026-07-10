use super::*;

pub(super) struct AppScreen<'a> {
    pub(super) state: &'a AppState,
    pub(super) roots: &'a [db::RootRow],
    pub(super) files: &'a [FileViewRow],
    pub(super) selected_paths: &'a BTreeSet<String>,
    pub(super) selected_root: Option<&'a db::RootRow>,
    pub(super) selected_temporary: Option<&'a TemporaryBrowse>,
    pub(super) summary: Option<&'a db::RootSummary>,
    pub(super) selection: Option<&'a db::SelectionSummary>,
    pub(super) detail_content: Option<&'a db::ContentObjectRow>,
    pub(super) file_appearances: &'a [db::FileAppearanceRow],
    pub(super) events: &'a [db::JobEventRow],
    pub(super) root_count: usize,
    pub(super) transfer_progress: Option<TransferProgressSnapshot>,
    pub(super) import_progress: Option<&'a ImportProgress>,
    pub(super) detail_file_offset: usize,
}

impl Widget for AppScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Block::default().style(theme::base()).render(area, buf);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(5),
                Constraint::Percentage(38),
                Constraint::Length(9),
            ])
            .split(area);
        let middle = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
            .split(vertical[1]);
        let lower = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(vertical[2]);
        let bottom_right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(5)])
            .split(lower[1]);

        HeaderPane {
            state: self.state,
            has_temporary_browse: self.selected_temporary.is_some(),
        }
        .render(vertical[0], buf);
        RootsPane {
            roots: self.roots,
            state: self.state,
        }
        .render(middle[0], buf);
        FilesPane {
            files: self.files,
            selected_paths: self.selected_paths,
            state: self.state,
        }
        .render(middle[1], buf);
        DetailPane {
            data: DetailData {
                root: self.selected_root,
                temporary_browse: self.selected_temporary,
                persisted_browse_dir: self
                    .selected_root
                    .map(|root| current_persisted_root_dir(self.state, &root.id)),
                summary: self.summary,
                selection: self.selection,
                file: self.files.get(self.detail_file_offset),
                content: self.detail_content,
                appearances: self.file_appearances,
                selected_paths: self.selected_paths,
                plan: self.state.last_plan.as_ref(),
                collection: self.state.collection_result.as_ref(),
                transfer_progress: self.transfer_progress.clone(),
                import_progress: self.import_progress,
            },
        }
        .render(lower[0], buf);
        EventsPane {
            events: self.events,
            state: self.state,
        }
        .render(bottom_right[0], buf);
        ActivityPane { state: self.state }.render(bottom_right[1], buf);
        InfoBar {
            data: InfoBarData {
                root_name: selected_root_name(self.selected_root, self.selected_temporary),
                file: self.files.get(self.state.file_offset),
                selection: self.selection,
                event: self.events.get(self.state.event_offset),
                root_count: self.root_count,
                transfer_progress: self.transfer_progress,
                import_progress: self.import_progress,
            },
            state: self.state,
        }
        .render(vertical[3], buf);
        if let Some(modal) = decision_modal(self.state) {
            render_decision_modal(modal, area, buf);
        } else if self.state.focus == FocusPane::Plan
            && (self.state.last_plan.is_some() || self.state.collection_result.is_some())
        {
            render_plan_modal(self.state, area, buf);
        }
    }
}

struct DecisionModal {
    title: &'static str,
    lines: Vec<Line<'static>>,
    width: u16,
    height: u16,
}

fn decision_modal(state: &AppState) -> Option<DecisionModal> {
    if let Some(draft) = state.pending_open_root.as_ref() {
        return Some(DecisionModal {
            title: "Open Root",
            lines: vec![
                Line::from("Enter a local path, file:// path, or SSH target."),
                Line::from("Open a snapshot directory, then press i on the JSON file."),
                Line::from(format!("Location: {}", draft.input)),
                Line::from("Enter open  Esc cancel"),
            ],
            width: 72,
            height: 8,
        });
    }
    if let Some(pending) = state.pending_import.as_ref() {
        return Some(DecisionModal {
            title: "Import Remote Root",
            lines: vec![
                Line::from(format!("Path: {}", pending.remote_path)),
                Line::from("n root only  f fast recursive stat  h remote SHA-256+CRC hash"),
                Line::from("Esc cancel"),
            ],
            width: 76,
            height: 7,
        });
    }
    if let Some(pending) = state.pending_scoped_job.as_ref() {
        return Some(DecisionModal {
            title: "Job Scope",
            lines: vec![
                Line::from(format!("{} marked path-aware job", pending.kind)),
                Line::from("a run against all files in root"),
                Line::from("m run against marked paths only"),
                Line::from("Esc cancel"),
            ],
            width: 64,
            height: 8,
        });
    }
    if state.pending_delete_root_id.is_some() {
        return Some(DecisionModal {
            title: "Remove Root",
            lines: vec![
                Line::from("Remove this root from the database?"),
                Line::from("Files on disk are not deleted."),
                Line::from("y confirm  n/Esc cancel"),
            ],
            width: 58,
            height: 7,
        });
    }
    if state.pending_drop_transfer_plan_id.is_some() {
        return Some(DecisionModal {
            title: "Drop Queued Transfer",
            lines: vec![
                Line::from("Remove this transfer plan from the run queue?"),
                Line::from("The plan history stays in the database."),
                Line::from("y confirm  n/Esc cancel"),
            ],
            width: 64,
            height: 7,
        });
    }
    if let Some(draft) = state.retarget_draft.as_ref() {
        return Some(DecisionModal {
            title: "Retarget Copy",
            lines: vec![
                Line::from(format!("Source: {}", draft.relative_path)),
                Line::from(format!("Destination: {}", draft.value)),
                Line::from("Enter apply  Backspace edit  Esc cancel"),
            ],
            width: 82,
            height: 7,
        });
    }
    if let Some(draft) = state.transfer_plan_draft.as_ref() {
        return Some(DecisionModal {
            title: "Choose Destination",
            lines: vec![
                Line::from(format!("Source: {}", draft.source_name)),
                Line::from(format!("Path: {}", draft.source_path)),
                Line::from(format!(
                    "Marked: {} ({})",
                    draft.marked_count,
                    human_size(draft.marked_bytes as u64)
                )),
                Line::from("Move to destination root, then press Enter."),
                Line::from("Esc cancel source selection"),
            ],
            width: 78,
            height: 9,
        });
    }
    None
}

fn render_decision_modal(modal: DecisionModal, area: Rect, buf: &mut Buffer) {
    let modal_area = centered_rect(modal.width, modal.height, area);
    Clear.render(modal_area, buf);
    Paragraph::new(modal.lines)
        .style(theme::attention())
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(modal.title)
                .borders(Borders::ALL)
                .style(theme::attention())
                .border_style(
                    Style::default()
                        .fg(theme::BORDER_ACTIVE)
                        .bg(theme::ATTENTION),
                )
                .title_style(theme::active_title()),
        )
        .render(modal_area, buf);
}

fn render_plan_modal(state: &AppState, area: Rect, buf: &mut Buffer) {
    let modal_area = centered_rect(116, 24, area);
    Clear.render(modal_area, buf);
    PlanReviewPane {
        plan: state.last_plan.as_ref(),
        collection: state.collection_result.as_ref(),
        state,
    }
    .render(modal_area, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
