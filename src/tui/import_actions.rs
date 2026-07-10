use super::*;
use std::path::PathBuf;
pub(super) fn start_delete_root_confirmation(
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) {
    let Some(root) = selected_root else {
        state.status = "No persisted root selected to remove".to_string();
        return;
    };
    state.pending_delete_root_id = Some(root.id.clone());
    state.status = format!(
        "Remove root {} from database? y confirms, n/Esc cancels; files stay on disk",
        root_display_name(root)
    );
}

pub(super) fn handle_delete_root_confirmation(
    conn: &Connection,
    state: &mut AppState,
    code: KeyCode,
) -> anyhow::Result<()> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(root_id) = state.pending_delete_root_id.take() else {
                return Ok(());
            };
            match db::delete_root(conn, &root_id)? {
                Some(summary) => {
                    state.selected_root = state.selected_root.saturating_sub(1);
                    state.file_offset = 0;
                    state.event_offset = 0;
                    if state
                        .transfer_plan_draft
                        .as_ref()
                        .is_some_and(|draft| draft.source_root_id == root_id)
                    {
                        state.transfer_plan_draft = None;
                    }
                    state.last_plan = None;
                    state.status = format!(
                        "removed root {} ({} observations, {} plans); files untouched",
                        short_id(&summary.root_id),
                        summary.path_observations,
                        summary.transfer_plans
                    );
                }
                None => {
                    state.status = format!("root {} was already gone", short_id(&root_id));
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.pending_delete_root_id = None;
            state.status = "root removal canceled".to_string();
        }
        _ => {
            state.status = "Confirm root removal with y, or cancel with n/Esc".to_string();
        }
    }
    Ok(())
}

pub(super) fn start_temporary_import_prompt(
    state: &mut AppState,
    selected_file: Option<&FileViewRow>,
) {
    let Some(browse) = selected_temporary_browse(state) else {
        state.status = "Select a temporary SSH browse root to import".to_string();
        return;
    };
    if browse.import_provider.is_none() {
        state.status = "Import is unavailable for this temporary root".to_string();
        return;
    }
    let selected_entry = selected_file.filter(|_| state.focus == FocusPane::Files);
    let remote_path = selected_entry
        .map(|file| remote_child_path(&browse.current_path, &file.relative_path))
        .unwrap_or_else(|| browse.current_path.clone());
    let target_kind = selected_entry
        .map(|file| {
            if file.kind == FileKind::Directory {
                "directory"
            } else {
                "file"
            }
        })
        .unwrap_or("directory");
    state.pending_import = Some(PendingTemporaryImport {
        remote_path: remote_path.clone(),
    });
    state.status = format!(
        "Import remote {target_kind} {remote_path}? n=root only, f=fast recursive stat, h=remote hash, Esc cancels"
    );
}

pub(super) fn start_selected_snapshot_import(
    state: &mut AppState,
    selected_root: Option<&db::RootRow>,
    selected_file: Option<&FileViewRow>,
    open_root_provider: OpenRootProvider,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) -> bool {
    let Some(root) = selected_root else {
        return false;
    };
    let Some(file) = selected_file.filter(|_| state.focus == FocusPane::Files) else {
        return false;
    };
    if file.kind != FileKind::File
        || !file
            .relative_path
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("json"))
    {
        return false;
    }
    let snapshot_path = PathBuf::from(&root.path).join(&file.relative_path);
    state.background_started(format!(
        "importing root snapshot {}",
        snapshot_path.display()
    ));
    task::spawn_blocking(move || {
        let result = open_root_provider(&snapshot_path.to_string_lossy()).map_err(|err| {
            format!(
                "snapshot import failed for {}: {err}",
                snapshot_path.display()
            )
        });
        let _ = job_tx.send(TuiMessage::OpenRootFinished(result));
    });
    true
}

pub(super) fn handle_temporary_import_choice(
    state: &mut AppState,
    code: KeyCode,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    let mode = match code {
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ImportMode::No),
        KeyCode::Char('f') | KeyCode::Char('F') => Some(ImportMode::Fast),
        KeyCode::Char('h') | KeyCode::Char('H') => Some(ImportMode::Hash),
        KeyCode::Esc => {
            state.pending_import = None;
            state.status = "import canceled".to_string();
            return;
        }
        _ => {
            state.status = "Choose n root-only, f fast stat, h remote hash, or Esc".to_string();
            return;
        }
    };
    let Some(mode) = mode else {
        return;
    };
    let Some(pending) = state.pending_import.take() else {
        state.status = "No pending import".to_string();
        return;
    };
    let Some(provider) =
        selected_temporary_browse(state).and_then(|browse| browse.import_provider.clone())
    else {
        state.status = "No temporary root selected".to_string();
        return;
    };
    let remote_path = pending.remote_path;
    state.background_started(format!(
        "importing {remote_path} ({})",
        import_mode_label(mode)
    ));
    let progress_tx = job_tx.clone();
    let progress: ImportProgressCallback = Arc::new(move |progress| {
        let _ = progress_tx.send(TuiMessage::ImportProgress(progress));
    });
    task::spawn_blocking(move || {
        let status = match provider(mode, &remote_path, progress) {
            Ok(result) => format!(
                "imported {} as root {} ({}, {} files)",
                result.root_path,
                short_id(&result.root_id),
                import_mode_label(result.mode),
                result.files_imported
            ),
            Err(err) => format!("import failed: {err}"),
        };
        let _ = job_tx.send(TuiMessage::ImportFinished(status));
    });
}

pub(super) fn import_mode_label(mode: ImportMode) -> &'static str {
    match mode {
        ImportMode::No => "root only",
        ImportMode::Fast => "fast",
        ImportMode::Hash => "hash",
    }
}
