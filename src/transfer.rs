use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::Context;
use chrono::DateTime;
use filetime::FileTime;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::db::{self, RootRow, TransferPlanActionSummary};
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{basename, local_machine_id, now_rfc3339, parent_path, system_time_rfc3339};

struct JobEventInput<'a> {
    event_kind: EventKind,
    kind: &'a str,
    path: Option<&'a str>,
    message: &'a str,
    files_seen: Option<u64>,
    errors: u64,
}

struct TransferFileEventInput<'a> {
    event_kind: EventKind,
    relative_path: &'a str,
    source_path: &'a str,
    dest_path: &'a str,
    size_bytes: u64,
    action: &'a str,
    message: Option<&'a str>,
    error: Option<&'a str>,
}

struct TransferProgressEventInput<'a> {
    current_path: &'a str,
    files_total: u64,
    files_seen: u64,
    files_done: u64,
    files_skipped: u64,
    errors: u64,
    bytes_done: u64,
    bytes_total: u64,
    file_bytes_done: u64,
    file_bytes_total: u64,
    bytes_per_second: f64,
}

struct CopyContext<'a> {
    conn: &'a Connection,
    job_id: &'a str,
    plan_id: &'a str,
    dest_root: &'a RootRow,
}

#[derive(Debug, Clone)]
pub struct TransferPlanResult {
    pub plan_id: String,
    pub job_id: String,
    pub selection_set_id: String,
    pub marked_count: i64,
    pub marked_bytes: i64,
    pub summary: Vec<TransferPlanActionSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct TransferRunResult {
    pub job_id: String,
    pub plan_id: String,
    pub copied: u64,
    pub skipped: u64,
    pub errors: u64,
    pub bytes_copied: u64,
    pub canceled: bool,
}

struct CopyHashResult {
    bytes: u64,
    blake3: String,
    sha256: String,
}

#[derive(Debug, Clone)]
struct DestinationObservation {
    size_bytes: u64,
    modified_at: Option<String>,
    content_id: Option<String>,
    source: DestinationObservationSource,
    conflict_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationObservationSource {
    Index,
    Probe,
}

impl DestinationObservationSource {
    fn label(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Probe => "probe",
        }
    }
}

#[derive(Debug, Default)]
struct DestinationProbeCache {
    existing_dirs: BTreeSet<String>,
    missing_dirs: BTreeSet<String>,
}

enum EndpointPathKind {
    Missing,
    Directory,
    File {
        size_bytes: u64,
        modified_at: Option<String>,
    },
    Other {
        size_bytes: u64,
        modified_at: Option<String>,
    },
}

#[derive(Debug, Clone)]
enum TransferEndpoint {
    Local(PathBuf),
    Ssh { host: String, path: String },
}

impl TransferEndpoint {
    fn display_path(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Ssh { host, path } => format!("{host}:{path}"),
        }
    }
}

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

fn complete_transfer_if_canceled(
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

enum CopyOutcome {
    Copied(u64),
    Skipped,
}

fn copy_one_entry(
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

fn copy_local_to_local(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest_path: &Path,
    paranoid: bool,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<CopyOutcome> {
    let source_meta = std::fs::metadata(source_path)
        .with_context(|| format!("reading source {}", source_path.display()))?;
    if !source_meta.is_file() {
        anyhow::bail!("source is not a regular file: {}", source_path.display());
    }
    if source_meta.len() != entry.size_bytes {
        anyhow::bail!(
            "source size changed for {}: planned {} bytes, found {} bytes",
            entry.relative_path,
            entry.size_bytes,
            source_meta.len()
        );
    }
    let source_modified_at = source_meta.modified().ok().map(system_time_rfc3339);

    if let Ok(dest_meta) = std::fs::metadata(dest_path) {
        if dest_meta.is_file() && dest_meta.len() == entry.size_bytes {
            let verified_content_id = if paranoid {
                sync_for_paranoid_readback(dest_path, None)?;
                let readback_hash = hash_existing_file(dest_path)?;
                verify_copy_hash(ctx.conn, entry, &readback_hash)?;
                Some(db::ensure_content_object(
                    ctx.conn,
                    readback_hash.bytes,
                    &readback_hash.blake3,
                    &readback_hash.sha256,
                )?)
            } else {
                None
            };
            let dest_modified_at = dest_meta.modified().ok().map(system_time_rfc3339);
            insert_dest_observation(
                ctx.conn,
                ctx.dest_root,
                entry,
                verified_content_id.as_deref(),
                dest_modified_at.as_deref(),
            )?;
            persist_transfer_file_event(
                ctx.conn,
                ctx.job_id,
                TransferFileEventInput {
                    event_kind: EventKind::TransferSkipped,
                    relative_path: &entry.relative_path,
                    source_path: &source_path.display().to_string(),
                    dest_path: &dest_path.display().to_string(),
                    size_bytes: entry.size_bytes,
                    action: "skip",
                    message: Some("destination already has planned size"),
                    error: None,
                },
            )?;
            return Ok(CopyOutcome::Skipped);
        }
        anyhow::bail!("destination exists and differs: {}", dest_path.display());
    }

    let parent_created = ensure_dest_parent(dest_path)?;
    let copy_hash = copy_with_hash(source_path, dest_path, Some(on_progress))?;
    if copy_hash.bytes != entry.size_bytes {
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            copy_hash.bytes
        );
    }
    verify_copy_hash(ctx.conn, entry, &copy_hash)?;
    set_local_file_mtime(dest_path, source_modified_at.as_deref())?;
    if paranoid {
        sync_for_paranoid_readback(dest_path, parent_created)?;
        let readback_hash = hash_existing_file(dest_path)?;
        if readback_hash.bytes != copy_hash.bytes
            || readback_hash.blake3 != copy_hash.blake3
            || readback_hash.sha256 != copy_hash.sha256
        {
            anyhow::bail!(
                "paranoid readback hash mismatch for {}",
                dest_path.display()
            );
        }
    }

    let content_id = db::ensure_content_object(
        ctx.conn,
        copy_hash.bytes,
        &copy_hash.blake3,
        &copy_hash.sha256,
    )?;
    insert_dest_observation(
        ctx.conn,
        ctx.dest_root,
        entry,
        Some(&content_id),
        source_modified_at.as_deref(),
    )?;
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_path.display().to_string(),
            dest_path: &dest_path.display().to_string(),
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(copy_hash.bytes))
}

fn copy_ssh_to_local(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source: &TransferEndpoint,
    dest_path: &Path,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<CopyOutcome> {
    if std::fs::metadata(dest_path).is_ok() {
        anyhow::bail!("destination exists: {}", dest_path.display());
    }
    let parent = ensure_dest_parent(dest_path)?;
    let temp_path = transfer_temp_path(dest_path);
    let copy_result = copy_ssh_to_local_chunked(
        ctx.conn,
        ctx.job_id,
        ctx.plan_id,
        entry,
        source,
        &temp_path,
        on_progress,
    )
    .with_context(|| {
        format!(
            "copying {} to {}",
            source.display_path(),
            dest_path.display()
        )
    });
    if let Err(err) = copy_result {
        return Err(err);
    }
    let copy_hash = copy_result?;
    if copy_hash.bytes != entry.size_bytes {
        let _ = std::fs::remove_file(&temp_path);
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            copy_hash.bytes
        );
    }
    if let Err(err) = verify_copy_hash(ctx.conn, entry, &copy_hash) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    std::fs::rename(&temp_path, dest_path)
        .with_context(|| format!("installing copy at {}", dest_path.display()))?;
    let source_modified_at = source_modified_at(entry)?;
    set_local_file_mtime(dest_path, source_modified_at.as_deref())?;
    sync_for_paranoid_readback(dest_path, parent)?;
    let content_id = db::ensure_content_object(
        ctx.conn,
        copy_hash.bytes,
        &copy_hash.blake3,
        &copy_hash.sha256,
    )?;
    insert_dest_observation(
        ctx.conn,
        ctx.dest_root,
        entry,
        Some(&content_id),
        source_modified_at.as_deref(),
    )?;
    let source_display = source.display_path();
    let dest_display = dest_path.display().to_string();
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_display,
            dest_path: &dest_display,
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied over ssh"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(copy_hash.bytes))
}

fn copy_local_to_ssh(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest: &TransferEndpoint,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<CopyOutcome> {
    let source_meta = std::fs::metadata(source_path)
        .with_context(|| format!("reading source {}", source_path.display()))?;
    if !source_meta.is_file() {
        anyhow::bail!("source is not a regular file: {}", source_path.display());
    }
    if source_meta.len() != entry.size_bytes {
        anyhow::bail!(
            "source size changed for {}: planned {} bytes, found {} bytes",
            entry.relative_path,
            entry.size_bytes,
            source_meta.len()
        );
    }
    let source_modified_at = source_meta.modified().ok().map(system_time_rfc3339);
    let source_hash = hash_existing_file(source_path)?;
    verify_copy_hash(ctx.conn, entry, &source_hash)?;

    let TransferEndpoint::Ssh { host, path } = dest else {
        anyhow::bail!("destination is not SSH");
    };
    let parent = remote_parent(path);
    if remote_path_exists(host, path)? {
        let checkpoint_count = db::transfer_copy_chunk_count_for_entry(
            ctx.conn,
            ctx.plan_id,
            &entry.relative_path,
            &entry.dest_relative_path,
        )?;
        if checkpoint_count == 0 {
            anyhow::bail!("remote destination exists: {host}:{path}");
        }
    }
    run_command(Command::new("ssh").arg(host).arg(format!(
        "test -f {} || mkdir -p {}",
        remote_shell_path(path),
        remote_shell_path(&parent)
    )))
    .with_context(|| format!("preparing remote destination {host}:{path}"))?;
    copy_local_to_ssh_chunked(
        ctx.conn,
        ctx.job_id,
        ctx.plan_id,
        entry,
        source_path,
        dest,
        on_progress,
    )
    .with_context(|| {
        format!(
            "copying {} to {}",
            source_path.display(),
            dest.display_path()
        )
    })?;
    set_remote_file_mtime(host, path, source_modified_at.as_deref())?;
    let content_id = db::ensure_content_object(
        ctx.conn,
        source_hash.bytes,
        &source_hash.blake3,
        &source_hash.sha256,
    )?;
    insert_dest_observation(
        ctx.conn,
        ctx.dest_root,
        entry,
        Some(&content_id),
        source_modified_at.as_deref(),
    )?;
    let source_display = source_path.display().to_string();
    let dest_display = dest.display_path();
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_display,
            dest_path: &dest_display,
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied over ssh"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(source_hash.bytes))
}

fn copy_ssh_to_local_chunked(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    source: &TransferEndpoint,
    temp_path: &Path,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<CopyHashResult> {
    let TransferEndpoint::Ssh { host, path } = source else {
        anyhow::bail!("source is not SSH");
    };
    let mut dest = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(temp_path)
        .with_context(|| format!("creating {}", temp_path.display()))?;
    dest.set_len(entry.size_bytes)
        .with_context(|| format!("sizing {}", temp_path.display()))?;
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut copied = 0_u64;
    let started_at = Instant::now();
    for chunk in transfer_chunks(entry.size_bytes) {
        let bytes = if let Some(checkpoint) =
            matching_copy_chunk_checkpoint(conn, plan_id, entry, chunk)?
        {
            match local_chunk_bytes(temp_path, chunk) {
                Ok(bytes) if format!("{:x}", md5::compute(&bytes)) == checkpoint.digest => bytes,
                _ => fetch_verified_remote_chunk(host, path, chunk)?,
            }
        } else {
            fetch_verified_remote_chunk(host, path, chunk)?
        };
        if bytes.len() as u64 != chunk.size {
            anyhow::bail!("chunk {} size changed while copying {}", chunk.index, path);
        }
        let local_md5 = format!("{:x}", md5::compute(&bytes));
        dest.seek(SeekFrom::Start(chunk.offset))
            .with_context(|| format!("seeking {}", temp_path.display()))?;
        dest.write_all(&bytes)
            .with_context(|| format!("writing {}", temp_path.display()))?;
        persist_copy_chunk_checkpoint(conn, job_id, plan_id, entry, chunk, &local_md5)?;
        blake3_hasher.update(&bytes);
        sha256_hasher.update(&bytes);
        copied += bytes.len() as u64;
        on_progress(
            copied,
            entry.size_bytes,
            rate_per_second(copied, started_at),
        )?;
    }
    dest.sync_all()
        .with_context(|| format!("syncing {}", temp_path.display()))?;
    Ok(CopyHashResult {
        bytes: copied,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: bytes_to_hex(sha256_hasher.finalize()),
    })
}

fn copy_local_to_ssh_chunked(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest: &TransferEndpoint,
    on_progress: &mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let TransferEndpoint::Ssh { host, path } = dest else {
        anyhow::bail!("destination is not SSH");
    };
    if entry.size_bytes == 0 {
        run_command(
            Command::new("ssh")
                .arg(host)
                .arg(format!(": > {}", remote_shell_path(path))),
        )
        .with_context(|| format!("creating empty remote file {host}:{path}"))?;
        on_progress(0, 0, 0.0)?;
        return Ok(());
    }
    let mut source = std::fs::File::open(source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;
    let mut copied = 0_u64;
    let started_at = Instant::now();
    for chunk in transfer_chunks(entry.size_bytes) {
        let mut bytes = vec![0_u8; chunk.size as usize];
        source
            .seek(SeekFrom::Start(chunk.offset))
            .with_context(|| format!("seeking {}", source_path.display()))?;
        source
            .read_exact(&mut bytes)
            .with_context(|| format!("reading {}", source_path.display()))?;
        let local_md5 = format!("{:x}", md5::compute(&bytes));
        let checkpoint = matching_copy_chunk_checkpoint(conn, plan_id, entry, chunk)?;
        let remote_md5 = if checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.digest == local_md5)
        {
            let remote_md5 = remote_chunk_md5(host, path, chunk.index)?;
            if remote_md5 == local_md5 {
                remote_md5
            } else {
                write_remote_chunk(host, path, chunk.index, &bytes)?;
                remote_chunk_md5(host, path, chunk.index)?
            }
        } else {
            write_remote_chunk(host, path, chunk.index, &bytes)?;
            remote_chunk_md5(host, path, chunk.index)?
        };
        if remote_md5 != local_md5 {
            anyhow::bail!(
                "MD5 chunk mismatch after SSH write for {} chunk {}: local {}, remote {}",
                path,
                chunk.index,
                local_md5,
                remote_md5
            );
        }
        persist_copy_chunk_checkpoint(conn, job_id, plan_id, entry, chunk, &local_md5)?;
        copied += chunk.size;
        on_progress(
            copied,
            entry.size_bytes,
            rate_per_second(copied, started_at),
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct TransferChunk {
    index: u64,
    offset: u64,
    size: u64,
}

fn transfer_chunks(total: u64) -> Vec<TransferChunk> {
    let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    let mut index = 0_u64;
    while offset < total {
        let size = (total - offset).min(chunk_size);
        chunks.push(TransferChunk {
            index,
            offset,
            size,
        });
        offset += size;
        index += 1;
    }
    chunks
}

fn matching_copy_chunk_checkpoint(
    conn: &Connection,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    chunk: TransferChunk,
) -> rusqlite::Result<Option<db::TransferCopyChunkRow>> {
    db::transfer_copy_chunk(
        conn,
        plan_id,
        &entry.relative_path,
        &entry.dest_relative_path,
        crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
        chunk.index,
        "md5",
    )
    .map(|checkpoint| {
        checkpoint.filter(|checkpoint| {
            checkpoint.offset_bytes == chunk.offset
                && checkpoint.size_bytes == chunk.size
                && checkpoint.chunk_size_bytes == crate::fswork::DEFAULT_CHUNK_SIZE_BYTES
                && checkpoint.chunk_index == chunk.index
                && checkpoint.algorithm == "md5"
        })
    })
}

fn persist_copy_chunk_checkpoint(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    chunk: TransferChunk,
    md5_digest: &str,
) -> rusqlite::Result<()> {
    db::upsert_transfer_copy_chunk(
        conn,
        db::TransferCopyChunkInput {
            plan_id,
            relative_path: &entry.relative_path,
            dest_relative_path: &entry.dest_relative_path,
            chunk_size_bytes: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
            chunk_index: chunk.index,
            offset_bytes: chunk.offset,
            size_bytes: chunk.size,
            algorithm: "md5",
            digest: md5_digest,
            job_id,
        },
    )
}

fn local_chunk_bytes(path: &Path, chunk: TransferChunk) -> anyhow::Result<Vec<u8>> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(chunk.offset))
        .with_context(|| format!("seeking {}", path.display()))?;
    let mut bytes = vec![0_u8; chunk.size as usize];
    file.read_exact(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(bytes)
}

fn fetch_verified_remote_chunk(
    host: &str,
    path: &str,
    chunk: TransferChunk,
) -> anyhow::Result<Vec<u8>> {
    let remote_md5 = remote_chunk_md5(host, path, chunk.index)?;
    let bytes = remote_chunk_bytes(host, path, chunk.index)?;
    if bytes.len() as u64 != chunk.size {
        anyhow::bail!(
            "remote chunk size mismatch for {} chunk {}: expected {}, got {}",
            path,
            chunk.index,
            chunk.size,
            bytes.len()
        );
    }
    let local_md5 = format!("{:x}", md5::compute(&bytes));
    if local_md5 != remote_md5 {
        anyhow::bail!(
            "MD5 chunk mismatch for {} chunk {}: remote {}, copied {}",
            path,
            chunk.index,
            remote_md5,
            local_md5
        );
    }
    Ok(bytes)
}

fn remote_chunk_md5(host: &str, path: &str, chunk_index: u64) -> anyhow::Result<String> {
    let output = remote_chunk_command(host, path, chunk_index, " | md5sum")?;
    let text = String::from_utf8(output).context("remote md5sum output was not UTF-8")?;
    text.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("remote md5sum produced no digest for {host}:{path}"))
}

fn remote_chunk_bytes(host: &str, path: &str, chunk_index: u64) -> anyhow::Result<Vec<u8>> {
    remote_chunk_command(host, path, chunk_index, "")
}

fn remote_chunk_command(
    host: &str,
    path: &str,
    chunk_index: u64,
    suffix: &str,
) -> anyhow::Result<Vec<u8>> {
    let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
    let command = format!(
        "dd if={} bs={} skip={} count=1 iflag=fullblock status=none{}",
        remote_shell_path(path),
        chunk_size,
        chunk_index,
        suffix
    );
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("reading remote chunk {chunk_index} from {host}:{path}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        anyhow::bail!(
            "remote chunk command failed for {host}:{path} chunk {}: {}",
            chunk_index,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

fn write_remote_chunk(
    host: &str,
    path: &str,
    chunk_index: u64,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let command = format!(
        "dd of={} bs={} seek={} count=1 conv=notrunc status=none",
        remote_shell_path(path),
        crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
        chunk_index
    );
    let mut child = Command::new("ssh")
        .arg(host)
        .arg(command)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting remote chunk write to {host}:{path}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to open ssh stdin"))?;
        stdin
            .write_all(bytes)
            .with_context(|| format!("writing chunk {} to {host}:{path}", chunk_index))?;
    }
    let status = child
        .wait()
        .with_context(|| format!("waiting for remote chunk write to {host}:{path}"))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("remote chunk write failed for {host}:{path} chunk {chunk_index}");
    }
}

fn transfer_temp_path(dest_path: &Path) -> PathBuf {
    let file_name = dest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("copy");
    dest_path.with_file_name(format!(".{file_name}.gremlin-part"))
}

fn remote_path_exists(host: &str, path: &str) -> anyhow::Result<bool> {
    let status = Command::new("ssh")
        .arg(host)
        .arg(format!("test -e {}", remote_shell_path(path)))
        .status()
        .with_context(|| format!("checking remote path {host}:{path}"))?;
    Ok(status.success())
}

fn ensure_dest_parent(dest_path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let Some(parent) = dest_path.parent() else {
        return Ok(None);
    };
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating destination directory {}", parent.display()))?;
    Ok(Some(parent.to_path_buf()))
}

fn insert_dest_observation(
    conn: &Connection,
    dest_root: &RootRow,
    entry: &db::TransferPlanEntryRow,
    content_id: Option<&str>,
    modified_at: Option<&str>,
) -> rusqlite::Result<()> {
    let base = basename(Path::new(&entry.dest_relative_path))
        .unwrap_or_else(|_| entry.dest_relative_path.clone());
    db::insert_path_observation(
        conn,
        db::PathObservationInput {
            machine_id: &dest_root.machine_id,
            root_id: &dest_root.id,
            relative_path: &entry.dest_relative_path,
            basename: &base,
            parent_path: &parent_path(&entry.dest_relative_path),
            size_bytes: entry.size_bytes,
            modified_at,
            content_id,
        },
    )
}

fn root_transfer_endpoint(conn: &Connection, root: &RootRow) -> anyhow::Result<TransferEndpoint> {
    if root.machine_id == local_machine_id() {
        return Ok(TransferEndpoint::Local(PathBuf::from(&root.path)));
    }
    let machine = db::machine_by_id(conn, &root.machine_id)?
        .ok_or_else(|| anyhow::anyhow!("machine not found for root {}", root.id))?;
    if machine.platform.as_deref() == Some("ssh") {
        let path = ssh_root_remote_path(&machine.label, &root.path);
        return Ok(TransferEndpoint::Ssh {
            host: machine.label,
            path,
        });
    }
    anyhow::bail!(
        "transfer run does not support machine {} ({})",
        machine.id,
        machine.platform.as_deref().unwrap_or("unknown")
    )
}

fn ssh_root_remote_path(host: &str, root_path: &str) -> String {
    root_path
        .strip_prefix(&format!("{host}:"))
        .unwrap_or(root_path)
        .to_string()
}

fn endpoint_join(root: &TransferEndpoint, relative_path: &str) -> anyhow::Result<TransferEndpoint> {
    match root {
        TransferEndpoint::Local(root) => Ok(TransferEndpoint::Local(safe_join(
            root.to_string_lossy().as_ref(),
            relative_path,
        )?)),
        TransferEndpoint::Ssh { host, path } => Ok(TransferEndpoint::Ssh {
            host: host.clone(),
            path: remote_join(path, relative_path)?,
        }),
    }
}

fn probe_destination_observation(
    endpoint: &TransferEndpoint,
    relative_path: &str,
    cache: &mut DestinationProbeCache,
) -> anyhow::Result<Option<DestinationObservation>> {
    for parent in relative_parent_prefixes(relative_path) {
        if cache.missing_dirs.iter().any(|missing| {
            parent == *missing
                || parent
                    .strip_prefix(missing)
                    .is_some_and(|rest| rest.starts_with('/'))
        }) {
            return Ok(None);
        }
        if cache.existing_dirs.contains(&parent) {
            continue;
        }
        match probe_endpoint_path(&endpoint_join(endpoint, &parent)?)? {
            EndpointPathKind::Missing => {
                cache.missing_dirs.insert(parent);
                return Ok(None);
            }
            EndpointPathKind::Directory => {
                cache.existing_dirs.insert(parent);
            }
            EndpointPathKind::File {
                size_bytes,
                modified_at,
            }
            | EndpointPathKind::Other {
                size_bytes,
                modified_at,
            } => {
                return Ok(Some(DestinationObservation {
                    size_bytes,
                    modified_at,
                    content_id: None,
                    source: DestinationObservationSource::Probe,
                    conflict_reason: Some("destination parent path exists but is not a directory"),
                }));
            }
        }
    }

    match probe_endpoint_path(&endpoint_join(endpoint, relative_path)?)? {
        EndpointPathKind::Missing => Ok(None),
        EndpointPathKind::File {
            size_bytes,
            modified_at,
        } => Ok(Some(DestinationObservation {
            size_bytes,
            modified_at,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: None,
        })),
        EndpointPathKind::Directory => Ok(Some(DestinationObservation {
            size_bytes: 0,
            modified_at: None,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: Some("destination path exists as a directory"),
        })),
        EndpointPathKind::Other {
            size_bytes,
            modified_at,
        } => Ok(Some(DestinationObservation {
            size_bytes,
            modified_at,
            content_id: None,
            source: DestinationObservationSource::Probe,
            conflict_reason: Some("destination path exists but is not a regular file"),
        })),
    }
}

fn relative_parent_prefixes(relative_path: &str) -> Vec<String> {
    let parts = Path::new(relative_path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>();
    if parts.len() <= 1 {
        return Vec::new();
    }
    (1..parts.len()).map(|len| parts[..len].join("/")).collect()
}

fn probe_endpoint_path(endpoint: &TransferEndpoint) -> anyhow::Result<EndpointPathKind> {
    match endpoint {
        TransferEndpoint::Local(path) => probe_local_path(path),
        TransferEndpoint::Ssh { host, path } => probe_remote_path(host, path),
    }
}

fn probe_local_path(path: &Path) -> anyhow::Result<EndpointPathKind> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(EndpointPathKind::Directory),
        Ok(metadata) if metadata.is_file() => Ok(EndpointPathKind::File {
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(system_time_rfc3339),
        }),
        Ok(metadata) => Ok(EndpointPathKind::Other {
            size_bytes: metadata.len(),
            modified_at: metadata.modified().ok().map(system_time_rfc3339),
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(EndpointPathKind::Missing),
        Err(err) => Err(err).with_context(|| format!("checking destination {}", path.display())),
    }
}

fn probe_remote_path(host: &str, path: &str) -> anyhow::Result<EndpointPathKind> {
    let command = format!(
        "if test ! -e {path}; then exit 3; elif test -d {path}; then printf 'dir\\n'; elif test -f {path}; then printf 'file '; stat -c '%s' {path}; else printf 'other '; stat -c '%s' {path}; fi",
        path = remote_shell_path(path)
    );
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("checking remote destination {host}:{path}"))?;
    if output.status.code() == Some(3) {
        return Ok(EndpointPathKind::Missing);
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "remote destination probe failed for {host}:{path}: {}",
            stderr.trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("remote stat output was not UTF-8")?;
    let mut fields = stdout.split_whitespace();
    match fields.next() {
        Some("dir") => Ok(EndpointPathKind::Directory),
        Some("file") => Ok(EndpointPathKind::File {
            size_bytes: parse_remote_stat_size(fields.next(), host, path)?,
            modified_at: None,
        }),
        Some("other") => Ok(EndpointPathKind::Other {
            size_bytes: parse_remote_stat_size(fields.next(), host, path)?,
            modified_at: None,
        }),
        _ => anyhow::bail!("remote stat produced invalid output for {host}:{path}"),
    }
}

fn parse_remote_stat_size(value: Option<&str>, host: &str, path: &str) -> anyhow::Result<u64> {
    value
        .ok_or_else(|| anyhow::anyhow!("remote stat produced no size for {host}:{path}"))?
        .parse::<u64>()
        .with_context(|| format!("remote stat produced invalid size for {host}:{path}"))
}

fn remote_join(root: &str, relative_path: &str) -> anyhow::Result<String> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        anyhow::bail!("refusing absolute transfer path: {relative_path}");
    }
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing unsafe transfer path: {relative_path}");
            }
        }
    }
    if parts.is_empty() {
        anyhow::bail!("empty transfer path");
    }
    let suffix = parts.join("/");
    let root = root.trim_end_matches('/');
    if root.is_empty() || root == "." {
        Ok(suffix)
    } else {
        Ok(format!("{root}/{suffix}"))
    }
}

fn remote_parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            }
        })
        .unwrap_or_else(|| ".".to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_shell_path(path: &str) -> String {
    if path == "~" {
        "$HOME".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("$HOME/{}", shell_quote(rest))
    } else {
        shell_quote(path)
    }
}

fn run_command(command: &mut Command) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("running {:?}", command))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("{:?} failed: {}", command, stderr.trim());
}

fn copy_with_hash(
    source_path: &Path,
    dest_path: &Path,
    on_progress: Option<&mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>>,
) -> anyhow::Result<CopyHashResult> {
    let mut source = std::fs::File::open(source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;
    let total = source
        .metadata()
        .with_context(|| format!("reading metadata for {}", source_path.display()))?
        .len();
    let mut dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest_path)
        .with_context(|| format!("creating destination {}", dest_path.display()))?;
    hash_stream_to_writer(
        &mut source,
        Some(&mut dest),
        source_path,
        total,
        on_progress,
    )
}

fn hash_existing_file(path: &Path) -> anyhow::Result<CopyHashResult> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let total = file
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .len();
    hash_stream_to_writer(&mut file, None, path, total, None)
}

fn sync_for_paranoid_readback(dest_path: &Path, parent: Option<PathBuf>) -> anyhow::Result<()> {
    let file = std::fs::File::open(dest_path)
        .with_context(|| format!("opening {}", dest_path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", dest_path.display()))?;
    if let Some(parent) = parent {
        let dir = std::fs::File::open(&parent)
            .with_context(|| format!("opening directory {}", parent.display()))?;
        dir.sync_all()
            .with_context(|| format!("syncing directory {}", parent.display()))?;
    }
    Ok(())
}

fn source_modified_at(entry: &db::TransferPlanEntryRow) -> anyhow::Result<Option<String>> {
    let metadata: serde_json::Value = serde_json::from_str(&entry.metadata_json)
        .with_context(|| format!("parsing transfer metadata for {}", entry.relative_path))?;
    Ok(metadata
        .get("source_modified_at")
        .and_then(|value| value.as_str())
        .map(str::to_string))
}

fn set_local_file_mtime(path: &Path, modified_at: Option<&str>) -> anyhow::Result<()> {
    let Some(modified_at) = modified_at else {
        return Ok(());
    };
    let time = file_time_from_rfc3339(modified_at)?;
    filetime::set_file_mtime(path, time)
        .with_context(|| format!("setting mtime on {}", path.display()))
}

fn set_remote_file_mtime(host: &str, path: &str, modified_at: Option<&str>) -> anyhow::Result<()> {
    let Some(modified_at) = modified_at else {
        return Ok(());
    };
    run_command(Command::new("ssh").arg(host).arg(format!(
        "touch -d {} {}",
        shell_quote(modified_at),
        remote_shell_path(path)
    )))
    .with_context(|| format!("setting remote mtime on {host}:{path}"))
}

fn file_time_from_rfc3339(value: &str) -> anyhow::Result<FileTime> {
    let dt = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid RFC3339 timestamp: {value}"))?;
    Ok(FileTime::from_unix_time(
        dt.timestamp(),
        dt.timestamp_subsec_nanos(),
    ))
}

fn hash_stream_to_writer(
    reader: &mut std::fs::File,
    mut writer: Option<&mut std::fs::File>,
    path: &Path,
    total: u64,
    mut on_progress: Option<&mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>>,
) -> anyhow::Result<CopyHashResult> {
    use std::io::{Read, Write};

    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buf = [0_u8; 64 * 1024];
    let started_at = Instant::now();

    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        if let Some(writer) = writer.as_deref_mut() {
            writer
                .write_all(chunk)
                .with_context(|| format!("writing copy for {}", path.display()))?;
        }
        bytes += read as u64;
        if let Some(callback) = on_progress.as_deref_mut() {
            callback(bytes, total, rate_per_second(bytes, started_at))?;
        }
    }
    if let Some(writer) = writer {
        writer
            .sync_all()
            .with_context(|| format!("syncing copy for {}", path.display()))?;
    }

    Ok(CopyHashResult {
        bytes,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: bytes_to_hex(sha256_hasher.finalize()),
    })
}

fn verify_copy_hash(
    conn: &Connection,
    entry: &db::TransferPlanEntryRow,
    actual: &CopyHashResult,
) -> anyhow::Result<()> {
    let Some(source_content_id) = entry.source_content_id.as_deref() else {
        return Ok(());
    };
    let Some(expected) = db::content_object_by_id(conn, source_content_id)? else {
        anyhow::bail!("planned source content object not found: {source_content_id}");
    };
    if expected.size_bytes != actual.bytes {
        anyhow::bail!(
            "source content size mismatch for {}: expected {}, copied {}",
            entry.relative_path,
            expected.size_bytes,
            actual.bytes
        );
    }
    if let Some(expected_blake3) = expected.blake3.as_deref() {
        if expected_blake3 != actual.blake3 {
            anyhow::bail!("BLAKE3 mismatch while copying {}", entry.relative_path);
        }
    }
    if let Some(expected_sha256) = expected.sha256.as_deref() {
        if expected_sha256 != actual.sha256 {
            anyhow::bail!("SHA-256 mismatch while copying {}", entry.relative_path);
        }
    }
    Ok(())
}

fn bytes_to_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

fn persist_job_event(
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

fn persist_transfer_file_event(
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

fn persist_transfer_progress_event(
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
            message: None,
        },
    };
    db::persist_event(conn, &envelope)?;
    Ok(())
}

fn rate_per_second(bytes: u64, started_at: Instant) -> f64 {
    let elapsed = started_at.elapsed().as_secs_f64();
    if elapsed <= f64::EPSILON {
        0.0
    } else {
        bytes as f64 / elapsed
    }
}

fn safe_join(root: &str, relative_path: &str) -> anyhow::Result<PathBuf> {
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        anyhow::bail!("refusing absolute transfer path: {relative_path}");
    }
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("refusing unsafe transfer path: {relative_path}");
            }
        }
    }
    Ok(Path::new(root).join(rel))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::db::{PathObservationInput, TransferPlanEntryRow};

    fn setup() -> (Connection, String, RootRow, RootRow) {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
        let source = db::find_root_by_machine_path(&conn, &machine_id, "/tmp/source")
            .unwrap()
            .unwrap();
        let dest = db::find_root_by_machine_path(&conn, &machine_id, "/tmp/dest")
            .unwrap()
            .unwrap();
        assert_eq!(source.id, source_id);
        assert_eq!(dest.id, dest_id);
        (conn, machine_id, source, dest)
    }

    fn observe(
        conn: &Connection,
        machine_id: &str,
        root_id: &str,
        relative_path: &str,
        size_bytes: u64,
        content_id: Option<&str>,
    ) {
        db::insert_path_observation(
            conn,
            PathObservationInput {
                machine_id,
                root_id,
                relative_path,
                basename: relative_path.rsplit('/').next().unwrap_or(relative_path),
                parent_path: ".",
                size_bytes,
                modified_at: None,
                content_id,
            },
        )
        .unwrap();
    }

    fn only_entry(conn: &Connection, plan_id: &str) -> TransferPlanEntryRow {
        let entries = db::transfer_plan_entries(conn, plan_id).unwrap();
        assert_eq!(entries.len(), 1);
        entries.into_iter().next().unwrap()
    }

    #[test]
    fn plans_copy_when_destination_is_missing() {
        let (conn, machine_id, source, dest) = setup();
        observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
        db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &result.plan_id);

        assert_eq!(entry.action, "copy");
        assert_eq!(entry.size_bytes, 10);
        let job = db::job_by_id(&conn, &result.job_id).unwrap().unwrap();
        assert_eq!(job.kind, "transfer_plan");
        assert_eq!(job.status, "completed");
        let events = db::events_for_job(&conn, &result.job_id).unwrap();
        assert_eq!(events.first().unwrap().event_kind, "job_created");
        assert_eq!(events.last().unwrap().event_kind, "job_completed");
    }

    #[test]
    fn plans_review_when_destination_root_has_same_content_elsewhere() {
        let (conn, machine_id, source, dest) = setup();
        let content_id = db::ensure_content_object(&conn, 10, "abc", "def").unwrap();
        observe(
            &conn,
            &machine_id,
            &source.id,
            "incoming/foo.png",
            10,
            Some(&content_id),
        );
        observe(
            &conn,
            &machine_id,
            &dest.id,
            "existing/foo.png",
            10,
            Some(&content_id),
        );
        db::toggle_selection_entry(&conn, &source.id, "incoming/foo.png").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &result.plan_id);

        assert_eq!(entry.action, "review");
        assert!(entry.reason.contains("collisions"));
        assert!(entry.metadata_json.contains("existing/foo.png"));
        assert!(entry.metadata_json.contains("hash_collisions"));
    }

    #[test]
    fn plans_review_when_destination_root_has_same_name_size_date_elsewhere() {
        let (conn, machine_id, source, dest) = setup();
        observe(&conn, &machine_id, &source.id, "incoming/foo.png", 10, None);
        observe(&conn, &machine_id, &dest.id, "existing/foo.png", 10, None);
        db::toggle_selection_entry(&conn, &source.id, "incoming/foo.png").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &result.plan_id);

        assert_eq!(entry.action, "review");
        assert!(entry.metadata_json.contains("existing/foo.png"));
        assert!(entry
            .metadata_json
            .contains("filename_size_date_collisions"));
    }

    #[test]
    fn plans_skip_when_content_matches() {
        let (conn, machine_id, source, dest) = setup();
        let content_id = db::ensure_content_object(&conn, 10, "abc", "def").unwrap();
        observe(
            &conn,
            &machine_id,
            &source.id,
            "a.txt",
            10,
            Some(&content_id),
        );
        observe(&conn, &machine_id, &dest.id, "a.txt", 10, Some(&content_id));
        db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        assert_eq!(only_entry(&conn, &result.plan_id).action, "skip");
    }

    #[test]
    fn plans_verify_needed_when_size_matches_without_hash_proof() {
        let (conn, machine_id, source, dest) = setup();
        observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
        observe(&conn, &machine_id, &dest.id, "a.txt", 10, None);
        db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        assert_eq!(only_entry(&conn, &result.plan_id).action, "verify_needed");
    }

    #[test]
    fn plans_conflict_when_destination_differs() {
        let (conn, machine_id, source, dest) = setup();
        observe(&conn, &machine_id, &source.id, "a.txt", 10, None);
        observe(&conn, &machine_id, &dest.id, "a.txt", 20, None);
        db::toggle_selection_entry(&conn, &source.id, "a.txt").unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        assert_eq!(only_entry(&conn, &result.plan_id).action, "conflict");
    }

    #[test]
    fn plans_review_when_unindexed_destination_path_exists_with_same_size() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        std::fs::write(dest_dir.path().join("a.txt"), b"1234567890").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id =
            db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
        let dest_id =
            db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
        observe(&conn, &machine_id, &source_id, "a.txt", 10, None);
        db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &result.plan_id);

        assert_eq!(entry.action, "review");
        assert!(entry.reason.contains("not indexed"));
        assert!(entry
            .metadata_json
            .contains("\"dest_observation_source\":\"probe\""));
    }

    #[test]
    fn plans_conflict_when_unindexed_destination_path_exists_with_different_size() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        std::fs::write(dest_dir.path().join("a.txt"), b"different-size").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id =
            db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
        let dest_id =
            db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
        observe(&conn, &machine_id, &source_id, "a.txt", 10, None);
        db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &result.plan_id);

        assert_eq!(entry.action, "conflict");
        assert!(entry.reason.contains("exists"));
        assert!(entry
            .metadata_json
            .contains("\"dest_observation_source\":\"probe\""));
    }

    #[test]
    fn plans_copy_when_unindexed_destination_parent_is_missing() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id =
            db::ensure_root(&conn, &machine_id, &source_dir.path().to_string_lossy()).unwrap();
        let dest_id =
            db::ensure_root(&conn, &machine_id, &dest_dir.path().to_string_lossy()).unwrap();
        observe(
            &conn,
            &machine_id,
            &source_id,
            "missing/dir/a.txt",
            10,
            None,
        );
        observe(
            &conn,
            &machine_id,
            &source_id,
            "missing/dir/b.txt",
            20,
            None,
        );
        db::toggle_selection_entry(&conn, &source_id, "missing/dir/a.txt").unwrap();
        db::toggle_selection_entry(&conn, &source_id, "missing/dir/b.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();

        let result = plan_selected_files(&conn, &source, &dest).unwrap();
        let entries = db::transfer_plan_entries(&conn, &result.plan_id).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry.action == "copy"));
        assert!(!dest_dir.path().join("missing").exists());
    }

    #[test]
    fn runs_copy_entries_and_updates_destination_projection() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source_file = source_dir.path().join("a.txt");
        let source_mtime = filetime::FileTime::from_unix_time(1_700_000_000, 123_000_000);
        std::fs::write(&source_file, b"hello").unwrap();
        filetime::set_file_mtime(&source_file, source_mtime).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_path = source_dir.path().to_string_lossy().to_string();
        let dest_path = dest_dir.path().to_string_lossy().to_string();
        let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
        let content_id = db::ensure_content_object(
            &conn,
            5,
            blake3::hash(b"hello").to_hex().as_ref(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )
        .unwrap();
        observe(
            &conn,
            &machine_id,
            &source_id,
            "a.txt",
            5,
            Some(&content_id),
        );
        db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
        let plan = plan_selected_files(&conn, &source, &dest).unwrap();

        let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

        assert_eq!(result.copied, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(
            std::fs::read(dest_dir.path().join("a.txt")).unwrap(),
            b"hello"
        );
        let dest_mtime = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(dest_dir.path().join("a.txt")).unwrap(),
        );
        assert_eq!(dest_mtime.unix_seconds(), source_mtime.unix_seconds());
        assert_eq!(dest_mtime.nanoseconds(), source_mtime.nanoseconds());
        let dest_obs = db::path_observation_for_root_path(&conn, &dest_id, "a.txt")
            .unwrap()
            .unwrap();
        assert_eq!(dest_obs.size_bytes, 5);
        assert_eq!(dest_obs.content_id.as_deref(), Some(content_id.as_str()));
        assert_eq!(
            dest_obs.modified_at.as_deref(),
            Some("2023-11-14T22:13:20.123+00:00")
        );
        let job = db::job_by_id(&conn, &result.job_id).unwrap().unwrap();
        assert_eq!(job.kind, "transfer_copy");
        assert_eq!(job.status, "completed");
    }

    #[test]
    fn copy_entries_preserve_source_relative_subdirectories() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source_dir.path().join("some")).unwrap();
        std::fs::write(source_dir.path().join("some/file.png"), b"hello").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_path = source_dir.path().to_string_lossy().to_string();
        let dest_path = dest_dir.path().to_string_lossy().to_string();
        let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
        let content_id = db::ensure_content_object(
            &conn,
            5,
            blake3::hash(b"hello").to_hex().as_ref(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )
        .unwrap();
        observe(
            &conn,
            &machine_id,
            &source_id,
            "some/file.png",
            5,
            Some(&content_id),
        );
        db::toggle_selection_entry(&conn, &source_id, "some/file.png").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
        let plan = plan_selected_files(&conn, &source, &dest).unwrap();
        let entry = only_entry(&conn, &plan.plan_id);
        assert_eq!(entry.relative_path, "some/file.png");
        assert_eq!(entry.dest_relative_path, "some/file.png");

        let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

        assert_eq!(result.copied, 1);
        assert_eq!(
            std::fs::read(dest_dir.path().join("some/file.png")).unwrap(),
            b"hello"
        );
        assert!(!dest_dir.path().join("file.png").exists());
    }

    #[test]
    fn transfer_copy_checkpoint_honors_cancel_request() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
        let plan_id = db::create_transfer_plan(
            &conn,
            None,
            &source_id,
            &dest_id,
            None,
            serde_json::json!({}),
        )
        .unwrap();
        let job_id = db::create_job(
            &conn,
            "transfer_copy",
            Some(&machine_id),
            Some(&source_id),
            serde_json::json!({ "plan_id": plan_id }),
        )
        .unwrap();
        db::start_job(&conn, &job_id).unwrap();
        assert!(db::request_job_cancel(&conn, &job_id).unwrap());
        let mut result = TransferRunResult {
            job_id: job_id.clone(),
            plan_id: plan_id.clone(),
            copied: 1,
            bytes_copied: 5,
            ..TransferRunResult::default()
        };

        assert!(complete_transfer_if_canceled(
            &conn,
            &job_id,
            &plan_id,
            "/tmp/source",
            2,
            10,
            &mut result
        )
        .unwrap());

        assert!(result.canceled);
        let job = db::job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, "canceled");
        let plan = db::transfer_plan_by_id(&conn, &plan_id).unwrap().unwrap();
        assert_eq!(plan.status, "canceled");
        let events = db::events_for_job(&conn, &job_id).unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_kind == "job_canceled"));
    }

    #[test]
    fn runs_copy_entries_to_retargeted_destination_paths() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_path = source_dir.path().to_string_lossy().to_string();
        let dest_path = dest_dir.path().to_string_lossy().to_string();
        let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
        let content_id = db::ensure_content_object(
            &conn,
            5,
            blake3::hash(b"hello").to_hex().as_ref(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        )
        .unwrap();
        observe(
            &conn,
            &machine_id,
            &source_id,
            "a.txt",
            5,
            Some(&content_id),
        );
        db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
        let plan = plan_selected_files(&conn, &source, &dest).unwrap();
        db::insert_transfer_plan_entry(
            &conn,
            db::TransferPlanEntryInput {
                plan_id: &plan.plan_id,
                relative_path: "a.txt",
                dest_relative_path: Some("renamed/a-copy.txt"),
                size_bytes: 5,
                source_content_id: Some(&content_id),
                dest_content_id: None,
                action: "copy",
                reason: "review retargeted for copy",
                metadata_json: serde_json::json!({ "decision": "retarget" }),
            },
        )
        .unwrap();

        let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

        assert_eq!(result.copied, 1);
        assert_eq!(
            std::fs::read(dest_dir.path().join("renamed/a-copy.txt")).unwrap(),
            b"hello"
        );
        assert!(!dest_dir.path().join("a.txt").exists());
        let dest_obs = db::path_observation_for_root_path(&conn, &dest_id, "renamed/a-copy.txt")
            .unwrap()
            .unwrap();
        assert_eq!(dest_obs.content_id.as_deref(), Some(content_id.as_str()));
    }

    #[test]
    fn copy_fails_when_stream_hash_does_not_match_planned_content() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        std::fs::write(source_dir.path().join("a.txt"), b"hello").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_path = source_dir.path().to_string_lossy().to_string();
        let dest_path = dest_dir.path().to_string_lossy().to_string();
        let source_id = db::ensure_root(&conn, &machine_id, &source_path).unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, &dest_path).unwrap();
        let bad_content_id = db::ensure_content_object(&conn, 5, "bad", "bad").unwrap();
        observe(
            &conn,
            &machine_id,
            &source_id,
            "a.txt",
            5,
            Some(&bad_content_id),
        );
        db::toggle_selection_entry(&conn, &source_id, "a.txt").unwrap();
        let source = db::root_by_id(&conn, &source_id).unwrap().unwrap();
        let dest = db::root_by_id(&conn, &dest_id).unwrap().unwrap();
        let plan = plan_selected_files(&conn, &source, &dest).unwrap();

        let result = run_transfer_plan(&conn, &plan.plan_id, false).unwrap();

        assert_eq!(result.copied, 0);
        assert_eq!(result.errors, 1);
    }

    #[test]
    fn safe_join_rejects_parent_paths() {
        assert!(safe_join("/tmp/root", "../escape.txt").is_err());
        assert!(safe_join("/tmp/root", "/tmp/absolute.txt").is_err());
        assert_eq!(
            safe_join("/tmp/root", "dir/file.txt").unwrap(),
            std::path::Path::new("/tmp/root").join("dir/file.txt")
        );
    }

    #[test]
    fn remote_join_rejects_parent_paths() {
        assert!(remote_join("/srv/root", "../escape.txt").is_err());
        assert!(remote_join("/srv/root", "/tmp/absolute.txt").is_err());
        assert_eq!(
            remote_join("/srv/root", "dir/file.txt").unwrap(),
            "/srv/root/dir/file.txt"
        );
        assert_eq!(remote_join("~", "dir/file.txt").unwrap(), "~/dir/file.txt");
    }

    #[test]
    fn ssh_root_remote_path_strips_matching_host_prefix() {
        assert_eq!(
            ssh_root_remote_path("nas01", "nas01:/srv/archive/photos"),
            "/srv/archive/photos"
        );
        assert_eq!(
            ssh_root_remote_path("nas01", "/srv/archive/photos"),
            "/srv/archive/photos"
        );
        assert_eq!(
            ssh_root_remote_path("nas02", "nas01:/srv/archive/photos"),
            "nas01:/srv/archive/photos"
        );
    }

    #[test]
    fn shell_quote_handles_single_quotes() {
        assert_eq!(shell_quote("/srv/has space"), "'/srv/has space'");
        assert_eq!(shell_quote("/srv/it's"), "'/srv/it'\\''s'");
        assert_eq!(remote_shell_path("~"), "$HOME");
        assert_eq!(remote_shell_path("~/dir/it's"), "$HOME/'dir/it'\\''s'");
    }

    #[test]
    fn transfer_chunks_use_fixed_size_offsets_with_remainder() {
        let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
        let chunks = transfer_chunks(chunk_size * 2 + 7);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[0].size, chunk_size);
        assert_eq!(chunks[1].index, 1);
        assert_eq!(chunks[1].offset, chunk_size);
        assert_eq!(chunks[1].size, chunk_size);
        assert_eq!(chunks[2].index, 2);
        assert_eq!(chunks[2].offset, chunk_size * 2);
        assert_eq!(chunks[2].size, 7);
    }

    #[test]
    fn transfer_copy_chunk_checkpoints_round_trip() {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id = db::ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
        let dest_id = db::ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
        let plan_id = db::create_transfer_plan(
            &conn,
            None,
            &source_id,
            &dest_id,
            None,
            serde_json::json!({}),
        )
        .unwrap();
        let entry = db::TransferPlanEntryRow {
            relative_path: "some/file.bin".to_string(),
            dest_relative_path: "some/file.bin".to_string(),
            size_bytes: 7,
            source_content_id: None,
            dest_content_id: None,
            action: "copy".to_string(),
            reason: "test".to_string(),
            metadata_json: "{}".to_string(),
        };
        let chunk = TransferChunk {
            index: 2,
            offset: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES * 2,
            size: 7,
        };

        persist_copy_chunk_checkpoint(&conn, "job_1", &plan_id, &entry, chunk, "abc").unwrap();
        let checkpoint = matching_copy_chunk_checkpoint(&conn, &plan_id, &entry, chunk)
            .unwrap()
            .unwrap();

        assert_eq!(checkpoint.digest, "abc");
        assert_eq!(checkpoint.offset_bytes, chunk.offset);
        assert_eq!(checkpoint.size_bytes, chunk.size);
        assert_eq!(
            db::transfer_copy_chunk_count_for_entry(
                &conn,
                &plan_id,
                &entry.relative_path,
                &entry.dest_relative_path
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn paranoid_syncs_file_and_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        sync_for_paranoid_readback(&path, Some(dir.path().to_path_buf())).unwrap();
    }
}
