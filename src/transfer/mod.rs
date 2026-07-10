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
use crate::events::TransferChunkConfidence;
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
    message: Option<&'a str>,
    chunk_confidence: Option<TransferChunkConfidence>,
}

type TransferProgressCallback<'a> = dyn FnMut(u64, u64, f64, Option<&str>, Option<TransferChunkConfidence>) -> anyhow::Result<()>
    + 'a;

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

mod events;
mod io;
mod local;
mod paths;
mod plan;
mod run;
mod ssh;

pub use plan::{plan_all_files, plan_selected_files};
pub use run::run_transfer_plan;

use events::*;
use io::*;
use local::*;
use paths::*;
use run::*;
use ssh::*;

#[cfg(test)]
mod tests;
