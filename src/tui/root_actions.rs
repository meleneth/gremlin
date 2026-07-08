use super::*;
pub(super) fn queue_selected_root(
    conn: &Connection,
    db_path: &Path,
    root: Option<&db::RootRow>,
    kind: &str,
    machine_label: Option<&str>,
    job_tx: mpsc::UnboundedSender<TuiMessage>,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        state.status = "No root selected. Add one with `gremlin /path`.".to_string();
        return Ok(());
    };
    let job_id = db::queue_file_job(conn, kind, std::path::Path::new(&root.path), machine_label)?;
    state.status = format!("started {kind} job {job_id}");
    spawn_job_runner(
        db_path.to_path_buf(),
        job_id,
        kind.to_string(),
        machine_label.map(str::to_string),
        job_tx,
    );
    Ok(())
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
    if file.kind == FileKind::Directory {
        let change = db::toggle_selection_directory(conn, &root.id, &file.relative_path)?;
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
    let marked = db::toggle_selection_entry(conn, &root.id, &file.relative_path)?;
    state.status = if marked {
        format!("marked {}", file.relative_path)
    } else {
        format!("unmarked {}", file.relative_path)
    };
    Ok(())
}
