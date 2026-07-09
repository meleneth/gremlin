use super::*;
pub(super) struct HeaderPane<'a> {
    pub(super) state: &'a AppState,
    pub(super) has_temporary_browse: bool,
}

impl Widget for HeaderPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let needs_attention = command_hint_needs_attention(self.state);
        let header = Paragraph::new(command_hint_lines(self.state, self.has_temporary_browse))
            .style(if needs_attention {
                theme::attention()
            } else {
                theme::panel()
            })
            .wrap(Wrap { trim: true })
            .block(panel_block("Gremlin", true));
        header.render(area, buf);
    }
}

pub(super) fn command_hint_lines(
    state: &AppState,
    has_temporary_browse: bool,
) -> Vec<Line<'static>> {
    let mode = active_command_hint(state, has_temporary_browse);
    vec![
        Line::from(vec![
            Span::styled("Global  ", theme::header()),
            Span::styled(
                "q quit  Ctrl-O open root  Tab focus  arrows move  c cancel job",
                theme::muted(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Here    ", theme::header()),
            Span::styled(
                mode,
                if command_hint_needs_attention(state) {
                    theme::attention()
                } else {
                    theme::panel()
                },
            ),
        ]),
    ]
}

fn command_hint_needs_attention(state: &AppState) -> bool {
    state.file_filter_editing
        || state.retarget_draft.is_some()
        || state.pending_delete_root_id.is_some()
        || state.pending_import.is_some()
        || state.pending_open_root.is_some()
        || state.pending_scoped_job.is_some()
        || state.transfer_plan_draft.is_some()
}

pub(super) fn active_command_hint(state: &AppState, has_temporary_browse: bool) -> &'static str {
    if state.file_filter_editing {
        return "type filter text  Backspace edit  Enter keep  Esc clear";
    }
    if state.retarget_draft.is_some() {
        return "type destination path  Enter apply  Esc cancel";
    }
    if state.pending_delete_root_id.is_some() {
        return "y confirm remove root from database  n/Esc cancel";
    }
    if state.pending_import.is_some() {
        return "n root only  f fast stat import  h SHA-256 hash import  Esc cancel";
    }
    if state.pending_open_root.is_some() {
        return "type local path, file:// path, or host:/path  Enter open  Esc cancel";
    }
    if state.pending_scoped_job.is_some() {
        return "a all files in root  m marked paths only  Esc cancel";
    }
    if state.transfer_run_plan_id.is_some() {
        return "transfer running  c request cancel  Tab inspect panes";
    }
    if state.transfer_plan_draft.is_some() {
        return "choose destination root  Enter create transfer plan  Esc cancel source";
    }
    if state.collection_result.is_some() && state.focus == FocusPane::Plan {
        return "collection compare result  PgUp/PgDn jump  p load transfer plan";
    }
    match state.focus {
        FocusPane::Roots if has_temporary_browse && state.selected_root == 0 => {
            "Tab files  i import browsed path  t copy from browsed path  PgUp/PgDn jump"
        }
        FocusPane::Roots => "Enter load resume row  r run resume row  Space mark in Files  s scan  h hash  v verify  m compare  t source  p plan  x remove",
        FocusPane::Files if has_temporary_browse && state.selected_root == 0 => {
            "/ filter  PgUp/PgDn jump  Enter open dir  Backspace parent  i import  t copy"
        }
        FocusPane::Files => {
            "/ filter  PgUp/PgDn jump  Enter open dir  Backspace parent  Space mark  f fields"
        }
        FocusPane::Plan => "r run copy entries  a accept review  d drop review  e retarget review",
        FocusPane::Events => "c request cancel for selected job  Tab return to roots",
    }
}
