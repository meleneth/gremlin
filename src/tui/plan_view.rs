use super::*;
pub(super) struct PlanReviewPane<'a> {
    pub(super) plan: Option<&'a PlanSnapshot>,
    pub(super) state: &'a AppState,
}

impl Widget for PlanReviewPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items = if let Some(plan) = self.plan {
            let mut rows = vec![ListItem::new(plan_entry_header()).style(theme::header())];
            rows.extend(
                plan.entries
                    .iter()
                    .enumerate()
                    .skip(self.state.plan_offset)
                    .map(|(idx, entry)| {
                        let marker = if idx == self.state.plan_offset {
                            "> "
                        } else {
                            "  "
                        };
                        let style = if idx == self.state.plan_offset {
                            theme::selected()
                        } else {
                            plan_action_style(&entry.action)
                        };
                        ListItem::new(plan_entry_row(marker, entry)).style(style)
                    }),
            );
            rows
        } else {
            vec![ListItem::new("No transfer plan yet")]
        };
        List::new(items)
            .style(theme::panel())
            .block(focus_block("Plan", FocusPane::Plan, self.state.focus))
            .render(area, buf);
    }
}

pub(super) fn plan_summary_line(summary: &[db::TransferPlanActionSummary]) -> String {
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

pub(super) fn plan_review_count(plan: &PlanSnapshot) -> usize {
    plan.entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.action.as_str(),
                "review" | "conflict" | "verify_needed"
            )
        })
        .count()
}

pub(super) fn plan_copy_count(plan: &PlanSnapshot) -> usize {
    plan.entries
        .iter()
        .filter(|entry| entry.action == "copy")
        .count()
}

pub(super) fn plan_entry_header() -> String {
    format!(
        "{:<2} {:<10} {:<18} {:>9} {}",
        "", "ACTION", "PATH", "SIZE", "WHY"
    )
}

pub(super) fn plan_entry_row(marker: &str, entry: &db::TransferPlanEntryRow) -> String {
    let path = if entry.dest_relative_path == entry.relative_path {
        entry.relative_path.clone()
    } else {
        format!("{} -> {}", entry.relative_path, entry.dest_relative_path)
    };
    format!(
        "{:<2} {:<10} {:<18} {:>9} {}",
        marker,
        truncate(&entry.action, 10),
        truncate(&path, 18),
        human_size(entry.size_bytes),
        truncate(&plan_entry_hint(entry), 26)
    )
}

pub(super) fn plan_entry_hint(entry: &db::TransferPlanEntryRow) -> String {
    if entry.action == "review" {
        let payload: serde_json::Value =
            serde_json::from_str(&entry.metadata_json).unwrap_or(serde_json::Value::Null);
        let hash_count = payload
            .get("hash_collisions")
            .and_then(|value| value.as_array())
            .map(|value| value.len())
            .unwrap_or(0);
        let name_count = payload
            .get("filename_size_date_collisions")
            .and_then(|value| value.as_array())
            .map(|value| value.len())
            .unwrap_or(0);
        return format!("review hash={hash_count} name={name_count}");
    }
    entry.reason.clone()
}

pub(super) fn plan_action_style(action: &str) -> Style {
    match action {
        "copy" => theme::ok(),
        "review" | "verify_needed" => theme::warn(),
        "conflict" | "unavailable" => theme::error(),
        "skip" => theme::muted(),
        _ => theme::panel(),
    }
}
