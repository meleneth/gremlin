use super::*;
pub fn plan_selected_files(
    conn: &Connection,
    source_root: &RootRow,
    dest_root: &RootRow,
) -> anyhow::Result<TransferPlanResult> {
    if source_root.id == dest_root.id {
        anyhow::bail!("source and destination roots are the same root");
    }

    let selection = db::selection_summary_for_root(conn, &source_root.id)?;
    let selected = db::selected_paths_for_root(conn, &source_root.id)?;
    if selected.is_empty() {
        anyhow::bail!("source root has no marked files; mark files in the TUI with Space first");
    }

    let job_id = db::create_job(
        conn,
        "transfer_plan",
        Some(&source_root.machine_id),
        Some(&source_root.id),
        serde_json::json!({
            "source_root_id": source_root.id,
            "source_path": source_root.path,
            "dest_root_id": dest_root.id,
            "dest_path": dest_root.path,
            "selection_set_id": selection.set_id,
            "marked_count": selection.marked_count,
            "marked_bytes": selection.marked_bytes,
        }),
    )?;
    persist_job_event(
        conn,
        &job_id,
        JobEventInput {
            event_kind: EventKind::JobCreated,
            kind: "transfer_plan",
            path: Some(&source_root.path),
            message: "transfer planning queued",
            files_seen: Some(selected.len() as u64),
            errors: 0,
        },
    )?;
    db::start_job(conn, &job_id)?;
    persist_job_event(
        conn,
        &job_id,
        JobEventInput {
            event_kind: EventKind::JobStarted,
            kind: "transfer_plan",
            path: Some(&source_root.path),
            message: "transfer planning started",
            files_seen: Some(selected.len() as u64),
            errors: 0,
        },
    )?;

    let plan_result =
        build_transfer_plan(conn, source_root, dest_root, &selection, &selected, &job_id);

    match plan_result {
        Ok((plan_id, summary)) => {
            db::update_job_progress(
                conn,
                &job_id,
                db::JobProgressInput {
                    phase: "planned",
                    current_path: None,
                    files_total: Some(selected.len() as u64),
                    files_seen: selected.len() as u64,
                    files_done: selected.len() as u64,
                    files_skipped: 0,
                    errors: 0,
                },
            )?;
            let progress = EventEnvelope {
                event_kind: EventKind::JobProgress,
                job_id: Some(job_id.clone()),
                sequence: None,
                created_at: now_rfc3339(),
                payload: EventPayload::JobProgress {
                    phase: "planned".to_string(),
                    current_path: None,
                    files_total: Some(selected.len() as u64),
                    files_seen: selected.len() as u64,
                    files_done: selected.len() as u64,
                    files_skipped: 0,
                    errors: 0,
                    bytes_done: None,
                    bytes_total: None,
                    file_bytes_done: None,
                    file_bytes_total: None,
                    bytes_per_second: None,
                    message: Some(format!("transfer plan {plan_id} created")),
                },
            };
            db::persist_event(conn, &progress)?;
            db::complete_job(conn, &job_id, "completed")?;
            persist_job_event(
                conn,
                &job_id,
                JobEventInput {
                    event_kind: EventKind::JobCompleted,
                    kind: "transfer_plan",
                    path: Some(&source_root.path),
                    message: &format!("transfer plan {plan_id} created"),
                    files_seen: Some(selected.len() as u64),
                    errors: 0,
                },
            )?;
            Ok(TransferPlanResult {
                plan_id,
                job_id,
                selection_set_id: selection.set_id,
                marked_count: selection.marked_count,
                marked_bytes: selection.marked_bytes,
                summary,
            })
        }
        Err(err) => {
            let _ = db::complete_job(conn, &job_id, "failed");
            let _ = persist_job_event(
                conn,
                &job_id,
                JobEventInput {
                    event_kind: EventKind::JobFailed,
                    kind: "transfer_plan",
                    path: Some(&source_root.path),
                    message: &err.to_string(),
                    files_seen: Some(selected.len() as u64),
                    errors: 1,
                },
            );
            Err(err)
        }
    }
}

fn build_transfer_plan(
    conn: &Connection,
    source_root: &RootRow,
    dest_root: &RootRow,
    selection: &db::SelectionSummary,
    selected: &BTreeSet<String>,
    job_id: &str,
) -> anyhow::Result<(String, Vec<TransferPlanActionSummary>)> {
    let dest_endpoint = root_transfer_endpoint(conn, dest_root)?;
    let mut probe_cache = DestinationProbeCache::default();
    let plan_id = db::create_transfer_plan(
        conn,
        Some(job_id),
        &source_root.id,
        &dest_root.id,
        Some(&selection.set_id),
        serde_json::json!({
            "source_root_id": source_root.id,
            "source_path": source_root.path,
            "dest_root_id": dest_root.id,
            "dest_path": dest_root.path,
            "selection_set_id": selection.set_id,
        }),
    )?;

    for relative_path in selected {
        let source = db::path_observation_for_root_path(conn, &source_root.id, relative_path)?;
        let Some(source) = source else {
            db::insert_transfer_plan_entry(
                conn,
                db::TransferPlanEntryInput {
                    plan_id: &plan_id,
                    relative_path,
                    dest_relative_path: None,
                    size_bytes: 0,
                    source_content_id: None,
                    dest_content_id: None,
                    action: "unavailable",
                    reason: "marked path is no longer indexed on source root",
                    metadata_json: serde_json::json!({}),
                },
            )?;
            continue;
        };

        let indexed_dest = db::path_observation_for_root_path(conn, &dest_root.id, relative_path)?;
        let dest = match indexed_dest {
            Some(row) => Some(DestinationObservation {
                size_bytes: row.size_bytes,
                modified_at: row.modified_at,
                content_id: row.content_id,
                source: DestinationObservationSource::Index,
                conflict_reason: None,
            }),
            None => probe_destination_observation(&dest_endpoint, relative_path, &mut probe_cache)?,
        };
        let source_basename = basename(Path::new(&source.relative_path))
            .unwrap_or_else(|_| source.relative_path.clone());
        let hash_collisions = match source.content_id.as_deref() {
            Some(content_id) => db::content_collisions_for_root(
                conn,
                &dest_root.id,
                content_id,
                &source.relative_path,
            )?,
            None => Vec::new(),
        };
        let filename_collisions = db::filename_size_date_collisions_for_root(
            conn,
            &dest_root.id,
            &source_basename,
            source.size_bytes,
            source.modified_at.as_deref(),
            &source.relative_path,
        )?;
        let needs_review = !hash_collisions.is_empty() || !filename_collisions.is_empty();
        let (action, reason, dest_content_id) = match dest.as_ref() {
            None if needs_review => (
                "review",
                "destination root has hash or filename/size/date collisions; review before copy",
                hash_collisions
                    .first()
                    .or_else(|| filename_collisions.first())
                    .and_then(|row| row.content_id.as_deref()),
            ),
            None => (
                "copy",
                "destination path does not exist or is not indexed",
                None,
            ),
            Some(dest) if dest.conflict_reason.is_some() => (
                "conflict",
                dest.conflict_reason
                    .unwrap_or("destination path cannot receive this copy"),
                dest.content_id.as_deref(),
            ),
            Some(dest) => {
                match (
                    source.content_id.as_deref(),
                    dest.content_id.as_deref(),
                    source.size_bytes == dest.size_bytes,
                ) {
                    (Some(source_id), Some(dest_id), _) if source_id == dest_id => {
                        ("skip", "destination content already matches", Some(dest_id))
                    }
                    (_, _, true) if dest.source == DestinationObservationSource::Probe => (
                        "review",
                        "destination path exists but is not indexed; review before copy",
                        dest.content_id.as_deref(),
                    ),
                    (_, _, true) => (
                        "verify_needed",
                        "destination has the same size but lacks matching hash proof",
                        dest.content_id.as_deref(),
                    ),
                    _ => (
                        "conflict",
                        "destination path exists with different projected content",
                        dest.content_id.as_deref(),
                    ),
                }
            }
        };

        db::insert_transfer_plan_entry(
            conn,
            db::TransferPlanEntryInput {
                plan_id: &plan_id,
                relative_path: &source.relative_path,
                dest_relative_path: None,
                size_bytes: source.size_bytes,
                source_content_id: source.content_id.as_deref(),
                dest_content_id,
                action,
                reason,
                metadata_json: serde_json::json!({
                    "source_modified_at": source.modified_at,
                    "dest_modified_at": dest.as_ref().and_then(|row| row.modified_at.clone()),
                    "dest_observation_source": dest.as_ref().map(|row| row.source.label()),
                    "hash_collisions": collision_metadata(&hash_collisions),
                    "filename_size_date_collisions": collision_metadata(&filename_collisions),
                }),
            },
        )?;
    }

    let summary = db::transfer_plan_action_summary(conn, &plan_id)?;
    Ok((plan_id, summary))
}

fn collision_metadata(rows: &[db::CollisionRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            serde_json::json!({
                "relative_path": row.relative_path,
                "size_bytes": row.size_bytes,
                "modified_at": row.modified_at,
                "content_id": row.content_id,
            })
        })
        .collect()
}
