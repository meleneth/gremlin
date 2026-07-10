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
            let hash_lines = file_hash_lines(data.content);
            let appearance_lines = file_appearance_lines(data.appearances);
            format!(
                "File: {}\nSize: {} ({} bytes) | Status: {} | Marked: {}\nModified: {} | Content: {}\n{}{}Metadata: not extracted yet",
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
                file.content_id.as_deref().map(short_id).unwrap_or("stat-only"),
                hash_lines,
                appearance_lines
            )
        } else {
            "File: -\nSize: - | Status: - | Modified: -\nContent: -\nHashes: -\nAppearances: -\nMetadata: not extracted yet"
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
        let mut lines = detail_text_lines(format!(
            "{root_lines}\n{file_lines}\n{plan_lines}{collection_lines}"
        ));
        match data.transfer_progress.as_ref() {
            Some(progress) => lines.push(Line::from(format!(
                "Transfer: active in Info | {}",
                truncate(&progress.current_path, 72)
            ))),
            None => lines.push(Line::from("Transfer: -")),
        }
        if let Some(progress) = data.import_progress {
            lines.extend(import_progress_lines(progress));
        }
        Paragraph::new(lines)
            .style(theme::panel_dark())
            .block(panel_block("Details", false))
            .render(area, buf);
    }
}

fn import_progress_lines(progress: &ImportProgress) -> Vec<Line<'static>> {
    vec![
        Line::from(format!(
            "Import: {} | {}",
            progress.phase,
            truncate(&progress.root_path, 48)
        )),
        Line::from(format!(
            "Import files: {} processed | {} queued",
            progress.files_imported, progress.files_queued
        )),
        Line::from(format!(
            "Import dirs: {} processed | {} queued",
            progress.directories_processed, progress.directories_queued
        )),
        Line::from(format!(
            "Import current: {}",
            progress.current_path.as_deref().unwrap_or("-")
        )),
    ]
}

fn detail_text_lines(text: String) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| Line::from(line.to_string()))
        .collect()
}

fn progress_animation_phase() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 160) as usize)
        .unwrap_or(0)
}

fn file_hash_lines(content: Option<&db::ContentObjectRow>) -> String {
    let Some(content) = content else {
        return "Hashes: -\n".to_string();
    };
    format!(
        "BLAKE3: {}\nSHA-256: {}\n",
        content.blake3.as_deref().unwrap_or("-"),
        content.sha256.as_deref().unwrap_or("-")
    )
}

fn file_appearance_lines(appearances: &[db::FileAppearanceRow]) -> String {
    if appearances.is_empty() {
        return "Appearances: -\n".to_string();
    }
    let mut lines = vec![format!("Appearances: {}", appearances.len())];
    lines.extend(appearances.iter().take(8).map(|appearance| {
        format!(
            "- {} {}:{} | {} | {}",
            short_id(&appearance.root_id),
            truncate(&appearance_root_label(appearance), 24),
            truncate(&appearance.relative_path, 52),
            human_size(appearance.size_bytes),
            appearance
                .content_id
                .as_deref()
                .map(short_id)
                .or(appearance.modified_at.as_deref())
                .unwrap_or("stat-only")
        )
    }));
    if appearances.len() > 8 {
        lines.push(format!("... {} more", appearances.len() - 8));
    }
    format!("{}\n", lines.join("\n"))
}

fn appearance_root_label(appearance: &db::FileAppearanceRow) -> String {
    appearance
        .root_label
        .clone()
        .unwrap_or_else(|| appearance.root_path.clone())
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
        if let Some(progress) = self.data.transfer_progress.as_ref() {
            lines.push(Line::from(format!(
                "Transfer file: {}",
                truncate(&progress.current_path, 120)
            )));
            lines.extend(transfer_progress_styled_lines(
                progress,
                progress_animation_phase(),
            ));
        }
        let execution = active_execution_line(self.state);
        if let Some(execution) = execution.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("run ", theme::ok()),
                Span::styled(execution.clone(), theme::ok()),
            ]));
        }
        if let Some(progress) = self.data.import_progress {
            lines.push(Line::from(brief_import_execution_line(progress)));
        }
        let mut activity_lines = self
            .state
            .activities
            .iter()
            .rev()
            .take(info_activity_count(
                self.data.transfer_progress.is_some(),
                execution.is_some(),
                self.data.import_progress.is_some(),
            ))
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

fn active_execution_line(state: &AppState) -> Option<String> {
    let mut parts = Vec::new();
    if state.active_background_jobs > 0 {
        parts.push(format!("bg {}", state.active_background_jobs));
    }
    if !state.active_job_ids.is_empty() {
        let ids = state
            .active_job_ids
            .iter()
            .take(3)
            .map(|id| short_id(id))
            .collect::<Vec<_>>()
            .join(",");
        let suffix = if state.active_job_ids.len() > 3 {
            format!("+{}", state.active_job_ids.len() - 3)
        } else {
            String::new()
        };
        parts.push(format!("jobs {ids}{suffix}"));
    }
    if state.active_file_appearance_key.is_some() {
        parts.push("index lookup".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" | "))
}

fn brief_import_execution_line(progress: &ImportProgress) -> String {
    format!(
        "Import {} | files {} queued {} | dirs {} queued {} | {}",
        truncate(&progress.phase, 18),
        progress.files_imported,
        progress.files_queued,
        progress.directories_processed,
        progress.directories_queued,
        truncate(progress.current_path.as_deref().unwrap_or("-"), 50)
    )
}

fn info_activity_count(has_transfer: bool, has_execution: bool, has_import: bool) -> usize {
    let reserved =
        usize::from(has_transfer) * 3 + usize::from(has_execution) + usize::from(has_import);
    3_usize.saturating_sub(reserved)
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
            .block(panel_block("Activity Log", false))
            .render(area, buf);
    }
}
