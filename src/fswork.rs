use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::db;
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{
    absolute_path, basename, lossy, new_id, now_rfc3339, parent_path, relative_path,
    system_time_rfc3339,
};

#[derive(Debug, Clone, Copy)]
pub struct OutputOptions {
    pub details: bool,
    pub limit: usize,
    pub quiet: bool,
    pub json: bool,
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            details: false,
            limit: 20,
            quiet: false,
            json: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    New,
    Changed,
    Unchanged,
    Missing,
}

impl DeltaKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanDelta {
    pub kind: DeltaKind,
    pub relative_path: String,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
    pub previous_size_bytes: Option<u64>,
    pub previous_modified_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanSummary {
    pub job_id: String,
    pub files_seen: u64,
    pub errors: u64,
    pub new_count: usize,
    pub changed_count: usize,
    pub unchanged_count: usize,
    pub missing_count: usize,
    pub deltas: Vec<ScanDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HashSummary {
    pub job_id: String,
    pub files_hashed: u64,
    pub skipped_unchanged: u64,
    pub errors: u64,
    pub hashed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkHashSummary {
    pub job_id: String,
    pub chunk_size_bytes: u64,
    pub files_seen: u64,
    pub chunks_hashed: u64,
    pub bytes_hashed: u64,
    pub errors: u64,
    pub hashed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyKind {
    Ok,
    Changed,
    New,
    Missing,
    Error,
}

impl VerifyKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Changed => "changed",
            Self::New => "new",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyFinding {
    pub kind: VerifyKind,
    pub relative_path: String,
    pub basename: String,
    pub parent_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub expected_blake3: Option<String>,
    pub expected_sha256: Option<String>,
    pub actual_blake3: Option<String>,
    pub actual_sha256: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifySummary {
    pub job_id: String,
    pub ok: usize,
    pub changed: usize,
    pub new: usize,
    pub missing: usize,
    pub errors: usize,
    pub accepted: usize,
    pub findings: Vec<VerifyFinding>,
}

#[derive(Debug, Clone)]
struct FileMeta {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

#[derive(Debug, Clone)]
struct JobProgress {
    phase: &'static str,
    current_path: Option<String>,
    files_total: Option<u64>,
    files_seen: u64,
    files_done: u64,
    files_skipped: u64,
    errors: u64,
}

impl JobProgress {
    fn new(phase: &'static str) -> Self {
        Self {
            phase,
            current_path: None,
            files_total: None,
            files_seen: 0,
            files_done: 0,
            files_skipped: 0,
            errors: 0,
        }
    }
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

struct ChunkHashResult {
    relative_path: String,
    basename: String,
    parent_path: String,
    size_bytes: u64,
    modified_at: Option<String>,
    chunks: Vec<OwnedChunkHash>,
}

struct OwnedChunkHash {
    chunk_index: u64,
    offset_bytes: u64,
    size_bytes: u64,
    digest: String,
}

pub const DEFAULT_CHUNK_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const CHUNK_HASH_ALGORITHM: &str = "md5";

pub fn scan_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
    options: OutputOptions,
) -> anyhow::Result<ScanSummary> {
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
    let summary = run_scan_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
    )?;
    print_scan_summary(&summary, options);
    Ok(summary)
}

pub fn hash_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
    hash_all: bool,
    options: OutputOptions,
) -> anyhow::Result<HashSummary> {
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
        serde_json::json!({ "path": lossy(&root_path), "all": hash_all }),
    )?;
    let summary = run_hash_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
        hash_all,
    )?;
    print_hash_summary(&summary, options);
    Ok(summary)
}

pub fn chunk_hash_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
    chunk_size_bytes: u64,
    options: OutputOptions,
) -> anyhow::Result<ChunkHashSummary> {
    if chunk_size_bytes == 0 {
        anyhow::bail!("chunk size must be greater than zero");
    }
    db::init_schema(conn)?;
    let root_path = absolute_path(path).with_context(|| format!("resolving {}", path.display()))?;
    let skip_paths = db_sidecar_paths(db_path)?;
    let machine_id = db::ensure_local_machine_with_label(conn, machine_label)?;
    let root_id = db::ensure_root(conn, &machine_id, &lossy(&root_path))?;
    let job_id = db::create_job(
        conn,
        "chunk_hash",
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({
            "path": lossy(&root_path),
            "chunk_size_bytes": chunk_size_bytes,
            "algorithm": CHUNK_HASH_ALGORITHM,
        }),
    )?;
    let summary = run_chunk_hash_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
        chunk_size_bytes,
    )?;
    print_chunk_hash_summary(&summary, options);
    Ok(summary)
}

pub fn verify_to_db(
    conn: &Connection,
    path: &Path,
    db_path: &Path,
    machine_label: Option<&str>,
    accept: bool,
    options: OutputOptions,
) -> anyhow::Result<VerifySummary> {
    db::init_schema(conn)?;
    let root_path = absolute_path(path).with_context(|| format!("resolving {}", path.display()))?;
    let skip_paths = db_sidecar_paths(db_path)?;
    let machine_id = db::ensure_local_machine_with_label(conn, machine_label)?;
    let root_id = db::ensure_root(conn, &machine_id, &lossy(&root_path))?;
    let job_id = db::create_job(
        conn,
        "verify",
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({ "path": lossy(&root_path), "accept": accept }),
    )?;
    let summary = run_verify_job(
        conn,
        &root_path,
        &skip_paths,
        &machine_id,
        &root_id,
        &job_id,
        accept,
    )?;
    print_verify_summary(&summary, options);
    Ok(summary)
}

pub fn run_queued_job(
    conn: &Connection,
    job_id: &str,
    db_path: &Path,
    machine_label: Option<&str>,
    options: OutputOptions,
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
        "scan" => {
            let summary =
                run_scan_job(conn, &root_path, &skip_paths, &machine_id, &root_id, job_id)?;
            print_scan_summary(&summary, options);
        }
        "hash" => {
            let hash_all = params
                .get("all")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let summary = run_hash_job(
                conn,
                &root_path,
                &skip_paths,
                &machine_id,
                &root_id,
                job_id,
                hash_all,
            )?;
            print_hash_summary(&summary, options);
        }
        "verify" => {
            let accept = params
                .get("accept")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let summary = run_verify_job(
                conn,
                &root_path,
                &skip_paths,
                &machine_id,
                &root_id,
                job_id,
                accept,
            )?;
            print_verify_summary(&summary, options);
        }
        other => anyhow::bail!("unsupported queued job kind: {other}"),
    }
    Ok(())
}

fn run_scan_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
) -> anyhow::Result<ScanSummary> {
    db::start_job(conn, job_id)?;
    let previous = previous_observations(conn, machine_id, root_id)?;
    let mut seen = BTreeSet::new();
    let mut deltas = Vec::new();
    let mut sequence = db::next_sequence(conn, job_id)?;
    let mut progress = JobProgress::new("walking");
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
        if complete_if_canceled(conn, job_id, &mut sequence, "scan", root_path, &progress)? {
            return Ok(scan_summary(job_id, files_seen, errors, deltas));
        }
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
            Ok(entry) if entry.file_type().is_file() => {
                if should_skip(entry.path(), skip_paths) {
                    continue;
                }
                match file_meta(root_path, entry.path()) {
                    Ok(meta) => {
                        progress.current_path = Some(meta.relative_path.clone());
                        let delta = classify_scan_delta(&meta, previous.get(&meta.relative_path));
                        seen.insert(meta.relative_path.clone());
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
                        deltas.push(delta);
                        files_seen += 1;
                        progress.files_seen = files_seen;
                        progress.files_done = files_seen;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                    Err(err) => {
                        errors += 1;
                        progress.errors = errors;
                        persist_job_error(conn, job_id, &mut sequence, "scan", entry.path(), err)?;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                progress.errors = errors;
                persist_walk_error(conn, job_id, &mut sequence, "scan", err)?;
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
            }
        }
    }

    progress.phase = "finalizing";
    progress.current_path = None;
    persist_progress(conn, job_id, &mut sequence, &progress, None)?;

    for old in previous.values() {
        if !seen.contains(&old.relative_path) {
            deltas.push(ScanDelta {
                kind: DeltaKind::Missing,
                relative_path: old.relative_path.clone(),
                size_bytes: None,
                modified_at: None,
                previous_size_bytes: Some(old.size_bytes),
                previous_modified_at: old.modified_at.clone(),
            });
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
    Ok(scan_summary(job_id, files_seen, errors, deltas))
}

fn run_hash_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
    hash_all: bool,
) -> anyhow::Result<HashSummary> {
    db::start_job(conn, job_id)?;
    let previous = previous_observations(conn, machine_id, root_id)?;
    let mut sequence = db::next_sequence(conn, job_id)?;
    let mut progress = JobProgress::new("walking");
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

    let mut files_hashed = 0_u64;
    let mut skipped_unchanged = 0_u64;
    let mut errors = 0_u64;
    let mut hashed_paths = Vec::new();
    for entry in WalkDir::new(root_path) {
        if complete_if_canceled(conn, job_id, &mut sequence, "hash", root_path, &progress)? {
            return Ok(HashSummary {
                job_id: job_id.to_string(),
                files_hashed,
                skipped_unchanged,
                errors,
                hashed_paths,
            });
        }
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                let path = entry.path();
                if should_skip(path, skip_paths) {
                    continue;
                }
                let Ok(meta) = file_meta(root_path, path) else {
                    errors += 1;
                    progress.errors = errors;
                    persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    continue;
                };
                progress.current_path = Some(meta.relative_path.clone());
                progress.files_seen += 1;
                if !hash_all && !needs_hash(&meta, previous.get(&meta.relative_path)) {
                    skipped_unchanged += 1;
                    progress.files_skipped = skipped_unchanged;
                    persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    continue;
                }
                progress.phase = "processing";
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                persist_db_event(
                    conn,
                    job_id,
                    &mut sequence,
                    EventKind::HashStarted,
                    EventPayload::HashStarted {
                        relative_path: meta.relative_path.clone(),
                    },
                )?;
                match hash_file(root_path, path) {
                    Ok(result) => {
                        accept_hash_result(conn, machine_id, root_id, &result)?;
                        persist_hash_completed(conn, job_id, &mut sequence, &result)?;
                        hashed_paths.push(result.relative_path);
                        files_hashed += 1;
                        progress.files_done = files_hashed;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                    Err(err) => {
                        errors += 1;
                        progress.errors = errors;
                        persist_db_event(
                            conn,
                            job_id,
                            &mut sequence,
                            EventKind::HashFailed,
                            EventPayload::HashFailed {
                                relative_path: Some(meta.relative_path),
                                path: lossy(path),
                                error: err.to_string(),
                            },
                        )?;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                }
                progress.phase = "walking";
            }
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                progress.errors = errors;
                persist_walk_hash_error(conn, job_id, &mut sequence, err)?;
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
            }
        }
    }

    progress.phase = "finalizing";
    progress.current_path = None;
    persist_progress(conn, job_id, &mut sequence, &progress, None)?;

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
            files_seen: Some(files_hashed),
            errors: Some(errors),
        },
    )?;
    db::complete_job(conn, job_id, status)?;
    Ok(HashSummary {
        job_id: job_id.to_string(),
        files_hashed,
        skipped_unchanged,
        errors,
        hashed_paths,
    })
}

fn run_chunk_hash_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
    chunk_size_bytes: u64,
) -> anyhow::Result<ChunkHashSummary> {
    db::start_job(conn, job_id)?;
    let mut sequence = db::next_sequence(conn, job_id)?;
    let mut progress = JobProgress::new("walking");
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "chunk_hash".to_string(),
            path: Some(lossy(root_path)),
            message: Some(format!(
                "{CHUNK_HASH_ALGORITHM} chunks of {} bytes",
                chunk_size_bytes
            )),
            files_seen: None,
            errors: None,
        },
    )?;

    let mut files_seen = 0_u64;
    let mut chunks_hashed = 0_u64;
    let mut bytes_hashed = 0_u64;
    let mut errors = 0_u64;
    let mut hashed_paths = Vec::new();

    for entry in WalkDir::new(root_path) {
        if complete_if_canceled(
            conn,
            job_id,
            &mut sequence,
            "chunk_hash",
            root_path,
            &progress,
        )? {
            return Ok(ChunkHashSummary {
                job_id: job_id.to_string(),
                chunk_size_bytes,
                files_seen,
                chunks_hashed,
                bytes_hashed,
                errors,
                hashed_paths,
            });
        }
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                let path = entry.path();
                if should_skip(path, skip_paths) {
                    continue;
                }
                progress.phase = "processing";
                match chunk_hash_file(root_path, path, chunk_size_bytes) {
                    Ok(result) => {
                        progress.current_path = Some(result.relative_path.clone());
                        files_seen += 1;
                        progress.files_seen = files_seen;
                        accept_chunk_hash_result(
                            conn,
                            machine_id,
                            root_id,
                            job_id,
                            chunk_size_bytes,
                            &result,
                        )?;
                        chunks_hashed += result.chunks.len() as u64;
                        bytes_hashed += result.size_bytes;
                        hashed_paths.push(result.relative_path);
                        progress.files_done = files_seen;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                    Err(err) => {
                        errors += 1;
                        progress.errors = errors;
                        persist_job_error(conn, job_id, &mut sequence, "chunk_hash", path, err)?;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                }
                progress.phase = "walking";
            }
            Ok(_) => {}
            Err(err) => {
                errors += 1;
                progress.errors = errors;
                persist_walk_error(conn, job_id, &mut sequence, "chunk_hash", err)?;
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
            }
        }
    }

    progress.phase = "finalizing";
    progress.current_path = None;
    persist_progress(conn, job_id, &mut sequence, &progress, None)?;

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
            kind: "chunk_hash".to_string(),
            path: Some(lossy(root_path)),
            message: Some(status.to_string()),
            files_seen: Some(files_seen),
            errors: Some(errors),
        },
    )?;
    db::complete_job(conn, job_id, status)?;
    Ok(ChunkHashSummary {
        job_id: job_id.to_string(),
        chunk_size_bytes,
        files_seen,
        chunks_hashed,
        bytes_hashed,
        errors,
        hashed_paths,
    })
}

fn run_verify_job(
    conn: &Connection,
    root_path: &Path,
    skip_paths: &[PathBuf],
    machine_id: &str,
    root_id: &str,
    job_id: &str,
    accept: bool,
) -> anyhow::Result<VerifySummary> {
    db::start_job(conn, job_id)?;
    let baselines = hash_baselines(conn, machine_id, root_id)?;
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    let mut sequence = db::next_sequence(conn, job_id)?;
    let mut progress = JobProgress::new("walking");
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::VerifyStarted,
        EventPayload::Job {
            kind: "verify".to_string(),
            path: Some(lossy(root_path)),
            message: None,
            files_seen: None,
            errors: None,
        },
    )?;

    for entry in WalkDir::new(root_path) {
        if complete_if_canceled(conn, job_id, &mut sequence, "verify", root_path, &progress)? {
            return Ok(verify_summary(job_id, findings));
        }
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                let path = entry.path();
                if should_skip(path, skip_paths) {
                    continue;
                }
                let current_path = relative_path(root_path, path).unwrap_or_else(|_| lossy(path));
                progress.current_path = Some(current_path);
                progress.files_seen += 1;
                progress.phase = "processing";
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                match hash_file(root_path, path) {
                    Ok(result) => {
                        seen.insert(result.relative_path.clone());
                        let finding =
                            classify_verify_result(&result, baselines.get(&result.relative_path));
                        if accept && matches!(finding.kind, VerifyKind::Changed | VerifyKind::New) {
                            accept_hash_result(conn, machine_id, root_id, &result)?;
                        }
                        if finding.kind != VerifyKind::Ok {
                            persist_verify_finding(conn, job_id, &mut sequence, &finding)?;
                        }
                        findings.push(finding);
                        progress.files_done += 1;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                    Err(err) => {
                        let rel = relative_path(root_path, path).unwrap_or_else(|_| lossy(path));
                        let base = basename(path).unwrap_or_else(|_| rel.clone());
                        let finding = VerifyFinding {
                            kind: VerifyKind::Error,
                            relative_path: rel.clone(),
                            basename: base,
                            parent_path: parent_path(&rel),
                            size_bytes: 0,
                            modified_at: None,
                            expected_blake3: baselines.get(&rel).map(|row| row.blake3.clone()),
                            expected_sha256: baselines.get(&rel).map(|row| row.sha256.clone()),
                            actual_blake3: None,
                            actual_sha256: None,
                            error: Some(err.to_string()),
                        };
                        persist_verify_finding(conn, job_id, &mut sequence, &finding)?;
                        findings.push(finding);
                        progress.errors += 1;
                        persist_progress(conn, job_id, &mut sequence, &progress, None)?;
                    }
                }
                progress.phase = "walking";
            }
            Ok(_) => {}
            Err(err) => {
                let path = err
                    .path()
                    .map(lossy)
                    .unwrap_or_else(|| "<unknown>".to_string());
                let finding = VerifyFinding {
                    kind: VerifyKind::Error,
                    relative_path: path.clone(),
                    basename: path.clone(),
                    parent_path: ".".to_string(),
                    size_bytes: 0,
                    modified_at: None,
                    expected_blake3: None,
                    expected_sha256: None,
                    actual_blake3: None,
                    actual_sha256: None,
                    error: Some(err.to_string()),
                };
                persist_verify_finding(conn, job_id, &mut sequence, &finding)?;
                findings.push(finding);
                progress.errors += 1;
                persist_progress(conn, job_id, &mut sequence, &progress, None)?;
            }
        }
    }
    for baseline in baselines.values() {
        if !seen.contains(&baseline.relative_path) {
            let finding = VerifyFinding {
                kind: VerifyKind::Missing,
                relative_path: baseline.relative_path.clone(),
                basename: basename(Path::new(&baseline.relative_path))
                    .unwrap_or_else(|_| baseline.relative_path.clone()),
                parent_path: parent_path(&baseline.relative_path),
                size_bytes: baseline.size_bytes,
                modified_at: None,
                expected_blake3: Some(baseline.blake3.clone()),
                expected_sha256: Some(baseline.sha256.clone()),
                actual_blake3: None,
                actual_sha256: None,
                error: None,
            };
            persist_verify_finding(conn, job_id, &mut sequence, &finding)?;
            findings.push(finding);
            progress.files_done += 1;
            persist_progress(conn, job_id, &mut sequence, &progress, None)?;
        }
    }
    let mut summary = verify_summary(job_id, findings);
    if accept {
        summary.accepted = summary.changed + summary.new;
    }
    let errors = summary.errors as u64;
    let status = if errors == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    progress.phase = "finalizing";
    progress.current_path = None;
    progress.errors = errors;
    persist_progress(conn, job_id, &mut sequence, &progress, None)?;
    persist_db_event(
        conn,
        job_id,
        &mut sequence,
        EventKind::VerifyCompleted,
        EventPayload::Job {
            kind: "verify".to_string(),
            path: Some(lossy(root_path)),
            message: Some(format!(
                "ok={} changed={} new={} missing={} errors={} accepted={}",
                summary.ok,
                summary.changed,
                summary.new,
                summary.missing,
                summary.errors,
                summary.accepted
            )),
            files_seen: Some((summary.ok + summary.changed + summary.new) as u64),
            errors: Some(errors),
        },
    )?;
    db::complete_job(conn, job_id, status)?;
    Ok(summary)
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

fn chunk_hash_file(
    root: &Path,
    path: &Path,
    chunk_size_bytes: u64,
) -> anyhow::Result<ChunkHashResult> {
    let meta = file_meta(root, path)?;
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut chunks = Vec::new();
    let mut buf = [0_u8; 64 * 1024];
    let mut chunk_hasher = md5::Context::new();
    let mut chunk_index = 0_u64;
    let mut chunk_offset = 0_u64;
    let mut chunk_bytes = 0_u64;

    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let mut consumed = 0_usize;
        while consumed < read {
            let remaining_in_chunk = (chunk_size_bytes - chunk_bytes) as usize;
            let take = remaining_in_chunk.min(read - consumed);
            chunk_hasher.consume(&buf[consumed..consumed + take]);
            consumed += take;
            chunk_bytes += take as u64;
            if chunk_bytes == chunk_size_bytes {
                chunks.push(OwnedChunkHash {
                    chunk_index,
                    offset_bytes: chunk_offset,
                    size_bytes: chunk_bytes,
                    digest: format!("{:x}", chunk_hasher.compute()),
                });
                chunk_index += 1;
                chunk_offset += chunk_bytes;
                chunk_bytes = 0;
                chunk_hasher = md5::Context::new();
            }
        }
    }
    if chunk_bytes > 0 {
        chunks.push(OwnedChunkHash {
            chunk_index,
            offset_bytes: chunk_offset,
            size_bytes: chunk_bytes,
            digest: format!("{:x}", chunk_hasher.compute()),
        });
    }

    Ok(ChunkHashResult {
        relative_path: meta.relative_path,
        basename: meta.basename,
        parent_path: meta.parent_path,
        size_bytes: meta.size_bytes,
        modified_at: meta.modified_at,
        chunks,
    })
}

fn previous_observations(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
) -> anyhow::Result<BTreeMap<String, db::PathObservationRow>> {
    Ok(db::path_observations_for_root(conn, machine_id, root_id)?
        .into_iter()
        .map(|row| (row.relative_path.clone(), row))
        .collect())
}

fn hash_baselines(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
) -> anyhow::Result<BTreeMap<String, db::HashBaselineRow>> {
    Ok(db::hash_baselines_for_root(conn, machine_id, root_id)?
        .into_iter()
        .map(|row| (row.relative_path.clone(), row))
        .collect())
}

fn classify_scan_delta(meta: &FileMeta, previous: Option<&db::PathObservationRow>) -> ScanDelta {
    let Some(previous) = previous else {
        return ScanDelta {
            kind: DeltaKind::New,
            relative_path: meta.relative_path.clone(),
            size_bytes: Some(meta.size_bytes),
            modified_at: meta.modified_at.clone(),
            previous_size_bytes: None,
            previous_modified_at: None,
        };
    };
    let changed =
        previous.size_bytes != meta.size_bytes || previous.modified_at != meta.modified_at;
    ScanDelta {
        kind: if changed {
            DeltaKind::Changed
        } else {
            DeltaKind::Unchanged
        },
        relative_path: meta.relative_path.clone(),
        size_bytes: Some(meta.size_bytes),
        modified_at: meta.modified_at.clone(),
        previous_size_bytes: Some(previous.size_bytes),
        previous_modified_at: previous.modified_at.clone(),
    }
}

fn needs_hash(meta: &FileMeta, previous: Option<&db::PathObservationRow>) -> bool {
    match previous {
        None => true,
        Some(previous) => {
            previous.content_id.is_none()
                || previous.size_bytes != meta.size_bytes
                || previous.modified_at != meta.modified_at
        }
    }
}

fn classify_verify_result(
    result: &HashResult,
    baseline: Option<&db::HashBaselineRow>,
) -> VerifyFinding {
    let (kind, expected_blake3, expected_sha256) = match baseline {
        None => (VerifyKind::New, None, None),
        Some(baseline) if baseline.blake3 == result.blake3 && baseline.sha256 == result.sha256 => (
            VerifyKind::Ok,
            Some(baseline.blake3.clone()),
            Some(baseline.sha256.clone()),
        ),
        Some(baseline) => (
            VerifyKind::Changed,
            Some(baseline.blake3.clone()),
            Some(baseline.sha256.clone()),
        ),
    };
    VerifyFinding {
        kind,
        relative_path: result.relative_path.clone(),
        basename: result.basename.clone(),
        parent_path: result.parent_path.clone(),
        size_bytes: result.size_bytes,
        modified_at: result.modified_at.clone(),
        expected_blake3,
        expected_sha256,
        actual_blake3: Some(result.blake3.clone()),
        actual_sha256: Some(result.sha256.clone()),
        error: None,
    }
}

fn scan_summary(job_id: &str, files_seen: u64, errors: u64, deltas: Vec<ScanDelta>) -> ScanSummary {
    let mut summary = ScanSummary {
        job_id: job_id.to_string(),
        files_seen,
        errors,
        deltas,
        ..ScanSummary::default()
    };
    for delta in &summary.deltas {
        match delta.kind {
            DeltaKind::New => summary.new_count += 1,
            DeltaKind::Changed => summary.changed_count += 1,
            DeltaKind::Unchanged => summary.unchanged_count += 1,
            DeltaKind::Missing => summary.missing_count += 1,
        }
    }
    summary
}

fn verify_summary(job_id: &str, findings: Vec<VerifyFinding>) -> VerifySummary {
    let mut summary = VerifySummary {
        job_id: job_id.to_string(),
        findings,
        ..VerifySummary::default()
    };
    for finding in &summary.findings {
        match finding.kind {
            VerifyKind::Ok => summary.ok += 1,
            VerifyKind::Changed => summary.changed += 1,
            VerifyKind::New => summary.new += 1,
            VerifyKind::Missing => summary.missing += 1,
            VerifyKind::Error => summary.errors += 1,
        }
    }
    summary
}

fn accept_hash_result(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
    result: &HashResult,
) -> anyhow::Result<()> {
    let content_id =
        db::ensure_content_object(conn, result.size_bytes, &result.blake3, &result.sha256)?;
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
    Ok(())
}

fn accept_chunk_hash_result(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
    job_id: &str,
    chunk_size_bytes: u64,
    result: &ChunkHashResult,
) -> anyhow::Result<()> {
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
            content_id: None,
        },
    )?;
    let Some(path_observation_id) = db::path_observation_id(conn, root_id, &result.relative_path)?
    else {
        anyhow::bail!(
            "path observation missing after insert: {}",
            result.relative_path
        );
    };
    let inputs = result
        .chunks
        .iter()
        .map(|chunk| db::ObservationChunkHashInput {
            chunk_size_bytes,
            chunk_index: chunk.chunk_index,
            offset_bytes: chunk.offset_bytes,
            size_bytes: chunk.size_bytes,
            algorithm: CHUNK_HASH_ALGORITHM,
            digest: &chunk.digest,
            job_id: Some(job_id),
        })
        .collect::<Vec<_>>();
    db::replace_observation_chunk_hashes(
        conn,
        &path_observation_id,
        chunk_size_bytes,
        CHUNK_HASH_ALGORITHM,
        &inputs,
    )?;
    Ok(())
}

fn persist_hash_completed(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    result: &HashResult,
) -> rusqlite::Result<()> {
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::HashCompleted,
        EventPayload::HashCompleted {
            relative_path: result.relative_path.clone(),
            basename: result.basename.clone(),
            parent_path: result.parent_path.clone(),
            size_bytes: result.size_bytes,
            modified_at: result.modified_at.clone(),
            blake3: result.blake3.clone(),
            sha256: result.sha256.clone(),
        },
    )
}

fn persist_verify_finding(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    finding: &VerifyFinding,
) -> rusqlite::Result<()> {
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::VerifyFinding,
        EventPayload::VerifyFinding {
            result: finding.kind.as_str().to_string(),
            relative_path: finding.relative_path.clone(),
            basename: finding.basename.clone(),
            parent_path: finding.parent_path.clone(),
            size_bytes: finding.size_bytes,
            modified_at: finding.modified_at.clone(),
            expected_blake3: finding.expected_blake3.clone(),
            expected_sha256: finding.expected_sha256.clone(),
            actual_blake3: finding.actual_blake3.clone(),
            actual_sha256: finding.actual_sha256.clone(),
            error: finding.error.clone(),
        },
    )
}

fn persist_job_error(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    kind: &str,
    path: &Path,
    err: anyhow::Error,
) -> rusqlite::Result<()> {
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::JobFailed,
        EventPayload::Job {
            kind: kind.to_string(),
            path: Some(lossy(path)),
            message: Some(err.to_string()),
            files_seen: None,
            errors: None,
        },
    )
}

fn persist_walk_error(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    kind: &str,
    err: walkdir::Error,
) -> rusqlite::Result<()> {
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::JobFailed,
        EventPayload::Job {
            kind: kind.to_string(),
            path: err.path().map(lossy),
            message: Some(err.to_string()),
            files_seen: None,
            errors: None,
        },
    )
}

fn persist_walk_hash_error(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    err: walkdir::Error,
) -> rusqlite::Result<()> {
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::HashFailed,
        EventPayload::HashFailed {
            relative_path: None,
            path: err
                .path()
                .map(lossy)
                .unwrap_or_else(|| "<unknown>".to_string()),
            error: err.to_string(),
        },
    )
}

fn persist_progress(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    progress: &JobProgress,
    message: Option<String>,
) -> rusqlite::Result<()> {
    db::update_job_progress(
        conn,
        job_id,
        db::JobProgressInput {
            phase: progress.phase,
            current_path: progress.current_path.as_deref(),
            files_total: progress.files_total,
            files_seen: progress.files_seen,
            files_done: progress.files_done,
            files_skipped: progress.files_skipped,
            errors: progress.errors,
        },
    )?;
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::JobProgress,
        EventPayload::JobProgress {
            phase: progress.phase.to_string(),
            current_path: progress.current_path.clone(),
            files_total: progress.files_total,
            files_seen: progress.files_seen,
            files_done: progress.files_done,
            files_skipped: progress.files_skipped,
            errors: progress.errors,
            bytes_done: None,
            bytes_total: None,
            file_bytes_done: None,
            file_bytes_total: None,
            bytes_per_second: None,
            message,
        },
    )
}

fn complete_if_canceled(
    conn: &Connection,
    job_id: &str,
    sequence: &mut i64,
    kind: &str,
    root_path: &Path,
    progress: &JobProgress,
) -> rusqlite::Result<bool> {
    if !db::job_cancel_requested(conn, job_id)? {
        return Ok(false);
    }
    persist_progress(
        conn,
        job_id,
        sequence,
        progress,
        Some("cancel requested".to_string()),
    )?;
    persist_db_event(
        conn,
        job_id,
        sequence,
        EventKind::JobCanceled,
        EventPayload::Job {
            kind: kind.to_string(),
            path: Some(lossy(root_path)),
            message: Some("canceled between files".to_string()),
            files_seen: Some(progress.files_seen),
            errors: Some(progress.errors),
        },
    )?;
    db::complete_job(conn, job_id, "canceled")?;
    Ok(true)
}

fn file_meta(root: &Path, path: &Path) -> anyhow::Result<FileMeta> {
    let metadata = path
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    let basename = basename(path)?;
    let relative_path = match relative_path(root, path)?.as_str() {
        "." => basename.clone(),
        other => other.to_string(),
    };
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

fn print_scan_summary(summary: &ScanSummary, options: OutputOptions) {
    if options.quiet {
        return;
    }
    if options.json {
        print_json_summary(summary);
        return;
    }
    println!(
        "scan job {}: {} files, {} new, {} changed, {} missing, {} errors",
        summary.job_id,
        summary.files_seen,
        summary.new_count,
        summary.changed_count,
        summary.missing_count,
        summary.errors
    );
    print_scan_deltas(&summary.deltas, options);
}

fn print_hash_summary(summary: &HashSummary, options: OutputOptions) {
    if options.quiet {
        return;
    }
    if options.json {
        print_json_summary(summary);
        return;
    }
    println!(
        "hash job {}: {} hashed, {} skipped unchanged, {} errors",
        summary.job_id, summary.files_hashed, summary.skipped_unchanged, summary.errors
    );
    let limit = if options.details {
        summary.hashed_paths.len()
    } else {
        options.limit
    };
    for path in summary.hashed_paths.iter().take(limit) {
        println!("hashed\t{path}");
    }
}

fn print_chunk_hash_summary(summary: &ChunkHashSummary, options: OutputOptions) {
    if options.quiet {
        return;
    }
    if options.json {
        print_json_summary(summary);
        return;
    }
    println!(
        "chunk hash job {}: {} files, {} chunks, {}, {} errors, chunk_size={}",
        summary.job_id,
        summary.files_seen,
        summary.chunks_hashed,
        crate::util::human_size(summary.bytes_hashed),
        summary.errors,
        crate::util::human_size(summary.chunk_size_bytes)
    );
    let limit = if options.details {
        summary.hashed_paths.len()
    } else {
        options.limit
    };
    for path in summary.hashed_paths.iter().take(limit) {
        println!("chunked\t{path}");
    }
}

fn print_verify_summary(summary: &VerifySummary, options: OutputOptions) {
    if options.quiet {
        return;
    }
    if options.json {
        print_json_summary(summary);
        return;
    }
    println!(
        "verify job {}: {} ok, {} changed, {} new, {} missing, {} errors, {} accepted",
        summary.job_id,
        summary.ok,
        summary.changed,
        summary.new,
        summary.missing,
        summary.errors,
        summary.accepted
    );
    let notable = summary
        .findings
        .iter()
        .filter(|finding| finding.kind != VerifyKind::Ok)
        .collect::<Vec<_>>();
    let limit = if options.details {
        notable.len()
    } else {
        options.limit
    };
    for finding in notable.into_iter().take(limit) {
        println!("{}\t{}", finding.kind.as_str(), finding.relative_path);
    }
}

fn print_json_summary(summary: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(summary).expect("serializing summary should not fail")
    );
}

fn print_scan_deltas(deltas: &[ScanDelta], options: OutputOptions) {
    let notable = deltas
        .iter()
        .filter(|delta| delta.kind != DeltaKind::Unchanged)
        .collect::<Vec<_>>();
    let limit = if options.details {
        notable.len()
    } else {
        options.limit
    };
    for delta in notable.into_iter().take(limit) {
        println!(
            "{}\t{}\t{}\t{}\tprev:{}\tprev_mtime:{}",
            delta.kind.as_str(),
            delta.relative_path,
            delta
                .size_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            delta.modified_at.as_deref().unwrap_or("-"),
            delta
                .previous_size_bytes
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            delta.previous_modified_at.as_deref().unwrap_or("-")
        );
    }
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
    fn hashes_single_file_root_with_basename_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();

        let result = hash_file(&file, &file).unwrap();

        assert_eq!(result.relative_path, "hello.txt");
        assert_eq!(result.basename, "hello.txt");
        assert_eq!(result.parent_path, ".");
    }

    #[test]
    fn runs_queued_scan_job() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let job_id = db::queue_file_job(&conn, "scan", dir.path(), None).unwrap();

        run_queued_job(
            &conn,
            &job_id,
            &dir.path().join("gremlin.db"),
            None,
            OutputOptions::default(),
        )
        .unwrap();

        let job = db::job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, "completed");
        assert_eq!(db::table_count(&conn, "path_observations").unwrap(), 1);
    }

    #[test]
    fn canceled_queued_scan_stops_before_file_work() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let job_id = db::queue_file_job(&conn, "scan", dir.path(), None).unwrap();
        assert!(db::request_job_cancel(&conn, &job_id).unwrap());

        run_queued_job(
            &conn,
            &job_id,
            &dir.path().join("gremlin.db"),
            None,
            OutputOptions::default(),
        )
        .unwrap();

        let job = db::job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, "canceled");
        assert_eq!(db::table_count(&conn, "path_observations").unwrap(), 0);
        assert!(db::events_for_job(&conn, &job_id)
            .unwrap()
            .iter()
            .any(|event| event.event_kind == "job_canceled"));
    }

    #[test]
    fn scan_reports_new_changed_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let db_path = dir.path().join("gremlin.db");
        let conn = db::open_or_create(&db_path).unwrap();
        db::init_schema(&conn).unwrap();
        let first =
            scan_to_db(&conn, dir.path(), &db_path, None, OutputOptions::default()).unwrap();
        assert_eq!(first.new_count, 1);

        std::fs::write(&file, b"hello world").unwrap();
        let changed =
            scan_to_db(&conn, dir.path(), &db_path, None, OutputOptions::default()).unwrap();
        assert_eq!(changed.changed_count, 1);

        std::fs::remove_file(&file).unwrap();
        let missing =
            scan_to_db(&conn, dir.path(), &db_path, None, OutputOptions::default()).unwrap();
        assert_eq!(missing.missing_count, 1);
        assert_eq!(db::table_count(&conn, "path_observations").unwrap(), 1);
    }

    #[test]
    fn verify_accepts_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let db_path = dir.path().join("gremlin.db");
        let conn = db::open_or_create(&db_path).unwrap();
        db::init_schema(&conn).unwrap();
        hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            true,
            OutputOptions::default(),
        )
        .unwrap();

        std::fs::write(&file, b"changed").unwrap();
        let verify = verify_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            false,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(verify.changed, 1);
        let accepted = verify_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            true,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(accepted.accepted, 1);
        let clean = verify_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            false,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(clean.ok, 1);
        assert_eq!(clean.changed, 0);
    }

    #[test]
    fn hash_defaults_to_new_or_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hello").unwrap();
        let db_path = dir.path().join("gremlin.db");
        let conn = db::open_or_create(&db_path).unwrap();
        db::init_schema(&conn).unwrap();

        let first = hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            false,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(first.files_hashed, 1);
        let second = hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            false,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(second.files_hashed, 0);
        assert_eq!(second.skipped_unchanged, 1);

        std::fs::write(&file, b"changed").unwrap();
        let changed = hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            false,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(changed.files_hashed, 1);
    }

    #[test]
    fn chunk_hashes_are_stored_for_path_observations_only_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"abcdefghij").unwrap();
        let db_path = dir.path().join("gremlin.db");
        let conn = db::open_or_create(&db_path).unwrap();
        db::init_schema(&conn).unwrap();

        let hash = hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            true,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(hash.files_hashed, 1);
        let machine_id = db::ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = db::ensure_root(
            &conn,
            &machine_id,
            &lossy(&absolute_path(dir.path()).unwrap()),
        )
        .unwrap();
        let observation_id = db::path_observation_id(&conn, &root_id, "hello.txt")
            .unwrap()
            .unwrap();
        assert!(
            db::observation_chunk_hashes(&conn, &observation_id, 4, CHUNK_HASH_ALGORITHM)
                .unwrap()
                .is_empty()
        );

        let summary = chunk_hash_to_db(
            &conn,
            dir.path(),
            &db_path,
            None,
            4,
            OutputOptions::default(),
        )
        .unwrap();
        assert_eq!(summary.files_seen, 1);
        assert_eq!(summary.chunks_hashed, 3);
        assert_eq!(summary.bytes_hashed, 10);
        let chunks =
            db::observation_chunk_hashes(&conn, &observation_id, 4, CHUNK_HASH_ALGORITHM).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].path_observation_id, observation_id);
        assert_eq!(chunks[0].chunk_size_bytes, 4);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].offset_bytes, 0);
        assert_eq!(chunks[0].size_bytes, 4);
        assert_eq!(chunks[0].digest, format!("{:x}", md5::compute(b"abcd")));
        assert_eq!(chunks[2].offset_bytes, 8);
        assert_eq!(chunks[2].size_bytes, 2);
        assert_eq!(chunks[0].algorithm, "md5");
    }
}
