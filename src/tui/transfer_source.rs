use super::*;
pub(super) fn start_transfer_plan_selection(root: Option<&db::RootRow>, state: &mut AppState) {
    let Some(root) = root else {
        state.status = "No source root selected".to_string();
        return;
    };
    state.transfer_source_root_id = Some(root.id.clone());
    state.focus = FocusPane::Roots;
    state.status = format!(
        "transfer source: {}; choose destination root and press Enter",
        root_display_name(root)
    );
}

pub(super) fn start_temporary_transfer_source_import(
    state: &mut AppState,
    selected_file: Option<&FileViewRow>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let Some(browse) = selected_temporary_browse(state) else {
        state.status = "Select a temporary SSH browse root first".to_string();
        return;
    };
    let Some(provider) = browse.import_provider.clone() else {
        state.status = "Import is unavailable for this temporary root".to_string();
        return;
    };
    let target = temporary_transfer_import_target(state.focus, browse, selected_file);
    state.status = format!("importing transfer source {} (fast)", target.remote_path);
    task::spawn_blocking(move || {
        let message = match provider(ImportMode::Fast, &target.remote_path) {
            Ok(result) => {
                if result.files_imported == 0 {
                    TuiMessage::Status(format!(
                        "imported {} but found no files to mark for transfer",
                        result.root_path
                    ))
                } else {
                    TuiMessage::TemporaryTransferSourceImported {
                        root_id: result.root_id,
                        selected_relative_path: target.selected_relative_path,
                        mark_all: target.mark_all,
                        status: format!(
                            "transfer source imported {}; choose destination root and press Enter",
                            result.root_path
                        ),
                    }
                }
            }
            Err(err) => TuiMessage::Status(format!("transfer source import failed: {err}")),
        };
        let _ = job_tx.send(message);
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TemporaryTransferImportTarget {
    pub(super) remote_path: String,
    pub(super) selected_relative_path: Option<String>,
    pub(super) mark_all: bool,
}

pub(super) fn temporary_transfer_import_target(
    focus: FocusPane,
    browse: &TemporaryBrowse,
    selected_file: Option<&FileViewRow>,
) -> TemporaryTransferImportTarget {
    let selected_entry = selected_file.filter(|_| focus == FocusPane::Files);
    let remote_path = selected_entry
        .map(|file| remote_child_path(&browse.current_path, &file.relative_path))
        .unwrap_or_else(|| browse.current_path.clone());
    let selected_relative_path = selected_entry
        .filter(|file| file.kind == FileKind::File)
        .map(|file| file.relative_path.clone());
    TemporaryTransferImportTarget {
        remote_path,
        selected_relative_path,
        mark_all: selected_entry
            .map(|file| file.kind == FileKind::Directory)
            .unwrap_or(true),
    }
}

pub(super) fn mark_imported_transfer_source(
    conn: &Connection,
    root_id: &str,
    selected_relative_path: Option<&str>,
    mark_all: bool,
) -> anyhow::Result<()> {
    let mut already_selected = db::selected_paths_for_root(conn, root_id)?;
    if let Some(path) = selected_relative_path {
        if !already_selected.contains(path) {
            db::toggle_selection_entry(conn, root_id, path)?;
        }
        return Ok(());
    }
    if mark_all {
        for file in db::recent_files_for_root(conn, root_id, i64::MAX)? {
            if already_selected.insert(file.relative_path.clone()) {
                db::toggle_selection_entry(conn, root_id, &file.relative_path)?;
            }
        }
    }
    Ok(())
}

pub(super) fn cancel_transfer_plan_selection(state: &mut AppState) {
    if state.transfer_source_root_id.take().is_some() {
        state.status = "transfer planning canceled".to_string();
    }
}

pub(super) fn create_transfer_plan_from_selection(
    conn: &Connection,
    roots: &[db::RootRow],
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(source_root_id) = state.transfer_source_root_id.clone() else {
        return Ok(());
    };
    let Some(source) = roots.iter().find(|root| root.id == source_root_id) else {
        state.transfer_source_root_id = None;
        state.status = "transfer source root is no longer visible".to_string();
        return Ok(());
    };
    let Some(dest) = roots.get(state.selected_root) else {
        state.status = "No destination root selected".to_string();
        return Ok(());
    };
    match transfer::plan_selected_files(conn, source, dest) {
        Ok(result) => {
            let summary = result.summary.clone();
            let entries = db::transfer_plan_entries(conn, &result.plan_id)?;
            state.last_plan = Some(PlanSnapshot {
                plan_id: result.plan_id.clone(),
                source_root_id: source.id.clone(),
                status: "planned".to_string(),
                source_name: root_display_name(source),
                dest_name: root_display_name(dest),
                summary,
                entries,
            });
            state.transfer_source_root_id = None;
            state.plan_offset = 0;
            state.focus = FocusPane::Plan;
            state.status = format!("planned transfer {}", result.plan_id);
        }
        Err(err) => {
            state.status = format!("transfer plan failed: {err}");
        }
    }
    Ok(())
}

pub(super) fn load_latest_transfer_plan(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.status = "No root selected".to_string();
        return Ok(());
    };
    let Some(plan) = db::recent_transfer_plans(conn, 100)?
        .into_iter()
        .find(|plan| plan.source_root_id == root.id || plan.dest_root_id == root.id)
    else {
        state.status = format!("No transfer plans found for {}", root_display_name(root));
        return Ok(());
    };
    let summary = db::transfer_plan_action_summary(conn, &plan.id)?;
    let entries = db::transfer_plan_entries(conn, &plan.id)?;
    state.last_plan = Some(PlanSnapshot {
        plan_id: plan.id.clone(),
        source_root_id: plan.source_root_id.clone(),
        status: plan.status.clone(),
        source_name: display_name_from_path(&plan.source_path),
        dest_name: display_name_from_path(&plan.dest_path),
        summary,
        entries,
    });
    state.plan_offset = 0;
    state.focus = FocusPane::Plan;
    state.status = format!("loaded transfer plan {}", short_id(&plan.id));
    Ok(())
}
