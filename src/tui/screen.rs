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
    pub(super) events: &'a [db::JobEventRow],
    pub(super) root_count: usize,
    pub(super) transfer_progress: Option<TransferProgressSnapshot>,
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
                Constraint::Percentage(34),
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
        let bottom_left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(lower[0]);
        let bottom_right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
            .split(lower[1]);
        let activity = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(bottom_right[1]);

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
                selected_paths: self.selected_paths,
                plan: self.state.last_plan.as_ref(),
                collection: self.state.collection_result.as_ref(),
                transfer_progress: self.transfer_progress,
            },
        }
        .render(bottom_left[0], buf);
        PlanReviewPane {
            plan: self.state.last_plan.as_ref(),
            collection: self.state.collection_result.as_ref(),
            state: self.state,
        }
        .render(bottom_right[0], buf);
        InfoBar {
            data: InfoBarData {
                root_name: selected_root_name(self.selected_root, self.selected_temporary),
                file: self.files.get(self.state.file_offset),
                selection: self.selection,
                event: self.events.get(self.state.event_offset),
                root_count: self.root_count,
            },
            state: self.state,
        }
        .render(bottom_left[1], buf);
        ActivityPane { state: self.state }.render(activity[0], buf);
        EventsPane {
            events: self.events,
            state: self.state,
        }
        .render(activity[1], buf);
    }
}
