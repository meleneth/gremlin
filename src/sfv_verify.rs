use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use rusqlite::Connection;

use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::{crc32, db, sfv};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub entries: u64,
    pub ok: u64,
    pub mismatched: u64,
    pub missing: u64,
    pub errors: u64,
    pub canceled: bool,
}

#[derive(Debug, Clone)]
struct VerifyEntry {
    manifest_path: String,
    root_relative_path: String,
    absolute_path: PathBuf,
    expected_crc32: String,
    size_bytes: u64,
}

#[derive(Debug, Clone)]
struct Progress {
    current_path: Option<String>,
    files_total: u64,
    files_seen: u64,
    files_done: u64,
    files_skipped: u64,
    errors: u64,
    bytes_done: u64,
    bytes_total: u64,
    file_bytes_done: u64,
    file_bytes_total: u64,
    started_at: Instant,
}

struct JobEventDetails {
    path: Option<String>,
    message: String,
    files_seen: u64,
    errors: u64,
}

pub fn verify_job(
    conn: &Connection,
    job_id: &str,
    root_path: &Path,
    sfv_relative_path: &str,
) -> anyhow::Result<Summary> {
    db::start_job(conn, job_id)?;
    let mut sequence = db::next_sequence(conn, job_id)?;
    persist_job_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobStarted,
        JobEventDetails {
            path: Some(sfv_relative_path.to_string()),
            message: "started".to_string(),
            files_seen: 0,
            errors: 0,
        },
    )?;

    let sfv_path = safe_join(root_path, sfv_relative_path)
        .with_context(|| format!("resolving SFV path {sfv_relative_path}"))?;
    let sfv_parent_abs = sfv_path.parent().unwrap_or(root_path);
    let sfv_parent_rel = parent_relative_path(sfv_relative_path);
    let parsed = parse_manifest(&sfv_path)?;
    let entries = prepare_entries(root_path, sfv_parent_abs, &sfv_parent_rel, parsed);
    let mut progress = Progress {
        current_path: None,
        files_total: entries.len() as u64,
        files_seen: 0,
        files_done: 0,
        files_skipped: 0,
        errors: 0,
        bytes_done: 0,
        bytes_total: entries.iter().map(|entry| entry.size_bytes).sum(),
        file_bytes_done: 0,
        file_bytes_total: 0,
        started_at: Instant::now(),
    };
    persist_progress(conn, job_id, &mut sequence, "preparing", &progress, None)?;

    let mut summary = Summary {
        entries: entries.len() as u64,
        ..Summary::default()
    };
    for entry in entries {
        if db::job_cancel_requested(conn, job_id)? {
            summary.canceled = true;
            break;
        }
        progress.current_path = Some(entry.root_relative_path.clone());
        progress.files_seen += 1;
        progress.file_bytes_done = 0;
        progress.file_bytes_total = entry.size_bytes;
        persist_progress(conn, job_id, &mut sequence, "verifying", &progress, None)?;

        match verify_entry(conn, job_id, &mut sequence, &entry, &mut progress) {
            Ok(VerifyOutcome::Ok) => {
                summary.ok += 1;
                progress.files_done += 1;
            }
            Ok(VerifyOutcome::Mismatch { actual }) => {
                summary.mismatched += 1;
                progress.errors += 1;
                progress.files_done += 1;
                persist_job_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::VerifyFinding,
                    JobEventDetails {
                        path: Some(entry.root_relative_path.clone()),
                        message: format!(
                            "crc mismatch expected {} actual {}",
                            entry.expected_crc32, actual
                        ),
                        files_seen: progress.files_seen,
                        errors: progress.errors,
                    },
                )?;
            }
            Ok(VerifyOutcome::Missing) => {
                summary.missing += 1;
                progress.errors += 1;
                progress.files_skipped += 1;
                persist_job_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::VerifyFinding,
                    JobEventDetails {
                        path: Some(entry.root_relative_path.clone()),
                        message: "missing".to_string(),
                        files_seen: progress.files_seen,
                        errors: progress.errors,
                    },
                )?;
            }
            Ok(VerifyOutcome::Canceled) => {
                summary.canceled = true;
                break;
            }
            Err(err) => {
                summary.errors += 1;
                progress.errors += 1;
                progress.files_skipped += 1;
                persist_job_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::VerifyFinding,
                    JobEventDetails {
                        path: Some(entry.root_relative_path.clone()),
                        message: err.to_string(),
                        files_seen: progress.files_seen,
                        errors: progress.errors,
                    },
                )?;
            }
        }
        progress.file_bytes_done = progress.file_bytes_total;
        persist_progress(conn, job_id, &mut sequence, "verifying", &progress, None)?;
    }

    progress.current_path = None;
    persist_progress(conn, job_id, &mut sequence, "finalizing", &progress, None)?;
    let status = if summary.canceled {
        "canceled"
    } else if summary.mismatched > 0 || summary.missing > 0 || summary.errors > 0 {
        "completed_with_errors"
    } else {
        "completed"
    };
    db::complete_job(conn, job_id, status)?;
    persist_job_event(
        conn,
        job_id,
        &mut sequence,
        if summary.canceled {
            EventKind::JobCanceled
        } else if status == "completed" {
            EventKind::JobCompleted
        } else {
            EventKind::JobFailed
        },
        JobEventDetails {
            path: Some(sfv_relative_path.to_string()),
            message: format!(
                "entries={} ok={} mismatched={} missing={} errors={}",
                summary.entries, summary.ok, summary.mismatched, summary.missing, summary.errors
            ),
            files_seen: progress.files_seen,
            errors: progress.errors,
        },
    )?;
    Ok(summary)
}

fn parse_manifest(path: &Path) -> anyhow::Result<Vec<sfv::Entry>> {
    let file = File::open(path).with_context(|| format!("opening SFV {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line =
            line.with_context(|| format!("reading line {} from {}", idx + 1, path.display()))?;
        if let Some(entry) = sfv::parse_line(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn prepare_entries(
    root_path: &Path,
    sfv_parent_abs: &Path,
    sfv_parent_rel: &str,
    entries: Vec<sfv::Entry>,
) -> Vec<VerifyEntry> {
    entries
        .into_iter()
        .map(|entry| {
            let absolute_path = safe_join(sfv_parent_abs, &entry.relative_path)
                .unwrap_or_else(|_| sfv_parent_abs.join(&entry.relative_path));
            let size_bytes = absolute_path.metadata().map(|meta| meta.len()).unwrap_or(0);
            let root_relative_path = if sfv_parent_rel == "." {
                entry.relative_path.clone()
            } else {
                crate::util::lossy(&Path::new(sfv_parent_rel).join(&entry.relative_path))
            };
            VerifyEntry {
                manifest_path: entry.relative_path,
                root_relative_path: crate::util::relative_path(root_path, &absolute_path)
                    .unwrap_or(root_relative_path),
                absolute_path,
                expected_crc32: entry.crc32,
                size_bytes,
            }
        })
        .collect()
}

enum VerifyOutcome {
    Ok,
    Mismatch { actual: String },
    Missing,
    Canceled,
}

fn verify_entry(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    entry: &VerifyEntry,
    progress: &mut Progress,
) -> anyhow::Result<VerifyOutcome> {
    let mut file = match File::open(&entry.absolute_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VerifyOutcome::Missing)
        }
        Err(err) => return Err(err).with_context(|| format!("opening {}", entry.manifest_path)),
    };
    let mut hasher = crc32::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut since_last_emit = 0_u64;
    loop {
        if db::job_cancel_requested(conn, job_id)? {
            return Ok(VerifyOutcome::Canceled);
        }
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", entry.manifest_path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        let read = read as u64;
        progress.file_bytes_done = progress.file_bytes_done.saturating_add(read);
        progress.bytes_done = progress.bytes_done.saturating_add(read);
        since_last_emit = since_last_emit.saturating_add(read);
        if since_last_emit >= 16 * 1024 * 1024
            || progress.file_bytes_done == progress.file_bytes_total
        {
            persist_progress(conn, job_id, sequence, "verifying", progress, None)?;
            since_last_emit = 0;
        }
    }
    let actual = format!("{:08X}", hasher.finalize());
    if actual == entry.expected_crc32 {
        Ok(VerifyOutcome::Ok)
    } else {
        Ok(VerifyOutcome::Mismatch { actual })
    }
}

fn persist_progress(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    phase: &str,
    progress: &Progress,
    message: Option<String>,
) -> rusqlite::Result<()> {
    db::update_job_progress(
        conn,
        job_id,
        db::JobProgressInput {
            phase,
            current_path: progress.current_path.as_deref(),
            files_total: Some(progress.files_total),
            files_seen: progress.files_seen,
            files_done: progress.files_done,
            files_skipped: progress.files_skipped,
            errors: progress.errors,
        },
    )?;
    let bytes_per_second = progress
        .started_at
        .elapsed()
        .as_secs_f64()
        .max(0.001)
        .recip()
        * progress.bytes_done as f64;
    persist_event(
        conn,
        job_id,
        sequence,
        EventKind::JobProgress,
        EventPayload::JobProgress {
            phase: phase.to_string(),
            current_path: progress.current_path.clone(),
            files_total: Some(progress.files_total),
            files_seen: progress.files_seen,
            files_done: progress.files_done,
            files_skipped: progress.files_skipped,
            errors: progress.errors,
            bytes_done: Some(progress.bytes_done),
            bytes_total: Some(progress.bytes_total),
            file_bytes_done: Some(progress.file_bytes_done),
            file_bytes_total: Some(progress.file_bytes_total),
            bytes_per_second: Some(bytes_per_second),
            message,
            chunk_confidence: None,
        },
    )
}

fn persist_job_event(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    event_kind: EventKind,
    details: JobEventDetails,
) -> rusqlite::Result<()> {
    persist_event(
        conn,
        job_id,
        sequence,
        event_kind,
        EventPayload::Job {
            kind: "sfv_verify".to_string(),
            path: details.path,
            message: Some(details.message),
            files_seen: Some(details.files_seen),
            errors: Some(details.errors),
        },
    )
}

fn persist_event(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    event_kind: EventKind,
    payload: EventPayload,
) -> rusqlite::Result<()> {
    let envelope = EventEnvelope {
        event_kind,
        job_id: Some(job_id.to_string()),
        sequence: Some(*sequence),
        created_at: crate::util::now_rfc3339(),
        payload,
    };
    *sequence += 1;
    db::persist_event(conn, &envelope)
}

fn parent_relative_path(relative_path: &str) -> String {
    Path::new(relative_path)
        .parent()
        .map(crate::util::lossy)
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

fn safe_join(base: &Path, relative_path: &str) -> anyhow::Result<PathBuf> {
    let mut out = base.to_path_buf();
    for component in Path::new(relative_path).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("SFV entry escapes the verify directory: {relative_path}");
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verifies_sfv_against_local_files_with_progress() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("a.bin"), b"123456789").unwrap();
        std::fs::write(
            root.join("root.sfv"),
            "a.bin CBF43926\nmissing.bin DEADBEEF\n",
        )
        .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = db::ensure_root(&conn, &machine_id, &crate::util::lossy(root)).unwrap();
        let job_id = db::create_job(
            &conn,
            "sfv_verify",
            Some(&machine_id),
            Some(&root_id),
            serde_json::json!({"path": root.display().to_string(), "sfv": "root.sfv"}),
        )
        .unwrap();

        let summary = verify_job(&conn, &job_id, root, "root.sfv").unwrap();

        assert_eq!(summary.entries, 2);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.missing, 1);
        assert!(db::recent_jobs_and_events(&conn, 20)
            .unwrap()
            .iter()
            .any(|event| event.event_kind == "job_progress"
                && event.payload_json.contains("\"file_bytes_total\":9")));
    }
}
