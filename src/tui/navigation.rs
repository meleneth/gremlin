use super::*;
pub(super) fn normalize_selection(state: &mut AppState, root_count: usize) {
    if root_count == 0 {
        state.selected_root = 0;
    } else if state.selected_root >= root_count {
        state.selected_root = root_count - 1;
    }
}

pub(super) fn visible_root_count(state: &AppState, persisted_count: usize) -> usize {
    persisted_count + usize::from(state.temporary_browse.is_some())
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
    roots.get(persisted_index_for_visible(state)?)
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

pub(super) fn open_temporary_file_entry(state: &mut AppState, selected_file: Option<&FileViewRow>) {
    let Some(file) = selected_file else {
        state.status = "No remote entry selected".to_string();
        return;
    };
    if file.kind != FileKind::Directory {
        state.status = format!("selected remote file {}", file.relative_path);
        return;
    }
    let Some(current) = state
        .temporary_browse
        .as_ref()
        .map(|browse| browse.current_path.clone())
    else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    let next_path = remote_child_path(&current, &file.relative_path);
    open_temporary_path(state, next_path);
}

pub(super) fn open_temporary_parent(state: &mut AppState) {
    let Some(browse) = state.temporary_browse.as_ref() else {
        state.status = "No temporary browse root selected".to_string();
        return;
    };
    if browse.current_path == browse.root_path {
        state.status = "Already at temporary root".to_string();
        return;
    }
    let Some(parent) = remote_parent_path(&browse.current_path, &browse.root_path) else {
        state.status = "Already at temporary root".to_string();
        return;
    };
    open_temporary_path(state, parent);
}

pub(super) fn open_temporary_path(state: &mut AppState, next_path: String) {
    let Some(provider) = state
        .temporary_browse
        .as_ref()
        .and_then(|browse| browse.browse_provider.clone())
    else {
        state.status = "Remote browsing is unavailable for this temporary root".to_string();
        return;
    };
    match provider(&next_path) {
        Ok(entries) => {
            if let Some(browse) = state.temporary_browse.as_mut() {
                browse.current_path = next_path.clone();
                browse.entries = entries;
            }
            state.file_offset = 0;
            state.status = format!("browsing {next_path}");
        }
        Err(err) => {
            state.status = format!("remote browse failed: {err}");
        }
    }
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
