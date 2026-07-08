use super::*;
pub fn run_transfer_plan(
    conn: &Connection,
    plan_id: &str,
    paranoid: bool,
) -> anyhow::Result<TransferRunResult> {
    let plan = db::transfer_plan_by_id(conn, plan_id)?
        .ok_or_else(|| anyhow::anyhow!("transfer plan not found: {plan_id}"))?;
    let source_root = db::root_by_id(conn, &plan.source_root_id)?
        .ok_or_else(|| anyhow::anyhow!("source root not found: {}", plan.source_root_id))?;
    let dest_root = db::root_by_id(conn, &plan.dest_root_id)?
        .ok_or_else(|| anyhow::anyhow!("destination root not found: {}", plan.dest_root_id))?;
    let source_endpoint = root_transfer_endpoint(conn, &source_root)?;
    let dest_endpoint = root_transfer_endpoint(conn, &dest_root)?;
    let entries = db::transfer_plan_entries_filtered(conn, plan_id, Some("copy"))?;
    if entries.is_empty() {
        anyhow::bail!("transfer plan has no copy entries: {plan_id}");
    }

    let job_id = db::create_job(
        conn,
        "transfer_copy",
        Some(&source_root.machine_id),
        Some(&source_root.id),
        serde_json::json!({
            "plan_id": plan_id,
            "source_root_id": source_root.id,
            "source_path": source_root.path,
            "dest_root_id": dest_root.id,
            "dest_path": dest_root.path,
            "copy_entries": entries.len(),
            "paranoid": paranoid,
        }),
    )?;
    persist_job_event(
        conn,
        &job_id,
        JobEventInput {
            event_kind: EventKind::JobCreated,
            kind: "transfer_copy",
            path: Some(&source_root.path),
            message: "transfer copy queued",
            files_seen: Some(entries.len() as u64),
            errors: 0,
        },
    )?;
    db::start_job(conn, &job_id)?;
    db::update_transfer_plan_status(conn, plan_id, "running")?;
    persist_job_event(
        conn,
        &job_id,
        JobEventInput {
            event_kind: EventKind::JobStarted,
            kind: "transfer_copy",
            path: Some(&source_root.path),
            message: "transfer copy started",
            files_seen: Some(entries.len() as u64),
            errors: 0,
        },
    )?;

    let mut result = TransferRunResult {
        job_id: job_id.clone(),
        plan_id: plan_id.to_string(),
        ..TransferRunResult::default()
    };
    let total = entries.len() as u64;
    let total_bytes = entries.iter().map(|entry| entry.size_bytes).sum::<u64>();

    for entry in entries {
        if complete_transfer_if_canceled(
            conn,
            &job_id,
            plan_id,
            &source_root.path,
            total,
            total_bytes,
            &mut result,
        )? {
            return Ok(result);
        }
        let source_path = endpoint_join(&source_endpoint, &entry.relative_path)?;
        let dest_path = endpoint_join(&dest_endpoint, &entry.dest_relative_path)?;
        let source_display = source_path.display_path();
        let dest_display = dest_path.display_path();
        let current = entry.relative_path.as_str();
        let bytes_before_file = result.bytes_copied;
        let started_at = Instant::now();
        let mut on_progress = |file_bytes_done: u64,
                               file_bytes_total: u64,
                               bytes_per_second: f64|
         -> anyhow::Result<()> {
            persist_transfer_progress_event(
                conn,
                &job_id,
                TransferProgressEventInput {
                    current_path: current,
                    files_total: total,
                    files_seen: result.copied + result.skipped + result.errors,
                    files_done: result.copied,
                    files_skipped: result.skipped,
                    errors: result.errors,
                    bytes_done: bytes_before_file + file_bytes_done,
                    bytes_total: total_bytes,
                    file_bytes_done,
                    file_bytes_total,
                    bytes_per_second,
                },
            )
        };

        let copy_result = copy_one_entry(
            CopyContext {
                conn,
                job_id: &job_id,
                plan_id,
                dest_root: &dest_root,
            },
            &entry,
            &source_path,
            &dest_path,
            paranoid,
            &mut on_progress,
        );
        match copy_result {
            Ok(CopyOutcome::Copied(bytes)) => {
                result.copied += 1;
                result.bytes_copied += bytes;
            }
            Ok(CopyOutcome::Skipped) => {
                result.skipped += 1;
            }
            Err(err) => {
                result.errors += 1;
                persist_transfer_file_event(
                    conn,
                    &job_id,
                    TransferFileEventInput {
                        event_kind: EventKind::TransferFailed,
                        relative_path: &entry.relative_path,
                        source_path: &source_display,
                        dest_path: &dest_display,
                        size_bytes: entry.size_bytes,
                        action: "error",
                        message: None,
                        error: Some(&err.to_string()),
                    },
                )?;
            }
        }
        db::update_job_progress(
            conn,
            &job_id,
            db::JobProgressInput {
                phase: "copying",
                current_path: Some(current),
                files_total: Some(total),
                files_seen: result.copied + result.skipped + result.errors,
                files_done: result.copied,
                files_skipped: result.skipped,
                errors: result.errors,
            },
        )?;
        let progress = EventEnvelope {
            event_kind: EventKind::JobProgress,
            job_id: Some(job_id.clone()),
            sequence: None,
            created_at: now_rfc3339(),
            payload: EventPayload::JobProgress {
                phase: "copying".to_string(),
                current_path: Some(current.to_string()),
                files_total: Some(total),
                files_seen: result.copied + result.skipped + result.errors,
                files_done: result.copied,
                files_skipped: result.skipped,
                errors: result.errors,
                bytes_done: Some(result.bytes_copied),
                bytes_total: Some(total_bytes),
                file_bytes_done: Some(entry.size_bytes),
                file_bytes_total: Some(entry.size_bytes),
                bytes_per_second: Some(rate_per_second(entry.size_bytes, started_at)),
                message: None,
            },
        };
        db::persist_event(conn, &progress)?;
        if complete_transfer_if_canceled(
            conn,
            &job_id,
            plan_id,
            &source_root.path,
            total,
            total_bytes,
            &mut result,
        )? {
            return Ok(result);
        }
    }

    let status = if result.errors == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    db::complete_job(conn, &job_id, status)?;
    db::update_transfer_plan_status(conn, plan_id, status)?;
    persist_job_event(
        conn,
        &job_id,
        JobEventInput {
            event_kind: if result.errors == 0 {
                EventKind::JobCompleted
            } else {
                EventKind::JobFailed
            },
            kind: "transfer_copy",
            path: Some(&source_root.path),
            message: status,
            files_seen: Some(total),
            errors: result.errors,
        },
    )?;
    Ok(result)
}

pub(super) fn complete_transfer_if_canceled(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    source_path: &str,
    total_files: u64,
    total_bytes: u64,
    result: &mut TransferRunResult,
) -> anyhow::Result<bool> {
    if !db::job_cancel_requested(conn, job_id)? {
        return Ok(false);
    }
    let files_seen = result.copied + result.skipped + result.errors;
    db::update_job_progress(
        conn,
        job_id,
        db::JobProgressInput {
            phase: "canceling",
            current_path: None,
            files_total: Some(total_files),
            files_seen,
            files_done: result.copied,
            files_skipped: result.skipped,
            errors: result.errors,
        },
    )?;
    let progress = EventEnvelope {
        event_kind: EventKind::JobProgress,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: now_rfc3339(),
        payload: EventPayload::JobProgress {
            phase: "canceling".to_string(),
            current_path: None,
            files_total: Some(total_files),
            files_seen,
            files_done: result.copied,
            files_skipped: result.skipped,
            errors: result.errors,
            bytes_done: Some(result.bytes_copied),
            bytes_total: Some(total_bytes),
            file_bytes_done: None,
            file_bytes_total: None,
            bytes_per_second: None,
            message: Some("cancel requested".to_string()),
        },
    };
    db::persist_event(conn, &progress)?;
    persist_job_event(
        conn,
        job_id,
        JobEventInput {
            event_kind: EventKind::JobCanceled,
            kind: "transfer_copy",
            path: Some(source_path),
            message: "canceled between files",
            files_seen: Some(files_seen),
            errors: result.errors,
        },
    )?;
    db::complete_job(conn, job_id, "canceled")?;
    db::update_transfer_plan_status(conn, plan_id, "canceled")?;
    result.canceled = true;
    Ok(true)
}

pub(super) enum CopyOutcome {
    Copied(u64),
    Skipped,
}

pub(super) fn copy_one_entry(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source_path: &TransferEndpoint,
    dest_path: &TransferEndpoint,
    paranoid: bool,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<CopyOutcome> {
    match (source_path, dest_path) {
        (TransferEndpoint::Local(source), TransferEndpoint::Local(dest)) => {
            copy_local_to_local(ctx, entry, source, dest, paranoid, on_progress)
        }
        (TransferEndpoint::Ssh { .. }, TransferEndpoint::Local(dest)) => {
            if paranoid {
                anyhow::bail!("--paranoid is not supported for SSH transfers yet");
            }
            copy_ssh_to_local(ctx, entry, source_path, dest, on_progress)
        }
        (TransferEndpoint::Local(source), TransferEndpoint::Ssh { .. }) => {
            if paranoid {
                anyhow::bail!("--paranoid is not supported for SSH transfers yet");
            }
            copy_local_to_ssh(ctx, entry, source, dest_path, on_progress)
        }
        (TransferEndpoint::Ssh { .. }, TransferEndpoint::Ssh { .. }) => {
            anyhow::bail!("remote-to-remote transfer run is not supported yet")
        }
    }
}
