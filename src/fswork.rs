use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::db;
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{
    absolute_path, basename, lossy, new_id, now_rfc3339, parent_path, relative_path,
    system_time_rfc3339,
};

#[derive(Debug, Clone)]
struct FileMeta {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HashResult {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
    blake3: String,
    sha256: String,
}

pub fn scan_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let root_path = absolute_path(path).with_context(|| format!("resolving {}", path.display()))?;
    let skip_paths = db_sidecar_paths(db_path)?;
    let machine_id = db::ensure_local_machine_with_label(conn, machine_label)?;
    let root_id = db::ensure_root(conn, &machine_id, &lossy(&root_path))?;
    let job_id = db::create_job(
        conn,
        "scan",
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({ "path": lossy(&root_path) }),
    )?;
    run_scan_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
    )
}

pub fn hash_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let root_path = absolute_path(path).with_context(|| format!("resolving {}", path.display()))?;
    let skip_paths = db_sidecar_paths(db_path)?;
    let machine_id = db::ensure_local_machine_with_label(conn, machine_label)?;
    let root_id = db::ensure_root(conn, &machine_id, &lossy(&root_path))?;
    let job_id = db::create_job(
        conn,
        "hash",
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({ "path": lossy(&root_path) }),
    )?;
    run_hash_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
    )
}

pub fn run_queued_job(
    conn: &Connection,
    job_id: &str,
    db_path: &Path,
    machine_label: Option<&str>,
) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let job =
        db::job_by_id(conn, job_id)?.ok_or_else(|| anyhow::anyhow!("job not found: {job_id}"))?;
    if job.status != "created" {
        anyhow::bail!("job {job_id} is not runnable from status {}", job.status);
    }
    let params: serde_json::Value =
        serde_json::from_str(job.params_json.as_deref().unwrap_or("{}"))?;
    let path = params
        .get("path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("job {job_id} has no params_json.path"))?;
    let root_path = absolute_path(Path::new(path)).with_context(|| format!("resolving {path}"))?;
    let skip_paths = db_sidecar_paths(db_path)?;
    let machine_id = match job.machine_id {
        Some(machine_id) => machine_id,
        None => db::ensure_local_machine_with_label(conn, machine_label)?,
    };
    let root_id = match job.root_id {
        Some(root_id) => root_id,
        None => db::ensure_root(conn, &machine_id, &lossy(&root_path))?,
    };

    match job.kind.as_str() {
        "scan" => run_scan_job(conn, &root_path, &skip_paths, &machine_id, &root_id, job_id),
        "hash" => run_hash_job(conn, &root_path, &skip_paths, &machine_id, &root_id, job_id),
        other => anyhow::bail!("unsupported queued job kind: {other}"),
    }
}

fn run_scan_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    db::start_job(conn, job_id)?;

    let mut sequence = db::next_sequence(conn, job_id)?;
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "scan".to_string(),
            path: Some(lossy(root_path)),
            message: None,
            files_seen: None,
            errors: None,
        },
    )?;

    let mut files_seen = 0_u64;
    let mut errors = 0_u64;
    for entry in WalkDir::new(root_path) {
        match entry {
            Ok(entry) if entry.file_type().is_dir() => {
                let rel =
                    relative_path(root_path, entry.path()).unwrap_or_else(|_| ".".to_string());
                persist_db_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::DirectorySeen,
                    EventPayload::DirectorySeen { relative_path: rel },
                )?;
            }
            Ok(entry) if entry.file_type().is_file() => match file_meta(root_path, entry.path()) {
                Ok(meta) => {
                    if should_skip(entry.path(), skip_paths) {
                        continue;
                    }
                    db::insert_path_observation(
                        conn,
                        db::PathObservationInput {
                            machine_id,
                            root_id,
                            relative_path: &meta.relative_path,
                            basename: &meta.basename,
                            parent_path: &meta.parent_path,
                            size_bytes: meta.size_bytes,
                            modified_at: meta.modified_at.as_deref(),
                            content_id: None,
                        },
                    )?;
                    persist_db_event(
                        conn,
                        job_id,
                        &mut sequence,
                        EventKind::FileSeen,
                        EventPayload::FileSeen {
                            relative_path: meta.relative_path,
                            basename: meta.basename,
                            parent_path: meta.parent_path,
                            size_bytes: meta.size_bytes,
                            modified_at: meta.modified_at,
                        },
                    )?;
                    files_seen += 1;
                }
                Err(err) => {
                    errors += 1;
                    persist_db_event(
                        conn,
                        job_id,
                        &mut sequence,
                        EventKind::JobFailed,
                        EventPayload::Job {
                            kind: "scan".to_string(),
                            path: Some(lossy(entry.path())),
                            message: Some(err.to_string()),
                            files_seen: None,
                            errors: Some(errors),
                        },
                    )?;
                }
            },
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                persist_db_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::JobFailed,
                    EventPayload::Job {
                        kind: "scan".to_string(),
                        path: err.path().map(lossy),
                        message: Some(err.to_string()),
                        files_seen: None,
                        errors: Some(errors),
                    },
                )?;
            }
        }
    }

    let status = if errors == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobCompleted,
        EventPayload::Job {
            kind: "scan".to_string(),
            path: Some(lossy(root_path)),
            message: Some(status.to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    )?;
    db::complete_job(conn, job_id, status)?;
    println!("scan job {job_id}: {files_seen} files, {errors} errors");
    Ok(())
}

fn run_hash_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    db::start_job(conn, job_id)?;

    let mut sequence = db::next_sequence(conn, job_id)?;
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "hash".to_string(),
            path: Some(lossy(root_path)),
            message: None,
            files_seen: None,
            errors: None,
        },
    )?;

    let mut files_seen = 0_u64;
    let mut errors = 0_u64;
    for entry in WalkDir::new(root_path) {
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                let path = entry.path();
                if should_skip(path, skip_paths) {
                    continue;
                }
                let rel = relative_path(root_path, path).ok();
                if let Some(relative_path) = rel.clone() {
                    persist_db_event(
                        conn,
                        job_id,
                        &mut sequence,
                        EventKind::HashStarted,
                        EventPayload::HashStarted { relative_path },
                    )?;
                }
                match hash_file(root_path, path) {
                    Ok(result) => {
                        let content_id = db::ensure_content_object(
                            conn,
                            result.size_bytes,
                            &result.blake3,
                            &result.sha256,
                        )?;
                        db::insert_path_observation(
                            conn,
                            db::PathObservationInput {
                                machine_id,
                                root_id,
                                relative_path: &result.relative_path,
                                basename: &result.basename,
                                parent_path: &result.parent_path,
                                size_bytes: result.size_bytes,
                                modified_at: result.modified_at.as_deref(),
                                content_id: Some(&content_id),
                            },
                        )?;
                        persist_db_event(
                            conn,
                            job_id,
                            &mut sequence,
                            EventKind::HashCompleted,
                            EventPayload::HashCompleted {
                                relative_path: result.relative_path,
                                basename: result.basename,
                                parent_path: result.parent_path,
                                size_bytes: result.size_bytes,
                                modified_at: result.modified_at,
                                blake3: result.blake3,
                                sha256: result.sha256,
                            },
                        )?;
                        files_seen += 1;
                    }
                    Err(err) => {
                        errors += 1;
                        persist_db_event(
                            conn,
                            job_id,
                            &mut sequence,
                            EventKind::HashFailed,
                            EventPayload::HashFailed {
                                relative_path: rel,
                                path: lossy(path),
                                error: err.to_string(),
                            },
                        )?;
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                persist_db_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::HashFailed,
                    EventPayload::HashFailed {
                        relative_path: None,
                        path: err
                            .path()
                            .map(lossy)
                            .unwrap_or_else(|| "<unknown>".to_string()),
                        error: err.to_string(),
                    },
                )?;
            }
        }
    }

    let status = if errors == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobCompleted,
        EventPayload::Job {
            kind: "hash".to_string(),
            path: Some(lossy(root_path)),
            message: Some(status.to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    )?;
    db::complete_job(conn, job_id, status)?;
    println!("hash job {job_id}: {files_seen} files, {errors} errors");
    Ok(())
}

pub fn worker_hash_jsonl(path: &Path, out: Option<&Path>) -> anyhow::Result<()> {
    let root_path = absolute_path(path).with_context(|| format!("resolving {}", path.display()))?;
    let job_id = new_id("worker_job");
    let mut sequence = 1_i64;
    let skip_paths = match out {
        Some(path) => vec![absolute_path(path)?],
        None => Vec::new(),
    };

    let writer: Box<dyn Write> = match out {
        Some(path) => {
            Box::new(File::create(path).with_context(|| format!("creating {}", path.display()))?)
        }
        None => Box::new(std::io::stdout()),
    };
    let mut writer = std::io::BufWriter::new(writer);

    write_jsonl_event(
        &mut writer,
        &job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "worker_hash".to_string(),
            path: Some(lossy(&root_path)),
            message: None,
            files_seen: None,
            errors: None,
        },
    )?;

    let mut files_seen = 0_u64;
    let mut errors = 0_u64;
    for entry in WalkDir::new(&root_path) {
        match entry {
            Ok(entry) if entry.file_type().is_file() && should_skip(entry.path(), &skip_paths) => {}
            Ok(entry) if entry.file_type().is_file() => match hash_file(&root_path, entry.path()) {
                Ok(result) => {
                    write_jsonl_event(
                        &mut writer,
                        &job_id,
                        &mut sequence,
                        EventKind::HashCompleted,
                        EventPayload::HashCompleted {
                            relative_path: result.relative_path,
                            basename: result.basename,
                            parent_path: result.parent_path,
                            size_bytes: result.size_bytes,
                            modified_at: result.modified_at,
                            blake3: result.blake3,
                            sha256: result.sha256,
                        },
                    )?;
                    files_seen += 1;
                }
                Err(err) => {
                    errors += 1;
                    write_jsonl_event(
                        &mut writer,
                        &job_id,
                        &mut sequence,
                        EventKind::HashFailed,
                        EventPayload::HashFailed {
                            relative_path: relative_path(&root_path, entry.path()).ok(),
                            path: lossy(entry.path()),
                            error: err.to_string(),
                        },
                    )?;
                }
            },
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                write_jsonl_event(
                    &mut writer,
                    &job_id,
                    &mut sequence,
                    EventKind::HashFailed,
                    EventPayload::HashFailed {
                        relative_path: None,
                        path: err
                            .path()
                            .map(lossy)
                            .unwrap_or_else(|| "<unknown>".to_string()),
                        error: err.to_string(),
                    },
                )?;
            }
        }
    }

    write_jsonl_event(
        &mut writer,
        &job_id,
        &mut sequence,
        EventKind::JobCompleted,
        EventPayload::Job {
            kind: "worker_hash".to_string(),
            path: Some(lossy(&root_path)),
            message: Some("completed".to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    )?;
    writer.flush()?;
    Ok(())
}

pub fn hash_file(root: &Path, path: &Path) -> anyhow::Result<HashResult> {
    let meta = file_meta(root, path)?;
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];

    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        blake3_hasher.update(&buf[..read]);
        sha256_hasher.update(&buf[..read]);
    }

    Ok(HashResult {
        relative_path: meta.relative_path,
        basename: meta.basename,
        parent_path: meta.parent_path,
        size_bytes: meta.size_bytes,
        modified_at: meta.modified_at,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: format!("{:x}", sha256_hasher.finalize()),
    })
}

fn file_meta(root: &Path, path: &Path) -> anyhow::Result<FileMeta> {
    let metadata = path
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    let relative_path = relative_path(root, path)?;
    let basename = basename(path)?;
    let parent_path = parent_path(&relative_path);
    let modified_at = metadata.modified().ok().map(system_time_rfc3339);
    Ok(FileMeta {
        relative_path,
        basename,
        parent_path,
        size_bytes: metadata.len(),
        modified_at,
    })
}

fn db_sidecar_paths(db_path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let db = absolute_path(db_path)?;
    let db_display = lossy(&db);
    Ok(vec![
        db,
        PathBuf::from(format!("{db_display}-wal")),
        PathBuf::from(format!("{db_display}-shm")),
    ])
}

fn should_skip(path: &Path, skip_paths: &[PathBuf]) -> bool {
    skip_paths.iter().any(|skip| skip == path)
}

fn persist_db_event(
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
        created_at: now_rfc3339(),
        payload,
    };
    *sequence += 1;
    db::persist_event(conn, &envelope)
}

fn write_jsonl_event(
    writer: &mut impl Write,
    job_id: &str,
    sequence: &mut i64,
    event_kind: EventKind,
    payload: EventPayload,
) -> anyhow::Result<()> {
    let envelope = EventEnvelope {
        event_kind,
        job_id: Some(job_id.to_string()),
        sequence: Some(*sequence),
        created_at: now_rfc3339(),
        payload,
    };
    *sequence += 1;
    writeln!(writer, "{}", envelope.to_json_line()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_small_directory_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let result = hash_file(dir.path(), &file).unwrap();
        assert_eq!(result.relative_path, "hello.txt");
        assert_eq!(result.size_bytes, 5);
        assert_eq!(result.blake3.len(), 64);
        assert_eq!(result.sha256.len(), 64);
    }

    #[test]
    fn runs_queued_scan_job() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let job_id = db::queue_file_job(&conn, "scan", dir.path(), None).unwrap();

        run_queued_job(&conn, &job_id, &dir.path().join("gremlin.db"), None).unwrap();

        let job = db::job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, "completed");
        assert_eq!(db::table_count(&conn, "path_observations").unwrap(), 1);
    }
}
