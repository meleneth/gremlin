use super::*;
pub(super) struct DetailPane<'a> {
    pub(super) data: DetailData<'a>,
}

impl Widget for DetailPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let data = self.data;
        let root_lines = if let Some(browse) = data.temporary_browse {
            format!(
                "Root: {} (temporary)\nPath: {}\nFiles: {} | Directories: {} | Current: {}\nMachine: {} | Set: browse-only",
                browse.label,
                browse.current_path,
                browse.entries.iter().filter(|entry| entry.kind != "dir").count(),
                browse.entries.iter().filter(|entry| entry.kind == "dir").count(),
                human_size(
                    browse
                        .entries
                        .iter()
                        .filter(|entry| entry.kind != "dir")
                        .map(|entry| entry.size_bytes)
                        .sum()
                ),
                short_id(&browse.machine_id),
            )
        } else {
            match (data.root, data.summary) {
            (Some(root), Some(summary)) => format!(
                "Root: {}\nPath: {}\nBrowse: {}\nFiles: {} | Hashed: {} | Current: {} | Marked: {} ({})\nMachine: {} | Set: {}",
                root_display_name(root),
                root.path,
                data.persisted_browse_dir.unwrap_or("."),
                summary.file_count,
                summary.content_count,
                human_size(root.current_size_bytes as u64),
                data.selection.map(|value| value.marked_count).unwrap_or(0),
                human_size(data.selection.map(|value| value.marked_bytes).unwrap_or(0) as u64),
                short_id(&root.machine_id),
                data.selection
                    .map(|value| short_id(&value.set_id))
                    .unwrap_or("-")
            ),
            _ => "Root: -\nPath: -\nBrowse: -\nMachine: - | Files: - | Hashed: - | Current size: -".to_string(),
            }
        };
        let file_lines = if let Some(file) = data.file {
            format!(
                "File: {}\nSize: {} ({} bytes) | Status: {} | Marked: {}\nModified: {} | Content: {} | Metadata: not extracted yet",
                file.relative_path,
                human_size(file.size_bytes as u64),
                file.size_bytes,
                file.status,
                if data.selected_paths.contains(&file.relative_path) {
                    "yes"
                } else {
                    "no"
                },
                file.modified_at.as_deref().unwrap_or("-"),
                file.content_id.as_deref().map(short_id).unwrap_or("stat-only")
            )
        } else {
            "File: -\nSize: - | Status: - | Modified: -\nContent: - | Metadata: not extracted yet"
                .to_string()
        };
        let plan_lines = if let Some(plan) = data.plan {
            format!(
                "Plan: {} | {} | {} -> {}\n{}",
                short_id(&plan.plan_id),
                plan.status,
                truncate(&plan.source_name, 18),
                truncate(&plan.dest_name, 18),
                plan_summary_line(&plan.summary)
            )
        } else {
            "Plan: -\nPress t on a source root, choose a destination root, Enter plans marked files"
                .to_string()
        };
        let collection_lines = data
            .collection
            .map(collection_detail_lines)
            .unwrap_or_default();
        let transfer_lines = data
            .transfer_progress
            .as_ref()
            .map(transfer_progress_lines)
            .unwrap_or_else(|| "Transfer: -".to_string());
        let text =
            format!("{root_lines}\n{file_lines}\n{plan_lines}{collection_lines}\n{transfer_lines}");
        Paragraph::new(text)
            .style(theme::panel_dark())
            .block(panel_block("Details", false))
            .render(area, buf);
    }
}

fn collection_detail_lines(collection: &CollectionSnapshot) -> String {
    format!(
        "\nCollection: {} | {} | entries {}\nAgainst: {} ({})\n{}",
        short_id(&collection.collection_id),
        truncate(&collection.collection_name, 24),
        collection.entries,
        truncate(&collection.root_path, 24),
        short_id(&collection.root_id),
        collection_summary_line(collection)
    )
}

pub(super) struct InfoBar<'a> {
    pub(super) data: InfoBarData<'a>,
    pub(super) state: &'a AppState,
}

impl Widget for InfoBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let root = self.data.root_name.as_deref().unwrap_or("-");
        let file = self
            .data
            .file
            .map(|file| file.relative_path.as_str())
            .unwrap_or("-");
        let event = self
            .data
            .event
            .map(event_summary)
            .unwrap_or_else(|| "event -".to_string());
        let plan_status = self
            .state
            .last_plan
            .as_ref()
            .map(|plan| {
                format!(
                    "copy {} review {}",
                    plan_copy_count(plan),
                    plan_review_count(plan)
                )
            })
            .unwrap_or_else(|| "-".to_string());
        let status = self
            .state
            .retarget_draft
            .as_ref()
            .map(|draft| {
                format!(
                    "retarget {} -> {}",
                    truncate(&draft.relative_path, 18),
                    draft.value
                )
            })
            .unwrap_or_else(|| self.state.status.clone());
        let context = format!(
            "focus {:?} | roots {} | marked {} | plan {} | root {} | file {} | {} | {}",
            self.state.focus,
            self.data.root_count,
            self.data
                .selection
                .map(|value| value.marked_count)
                .unwrap_or(0),
            plan_status,
            truncate(root, 24),
            truncate(file, 20),
            truncate(&event, 24),
            status
        );
        let mut lines = vec![Line::from(context)];
        let mut activity_lines = self
            .state
            .activities
            .iter()
            .rev()
            .take(3)
            .collect::<Vec<_>>();
        activity_lines.reverse();
        for activity in activity_lines {
            lines.push(Line::from(vec![
                Span::styled(activity.level.label(), activity.level.style()),
                Span::raw(" "),
                Span::styled(truncate(&activity.message, 180), activity.level.style()),
            ]));
        }
        Paragraph::new(lines)
            .style(theme::panel())
            .block(panel_block("Info", true))
            .render(area, buf);
    }
}

pub(super) struct ActivityPane<'a> {
    pub(super) state: &'a AppState,
}

impl Widget for ActivityPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut activities = self
            .state
            .activities
            .iter()
            .rev()
            .take(area.height.saturating_sub(2).max(1) as usize)
            .collect::<Vec<_>>();
        activities.reverse();
        let items = if activities.is_empty() {
            vec![ListItem::new("No activity yet").style(theme::muted())]
        } else {
            activities
                .into_iter()
                .map(|activity| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:<4}", activity.level.label()),
                            activity.level.style(),
                        ),
                        Span::raw(" "),
                        Span::styled(truncate(&activity.message, 80), activity.level.style()),
                    ]))
                })
                .collect()
        };
        List::new(items)
            .style(theme::panel())
            .block(panel_block("Activity", false))
            .render(area, buf);
    }
}
