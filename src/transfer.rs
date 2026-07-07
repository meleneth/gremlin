use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::db::{self, RootRow, TransferPlanActionSummary};
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{basename, local_machine_id, now_rfc3339, parent_path};

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
    source_path: &'a Path,
    dest_path: &'a Path,
    size_bytes: u64,
    action: &'a str,
    message: Option<&'a str>,
    error: Option<&'a str>,
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
}

struct CopyHashResult {
    bytes: u64,
    blake3: String,
    sha256: String,
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
    let local_machine = local_machine_id();
    if source_root.machine_id != local_machine || dest_root.machine_id != local_machine {
        anyhow::bail!("transfer run currently supports local source and destination roots only");
    }
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

    for entry in entries {
        let source_path = safe_join(&source_root.path, &entry.relative_path)?;
        let dest_path = safe_join(&dest_root.path, &entry.relative_path)?;
        let current = entry.relative_path.as_str();

        let copy_result = copy_one_entry(
            conn,
            &job_id,
            &dest_root,
            &entry,
            &source_path,
            &dest_path,
            paranoid,
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
                        source_path: &source_path,
                        dest_path: &dest_path,
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
                message: None,
            },
        };
        db::persist_event(conn, &progress)?;
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

enum CopyOutcome {
    Copied(u64),
    Skipped,
}

fn copy_one_entry(
    conn: &Connection,
    job_id: &str,
    dest_root: &RootRow,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest_path: &Path,
    paranoid: bool,
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

    if let Ok(dest_meta) = std::fs::metadata(dest_path) {
        if dest_meta.is_file() && dest_meta.len() == entry.size_bytes {
            let verified_content_id = if paranoid {
                sync_for_paranoid_readback(dest_path, None)?;
                let readback_hash = hash_existing_file(dest_path)?;
                verify_copy_hash(conn, entry, &readback_hash)?;
                Some(db::ensure_content_object(
                    conn,
                    readback_hash.bytes,
                    &readback_hash.blake3,
                    &readback_hash.sha256,
                )?)
            } else {
                None
            };
            insert_dest_observation(conn, dest_root, entry, verified_content_id.as_deref())?;
            persist_transfer_file_event(
                conn,
                job_id,
                TransferFileEventInput {
                    event_kind: EventKind::TransferSkipped,
                    relative_path: &entry.relative_path,
                    source_path,
                    dest_path,
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
    let copy_hash = copy_with_hash(source_path, dest_path)?;
    if copy_hash.bytes != entry.size_bytes {
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            copy_hash.bytes
        );
    }
    verify_copy_hash(conn, entry, &copy_hash)?;
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

    let content_id =
        db::ensure_content_object(conn, copy_hash.bytes, &copy_hash.blake3, &copy_hash.sha256)?;
    insert_dest_observation(conn, dest_root, entry, Some(&content_id))?;
    persist_transfer_file_event(
        conn,
        job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path,
            dest_path,
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(copy_hash.bytes))
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
) -> rusqlite::Result<()> {
    let base =
        basename(Path::new(&entry.relative_path)).unwrap_or_else(|_| entry.relative_path.clone());
    db::insert_path_observation(
        conn,
        db::PathObservationInput {
            machine_id: &dest_root.machine_id,
            root_id: &dest_root.id,
            relative_path: &entry.relative_path,
            basename: &base,
            parent_path: &parent_path(&entry.relative_path),
            size_bytes: entry.size_bytes,
            modified_at: None,
            content_id,
        },
    )
}

fn copy_with_hash(source_path: &Path, dest_path: &Path) -> anyhow::Result<CopyHashResult> {
    let mut source = std::fs::File::open(source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;
    let mut dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest_path)
        .with_context(|| format!("creating destination {}", dest_path.display()))?;
    hash_stream_to_writer(&mut source, Some(&mut dest), source_path)
}

fn hash_existing_file(path: &Path) -> anyhow::Result<CopyHashResult> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    hash_stream_to_writer(&mut file, None, path)
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

fn hash_stream_to_writer(
    reader: &mut std::fs::File,
    mut writer: Option<&mut std::fs::File>,
    path: &Path,
) -> anyhow::Result<CopyHashResult> {
    use std::io::{Read, Write};

    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buf = [0_u8; 64 * 1024];

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
    }
    if let Some(writer) = writer {
        writer
            .sync_all()
            .with_context(|| format!("syncing copy for {}", path.display()))?;
    }

    Ok(CopyHashResult {
        bytes,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: format!("{:x}", sha256_hasher.finalize()),
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

fn build_transfer_plan(
    conn: &Connection,
    source_root: &RootRow,
    dest_root: &RootRow,
    selection: &db::SelectionSummary,
    selected: &std::collections::BTreeSet<String>,
    job_id: &str,
) -> anyhow::Result<(String, Vec<TransferPlanActionSummary>)> {
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

        let dest = db::path_observation_for_root_path(conn, &dest_root.id, relative_path)?;
        let (action, reason, dest_content_id) = match dest.as_ref() {
            None => ("copy", "destination path is not indexed", None),
            Some(dest) => {
                match (
                    source.content_id.as_deref(),
                    dest.content_id.as_deref(),
                    source.size_bytes == dest.size_bytes,
                ) {
                    (Some(source_id), Some(dest_id), _) if source_id == dest_id => {
                        ("skip", "destination content already matches", Some(dest_id))
                    }
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
                size_bytes: source.size_bytes,
                source_content_id: source.content_id.as_deref(),
                dest_content_id,
                action,
                reason,
                metadata_json: serde_json::json!({
                    "source_modified_at": source.modified_at,
                    "dest_modified_at": dest.as_ref().and_then(|row| row.modified_at.clone()),
                }),
            },
        )?;
    }

    let summary = db::transfer_plan_action_summary(conn, &plan_id)?;
    Ok((plan_id, summary))
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
            source_path: input.source_path.display().to_string(),
            dest_path: input.dest_path.display().to_string(),
            size_bytes: input.size_bytes,
            action: input.action.to_string(),
            message: input.message.map(str::to_string),
            error: input.error.map(str::to_string),
        },
    };
    db::persist_event(conn, &envelope)
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
    fn runs_copy_entries_and_updates_destination_projection() {
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
            &blake3::hash(b"hello").to_hex().to_string(),
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
        let dest_obs = db::path_observation_for_root_path(&conn, &dest_id, "a.txt")
            .unwrap()
            .unwrap();
        assert_eq!(dest_obs.size_bytes, 5);
        assert_eq!(dest_obs.content_id.as_deref(), Some(content_id.as_str()));
        let job = db::job_by_id(&conn, &result.job_id).unwrap().unwrap();
        assert_eq!(job.kind, "transfer_copy");
        assert_eq!(job.status, "completed");
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
    fn paranoid_syncs_file_and_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, b"hello").unwrap();
        sync_for_paranoid_readback(&path, Some(dir.path().to_path_buf())).unwrap();
    }
}
