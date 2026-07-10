use super::*;
pub(super) fn normalize_selection(state: &mut AppState, root_count: usize) {
    if root_count == 0 {
        state.selected_root = 0;
    } else if state.selected_root >= root_count {
        state.selected_root = root_count - 1;
    }
}

pub(super) fn visible_root_count(state: &AppState, persisted_count: usize) -> usize {
    persisted_count
        + usize::from(state.temporary_browse.is_some())
        + state.resumable_transfer_plans.len()
}

pub(super) fn visible_index_for_persisted(state: &AppState, persisted_idx: usize) -> usize {
    persisted_idx + usize::from(state.temporary_browse.is_some())
}

pub(super) fn persisted_index_for_visible(state: &AppState) -> Option<usize> {
    if state.temporary_browse.is_some() {
        state.selected_root.checked_sub(1)
    } else {
        Some(state.selected_root)
    }
}

pub(super) fn selected_persisted_root<'a>(
    roots: &'a [db::RootRow],
    state: &AppState,
) -> Option<&'a db::RootRow> {
    let persisted_idx = persisted_index_for_visible(state)?;
    if persisted_idx >= roots.len() {
        return None;
    }
    roots.get(persisted_idx)
}

pub(super) fn selected_resume_plan(
    state: &AppState,
    persisted_count: usize,
) -> Option<&db::TransferPlanRow> {
    let first_resume = persisted_count + usize::from(state.temporary_browse.is_some());
    let resume_idx = state.selected_root.checked_sub(first_resume)?;
    state.resumable_transfer_plans.get(resume_idx)
}

pub(super) fn selected_temporary_browse(state: &AppState) -> Option<&TemporaryBrowse> {
    state
        .temporary_browse
        .as_ref()
        .filter(|_| state.selected_root == 0)
}

pub(super) fn selected_root_name(
    root: Option<&db::RootRow>,
    browse: Option<&TemporaryBrowse>,
) -> Option<String> {
    browse
        .map(|browse| format!("{}:{}", browse.label, browse.current_path))
        .or_else(|| root.map(root_display_name))
}

pub(super) fn filtered_root_rows(roots: &[db::RootRow], filter: &str) -> Vec<db::RootRow> {
    let needle = filter.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return roots.to_vec();
    }
    roots
        .iter()
        .filter(|root| root_matches_filter(root, &needle))
        .cloned()
        .collect()
}

fn root_matches_filter(root: &db::RootRow, needle: &str) -> bool {
    root_display_name(root)
        .to_ascii_lowercase()
        .contains(needle)
        || root.path.to_ascii_lowercase().contains(needle)
        || root.machine_id.to_ascii_lowercase().contains(needle)
        || root
            .label
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(needle)
        || root
            .latest_job_kind
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(needle)
        || root
            .latest_job_status
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(needle)
}

pub(super) fn filtered_file_rows(files: &[FileViewRow], filter: &str) -> Vec<FileViewRow> {
    let needle = filter.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return files.to_vec();
    }
    files
        .iter()
        .filter(|file| file_matches_filter(file, &needle))
        .cloned()
        .collect()
}

fn file_matches_filter(file: &FileViewRow, needle: &str) -> bool {
    file.relative_path.to_ascii_lowercase().contains(needle)
        || file.status.to_ascii_lowercase().contains(needle)
        || file
            .modified_at
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(needle)
        || file
            .content_id
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains(needle)
}

pub(super) fn normalize_root_filter_selection(state: &mut AppState) {
    state.selected_root = usize::from(state.temporary_browse.is_some());
    state.file_offset = 0;
    state.event_offset = 0;
}

pub(super) fn detail_selection_key(
    root: Option<&db::RootRow>,
    browse: Option<&TemporaryBrowse>,
    persisted_dir: Option<&str>,
) -> String {
    if let Some(browse) = browse {
        format!("temporary:{}:{}", browse.machine_id, browse.current_path)
    } else if let Some(root) = root {
        format!(
            "root:{}:{}:{}",
            root.id,
            persisted_dir.unwrap_or("."),
            root.latest_job_status.as_deref().unwrap_or("-")
        )
    } else {
        "none".to_string()
    }
}

pub(super) fn handle_root_filter_input(state: &mut AppState, key: KeyCode) -> bool {
    match key {
        KeyCode::Enter => {
            state.root_filter_editing = false;
            if state.root_filter.trim().is_empty() {
                state.root_filter.clear();
                state.status = "root filter cleared".to_string();
            } else {
                state.status = format!("root filter: {}", state.root_filter);
            }
            true
        }
        KeyCode::Esc => {
            state.root_filter_editing = false;
            state.root_filter.clear();
            normalize_root_filter_selection(state);
            state.status = "root filter cleared".to_string();
            true
        }
        KeyCode::Backspace => {
            state.root_filter.pop();
            normalize_root_filter_selection(state);
            state.status = if state.root_filter.is_empty() {
                "root filter cleared".to_string()
            } else {
                format!("root filter: {}", state.root_filter)
            };
            true
        }
        KeyCode::Char(ch) => {
            if !ch.is_control() {
                state.root_filter.push(ch);
                normalize_root_filter_selection(state);
                state.status = format!("root filter: {}", state.root_filter);
                return true;
            }
            false
        }
        _ => false,
    }
}

pub(super) fn current_persisted_root_dir<'a>(state: &'a AppState, root_id: &str) -> &'a str {
    state
        .root_browse_dirs
        .get(root_id)
        .map(String::as_str)
        .unwrap_or(".")
}

pub(super) fn open_persisted_file_entry(
    state: &mut AppState,
    root_id: Option<&str>,
    selected_file: Option<&FileViewRow>,
) {
    let Some(root_id) = root_id else {
        state.status = "No persisted root selected".to_string();
        return;
    };
    let Some(file) = selected_file else {
        state.status = "No indexed entry selected".to_string();
        return;
    };
    if file.kind != FileKind::Directory {
        state.status = format!("selected indexed file {}", file.relative_path);
        return;
    }
    state
        .root_browse_dirs
        .insert(root_id.to_string(), file.relative_path.clone());
    state.file_offset = 0;
    state.status = format!("browsing {}", file.relative_path);
}

pub(super) fn open_persisted_parent(state: &mut AppState, root_id: Option<&str>) {
    let Some(root_id) = root_id else {
        state.status = "No persisted root selected".to_string();
        return;
    };
    let current = current_persisted_root_dir(state, root_id).to_string();
    if current == "." {
        state.status = "Already at root".to_string();
        return;
    }
    let parent = current
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "." } else { parent })
        .unwrap_or(".");
    if parent == "." {
        state.root_browse_dirs.remove(root_id);
    } else {
        state
            .root_browse_dirs
            .insert(root_id.to_string(), parent.to_string());
    }
    state.file_offset = 0;
    state.status = if parent == "." {
        "browsing root".to_string()
    } else {
        format!("browsing {parent}")
    };
}

pub(super) fn remote_child_path(root_path: &str, child_path: &str) -> String {
    let child = child_path.trim().trim_matches('/');
    if child.is_empty() || child == "." {
        return root_path.to_string();
    }
    if root_path == "~" {
        format!("~/{child}")
    } else {
        format!("{}/{}", root_path.trim_end_matches('/'), child)
    }
}

pub(super) fn remote_parent_path(current_path: &str, root_path: &str) -> Option<String> {
    if current_path == root_path {
        return None;
    }
    if current_path.starts_with("~/") {
        let parent = current_path.rsplit_once('/').map(|(parent, _)| parent)?;
        return Some(if parent == "~" || parent.is_empty() {
            "~".to_string()
        } else {
            parent.to_string()
        });
    }
    let parent = current_path.trim_end_matches('/').rsplit_once('/')?.0;
    let parent = if parent.is_empty() { "/" } else { parent };
    if root_path != "/" && !parent.starts_with(root_path.trim_end_matches('/')) {
        Some(root_path.to_string())
    } else {
        Some(parent.to_string())
    }
}

pub(super) fn move_down(
    state: &mut AppState,
    root_count: usize,
    file_count: usize,
    plan_count: usize,
    event_count: usize,
) {
    match state.focus {
        FocusPane::Roots => {
            if state.selected_root + 1 < root_count {
                state.selected_root += 1;
                state.file_offset = 0;
                state.event_offset = 0;
            }
        }
        FocusPane::Files => {
            if state.file_offset + 1 < file_count {
                state.file_offset += 1;
            }
        }
        FocusPane::Plan => {
            if state.plan_offset + 1 < plan_count {
                state.plan_offset += 1;
            }
        }
        FocusPane::Events => {
            if state.event_offset + 1 < event_count {
                state.event_offset += 1;
            }
        }
    }
}

pub(super) fn active_plan_row_count(state: &AppState) -> usize {
    state
        .collection_result
        .as_ref()
        .map(|collection| collection.rows.len())
        .or_else(|| state.last_plan.as_ref().map(|plan| plan.entries.len()))
        .unwrap_or(0)
}

pub(super) fn move_up(state: &mut AppState) {
    match state.focus {
        FocusPane::Roots => {
            if state.selected_root > 0 {
                state.selected_root -= 1;
                state.file_offset = 0;
                state.event_offset = 0;
            }
        }
        FocusPane::Files => {
            state.file_offset = state.file_offset.saturating_sub(1);
        }
        FocusPane::Plan => {
            state.plan_offset = state.plan_offset.saturating_sub(1);
        }
        FocusPane::Events => {
            state.event_offset = state.event_offset.saturating_sub(1);
        }
    }
}

pub(super) fn move_page_down(
    state: &mut AppState,
    root_count: usize,
    file_count: usize,
    plan_count: usize,
    event_count: usize,
    page_len: usize,
) {
    let jump = page_len.max(1);
    match state.focus {
        FocusPane::Roots => {
            state.selected_root = page_target(state.selected_root, root_count, jump);
            state.file_offset = 0;
            state.event_offset = 0;
        }
        FocusPane::Files => {
            state.file_offset = page_target(state.file_offset, file_count, jump);
        }
        FocusPane::Plan => {
            state.plan_offset = page_target(state.plan_offset, plan_count, jump);
        }
        FocusPane::Events => {
            state.event_offset = page_target(state.event_offset, event_count, jump);
        }
    }
}

pub(super) fn move_page_up(state: &mut AppState, page_len: usize) {
    let jump = page_len.max(1);
    match state.focus {
        FocusPane::Roots => {
            state.selected_root = state.selected_root.saturating_sub(jump);
            state.file_offset = 0;
            state.event_offset = 0;
        }
        FocusPane::Files => {
            state.file_offset = state.file_offset.saturating_sub(jump);
        }
        FocusPane::Plan => {
            state.plan_offset = state.plan_offset.saturating_sub(jump);
        }
        FocusPane::Events => {
            state.event_offset = state.event_offset.saturating_sub(jump);
        }
    }
}

fn page_target(current: usize, count: usize, jump: usize) -> usize {
    if count == 0 {
        0
    } else {
        current.saturating_add(jump).min(count - 1)
    }
}

pub(super) fn visible_file_page_len(area_height: u16) -> usize {
    area_height.saturating_sub(32).max(1) as usize
}

pub(super) fn normalize_file_offset(state: &mut AppState, file_count: usize) {
    if file_count == 0 {
        state.file_offset = 0;
    } else if state.file_offset >= file_count {
        state.file_offset = file_count - 1;
    }
}

pub(super) fn handle_file_filter_input(state: &mut AppState, key: KeyCode) -> bool {
    match key {
        KeyCode::Enter => {
            state.file_filter_editing = false;
            if state.file_filter.trim().is_empty() {
                state.file_filter.clear();
                state.status = "file filter cleared".to_string();
            } else {
                state.status = format!("file filter: {}", state.file_filter);
            }
            true
        }
        KeyCode::Esc => {
            state.file_filter_editing = false;
            state.file_filter.clear();
            state.file_offset = 0;
            state.status = "file filter cleared".to_string();
            true
        }
        KeyCode::Backspace => {
            state.file_filter.pop();
            state.file_offset = 0;
            state.status = if state.file_filter.is_empty() {
                "file filter cleared".to_string()
            } else {
                format!("file filter: {}", state.file_filter)
            };
            true
        }
        KeyCode::Char(ch) => {
            if !ch.is_control() {
                state.file_filter.push(ch);
                state.file_offset = 0;
                state.status = format!("file filter: {}", state.file_filter);
                return true;
            }
            false
        }
        _ => false,
    }
}
