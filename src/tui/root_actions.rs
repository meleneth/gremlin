use super::*;
pub(super) fn queue_selected_root(
    conn: &Connection,
    db_path: &Path,
    root: Option<&db::RootRow>,
    kind: &str,
    selected_paths: Option<&BTreeSet<String>>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        state.status = "No root selected. Add one with `gremlin /path`.".to_string();
        return Ok(());
    };
    let job_id = queue_root_job(conn, root, kind, selected_paths)?;
    state.background_started_job(job_id.clone(), format!("started {kind} job {job_id}"));
    spawn_job_runner(
        db_path.to_path_buf(),
        job_id,
        kind.to_string(),
        None,
        job_tx,
    );
    Ok(())
}

pub(super) fn queue_or_prompt_selected_root(
    conn: &Connection,
    db_path: &Path,
    root: Option<&db::RootRow>,
    kind: &str,
    selected_paths: &BTreeSet<String>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        state.set_status(
            ActivityLevel::Warning,
            "No root selected. Add one with `gremlin /path`.",
        );
        return Ok(());
    };
    if selected_paths.is_empty() {
        return queue_selected_root(conn, db_path, Some(root), kind, None, job_tx, state);
    }
    state.pending_scoped_job = Some(PendingScopedJob {
        kind: kind.to_string(),
        root_id: root.id.clone(),
    });
    state.focus = FocusPane::Roots;
    state.set_status(
        ActivityLevel::Info,
        format!(
            "{kind}: run against all files or {} marked path(s)?",
            selected_paths.len()
        ),
    );
    Ok(())
}

pub(super) fn handle_scoped_job_choice(
    conn: &Connection,
    db_path: &Path,
    roots: &[db::RootRow],
    selected_paths: &BTreeSet<String>,
    key: KeyCode,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(pending) = state.pending_scoped_job.clone() else {
        return Ok(());
    };
    match key {
        KeyCode::Char('a') | KeyCode::Char('A') => {
            state.pending_scoped_job = None;
            let root = roots.iter().find(|root| root.id == pending.root_id);
            queue_selected_root(conn, db_path, root, &pending.kind, None, job_tx, state)?;
        }
        KeyCode::Char('m') | KeyCode::Char('M') => {
            state.pending_scoped_job = None;
            let root = roots.iter().find(|root| root.id == pending.root_id);
            queue_selected_root(
                conn,
                db_path,
                root,
                &pending.kind,
                Some(selected_paths),
                job_tx,
                state,
            )?;
        }
        KeyCode::Esc => {
            state.pending_scoped_job = None;
            state.set_status(ActivityLevel::Warning, "job scope selection canceled");
        }
        _ => {
            state.set_status(
                ActivityLevel::Info,
                "choose a for all files, m for marked paths, or Esc",
            );
        }
    }
    Ok(())
}

fn queue_root_job(
    conn: &Connection,
    root: &db::RootRow,
    kind: &str,
    selected_paths: Option<&BTreeSet<String>>,
) -> anyhow::Result<String> {
    let mut params = serde_json::json!({ "path": root.path });
    if let Some(selected_paths) = selected_paths.filter(|paths| !paths.is_empty()) {
        params["scope"] = serde_json::json!({
            "mode": "selected_paths",
            "paths": selected_paths.iter().cloned().collect::<Vec<_>>(),
        });
    }
    let job_id = db::create_job(conn, kind, Some(&root.machine_id), Some(&root.id), params)?;
    let event = crate::events::EventEnvelope {
        event_kind: crate::events::EventKind::JobCreated,
        job_id: Some(job_id.clone()),
        sequence: Some(1),
        created_at: crate::util::now_rfc3339(),
        payload: crate::events::EventPayload::Job {
            kind: kind.to_string(),
            path: Some(root.path.clone()),
            message: Some(if selected_paths.is_some_and(|paths| !paths.is_empty()) {
                "queued marked paths".to_string()
            } else {
                "queued".to_string()
            }),
            files_seen: selected_paths.map(|paths| paths.len() as u64),
            errors: None,
        },
    };
    db::persist_event(conn, &event)?;
    Ok(job_id)
}

pub(super) fn request_selected_cancel(
    conn: &Connection,
    selected_event: Option<&db::JobEventRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(event) = selected_event else {
        state.status = "No job selected to cancel".to_string();
        return Ok(());
    };
    if db::request_job_cancel(conn, &event.job_id)? {
        let envelope = crate::events::EventEnvelope {
            event_kind: crate::events::EventKind::JobCancelRequested,
            job_id: Some(event.job_id.clone()),
            sequence: None,
            created_at: crate::util::now_rfc3339(),
            payload: crate::events::EventPayload::Job {
                kind: event.job_kind.clone(),
                path: event.current_path.clone(),
                message: Some("cancel requested from tui".to_string()),
                files_seen: Some(event.files_seen as u64),
                errors: Some(event.errors as u64),
            },
        };
        db::persist_event(conn, &envelope)?;
        state.status = format!("cancel requested for {}", event.job_id);
    } else {
        state.status = format!("job {} is not cancelable", event.job_id);
    }
    Ok(())
}

pub(super) fn export_selected_root_snapshot(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.set_status(
            ActivityLevel::Warning,
            "No persisted root selected to export",
        );
        return Ok(());
    };
    let result = crate::root_snapshot::export_root(conn, root)?;
    state.set_status(
        ActivityLevel::Success,
        format!(
            "exported root {} to {} ({} files)",
            short_id(&root.id),
            result.path.display(),
            result.file_count
        ),
    );
    Ok(())
}

pub(super) fn export_selected_root_sfv(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.set_status(
            ActivityLevel::Warning,
            "No persisted root selected to export SFV",
        );
        return Ok(());
    };
    match crate::sfv::export_root_default_path(conn, root) {
        Ok(result) => {
            let path = result
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            state.set_status(
                ActivityLevel::Success,
                format!(
                    "exported SFV {} to {} ({} files)",
                    short_id(&root.id),
                    path,
                    result.file_count
                ),
            );
        }
        Err(err) => {
            state.set_status(ActivityLevel::Warning, format!("SFV export failed: {err}"));
        }
    }
    Ok(())
}

pub(super) fn export_current_directory_sfv(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.set_status(
            ActivityLevel::Warning,
            "No persisted root selected to export SFV",
        );
        return Ok(());
    };
    let relative_dir = current_persisted_root_dir(state, &root.id).to_string();
    match crate::sfv::export_directory_default_path(conn, root, &relative_dir) {
        Ok(result) => {
            state.clear_file_list_cache();
            let path = result
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            state.set_status(
                ActivityLevel::Success,
                format!(
                    "exported directory SFV {} to {} ({} files)",
                    if relative_dir == "." {
                        root_display_name(root)
                    } else {
                        relative_dir
                    },
                    path,
                    result.file_count
                ),
            );
        }
        Err(err) => {
            state.set_status(ActivityLevel::Warning, format!("SFV export failed: {err}"));
        }
    }
    Ok(())
}

pub(super) fn queue_selected_sfv_verify(
    conn: &Connection,
    db_path: &Path,
    selected_root: Option<&db::RootRow>,
    selected_file: Option<&FileViewRow>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<bool> {
    let Some(root) = selected_root else {
        return Ok(false);
    };
    let Some(file) = selected_file else {
        return Ok(false);
    };
    if file.kind != FileKind::File || !file.relative_path.to_ascii_lowercase().ends_with(".sfv") {
        return Ok(false);
    }
    let root_path = PathBuf::from(&root.path);
    if !root_path.is_dir() {
        state.set_status(
            ActivityLevel::Warning,
            format!("SFV verify needs a local directory root: {}", root.path),
        );
        return Ok(true);
    }
    let job_id = db::create_job(
        conn,
        "sfv_verify",
        Some(&root.machine_id),
        Some(&root.id),
        serde_json::json!({
            "path": root.path.clone(),
            "sfv": file.relative_path.clone(),
        }),
    )?;
    let event = crate::events::EventEnvelope {
        event_kind: crate::events::EventKind::JobCreated,
        job_id: Some(job_id.clone()),
        sequence: Some(1),
        created_at: crate::util::now_rfc3339(),
        payload: crate::events::EventPayload::Job {
            kind: "sfv_verify".to_string(),
            path: Some(file.relative_path.clone()),
            message: Some("queued".to_string()),
            files_seen: None,
            errors: None,
        },
    };
    db::persist_event(conn, &event)?;
    state.background_started_job(job_id.clone(), format!("started SFV verify job {job_id}"));
    spawn_sfv_verify_runner(
        db_path.to_path_buf(),
        job_id,
        root_path,
        file.relative_path.clone(),
        job_tx,
    );
    Ok(true)
}

fn spawn_sfv_verify_runner(
    db_path: PathBuf,
    job_id: String,
    root_path: PathBuf,
    sfv_relative_path: String,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
) {
    task::spawn_blocking(move || {
        let status = match db::open_existing(&db_path).and_then(|conn| {
            crate::sfv_verify::verify_job(&conn, &job_id, &root_path, &sfv_relative_path)
        }) {
            Ok(summary) if summary.canceled => {
                format!("SFV verify canceled: {} entries", summary.entries)
            }
            Ok(summary) if summary.mismatched > 0 || summary.missing > 0 || summary.errors > 0 => {
                format!(
                    "SFV verify completed with errors: {} ok, {} mismatched, {} missing, {} errors",
                    summary.ok, summary.mismatched, summary.missing, summary.errors
                )
            }
            Ok(summary) => format!("SFV verify completed: {} ok", summary.ok),
            Err(err) => format!("SFV verify failed: {err}"),
        };
        let _ = job_tx.send(TuiMessage::JobFinished { job_id, status });
    });
}

pub(super) fn toggle_selected_file_mark(
    conn: &Connection,
    selected_root: Option<&db::RootRow>,
    selected_file: Option<&FileViewRow>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = selected_root else {
        state.status = "No root selected".to_string();
        return Ok(());
    };
    let Some(file) = selected_file else {
        state.status = "No file selected".to_string();
        return Ok(());
    };
    match file.kind {
        FileKind::Directory => {
            let change = db::toggle_selection_directory(conn, &root.id, &file.relative_path)?;
            state.clear_file_list_cache();
            state.status = if change.files_changed == 0 {
                format!("{} has no indexed files to mark", file.relative_path)
            } else if change.selected {
                format!(
                    "marked {} files under {} ({})",
                    change.files_changed,
                    file.relative_path,
                    human_size(change.bytes_changed)
                )
            } else {
                format!(
                    "unmarked {} files under {} ({})",
                    change.files_changed,
                    file.relative_path,
                    human_size(change.bytes_changed)
                )
            };
            return Ok(());
        }
        FileKind::Section => {
            state.status = format!("selection group {}", file.relative_path);
            return Ok(());
        }
        FileKind::File => {}
    }
    let marked = db::toggle_selection_entry(conn, &root.id, &file.relative_path)?;
    state.clear_file_list_cache();
    state.status = if marked {
        format!("marked {}", file.relative_path)
    } else {
        format!("unmarked {}", file.relative_path)
    };
    Ok(())
}
