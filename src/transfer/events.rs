use super::*;
pub(super) fn persist_job_event(
    conn: &Connection,
    job_id: &str,
    input: JobEventInput<'_>,
) -> rusqlite::Result<()> {
    let envelope = EventEnvelope {
        event_kind: input.event_kind,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: now_rfc3339(),
        payload: EventPayload::Job {
            kind: input.kind.to_string(),
            path: input.path.map(str::to_string),
            message: Some(input.message.to_string()),
            files_seen: input.files_seen,
            errors: Some(input.errors),
        },
    };
    db::persist_event(conn, &envelope)
}

pub(super) fn persist_transfer_file_event(
    conn: &Connection,
    job_id: &str,
    input: TransferFileEventInput<'_>,
) -> rusqlite::Result<()> {
    let envelope = EventEnvelope {
        event_kind: input.event_kind,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: now_rfc3339(),
        payload: EventPayload::TransferFile {
            relative_path: input.relative_path.to_string(),
            source_path: input.source_path.to_string(),
            dest_path: input.dest_path.to_string(),
            size_bytes: input.size_bytes,
            action: input.action.to_string(),
            message: input.message.map(str::to_string),
            error: input.error.map(str::to_string),
        },
    };
    db::persist_event(conn, &envelope)
}

pub(super) fn persist_transfer_progress_event(
    conn: &Connection,
    job_id: &str,
    input: TransferProgressEventInput<'_>,
) -> anyhow::Result<()> {
    let envelope = EventEnvelope {
        event_kind: EventKind::JobProgress,
        job_id: Some(job_id.to_string()),
        sequence: None,
        created_at: now_rfc3339(),
        payload: EventPayload::JobProgress {
            phase: "copying".to_string(),
            current_path: Some(input.current_path.to_string()),
            files_total: Some(input.files_total),
            files_seen: input.files_seen,
            files_done: input.files_done,
            files_skipped: input.files_skipped,
            errors: input.errors,
            bytes_done: Some(input.bytes_done),
            bytes_total: Some(input.bytes_total),
            file_bytes_done: Some(input.file_bytes_done),
            file_bytes_total: Some(input.file_bytes_total),
            bytes_per_second: Some(input.bytes_per_second),
            message: input.message.map(str::to_string),
            chunk_confidence: input.chunk_confidence,
        },
    };
    db::persist_event(conn, &envelope)?;
    Ok(())
}
