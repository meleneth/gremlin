use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use rusqlite::Connection;

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

pub fn run_transfer_plan(conn: &Connection, plan_id: &str) -> anyhow::Result<TransferRunResult> {
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

        let copy_result =
            copy_one_entry(conn, &job_id, &dest_root, &entry, &source_path, &dest_path);
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
            insert_dest_observation(conn, dest_root, entry)?;
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

    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating destination directory {}", parent.display()))?;
    }
    let bytes = std::fs::copy(source_path, dest_path).with_context(|| {
        format!(
            "copying {} to {}",
            source_path.display(),
            dest_path.display()
        )
    })?;
    if bytes != entry.size_bytes {
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            bytes
        );
    }

    insert_dest_observation(conn, dest_root, entry)?;
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
    Ok(CopyOutcome::Copied(bytes))
}

fn insert_dest_observation(
    conn: &Connection,
    dest_root: &RootRow,
    entry: &db::TransferPlanEntryRow,
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
            content_id: entry.source_content_id.as_deref(),
        },
    )
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
        let content_id = db::ensure_content_object(&conn, 5, "b3", "s256").unwrap();
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

        let result = run_transfer_plan(&conn, &plan.plan_id).unwrap();

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
    fn safe_join_rejects_parent_paths() {
        assert!(safe_join("/tmp/root", "../escape.txt").is_err());
        assert!(safe_join("/tmp/root", "/tmp/absolute.txt").is_err());
        assert_eq!(
            safe_join("/tmp/root", "dir/file.txt").unwrap(),
            std::path::Path::new("/tmp/root").join("dir/file.txt")
        );
    }
}
