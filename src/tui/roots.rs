use super::*;
pub(super) struct RootsPane<'a> {
    pub(super) roots: &'a [db::RootRow],
    pub(super) state: &'a AppState,
}

impl Widget for RootsPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let root_count = visible_root_count(self.state, self.roots.len());
        let items = if root_count == 0 {
            vec![ListItem::new(
                "No roots yet\nRun `gremlin /path` or `gremlin target add /path`",
            )]
        } else {
            let mut rows = vec![ListItem::new(root_header()).style(theme::header())];
            if let Some(browse) = self.state.temporary_browse.as_ref() {
                let style = if self.state.selected_root == 0 {
                    theme::selected()
                } else {
                    theme::warn()
                };
                rows.push(
                    ListItem::new(temporary_root_row(self.state.selected_root == 0, browse))
                        .style(style),
                );
            }
            rows.extend(self.roots.iter().enumerate().map(|(root_idx, root)| {
                let idx = visible_index_for_persisted(self.state, root_idx);
                let marker = if idx == self.state.selected_root {
                    "> "
                } else {
                    "  "
                };
                let transfer_marker =
                    if self.state.transfer_source_root_id.as_deref() == Some(&root.id) {
                        "S"
                    } else {
                        " "
                    };
                let style = if idx == self.state.selected_root {
                    theme::selected()
                } else if self.state.transfer_source_root_id.as_deref() == Some(&root.id) {
                    theme::marked()
                } else {
                    theme::panel()
                };
                ListItem::new(root_row(marker, transfer_marker, root)).style(style)
            }));
            if !self.state.resumable_transfer_plans.is_empty() {
                rows.push(ListItem::new("  Resume").style(theme::header()));
                let first_resume =
                    self.roots.len() + usize::from(self.state.temporary_browse.is_some());
                rows.extend(self.state.resumable_transfer_plans.iter().enumerate().map(
                    |(resume_idx, plan)| {
                        let idx = first_resume + resume_idx;
                        let marker = if idx == self.state.selected_root {
                            "> "
                        } else {
                            "  "
                        };
                        let style = if idx == self.state.selected_root {
                            theme::selected()
                        } else {
                            theme::warn()
                        };
                        ListItem::new(resume_plan_row(marker, plan)).style(style)
                    },
                ));
            }
            rows
        };
        List::new(items)
            .style(theme::panel())
            .block(focus_block("Roots", FocusPane::Roots, self.state.focus))
            .render(area, buf);
    }
}

pub(super) fn resume_plan_row(marker: &str, plan: &db::TransferPlanRow) -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<11}",
        marker,
        "R",
        short_id(&plan.id),
        human_size(plan.total_bytes as u64),
        truncate(&format!("{} {}", plan.status, plan.entry_count), 11)
    )
}

pub(super) fn root_header() -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<11}",
        "", "T", "ROOT", "SIZE", "STATE"
    )
}

pub(super) fn root_row(marker: &str, transfer_marker: &str, root: &db::RootRow) -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<11}",
        marker,
        transfer_marker,
        truncate(&root_display_name(root), 8),
        human_size(root.current_size_bytes as u64),
        truncate(&root_job_label(root), 11)
    )
}

pub(super) fn temporary_root_row(selected: bool, browse: &TemporaryBrowse) -> String {
    format!(
        "{:<2} {:<1} {:<8} {:>5} {:<11}",
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

pub(super) fn root_job_label(root: &db::RootRow) -> String {
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

pub(super) fn compact_job_kind(kind: &str) -> &str {
    match kind {
        "scan" => "scan",
        "hash" => "hash",
        "verify" => "verify",
        other => other,
    }
}

pub(super) fn compact_status(status: &str) -> &str {
    match status {
        "created" => "new",
        "running" => "run",
        "completed" => "done",
        "completed_with_errors" => "errors",
        "failed" => "fail",
        other => other,
    }
}

pub(super) fn compact_phase(phase: &str) -> &str {
    match phase {
        "queued" => "new",
        "preparing" => "prep",
        "walking" => "walk",
        "processing" => "work",
        "finalizing" => "done",
        other => other,
    }
}

pub(super) fn root_display_name(root: &db::RootRow) -> String {
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

pub(super) fn display_name_from_path(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
        .to_string()
}
