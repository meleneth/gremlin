use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use rusqlite::{params, Connection, OptionalExtension};
use sea_query::{Alias, Expr, ExprTrait, Query, SqliteQueryBuilder, TableRef};
use serde_json::Value;

use crate::error::GremlinError;
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{absolute_path, local_hostname, local_machine_id, lossy, new_id, now_rfc3339};

#[derive(Debug, Clone)]
pub struct RootRow {
    pub id: String,
    pub machine_id: String,
    pub path: String,
    pub label: Option<String>,
    pub current_size_bytes: i64,
    pub latest_job_kind: Option<String>,
    pub latest_job_status: Option<String>,
    pub latest_job_phase: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MachineRow {
    pub id: String,
    pub label: String,
    pub platform: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileRow {
    pub relative_path: String,
    pub size_bytes: i64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
    pub sha256: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct CachedDirectoryEntry {
    pub kind: String,
    pub name: String,
    pub relative_path: String,
    pub file_count: i64,
    pub occurrence_count: Option<i64>,
    pub size_bytes: i64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
    pub sha256: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SelectedFileEntry {
    pub relative_path: String,
    pub parent_path: String,
    pub size_bytes: i64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
    pub sha256: Option<String>,
    pub status: String,
    pub occurrence_count: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct FileAppearanceRow {
    pub root_id: String,
    pub root_path: String,
    pub root_label: Option<String>,
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct LocalFileCandidate<'a> {
    pub key: &'a str,
    pub content_id: Option<&'a str>,
    pub basename: &'a str,
    pub size_bytes: u64,
    pub modified_at: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct RootSummary {
    pub file_count: i64,
    pub content_count: i64,
    pub hashed_file_count: i64,
    pub sha256_file_count: i64,
    pub crc32_file_count: i64,
    pub chunk_hashed_file_count: i64,
}

#[derive(Debug, Clone)]
pub struct RootDeleteSummary {
    pub root_id: String,
    pub path_observations: i64,
    pub chunk_hashes: i64,
    pub selection_sets: i64,
    pub selection_entries: i64,
    pub transfer_plans: i64,
    pub transfer_plan_entries: i64,
    pub checksum_collections: i64,
    pub checksum_entries: i64,
    pub jobs: i64,
    pub job_events: i64,
}

#[derive(Debug, Clone)]
pub struct SelectionSummary {
    pub set_id: String,
    pub marked_count: i64,
    pub marked_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySelectionChange {
    pub selected: bool,
    pub files_changed: usize,
    pub bytes_changed: u64,
}

#[derive(Debug, Clone)]
pub struct TransferPlanRow {
    pub id: String,
    pub job_id: Option<String>,
    pub source_root_id: String,
    pub source_path: String,
    pub dest_root_id: String,
    pub dest_path: String,
    pub selection_set_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub params_json: Option<String>,
    pub entry_count: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone)]
pub struct TransferPlanEntryInput<'a> {
    pub plan_id: &'a str,
    pub relative_path: &'a str,
    pub dest_relative_path: Option<&'a str>,
    pub size_bytes: u64,
    pub source_content_id: Option<&'a str>,
    pub dest_content_id: Option<&'a str>,
    pub action: &'a str,
    pub reason: &'a str,
    pub metadata_json: Value,
}

#[derive(Debug, Clone)]
pub struct TransferPlanEntryRow {
    pub relative_path: String,
    pub dest_relative_path: String,
    pub size_bytes: u64,
    pub source_content_id: Option<String>,
    pub dest_content_id: Option<String>,
    pub action: String,
    pub reason: String,
    pub metadata_json: String,
}

#[derive(Debug, Clone)]
pub struct TransferCopyChunkInput<'a> {
    pub plan_id: &'a str,
    pub relative_path: &'a str,
    pub dest_relative_path: &'a str,
    pub chunk_size_bytes: u64,
    pub chunk_index: u64,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub algorithm: &'a str,
    pub digest: &'a str,
    pub job_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct TransferCopyChunkRow {
    pub chunk_size_bytes: u64,
    pub chunk_index: u64,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub algorithm: String,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct TransferPlanActionSummary {
    pub action: String,
    pub files: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone)]
pub struct PathObservationRow {
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RootExportObservationRow {
    pub relative_path: String,
    pub basename: String,
    pub parent_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub status: String,
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    pub crc32: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CollisionRow {
    pub relative_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HashBaselineRow {
    pub relative_path: String,
    pub size_bytes: u64,
    pub blake3: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ContentObjectRow {
    pub size_bytes: u64,
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    pub crc32: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChecksumCollectionRow {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct RecentChecksumCollectionRow {
    pub id: String,
    pub name: String,
    pub source_kind: String,
    pub imported_at: Option<String>,
    pub root_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChecksumEntryRow {
    pub relative_path: String,
    pub size_bytes: u64,
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    pub crc32: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChecksumObservationRow {
    pub relative_path: String,
    pub size_bytes: u64,
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    pub crc32: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SfvExportEntry {
    pub relative_path: String,
    pub crc32: String,
}

#[derive(Debug, Clone)]
pub struct ObservationChunkHashInput<'a> {
    pub chunk_size_bytes: u64,
    pub chunk_index: u64,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub algorithm: &'a str,
    pub digest: &'a str,
    pub job_id: Option<&'a str>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ObservationChunkHashRow {
    pub path_observation_id: String,
    pub chunk_size_bytes: u64,
    pub chunk_index: u64,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub algorithm: String,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub job_id: String,
    pub sequence: i64,
    pub event_kind: String,
    pub created_at: String,
    pub payload_json: String,
}

#[derive(Debug, Clone)]
pub struct JobEventRow {
    pub job_id: String,
    pub job_kind: String,
    pub root_id: Option<String>,
    pub status: String,
    pub phase: Option<String>,
    pub current_path: Option<String>,
    pub files_seen: i64,
    pub files_done: i64,
    pub files_skipped: i64,
    pub errors: i64,
    pub cancel_requested: bool,
    pub sequence: i64,
    pub event_kind: String,
    pub payload_json: String,
    pub params_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub machine_id: Option<String>,
    pub root_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub params_json: Option<String>,
    pub phase: Option<String>,
    pub current_path: Option<String>,
    pub files_total: i64,
    pub files_seen: i64,
    pub files_done: i64,
    pub files_skipped: i64,
    pub errors: i64,
    pub cancel_requested: bool,
}

#[derive(Debug, Clone)]
pub struct TargetStatus {
    pub root: RootRow,
    pub file_count: i64,
    pub total_bytes: i64,
    pub content_count: i64,
    pub hashed_file_count: i64,
    pub sha256_file_count: i64,
    pub crc32_file_count: i64,
    pub chunk_hashed_file_count: i64,
    pub latest_job: Option<JobRow>,
    pub latest_event_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PathObservationInput<'a> {
    pub machine_id: &'a str,
    pub root_id: &'a str,
    pub relative_path: &'a str,
    pub basename: &'a str,
    pub parent_path: &'a str,
    pub size_bytes: u64,
    pub modified_at: Option<&'a str>,
    pub content_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct ChecksumEntryInput<'a> {
    pub collection_id: &'a str,
    pub relative_path: &'a str,
    pub basename: &'a str,
    pub size_bytes: u64,
    pub modified_at: Option<&'a str>,
    pub blake3: Option<&'a str>,
    pub sha256: Option<&'a str>,
    pub crc32: Option<&'a str>,
    pub metadata_json: Value,
}

#[derive(Debug, Clone)]
pub struct JobProgressInput<'a> {
    pub phase: &'a str,
    pub current_path: Option<&'a str>,
    pub files_total: Option<u64>,
    pub files_seen: u64,
    pub files_done: u64,
    pub files_skipped: u64,
    pub errors: u64,
}

pub fn open_or_create(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    Ok(conn)
}

pub fn open_existing(path: &Path) -> anyhow::Result<Connection> {
    if !path.exists() {
        return Err(GremlinError::MissingDatabase(path.display().to_string()).into());
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS machines (
            id TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            hostname TEXT,
            platform TEXT,
            created_at TEXT NOT NULL,
            last_seen_at TEXT
        );

        CREATE TABLE IF NOT EXISTS roots (
            id TEXT PRIMARY KEY,
            machine_id TEXT NOT NULL REFERENCES machines(id),
            path TEXT NOT NULL,
            label TEXT,
            created_at TEXT NOT NULL,
            current_size_bytes INTEGER NOT NULL DEFAULT 0,
            UNIQUE(machine_id, path)
        );

        CREATE TABLE IF NOT EXISTS content_objects (
            id TEXT PRIMARY KEY,
            size_bytes INTEGER NOT NULL,
            blake3 TEXT,
            sha256 TEXT,
            crc32 TEXT,
            first_seen_at TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_content_objects_identity
            ON content_objects(size_bytes, blake3, sha256)
            WHERE blake3 IS NOT NULL AND sha256 IS NOT NULL;

        CREATE TABLE IF NOT EXISTS path_observations (
            id TEXT PRIMARY KEY,
            machine_id TEXT NOT NULL REFERENCES machines(id),
            root_id TEXT NOT NULL REFERENCES roots(id),
            relative_path TEXT NOT NULL,
            basename TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            modified_at TEXT,
            content_id TEXT REFERENCES content_objects(id),
            last_seen_at TEXT NOT NULL,
            status TEXT NOT NULL,
            UNIQUE(machine_id, root_id, relative_path)
        );

        CREATE TABLE IF NOT EXISTS path_observation_chunk_hashes (
            id TEXT PRIMARY KEY,
            path_observation_id TEXT NOT NULL REFERENCES path_observations(id),
            chunk_size_bytes INTEGER NOT NULL,
            chunk_index INTEGER NOT NULL,
            offset_bytes INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            algorithm TEXT NOT NULL,
            digest TEXT NOT NULL,
            job_id TEXT,
            created_at TEXT NOT NULL,
            UNIQUE(path_observation_id, chunk_size_bytes, chunk_index, algorithm)
        );

        CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            machine_id TEXT,
            root_id TEXT,
            created_at TEXT NOT NULL,
            started_at TEXT,
            completed_at TEXT,
            phase TEXT,
            current_path TEXT,
            files_total INTEGER NOT NULL DEFAULT 0,
            files_seen INTEGER NOT NULL DEFAULT 0,
            files_done INTEGER NOT NULL DEFAULT 0,
            files_skipped INTEGER NOT NULL DEFAULT 0,
            errors INTEGER NOT NULL DEFAULT 0,
            cancel_requested INTEGER NOT NULL DEFAULT 0,
            params_json TEXT
        );

        CREATE TABLE IF NOT EXISTS job_events (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            event_kind TEXT NOT NULL,
            created_at TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            UNIQUE(job_id, sequence)
        );

        CREATE INDEX IF NOT EXISTS idx_job_events_recent
            ON job_events(created_at DESC, sequence DESC);

        CREATE INDEX IF NOT EXISTS idx_job_events_job_recent
            ON job_events(job_id, created_at DESC, sequence DESC);

        CREATE INDEX IF NOT EXISTS idx_jobs_root_recent
            ON jobs(root_id, created_at DESC);

        CREATE TABLE IF NOT EXISTS checksum_collections (
            id TEXT PRIMARY KEY,
            machine_id TEXT,
            root_id TEXT,
            name TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            generated_at TEXT,
            imported_at TEXT,
            job_id TEXT
        );

        CREATE TABLE IF NOT EXISTS checksum_entries (
            id TEXT PRIMARY KEY,
            collection_id TEXT NOT NULL REFERENCES checksum_collections(id),
            relative_path TEXT NOT NULL,
            basename TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            modified_at TEXT,
            blake3 TEXT,
            sha256 TEXT,
            crc32 TEXT,
            metadata_json TEXT
        );

        CREATE TABLE IF NOT EXISTS selection_sets (
            id TEXT PRIMARY KEY,
            root_id TEXT NOT NULL REFERENCES roots(id),
            name TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(root_id, name)
        );

        CREATE TABLE IF NOT EXISTS selection_entries (
            id TEXT PRIMARY KEY,
            selection_set_id TEXT NOT NULL REFERENCES selection_sets(id),
            root_id TEXT NOT NULL REFERENCES roots(id),
            relative_path TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(selection_set_id, relative_path)
        );

        CREATE TABLE IF NOT EXISTS transfer_plans (
            id TEXT PRIMARY KEY,
            job_id TEXT,
            source_root_id TEXT NOT NULL REFERENCES roots(id),
            dest_root_id TEXT NOT NULL REFERENCES roots(id),
            selection_set_id TEXT REFERENCES selection_sets(id),
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            params_json TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_transfer_plans_status_created
            ON transfer_plans(status, created_at);

        CREATE INDEX IF NOT EXISTS idx_transfer_plans_roots
            ON transfer_plans(source_root_id, dest_root_id);

        CREATE TABLE IF NOT EXISTS transfer_plan_entries (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL REFERENCES transfer_plans(id),
            relative_path TEXT NOT NULL,
            size_bytes INTEGER NOT NULL,
            source_content_id TEXT,
            dest_content_id TEXT,
            action TEXT NOT NULL,
            reason TEXT NOT NULL,
            metadata_json TEXT,
            UNIQUE(plan_id, relative_path)
        );

        CREATE TABLE IF NOT EXISTS transfer_copy_chunks (
            id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL REFERENCES transfer_plans(id),
            relative_path TEXT NOT NULL,
            dest_relative_path TEXT NOT NULL,
            chunk_size_bytes INTEGER NOT NULL,
            chunk_index INTEGER NOT NULL,
            offset_bytes INTEGER NOT NULL,
            size_bytes INTEGER NOT NULL,
            algorithm TEXT NOT NULL,
            digest TEXT NOT NULL,
            job_id TEXT NOT NULL,
            verified_at TEXT NOT NULL,
            UNIQUE(plan_id, relative_path, dest_relative_path, chunk_size_bytes, chunk_index, algorithm)
        );
        "#,
    )?;
    ensure_column(
        conn,
        "content_objects",
        "crc32",
        "ALTER TABLE content_objects ADD COLUMN crc32 TEXT",
    )?;
    ensure_column(
        conn,
        "checksum_entries",
        "crc32",
        "ALTER TABLE checksum_entries ADD COLUMN crc32 TEXT",
    )?;
    ensure_column(
        conn,
        "roots",
        "current_size_bytes",
        "ALTER TABLE roots ADD COLUMN current_size_bytes INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "phase",
        "ALTER TABLE jobs ADD COLUMN phase TEXT",
    )?;
    ensure_column(
        conn,
        "jobs",
        "current_path",
        "ALTER TABLE jobs ADD COLUMN current_path TEXT",
    )?;
    ensure_column(
        conn,
        "jobs",
        "files_total",
        "ALTER TABLE jobs ADD COLUMN files_total INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "files_seen",
        "ALTER TABLE jobs ADD COLUMN files_seen INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "files_done",
        "ALTER TABLE jobs ADD COLUMN files_done INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "files_skipped",
        "ALTER TABLE jobs ADD COLUMN files_skipped INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "errors",
        "ALTER TABLE jobs ADD COLUMN errors INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "jobs",
        "cancel_requested",
        "ALTER TABLE jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "transfer_plans",
        "job_id",
        "ALTER TABLE transfer_plans ADD COLUMN job_id TEXT",
    )?;
    ensure_column(
        conn,
        "transfer_plan_entries",
        "dest_relative_path",
        "ALTER TABLE transfer_plan_entries ADD COLUMN dest_relative_path TEXT",
    )?;
    dedupe_path_observations(conn)?;
    conn.execute(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_path_observations_root_relative_path
            ON path_observations(root_id, relative_path)
        "#,
        [],
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute(alter_sql, [])?;
    Ok(())
}

fn dedupe_path_observations(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TEMP TABLE IF NOT EXISTS path_observation_dedupe_keep (
            id TEXT PRIMARY KEY
        );
        DELETE FROM path_observation_dedupe_keep;
        INSERT INTO path_observation_dedupe_keep (id)
        SELECT p.id
        FROM path_observations p
        WHERE NOT EXISTS (
            SELECT 1
            FROM path_observations q
            WHERE q.root_id = p.root_id
              AND q.relative_path = p.relative_path
              AND (
                CASE WHEN q.status = 'present' THEN 1 ELSE 0 END >
                    CASE WHEN p.status = 'present' THEN 1 ELSE 0 END
                OR (
                    CASE WHEN q.status = 'present' THEN 1 ELSE 0 END =
                        CASE WHEN p.status = 'present' THEN 1 ELSE 0 END
                    AND CASE WHEN q.content_id IS NOT NULL THEN 1 ELSE 0 END >
                        CASE WHEN p.content_id IS NOT NULL THEN 1 ELSE 0 END
                )
                OR (
                    CASE WHEN q.status = 'present' THEN 1 ELSE 0 END =
                        CASE WHEN p.status = 'present' THEN 1 ELSE 0 END
                    AND CASE WHEN q.content_id IS NOT NULL THEN 1 ELSE 0 END =
                        CASE WHEN p.content_id IS NOT NULL THEN 1 ELSE 0 END
                    AND COALESCE(q.last_seen_at, '') > COALESCE(p.last_seen_at, '')
                )
                OR (
                    CASE WHEN q.status = 'present' THEN 1 ELSE 0 END =
                        CASE WHEN p.status = 'present' THEN 1 ELSE 0 END
                    AND CASE WHEN q.content_id IS NOT NULL THEN 1 ELSE 0 END =
                        CASE WHEN p.content_id IS NOT NULL THEN 1 ELSE 0 END
                    AND COALESCE(q.last_seen_at, '') = COALESCE(p.last_seen_at, '')
                    AND q.id > p.id
                )
              )
        );
        DELETE FROM path_observation_chunk_hashes
        WHERE path_observation_id NOT IN (
            SELECT id FROM path_observation_dedupe_keep
        );
        DELETE FROM path_observations
        WHERE id NOT IN (
            SELECT id FROM path_observation_dedupe_keep
        );
        DROP TABLE path_observation_dedupe_keep;
        "#,
    )?;
    refresh_all_root_current_sizes(conn)?;
    Ok(())
}

fn refresh_all_root_current_sizes(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        UPDATE roots
        SET current_size_bytes = (
            SELECT COALESCE(SUM(size_bytes), 0)
            FROM path_observations
            WHERE path_observations.root_id = roots.id
              AND path_observations.status = 'present'
        )
        "#,
        [],
    )?;
    Ok(())
}

pub fn ensure_local_machine_with_label(
    conn: &Connection,
    machine_label: Option<&str>,
) -> rusqlite::Result<String> {
    let id = local_machine_id();
    let now = now_rfc3339();
    let label = machine_label
        .map(ToOwned::to_owned)
        .or_else(local_hostname)
        .unwrap_or_else(|| "local".to_string());
    conn.execute(
        r#"
        INSERT INTO machines (id, label, hostname, platform, created_at, last_seen_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?5)
        ON CONFLICT(id) DO UPDATE SET last_seen_at = excluded.last_seen_at
        "#,
        params![id, label, local_hostname(), std::env::consts::OS, now],
    )?;
    Ok(id)
}

pub fn ensure_root(conn: &Connection, machine_id: &str, path: &str) -> rusqlite::Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM roots WHERE machine_id = ?1 AND path = ?2",
            params![machine_id, path],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }

    let id = new_id("root");
    conn.execute(
        "INSERT INTO roots (id, machine_id, path, label, created_at, current_size_bytes) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![id, machine_id, path, Option::<&str>::None, now_rfc3339()],
    )?;
    Ok(id)
}

pub fn set_root_label(conn: &Connection, root_id: &str, label: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE roots SET label = ?2 WHERE id = ?1",
        params![root_id, label],
    )?;
    Ok(())
}

pub fn delete_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<Option<RootDeleteSummary>> {
    if root_by_id(conn, root_id)?.is_none() {
        return Ok(None);
    }
    let summary = root_delete_summary(conn, root_id)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        r#"
        DELETE FROM job_events
        WHERE job_id IN (
            SELECT id FROM jobs WHERE root_id = ?1
            UNION
            SELECT job_id FROM transfer_plans
            WHERE (source_root_id = ?1 OR dest_root_id = ?1) AND job_id IS NOT NULL
            UNION
            SELECT job_id FROM checksum_collections
            WHERE root_id = ?1 AND job_id IS NOT NULL
        )
        "#,
        params![root_id],
    )?;
    tx.execute(
        r#"
        DELETE FROM jobs
        WHERE root_id = ?1
           OR id IN (
                SELECT job_id FROM transfer_plans
                WHERE (source_root_id = ?1 OR dest_root_id = ?1) AND job_id IS NOT NULL
                UNION
                SELECT job_id FROM checksum_collections
                WHERE root_id = ?1 AND job_id IS NOT NULL
           )
        "#,
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM transfer_plan_entries WHERE plan_id IN (SELECT id FROM transfer_plans WHERE source_root_id = ?1 OR dest_root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM transfer_copy_chunks WHERE plan_id IN (SELECT id FROM transfer_plans WHERE source_root_id = ?1 OR dest_root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM transfer_plans WHERE source_root_id = ?1 OR dest_root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM checksum_entries WHERE collection_id IN (SELECT id FROM checksum_collections WHERE root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM checksum_collections WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM selection_entries WHERE root_id = ?1 OR selection_set_id IN (SELECT id FROM selection_sets WHERE root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM selection_sets WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM selection_sets WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM path_observation_chunk_hashes WHERE path_observation_id IN (SELECT id FROM path_observations WHERE root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM path_observations WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute("DELETE FROM roots WHERE id = ?1", params![root_id])?;
    tx.commit()?;
    Ok(Some(summary))
}

pub fn root_delete_summary(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<RootDeleteSummary> {
    let path_observations = count_for_root(conn, "path_observations", root_id)?;
    let chunk_hashes = conn.query_row(
        "SELECT COUNT(*) FROM path_observation_chunk_hashes WHERE path_observation_id IN (SELECT id FROM path_observations WHERE root_id = ?1)",
        params![root_id],
        |row| row.get(0),
    )?;
    let selection_sets = count_for_root(conn, "selection_sets", root_id)?;
    let selection_entries = conn.query_row(
        "SELECT COUNT(*) FROM selection_entries WHERE root_id = ?1 OR selection_set_id IN (SELECT id FROM selection_sets WHERE root_id = ?1)",
        params![root_id],
        |row| row.get(0),
    )?;
    let transfer_plans = conn.query_row(
        "SELECT COUNT(*) FROM transfer_plans WHERE source_root_id = ?1 OR dest_root_id = ?1",
        params![root_id],
        |row| row.get(0),
    )?;
    let transfer_plan_entries = conn.query_row(
        "SELECT COUNT(*) FROM transfer_plan_entries WHERE plan_id IN (SELECT id FROM transfer_plans WHERE source_root_id = ?1 OR dest_root_id = ?1)",
        params![root_id],
        |row| row.get(0),
    )?;
    let checksum_collections = count_for_root(conn, "checksum_collections", root_id)?;
    let checksum_entries = conn.query_row(
        "SELECT COUNT(*) FROM checksum_entries WHERE collection_id IN (SELECT id FROM checksum_collections WHERE root_id = ?1)",
        params![root_id],
        |row| row.get(0),
    )?;
    let jobs = conn.query_row(
        r#"
        SELECT COUNT(*) FROM jobs
        WHERE root_id = ?1
           OR id IN (
                SELECT job_id FROM transfer_plans
                WHERE (source_root_id = ?1 OR dest_root_id = ?1) AND job_id IS NOT NULL
                UNION
                SELECT job_id FROM checksum_collections
                WHERE root_id = ?1 AND job_id IS NOT NULL
           )
        "#,
        params![root_id],
        |row| row.get(0),
    )?;
    let job_events = conn.query_row(
        r#"
        SELECT COUNT(*) FROM job_events
        WHERE job_id IN (
            SELECT id FROM jobs WHERE root_id = ?1
            UNION
            SELECT job_id FROM transfer_plans
            WHERE (source_root_id = ?1 OR dest_root_id = ?1) AND job_id IS NOT NULL
            UNION
            SELECT job_id FROM checksum_collections
            WHERE root_id = ?1 AND job_id IS NOT NULL
        )
        "#,
        params![root_id],
        |row| row.get(0),
    )?;
    Ok(RootDeleteSummary {
        root_id: root_id.to_string(),
        path_observations,
        chunk_hashes,
        selection_sets,
        selection_entries,
        transfer_plans,
        transfer_plan_entries,
        checksum_collections,
        checksum_entries,
        jobs,
        job_events,
    })
}

fn count_for_root(conn: &Connection, table: &str, root_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE root_id = ?1"),
        params![root_id],
        |row| row.get(0),
    )
}

pub fn refresh_root_current_size(conn: &Connection, root_id: &str) -> rusqlite::Result<i64> {
    let current_size: i64 = conn.query_row(
        r#"
        SELECT COALESCE(SUM(size_bytes), 0)
        FROM path_observations
        WHERE root_id = ?1 AND status = 'present'
        "#,
        params![root_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE roots SET current_size_bytes = ?2 WHERE id = ?1",
        params![root_id, current_size],
    )?;
    Ok(current_size)
}

pub fn ensure_machine_hint(
    conn: &Connection,
    label: &str,
    platform: Option<&str>,
) -> rusqlite::Result<String> {
    let digest = blake3::hash(format!("{label}:{}", platform.unwrap_or("unknown")).as_bytes());
    let id = format!("machine_{}", &digest.to_hex()[..16]);
    let now = now_rfc3339();
    conn.execute(
        r#"
        INSERT INTO machines (id, label, hostname, platform, created_at, last_seen_at)
        VALUES (?1, ?2, ?2, ?3, ?4, ?4)
        ON CONFLICT(id) DO UPDATE SET last_seen_at = excluded.last_seen_at
        "#,
        params![id, label, platform, now],
    )?;
    Ok(id)
}

pub fn ensure_machine_record(
    conn: &Connection,
    id: &str,
    label: &str,
    platform: Option<&str>,
) -> rusqlite::Result<String> {
    let now = now_rfc3339();
    conn.execute(
        r#"
        INSERT INTO machines (id, label, hostname, platform, created_at, last_seen_at)
        VALUES (?1, ?2, ?2, ?3, ?4, ?4)
        ON CONFLICT(id) DO UPDATE SET
            label = excluded.label,
            platform = excluded.platform,
            last_seen_at = excluded.last_seen_at
        "#,
        params![id, label, platform, now],
    )?;
    Ok(id.to_string())
}

pub fn machine_by_id(conn: &Connection, machine_id: &str) -> rusqlite::Result<Option<MachineRow>> {
    conn.query_row(
        r#"
        SELECT id, label, platform
        FROM machines
        WHERE id = ?1
        "#,
        params![machine_id],
        |row| {
            Ok(MachineRow {
                id: row.get(0)?,
                label: row.get(1)?,
                platform: row.get(2)?,
            })
        },
    )
    .optional()
}

pub fn create_job(
    conn: &Connection,
    kind: &str,
    machine_id: Option<&str>,
    root_id: Option<&str>,
    params_json: Value,
) -> rusqlite::Result<String> {
    let id = new_id("job");
    let now = now_rfc3339();
    conn.execute(
        r#"
        INSERT INTO jobs (id, kind, status, machine_id, root_id, created_at, phase, params_json)
        VALUES (?1, ?2, 'created', ?3, ?4, ?5, 'queued', ?6)
        "#,
        params![
            id,
            kind,
            machine_id,
            root_id,
            now,
            serde_json::to_string(&params_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(id)
}

pub fn queue_file_job(
    conn: &Connection,
    kind: &str,
    path: &Path,
    machine_label: Option<&str>,
) -> anyhow::Result<String> {
    let root_path = absolute_path(path)?;
    let machine_id = ensure_local_machine_with_label(conn, machine_label)?;
    let root_id = ensure_root(conn, &machine_id, &lossy(&root_path))?;
    let job_id = create_job(
        conn,
        kind,
        Some(&machine_id),
        Some(&root_id),
        serde_json::json!({ "path": lossy(&root_path) }),
    )?;
    let event = EventEnvelope {
        event_kind: EventKind::JobCreated,
        job_id: Some(job_id.clone()),
        sequence: Some(1),
        created_at: now_rfc3339(),
        payload: EventPayload::Job {
            kind: kind.to_string(),
            path: Some(lossy(&root_path)),
            message: Some("queued".to_string()),
            files_seen: None,
            errors: None,
        },
    };
    persist_event(conn, &event)?;
    Ok(job_id)
}

pub fn start_job(conn: &Connection, job_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET status = 'running', phase = 'preparing', started_at = COALESCE(started_at, ?2) WHERE id = ?1",
        params![job_id, now_rfc3339()],
    )?;
    Ok(())
}

pub fn complete_job(conn: &Connection, job_id: &str, status: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET status = ?2, phase = 'finalizing', current_path = NULL, completed_at = ?3 WHERE id = ?1",
        params![job_id, status, now_rfc3339()],
    )?;
    Ok(())
}

pub fn update_job_progress(
    conn: &Connection,
    job_id: &str,
    progress: JobProgressInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        UPDATE jobs
        SET phase = ?2,
            current_path = ?3,
            files_total = COALESCE(?4, files_total),
            files_seen = ?5,
            files_done = ?6,
            files_skipped = ?7,
            errors = ?8
        WHERE id = ?1
        "#,
        params![
            job_id,
            progress.phase,
            progress.current_path,
            progress.files_total.map(|value| value as i64),
            progress.files_seen as i64,
            progress.files_done as i64,
            progress.files_skipped as i64,
            progress.errors as i64,
        ],
    )?;
    Ok(())
}

pub fn request_job_cancel(conn: &Connection, job_id: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        r#"
        UPDATE jobs
        SET cancel_requested = 1
        WHERE id = ?1 AND status IN ('created', 'running')
        "#,
        params![job_id],
    )?;
    Ok(changed > 0)
}

pub fn active_jobs(conn: &Connection) -> rusqlite::Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, kind, status, machine_id, root_id, created_at, started_at, completed_at,
               params_json, phase, current_path, files_total, files_seen, files_done,
               files_skipped, errors, cancel_requested
        FROM jobs
        WHERE status IN ('created', 'running')
        ORDER BY started_at DESC, created_at DESC
        "#,
    )?;
    let rows = stmt.query_map([], job_from_row)?;
    rows.collect()
}

pub fn job_cancel_requested(conn: &Connection, job_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT cancel_requested FROM jobs WHERE id = ?1",
        params![job_id],
        |row| {
            let value: i64 = row.get(0)?;
            Ok(value != 0)
        },
    )
    .optional()
    .map(|value| value.unwrap_or(false))
}

pub fn next_sequence(conn: &Connection, job_id: &str) -> rusqlite::Result<i64> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT MAX(sequence) FROM job_events WHERE job_id = ?1",
            params![job_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(current.unwrap_or(0) + 1)
}

pub fn persist_event(conn: &Connection, envelope: &EventEnvelope) -> rusqlite::Result<()> {
    let Some(job_id) = envelope.job_id.as_deref() else {
        return Ok(());
    };
    let sequence = match envelope.sequence {
        Some(sequence) => sequence,
        None => next_sequence(conn, job_id)?,
    };
    conn.execute(
        r#"
        INSERT OR IGNORE INTO job_events
            (id, job_id, sequence, event_kind, created_at, payload_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            new_id("event"),
            job_id,
            sequence,
            envelope.event_kind.as_str(),
            envelope.created_at,
            serde_json::to_string(&envelope.payload).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(())
}

pub fn insert_path_observation(
    conn: &Connection,
    input: PathObservationInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        INSERT INTO path_observations
            (id, machine_id, root_id, relative_path, basename, parent_path, size_bytes,
             modified_at, content_id, last_seen_at, status)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'present')
        ON CONFLICT(root_id, relative_path) DO UPDATE SET
            machine_id = excluded.machine_id,
            basename = excluded.basename,
            parent_path = excluded.parent_path,
            size_bytes = excluded.size_bytes,
            modified_at = excluded.modified_at,
            content_id = COALESCE(excluded.content_id, path_observations.content_id),
            last_seen_at = excluded.last_seen_at,
            status = 'present'
        "#,
        params![
            new_id("path"),
            input.machine_id,
            input.root_id,
            input.relative_path,
            input.basename,
            input.parent_path,
            input.size_bytes as i64,
            input.modified_at,
            input.content_id,
            now_rfc3339()
        ],
    )?;
    refresh_root_current_size(conn, input.root_id)?;
    Ok(())
}

pub fn path_observation_id(
    conn: &Connection,
    root_id: &str,
    relative_path: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM path_observations WHERE root_id = ?1 AND relative_path = ?2",
        params![root_id, relative_path],
        |row| row.get(0),
    )
    .optional()
}

pub fn ensure_content_object(
    conn: &Connection,
    size_bytes: u64,
    blake3: &str,
    sha256: &str,
) -> rusqlite::Result<String> {
    ensure_content_object_with_hashes(conn, size_bytes, Some(blake3), Some(sha256), None)
}

pub fn ensure_content_object_crc(
    conn: &Connection,
    size_bytes: u64,
    blake3: &str,
    sha256: &str,
    crc32: &str,
) -> rusqlite::Result<String> {
    ensure_content_object_with_hashes(conn, size_bytes, Some(blake3), Some(sha256), Some(crc32))
}

pub fn ensure_content_object_blake3_sha256(
    conn: &Connection,
    size_bytes: u64,
    blake3: &str,
    sha256: &str,
) -> rusqlite::Result<String> {
    ensure_content_object_with_hashes(conn, size_bytes, Some(blake3), Some(sha256), None)
}

pub fn ensure_content_object_sha256(
    conn: &Connection,
    size_bytes: u64,
    sha256: &str,
) -> rusqlite::Result<String> {
    ensure_content_object_with_hashes(conn, size_bytes, None, Some(sha256), None)
}

pub fn ensure_content_object_sha256_crc(
    conn: &Connection,
    size_bytes: u64,
    sha256: &str,
    crc32: &str,
) -> rusqlite::Result<String> {
    ensure_content_object_with_hashes(conn, size_bytes, None, Some(sha256), Some(crc32))
}

fn ensure_content_object_with_hashes(
    conn: &Connection,
    size_bytes: u64,
    blake3: Option<&str>,
    sha256: Option<&str>,
    crc32: Option<&str>,
) -> rusqlite::Result<String> {
    let lookup_params = params![size_bytes as i64, blake3, sha256];
    if let Some(id) = conn
        .query_row(
            r#"
            SELECT id
            FROM content_objects
            WHERE size_bytes = ?1
              AND ((blake3 IS NULL AND ?2 IS NULL) OR blake3 = ?2)
              AND ((sha256 IS NULL AND ?3 IS NULL) OR sha256 = ?3)
            "#,
            lookup_params,
            |row| row.get(0),
        )
        .optional()?
    {
        if let Some(crc32) = crc32 {
            conn.execute(
                "UPDATE content_objects SET crc32 = COALESCE(crc32, ?1) WHERE id = ?2",
                params![crc32, id],
            )?;
        }
        return Ok(id);
    }

    let id = new_id("content");
    conn.execute(
        "INSERT INTO content_objects (id, size_bytes, blake3, sha256, crc32, first_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, size_bytes as i64, blake3, sha256, crc32, now_rfc3339()],
    )?;
    Ok(id)
}

pub fn content_object_by_id(
    conn: &Connection,
    content_id: &str,
) -> rusqlite::Result<Option<ContentObjectRow>> {
    conn.query_row(
        r#"
        SELECT size_bytes, blake3, sha256, crc32
        FROM content_objects
        WHERE id = ?1
        "#,
        params![content_id],
        |row| {
            let size: i64 = row.get(0)?;
            Ok(ContentObjectRow {
                size_bytes: size as u64,
                blake3: row.get(1)?,
                sha256: row.get(2)?,
                crc32: row.get(3)?,
            })
        },
    )
    .optional()
}

pub fn replace_observation_chunk_hashes(
    conn: &Connection,
    path_observation_id: &str,
    chunk_size_bytes: u64,
    algorithm: &str,
    chunks: &[ObservationChunkHashInput<'_>],
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        DELETE FROM path_observation_chunk_hashes
        WHERE path_observation_id = ?1
          AND chunk_size_bytes = ?2
          AND algorithm = ?3
        "#,
        params![path_observation_id, chunk_size_bytes as i64, algorithm],
    )?;
    for chunk in chunks {
        conn.execute(
            r#"
            INSERT INTO path_observation_chunk_hashes
                (id, path_observation_id, chunk_size_bytes, chunk_index,
                 offset_bytes, size_bytes, algorithm, digest, job_id, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                new_id("chunk"),
                path_observation_id,
                chunk.chunk_size_bytes as i64,
                chunk.chunk_index as i64,
                chunk.offset_bytes as i64,
                chunk.size_bytes as i64,
                chunk.algorithm,
                chunk.digest,
                chunk.job_id,
                now_rfc3339()
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
pub fn observation_chunk_hashes(
    conn: &Connection,
    path_observation_id: &str,
    chunk_size_bytes: u64,
    algorithm: &str,
) -> rusqlite::Result<Vec<ObservationChunkHashRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT path_observation_id, chunk_size_bytes, chunk_index, offset_bytes,
               size_bytes, algorithm, digest
        FROM path_observation_chunk_hashes
        WHERE path_observation_id = ?1
          AND chunk_size_bytes = ?2
          AND algorithm = ?3
        ORDER BY chunk_index ASC
        "#,
    )?;
    let rows = stmt.query_map(
        params![path_observation_id, chunk_size_bytes as i64, algorithm],
        |row| {
            let chunk_size: i64 = row.get(1)?;
            let index: i64 = row.get(2)?;
            let offset: i64 = row.get(3)?;
            let size: i64 = row.get(4)?;
            Ok(ObservationChunkHashRow {
                path_observation_id: row.get(0)?,
                chunk_size_bytes: chunk_size as u64,
                chunk_index: index as u64,
                offset_bytes: offset as u64,
                size_bytes: size as u64,
                algorithm: row.get(5)?,
                digest: row.get(6)?,
            })
        },
    )?;
    rows.collect()
}

pub fn create_checksum_collection(
    conn: &Connection,
    name: &str,
    source_kind: &str,
    job_id: Option<&str>,
) -> rusqlite::Result<String> {
    let id = new_id("collection");
    conn.execute(
        r#"
        INSERT INTO checksum_collections
            (id, name, source_kind, imported_at, job_id)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![id, name, source_kind, now_rfc3339(), job_id],
    )?;
    Ok(id)
}

pub fn attach_checksum_collection_target(
    conn: &Connection,
    collection_id: &str,
    machine_id: &str,
    root_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        UPDATE checksum_collections
        SET machine_id = ?2, root_id = ?3
        WHERE id = ?1
        "#,
        params![collection_id, machine_id, root_id],
    )?;
    Ok(())
}

pub fn checksum_collection_by_id(
    conn: &Connection,
    collection_id: &str,
) -> rusqlite::Result<Option<ChecksumCollectionRow>> {
    conn.query_row(
        r#"
        SELECT id, name
        FROM checksum_collections
        WHERE id = ?1
        "#,
        params![collection_id],
        |row| {
            Ok(ChecksumCollectionRow {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        },
    )
    .optional()
}

pub fn latest_checksum_collection_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<Option<RecentChecksumCollectionRow>> {
    conn.query_row(
        r#"
        SELECT id, name, source_kind, imported_at, root_id
        FROM checksum_collections
        WHERE root_id = ?1 OR root_id IS NULL
        ORDER BY
            CASE WHEN root_id = ?1 THEN 0 ELSE 1 END,
            COALESCE(imported_at, generated_at, '') DESC
        LIMIT 1
        "#,
        params![root_id],
        |row| {
            Ok(RecentChecksumCollectionRow {
                id: row.get(0)?,
                name: row.get(1)?,
                source_kind: row.get(2)?,
                imported_at: row.get(3)?,
                root_id: row.get(4)?,
            })
        },
    )
    .optional()
}

pub fn checksum_entries_for_collection(
    conn: &Connection,
    collection_id: &str,
) -> rusqlite::Result<Vec<ChecksumEntryRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path, size_bytes, blake3, sha256, crc32
        FROM checksum_entries
        WHERE collection_id = ?1
        ORDER BY relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![collection_id], |row| {
        let size: i64 = row.get(1)?;
        Ok(ChecksumEntryRow {
            relative_path: row.get(0)?,
            size_bytes: size as u64,
            blake3: row.get(2)?,
            sha256: row.get(3)?,
            crc32: row.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn checksum_observations_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<BTreeMap<String, ChecksumObservationRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, p.size_bytes, c.blake3, c.sha256, c.crc32
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
          AND p.status = 'present'
        ORDER BY p.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        let size: i64 = row.get(1)?;
        Ok(ChecksumObservationRow {
            relative_path: row.get(0)?,
            size_bytes: size as u64,
            blake3: row.get(2)?,
            sha256: row.get(3)?,
            crc32: row.get(4)?,
        })
    })?;
    let mut observations = BTreeMap::new();
    for row in rows {
        let row = row?;
        observations.insert(row.relative_path.clone(), row);
    }
    Ok(observations)
}

pub fn sfv_entries_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<Vec<SfvExportEntry>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, c.crc32
        FROM path_observations p
        JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
          AND p.status = 'present'
          AND c.crc32 IS NOT NULL
        ORDER BY p.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        Ok(SfvExportEntry {
            relative_path: row.get(0)?,
            crc32: row.get(1)?,
        })
    })?;
    rows.collect()
}

pub fn insert_checksum_entry(
    conn: &Connection,
    input: ChecksumEntryInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        INSERT INTO checksum_entries
            (id, collection_id, relative_path, basename, size_bytes, modified_at, blake3, sha256, crc32, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        "#,
        params![
            new_id("entry"),
            input.collection_id,
            input.relative_path,
            input.basename,
            input.size_bytes as i64,
            input.modified_at,
            input.blake3,
            input.sha256,
            input.crc32,
            serde_json::to_string(&input.metadata_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(())
}

pub fn ensure_default_selection_set(conn: &Connection, root_id: &str) -> rusqlite::Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM selection_sets WHERE root_id = ?1 AND name = 'default'",
            params![root_id],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = new_id("selection");
    let now = now_rfc3339();
    conn.execute(
        r#"
        INSERT INTO selection_sets (id, root_id, name, created_at, updated_at)
        VALUES (?1, ?2, 'default', ?3, ?3)
        "#,
        params![id, root_id, now],
    )?;
    Ok(id)
}

pub fn toggle_selection_entry(
    conn: &Connection,
    root_id: &str,
    relative_path: &str,
) -> rusqlite::Result<bool> {
    let set_id = ensure_default_selection_set(conn, root_id)?;
    let existing: Option<String> = conn
        .query_row(
            r#"
            SELECT id
            FROM selection_entries
            WHERE selection_set_id = ?1 AND relative_path = ?2
            "#,
            params![set_id, relative_path],
            |row| row.get(0),
        )
        .optional()?;
    let now = now_rfc3339();
    if let Some(id) = existing {
        conn.execute("DELETE FROM selection_entries WHERE id = ?1", params![id])?;
        conn.execute(
            "UPDATE selection_sets SET updated_at = ?2 WHERE id = ?1",
            params![set_id, now],
        )?;
        Ok(false)
    } else {
        conn.execute(
            r#"
            INSERT INTO selection_entries
                (id, selection_set_id, root_id, relative_path, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![new_id("mark"), set_id, root_id, relative_path, now],
        )?;
        conn.execute(
            "UPDATE selection_sets SET updated_at = ?2 WHERE id = ?1",
            params![set_id, now],
        )?;
        Ok(true)
    }
}

pub fn toggle_selection_directory(
    conn: &Connection,
    root_id: &str,
    directory_path: &str,
) -> rusqlite::Result<DirectorySelectionChange> {
    let files = files_under_directory(conn, root_id, directory_path)?;
    if files.is_empty() {
        return Ok(DirectorySelectionChange {
            selected: false,
            files_changed: 0,
            bytes_changed: 0,
        });
    }
    let set_id = ensure_default_selection_set(conn, root_id)?;
    let selected = selected_paths_for_root(conn, root_id)?;
    let all_selected = files
        .iter()
        .all(|file| selected.contains(&file.relative_path));
    let now = now_rfc3339();
    let bytes_changed = files.iter().map(|file| file.size_bytes as u64).sum();
    if all_selected {
        for file in &files {
            conn.execute(
                "DELETE FROM selection_entries WHERE selection_set_id = ?1 AND relative_path = ?2",
                params![set_id, &file.relative_path],
            )?;
        }
    } else {
        for file in &files {
            if selected.contains(&file.relative_path) {
                continue;
            }
            conn.execute(
                r#"
                INSERT INTO selection_entries
                    (id, selection_set_id, root_id, relative_path, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![new_id("mark"), set_id, root_id, &file.relative_path, now],
            )?;
        }
    }
    conn.execute(
        "UPDATE selection_sets SET updated_at = ?2 WHERE id = ?1",
        params![set_id, now],
    )?;
    Ok(DirectorySelectionChange {
        selected: !all_selected,
        files_changed: files.len(),
        bytes_changed,
    })
}

fn files_under_directory(
    conn: &Connection,
    root_id: &str,
    directory_path: &str,
) -> rusqlite::Result<Vec<FileRow>> {
    let prefix = normalize_cached_parent(directory_path);
    let like = format!("{}/%", escape_like_pattern(&prefix));
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, p.size_bytes, p.modified_at, p.content_id, c.sha256, p.status
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1 AND p.relative_path LIKE ?2 ESCAPE '\'
        ORDER BY p.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![root_id, like], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            sha256: row.get(4)?,
            status: row.get(5)?,
        })
    })?;
    rows.collect()
}

fn escape_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn selected_paths_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<BTreeSet<String>> {
    let set_id = ensure_default_selection_set(conn, root_id)?;
    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path
        FROM selection_entries
        WHERE selection_set_id = ?1
        ORDER BY relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![set_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

pub fn selected_file_entries_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<Vec<SelectedFileEntry>> {
    let set_id = ensure_default_selection_set(conn, root_id)?;
    let mut stmt = conn.prepare(
        r#"
        SELECT
            e.relative_path,
            COALESCE(p.parent_path, '.'),
            COALESCE(p.size_bytes, 0),
            p.modified_at,
            p.content_id,
            c.sha256,
            COALESCE(p.status, 'selected'),
            CASE
                WHEN p.content_id IS NOT NULL THEN (
                    SELECT COUNT(*)
                    FROM path_observations x
                    WHERE x.content_id = p.content_id
                )
                WHEN p.relative_path IS NOT NULL THEN (
                    SELECT COUNT(*)
                    FROM path_observations x
                    WHERE x.basename = p.basename
                      AND x.size_bytes = p.size_bytes
                      AND (x.modified_at = p.modified_at OR (x.modified_at IS NULL AND p.modified_at IS NULL))
                )
                ELSE NULL
            END AS occurrence_count
        FROM selection_entries e
        LEFT JOIN path_observations p
            ON p.root_id = e.root_id AND p.relative_path = e.relative_path
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE e.selection_set_id = ?1
        ORDER BY COALESCE(p.parent_path, '.') ASC, e.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![set_id], |row| {
        Ok(SelectedFileEntry {
            relative_path: row.get(0)?,
            parent_path: row.get(1)?,
            size_bytes: row.get(2)?,
            modified_at: row.get(3)?,
            content_id: row.get(4)?,
            sha256: row.get(5)?,
            status: row.get(6)?,
            occurrence_count: row.get(7)?,
        })
    })?;
    rows.collect()
}

pub fn path_observation_for_root_path(
    conn: &Connection,
    root_id: &str,
    relative_path: &str,
) -> rusqlite::Result<Option<PathObservationRow>> {
    conn.query_row(
        r#"
        SELECT relative_path, size_bytes, modified_at, content_id
        FROM path_observations
        WHERE root_id = ?1 AND relative_path = ?2
        "#,
        params![root_id, relative_path],
        |row| {
            let size: i64 = row.get(1)?;
            Ok(PathObservationRow {
                relative_path: row.get(0)?,
                size_bytes: size as u64,
                modified_at: row.get(2)?,
                content_id: row.get(3)?,
            })
        },
    )
    .optional()
}

pub fn selection_summary_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<SelectionSummary> {
    let set_id = ensure_default_selection_set(conn, root_id)?;
    let (marked_count, marked_bytes): (i64, i64) = conn.query_row(
        r#"
        SELECT COUNT(e.relative_path), COALESCE(SUM(p.size_bytes), 0)
        FROM selection_entries e
        LEFT JOIN path_observations p
            ON p.root_id = e.root_id AND p.relative_path = e.relative_path
        WHERE e.selection_set_id = ?1
        "#,
        params![set_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(SelectionSummary {
        set_id,
        marked_count,
        marked_bytes,
    })
}

pub fn create_transfer_plan(
    conn: &Connection,
    job_id: Option<&str>,
    source_root_id: &str,
    dest_root_id: &str,
    selection_set_id: Option<&str>,
    params_json: Value,
) -> rusqlite::Result<String> {
    let id = new_id("plan");
    conn.execute(
        r#"
        INSERT INTO transfer_plans
            (id, job_id, source_root_id, dest_root_id, selection_set_id, status, created_at, params_json)
        VALUES (?1, ?2, ?3, ?4, ?5, 'planned', ?6, ?7)
        "#,
        params![
            id,
            job_id,
            source_root_id,
            dest_root_id,
            selection_set_id,
            now_rfc3339(),
            serde_json::to_string(&params_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(id)
}

pub fn update_transfer_plan_status(
    conn: &Connection,
    plan_id: &str,
    status: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE transfer_plans SET status = ?2 WHERE id = ?1",
        params![plan_id, status],
    )?;
    Ok(())
}

pub fn insert_transfer_plan_entry(
    conn: &Connection,
    input: TransferPlanEntryInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        INSERT INTO transfer_plan_entries
            (id, plan_id, relative_path, dest_relative_path, size_bytes, source_content_id, dest_content_id,
             action, reason, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(plan_id, relative_path) DO UPDATE SET
            dest_relative_path = excluded.dest_relative_path,
            size_bytes = excluded.size_bytes,
            source_content_id = excluded.source_content_id,
            dest_content_id = excluded.dest_content_id,
            action = excluded.action,
            reason = excluded.reason,
            metadata_json = excluded.metadata_json
        "#,
        params![
            new_id("planentry"),
            input.plan_id,
            input.relative_path,
            input.dest_relative_path.unwrap_or(input.relative_path),
            input.size_bytes as i64,
            input.source_content_id,
            input.dest_content_id,
            input.action,
            input.reason,
            serde_json::to_string(&input.metadata_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(())
}

pub fn transfer_plan_entries(
    conn: &Connection,
    plan_id: &str,
) -> rusqlite::Result<Vec<TransferPlanEntryRow>> {
    transfer_plan_entries_filtered(conn, plan_id, None)
}

pub fn transfer_plan_entries_filtered(
    conn: &Connection,
    plan_id: &str,
    action: Option<&str>,
) -> rusqlite::Result<Vec<TransferPlanEntryRow>> {
    if let Some(action) = action {
        let mut stmt = conn.prepare(
            r#"
            SELECT relative_path, COALESCE(dest_relative_path, relative_path), size_bytes, source_content_id, dest_content_id,
                   action, reason, metadata_json
            FROM transfer_plan_entries
            WHERE plan_id = ?1 AND action = ?2
            ORDER BY relative_path ASC
            "#,
        )?;
        let rows = stmt.query_map(params![plan_id, action], transfer_plan_entry_from_row)?;
        return rows.collect();
    }

    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path, COALESCE(dest_relative_path, relative_path), size_bytes, source_content_id, dest_content_id,
               action, reason, metadata_json
        FROM transfer_plan_entries
        WHERE plan_id = ?1
        ORDER BY relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![plan_id], transfer_plan_entry_from_row)?;
    rows.collect()
}

pub fn upsert_transfer_copy_chunk(
    conn: &Connection,
    input: TransferCopyChunkInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        INSERT INTO transfer_copy_chunks
            (id, plan_id, relative_path, dest_relative_path, chunk_size_bytes, chunk_index,
             offset_bytes, size_bytes, algorithm, digest, job_id, verified_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(plan_id, relative_path, dest_relative_path, chunk_size_bytes, chunk_index, algorithm)
        DO UPDATE SET
            offset_bytes = excluded.offset_bytes,
            size_bytes = excluded.size_bytes,
            digest = excluded.digest,
            job_id = excluded.job_id,
            verified_at = excluded.verified_at
        "#,
        params![
            new_id("copychunk"),
            input.plan_id,
            input.relative_path,
            input.dest_relative_path,
            input.chunk_size_bytes as i64,
            input.chunk_index as i64,
            input.offset_bytes as i64,
            input.size_bytes as i64,
            input.algorithm,
            input.digest,
            input.job_id,
            now_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn transfer_copy_chunk(
    conn: &Connection,
    plan_id: &str,
    relative_path: &str,
    dest_relative_path: &str,
    chunk_size_bytes: u64,
    chunk_index: u64,
    algorithm: &str,
) -> rusqlite::Result<Option<TransferCopyChunkRow>> {
    conn.query_row(
        r#"
        SELECT chunk_size_bytes, chunk_index, offset_bytes, size_bytes, algorithm, digest
        FROM transfer_copy_chunks
        WHERE plan_id = ?1
          AND relative_path = ?2
          AND dest_relative_path = ?3
          AND chunk_size_bytes = ?4
          AND chunk_index = ?5
          AND algorithm = ?6
        "#,
        params![
            plan_id,
            relative_path,
            dest_relative_path,
            chunk_size_bytes as i64,
            chunk_index as i64,
            algorithm,
        ],
        |row| {
            let chunk_size: i64 = row.get(0)?;
            let index: i64 = row.get(1)?;
            let offset: i64 = row.get(2)?;
            let size: i64 = row.get(3)?;
            Ok(TransferCopyChunkRow {
                chunk_size_bytes: chunk_size as u64,
                chunk_index: index as u64,
                offset_bytes: offset as u64,
                size_bytes: size as u64,
                algorithm: row.get(4)?,
                digest: row.get(5)?,
            })
        },
    )
    .optional()
}

pub fn transfer_copy_chunk_count_for_entry(
    conn: &Connection,
    plan_id: &str,
    relative_path: &str,
    dest_relative_path: &str,
) -> rusqlite::Result<i64> {
    conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM transfer_copy_chunks
        WHERE plan_id = ?1
          AND relative_path = ?2
          AND dest_relative_path = ?3
        "#,
        params![plan_id, relative_path, dest_relative_path],
        |row| row.get(0),
    )
}

pub fn decide_review_transfer_plan_entry(
    conn: &Connection,
    plan_id: &str,
    relative_path: &str,
    action: &str,
    reason: &str,
    metadata_json: serde_json::Value,
) -> rusqlite::Result<bool> {
    let updated = conn.execute(
        r#"
        UPDATE transfer_plan_entries
        SET action = ?3,
            reason = ?4,
            metadata_json = ?5
        WHERE plan_id = ?1
          AND relative_path = ?2
          AND action = 'review'
        "#,
        params![
            plan_id,
            relative_path,
            action,
            reason,
            serde_json::to_string(&metadata_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(updated > 0)
}

pub fn retarget_review_transfer_plan_entry(
    conn: &Connection,
    plan_id: &str,
    relative_path: &str,
    dest_relative_path: &str,
) -> rusqlite::Result<bool> {
    if !is_safe_relative_transfer_path(dest_relative_path) {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "unsafe destination relative path: {dest_relative_path}"
        )));
    }
    let duplicate_count: i64 = conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM transfer_plan_entries
        WHERE plan_id = ?1
          AND relative_path != ?2
          AND COALESCE(dest_relative_path, relative_path) = ?3
          AND action IN ('copy', 'review')
        "#,
        params![plan_id, relative_path, dest_relative_path],
        |row| row.get(0),
    )?;
    if duplicate_count > 0 {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "destination path is already used in this transfer plan: {dest_relative_path}"
        )));
    }
    let updated = conn.execute(
        r#"
        UPDATE transfer_plan_entries
        SET dest_relative_path = ?3,
            action = 'copy',
            reason = 'review retargeted for copy',
            metadata_json = ?4
        WHERE plan_id = ?1
          AND relative_path = ?2
          AND action = 'review'
        "#,
        params![
            plan_id,
            relative_path,
            dest_relative_path,
            serde_json::json!({
                "decision": "retarget",
                "dest_relative_path": dest_relative_path,
                "decided_at": now_rfc3339(),
            })
            .to_string()
        ],
    )?;
    Ok(updated > 0)
}

fn is_safe_relative_transfer_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    !relative_path.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn transfer_plan_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransferPlanEntryRow> {
    let size: i64 = row.get(2)?;
    Ok(TransferPlanEntryRow {
        relative_path: row.get(0)?,
        dest_relative_path: row.get(1)?,
        size_bytes: size as u64,
        source_content_id: row.get(3)?,
        dest_content_id: row.get(4)?,
        action: row.get(5)?,
        reason: row.get(6)?,
        metadata_json: row.get(7)?,
    })
}

pub fn transfer_plan_action_summary(
    conn: &Connection,
    plan_id: &str,
) -> rusqlite::Result<Vec<TransferPlanActionSummary>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT action, COUNT(*), COALESCE(SUM(size_bytes), 0)
        FROM transfer_plan_entries
        WHERE plan_id = ?1
        GROUP BY action
        ORDER BY action ASC
        "#,
    )?;
    let rows = stmt.query_map(params![plan_id], |row| {
        Ok(TransferPlanActionSummary {
            action: row.get(0)?,
            files: row.get(1)?,
            bytes: row.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn recent_transfer_plans(
    conn: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<TransferPlanRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            p.id,
            p.job_id,
            p.source_root_id,
            source.path,
            p.dest_root_id,
            dest.path,
            p.selection_set_id,
            p.status,
            p.created_at,
            p.params_json,
            COUNT(e.id),
            COALESCE(SUM(e.size_bytes), 0)
        FROM transfer_plans p
        JOIN roots source ON source.id = p.source_root_id
        JOIN roots dest ON dest.id = p.dest_root_id
        LEFT JOIN transfer_plan_entries e ON e.plan_id = p.id
        GROUP BY p.id
        ORDER BY p.created_at DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], transfer_plan_from_row)?;
    rows.collect()
}

pub fn queued_transfer_plans(
    conn: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<TransferPlanRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            p.id,
            p.job_id,
            p.source_root_id,
            source.path,
            p.dest_root_id,
            dest.path,
            p.selection_set_id,
            p.status,
            p.created_at,
            p.params_json,
            COUNT(e.id),
            COALESCE(SUM(e.size_bytes), 0)
        FROM transfer_plans p
        JOIN roots source ON source.id = p.source_root_id
        JOIN roots dest ON dest.id = p.dest_root_id
        LEFT JOIN transfer_plan_entries e ON e.plan_id = p.id
        WHERE p.status = 'queued'
        GROUP BY p.id
        ORDER BY p.created_at ASC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], transfer_plan_from_row)?;
    rows.collect()
}

pub fn transfer_plan_by_id(
    conn: &Connection,
    plan_id: &str,
) -> rusqlite::Result<Option<TransferPlanRow>> {
    conn.query_row(
        r#"
        SELECT
            p.id,
            p.job_id,
            p.source_root_id,
            source.path,
            p.dest_root_id,
            dest.path,
            p.selection_set_id,
            p.status,
            p.created_at,
            p.params_json,
            COUNT(e.id),
            COALESCE(SUM(e.size_bytes), 0)
        FROM transfer_plans p
        JOIN roots source ON source.id = p.source_root_id
        JOIN roots dest ON dest.id = p.dest_root_id
        LEFT JOIN transfer_plan_entries e ON e.plan_id = p.id
        WHERE p.id = ?1
        GROUP BY p.id
        "#,
        params![plan_id],
        transfer_plan_from_row,
    )
    .optional()
}

fn transfer_plan_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransferPlanRow> {
    Ok(TransferPlanRow {
        id: row.get(0)?,
        job_id: row.get(1)?,
        source_root_id: row.get(2)?,
        source_path: row.get(3)?,
        dest_root_id: row.get(4)?,
        dest_path: row.get(5)?,
        selection_set_id: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        params_json: row.get(9)?,
        entry_count: row.get(10)?,
        total_bytes: row.get(11)?,
    })
}

pub fn ensure_import_job(conn: &Connection, source: &str) -> rusqlite::Result<String> {
    let job_id = create_job(
        conn,
        "import_events",
        None,
        None,
        serde_json::json!({ "source": source }),
    )?;
    start_job(conn, &job_id)?;
    Ok(job_id)
}

pub fn recent_events(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT job_id, sequence, event_kind, created_at, payload_json
        FROM job_events
        ORDER BY created_at DESC, sequence DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(EventRow {
            job_id: row.get(0)?,
            sequence: row.get(1)?,
            event_kind: row.get(2)?,
            created_at: row.get(3)?,
            payload_json: row.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn recent_files(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, p.size_bytes, p.modified_at, p.content_id, c.sha256, p.status
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        ORDER BY p.last_seen_at DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            sha256: row.get(4)?,
            status: row.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn recent_files_for_root(
    conn: &Connection,
    root_id: &str,
    limit: i64,
) -> rusqlite::Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, p.size_bytes, p.modified_at, p.content_id, c.sha256, p.status
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
        ORDER BY p.last_seen_at DESC
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![root_id, limit], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            sha256: row.get(4)?,
            status: row.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn present_files_for_root(conn: &Connection, root_id: &str) -> rusqlite::Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, p.size_bytes, p.modified_at, p.content_id, c.sha256, p.status
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1 AND p.status = 'present'
        ORDER BY p.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            sha256: row.get(4)?,
            status: row.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn cached_directory_entries(
    conn: &Connection,
    root_id: &str,
    parent: &str,
) -> rusqlite::Result<Vec<CachedDirectoryEntry>> {
    let parent = normalize_cached_parent(parent);
    let mut entries = cached_child_directories(conn, root_id, &parent)?;
    entries.extend(cached_child_files(conn, root_id, &parent)?);
    Ok(entries)
}

fn cached_child_directories(
    conn: &Connection,
    root_id: &str,
    parent: &str,
) -> rusqlite::Result<Vec<CachedDirectoryEntry>> {
    if parent == "." {
        let mut stmt = conn.prepare(
            r#"
            SELECT
                substr(relative_path, 1, instr(relative_path, '/') - 1) AS child_name,
                COUNT(*),
                COALESCE(SUM(size_bytes), 0)
            FROM path_observations
            WHERE root_id = ?1 AND instr(relative_path, '/') > 0
            GROUP BY child_name
            ORDER BY child_name ASC
            "#,
        )?;
        let rows = stmt.query_map(params![root_id], |row| {
            let name: String = row.get(0)?;
            Ok(CachedDirectoryEntry {
                kind: "dir".to_string(),
                name: name.clone(),
                relative_path: name,
                file_count: row.get(1)?,
                occurrence_count: None,
                size_bytes: row.get(2)?,
                modified_at: None,
                content_id: None,
                sha256: None,
                status: None,
            })
        })?;
        return rows.collect();
    }

    let prefix = format!("{parent}/");
    let start = prefix.chars().count() + 1;
    let like = format!("{}%", escape_like_pattern(&prefix));
    let mut stmt = conn.prepare(
        r#"
        SELECT
            child_name,
            COUNT(*),
            COALESCE(SUM(size_bytes), 0)
        FROM (
            SELECT
                substr(
                    substr(relative_path, ?3),
                    1,
                    instr(substr(relative_path, ?3), '/') - 1
                ) AS child_name,
                size_bytes
            FROM path_observations
            WHERE root_id = ?1
              AND relative_path LIKE ?2 ESCAPE '\'
              AND instr(substr(relative_path, ?3), '/') > 0
        )
        GROUP BY child_name
        ORDER BY child_name ASC
        "#,
    )?;
    let rows = stmt.query_map(params![root_id, like, start as i64], |row| {
        let name: String = row.get(0)?;
        Ok(CachedDirectoryEntry {
            kind: "dir".to_string(),
            name: name.clone(),
            relative_path: format!("{parent}/{name}"),
            file_count: row.get(1)?,
            occurrence_count: None,
            size_bytes: row.get(2)?,
            modified_at: None,
            content_id: None,
            sha256: None,
            status: None,
        })
    })?;
    rows.collect()
}

fn cached_child_files(
    conn: &Connection,
    root_id: &str,
    parent: &str,
) -> rusqlite::Result<Vec<CachedDirectoryEntry>> {
    if parent == "." {
        let mut stmt = conn.prepare(
            r#"
            SELECT
                p.relative_path,
                p.size_bytes,
                p.modified_at,
                p.content_id,
                c.sha256,
                p.status,
                CASE
                    WHEN p.content_id IS NOT NULL THEN (
                        SELECT COUNT(*)
                        FROM path_observations x
                        WHERE x.content_id = p.content_id
                    )
                    ELSE (
                        SELECT COUNT(*)
                        FROM path_observations x
                        WHERE x.basename = p.basename
                          AND x.size_bytes = p.size_bytes
                          AND (x.modified_at = p.modified_at OR (x.modified_at IS NULL AND p.modified_at IS NULL))
                    )
                END AS occurrence_count
            FROM path_observations p
            LEFT JOIN content_objects c ON c.id = p.content_id
            WHERE p.root_id = ?1 AND instr(p.relative_path, '/') = 0
            ORDER BY p.relative_path ASC
            "#,
        )?;
        let rows = stmt.query_map(params![root_id], cached_file_entry_from_row)?;
        return rows.collect();
    }

    let prefix = format!("{parent}/");
    let start = prefix.chars().count() + 1;
    let like = format!("{}%", escape_like_pattern(&prefix));
    let mut stmt = conn.prepare(
        r#"
        SELECT
            p.relative_path,
            p.size_bytes,
            p.modified_at,
            p.content_id,
            c.sha256,
            p.status,
            CASE
                WHEN p.content_id IS NOT NULL THEN (
                    SELECT COUNT(*)
                    FROM path_observations x
                    WHERE x.content_id = p.content_id
                )
                ELSE (
                    SELECT COUNT(*)
                    FROM path_observations x
                    WHERE x.basename = p.basename
                      AND x.size_bytes = p.size_bytes
                      AND (x.modified_at = p.modified_at OR (x.modified_at IS NULL AND p.modified_at IS NULL))
                )
            END AS occurrence_count
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
          AND p.relative_path LIKE ?2 ESCAPE '\'
          AND instr(substr(p.relative_path, ?3), '/') = 0
        ORDER BY p.relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(
        params![root_id, like, start as i64],
        cached_file_entry_from_row,
    )?;
    rows.collect()
}

fn cached_file_entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedDirectoryEntry> {
    let relative_path: String = row.get(0)?;
    let name = Path::new(&relative_path)
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| relative_path.clone());
    Ok(CachedDirectoryEntry {
        kind: "file".to_string(),
        name,
        relative_path,
        file_count: 1,
        occurrence_count: row.get(6)?,
        size_bytes: row.get(1)?,
        modified_at: row.get(2)?,
        content_id: row.get(3)?,
        sha256: row.get(4)?,
        status: Some(row.get(5)?),
    })
}

fn normalize_cached_parent(parent: &str) -> String {
    let trimmed = parent.trim().trim_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        ".".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn root_summary(conn: &Connection, root_id: &str) -> rusqlite::Result<RootSummary> {
    let file_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM path_observations WHERE root_id = ?1",
        params![root_id],
        |row| row.get(0),
    )?;
    let content_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT content_id) FROM path_observations WHERE root_id = ?1 AND content_id IS NOT NULL",
        params![root_id],
        |row| row.get(0),
    )?;
    let evidence = root_integrity_summary(conn, root_id)?;
    Ok(RootSummary {
        file_count,
        content_count,
        hashed_file_count: evidence.hashed_file_count,
        sha256_file_count: evidence.sha256_file_count,
        crc32_file_count: evidence.crc32_file_count,
        chunk_hashed_file_count: evidence.chunk_hashed_file_count,
    })
}

#[derive(Debug, Clone, Copy)]
struct RootIntegrityCounts {
    hashed_file_count: i64,
    sha256_file_count: i64,
    crc32_file_count: i64,
    chunk_hashed_file_count: i64,
}

fn root_integrity_summary(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<RootIntegrityCounts> {
    conn.query_row(
        r#"
        SELECT
            SUM(CASE WHEN p.content_id IS NOT NULL THEN 1 ELSE 0 END),
            SUM(CASE WHEN c.sha256 IS NOT NULL THEN 1 ELSE 0 END),
            SUM(CASE WHEN c.crc32 IS NOT NULL THEN 1 ELSE 0 END),
            SUM(CASE WHEN EXISTS (
                SELECT 1
                FROM path_observation_chunk_hashes h
                WHERE h.path_observation_id = p.id
                LIMIT 1
            ) THEN 1 ELSE 0 END)
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
          AND p.status = 'present'
        "#,
        params![root_id],
        |row| {
            Ok(RootIntegrityCounts {
                hashed_file_count: row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                sha256_file_count: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                crc32_file_count: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                chunk_hashed_file_count: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        },
    )
}

pub fn path_observations_for_root(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
) -> rusqlite::Result<Vec<PathObservationRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path, size_bytes, modified_at, content_id
        FROM path_observations
        WHERE machine_id = ?1 AND root_id = ?2
        "#,
    )?;
    let rows = stmt.query_map(params![machine_id, root_id], |row| {
        let size: i64 = row.get(1)?;
        Ok(PathObservationRow {
            relative_path: row.get(0)?,
            size_bytes: size as u64,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn export_observations_for_root(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<Vec<RootExportObservationRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            p.relative_path,
            p.basename,
            p.parent_path,
            p.size_bytes,
            p.modified_at,
            p.status,
            c.blake3,
            c.sha256,
            c.crc32
        FROM path_observations p
        LEFT JOIN content_objects c ON c.id = p.content_id
        WHERE p.root_id = ?1
        ORDER BY p.relative_path
        "#,
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        let size: i64 = row.get(3)?;
        Ok(RootExportObservationRow {
            relative_path: row.get(0)?,
            basename: row.get(1)?,
            parent_path: row.get(2)?,
            size_bytes: size as u64,
            modified_at: row.get(4)?,
            status: row.get(5)?,
            blake3: row.get(6)?,
            sha256: row.get(7)?,
            crc32: row.get(8)?,
        })
    })?;
    rows.collect()
}

pub fn clear_root_file_metadata(conn: &Connection, root_id: &str) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM path_observation_chunk_hashes WHERE path_observation_id IN (SELECT id FROM path_observations WHERE root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM path_observations WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM checksum_entries WHERE collection_id IN (SELECT id FROM checksum_collections WHERE root_id = ?1)",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM checksum_collections WHERE root_id = ?1",
        params![root_id],
    )?;
    tx.execute(
        "DELETE FROM selection_entries WHERE root_id = ?1 OR selection_set_id IN (SELECT id FROM selection_sets WHERE root_id = ?1)",
        params![root_id],
    )?;
    refresh_root_current_size(&tx, root_id)?;
    tx.commit()
}

pub fn content_collisions_for_root(
    conn: &Connection,
    root_id: &str,
    content_id: &str,
    exclude_relative_path: &str,
) -> rusqlite::Result<Vec<CollisionRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path, size_bytes, modified_at, content_id
        FROM path_observations
        WHERE root_id = ?1
          AND content_id = ?2
          AND relative_path != ?3
        ORDER BY relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(
        params![root_id, content_id, exclude_relative_path],
        collision_from_row,
    )?;
    rows.collect()
}

pub fn filename_size_date_collisions_for_root(
    conn: &Connection,
    root_id: &str,
    basename: &str,
    size_bytes: u64,
    modified_at: Option<&str>,
    exclude_relative_path: &str,
) -> rusqlite::Result<Vec<CollisionRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT relative_path, size_bytes, modified_at, content_id
        FROM path_observations
        WHERE root_id = ?1
          AND basename = ?2
          AND size_bytes = ?3
          AND (modified_at = ?4 OR (modified_at IS NULL AND ?4 IS NULL))
          AND relative_path != ?5
        ORDER BY relative_path ASC
        "#,
    )?;
    let rows = stmt.query_map(
        params![
            root_id,
            basename,
            size_bytes as i64,
            modified_at,
            exclude_relative_path
        ],
        collision_from_row,
    )?;
    rows.collect()
}

fn collision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CollisionRow> {
    let size: i64 = row.get(1)?;
    Ok(CollisionRow {
        relative_path: row.get(0)?,
        size_bytes: size as u64,
        modified_at: row.get(2)?,
        content_id: row.get(3)?,
    })
}

pub fn file_appearances(
    conn: &Connection,
    content_id: Option<&str>,
    basename: &str,
    size_bytes: u64,
    modified_at: Option<&str>,
) -> rusqlite::Result<Vec<FileAppearanceRow>> {
    if let Some(content_id) = content_id {
        let p = file_appearances_aliases().path_observations;
        let sql = file_appearances_base_query()
            .and_where(Expr::col((p, Alias::new("content_id"))).eq(Expr::cust("?1")))
            .to_string(SqliteQueryBuilder);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![content_id], file_appearance_from_row)?;
        return rows.collect();
    }

    let p = file_appearances_aliases().path_observations;
    let modified_matches = Expr::col((p.clone(), Alias::new("modified_at")))
        .eq(Expr::cust("?3"))
        .or(Expr::col((p.clone(), Alias::new("modified_at")))
            .is_null()
            .and(Expr::cust("?3 IS NULL")));
    let sql = file_appearances_base_query()
        .and_where(Expr::col((p.clone(), Alias::new("basename"))).eq(Expr::cust("?1")))
        .and_where(Expr::col((p, Alias::new("size_bytes"))).eq(Expr::cust("?2")))
        .and_where(modified_matches)
        .to_string(SqliteQueryBuilder);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![basename, size_bytes as i64, modified_at],
        file_appearance_from_row,
    )?;
    rows.collect()
}

pub fn local_file_availability_keys(
    conn: &Connection,
    candidates: &[LocalFileCandidate<'_>],
) -> rusqlite::Result<BTreeSet<String>> {
    let mut available = BTreeSet::new();
    for chunk in candidates.chunks(150) {
        let placeholders = std::iter::repeat_n("(?, ?, ?, ?, ?)", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"
            WITH candidates(key, content_id, basename, size_bytes, modified_at) AS (
                VALUES {placeholders}
            )
            SELECT DISTINCT c.key
            FROM candidates c
            JOIN path_observations p
              ON (
                   (c.content_id IS NOT NULL AND p.content_id = c.content_id)
                   OR (
                        p.basename = c.basename
                        AND p.size_bytes = c.size_bytes
                        AND (p.modified_at = c.modified_at OR (p.modified_at IS NULL AND c.modified_at IS NULL))
                   )
                 )
            JOIN machines m ON m.id = p.machine_id
            WHERE COALESCE(m.platform, '') != 'ssh'
              AND p.status = 'present'
            "#
        );
        let mut values = Vec::with_capacity(chunk.len() * 5);
        for candidate in chunk {
            values.push(rusqlite::types::Value::Text(candidate.key.to_string()));
            values.push(
                candidate
                    .content_id
                    .map(|value| rusqlite::types::Value::Text(value.to_string()))
                    .unwrap_or(rusqlite::types::Value::Null),
            );
            values.push(rusqlite::types::Value::Text(candidate.basename.to_string()));
            values.push(rusqlite::types::Value::Integer(candidate.size_bytes as i64));
            values.push(
                candidate
                    .modified_at
                    .map(|value| rusqlite::types::Value::Text(value.to_string()))
                    .unwrap_or(rusqlite::types::Value::Null),
            );
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            available.insert(row?);
        }
    }
    Ok(available)
}

struct FileAppearanceAliases {
    roots: Alias,
    path_observations: Alias,
}

fn file_appearances_aliases() -> FileAppearanceAliases {
    FileAppearanceAliases {
        roots: Alias::new("r"),
        path_observations: Alias::new("p"),
    }
}

fn file_appearances_base_query() -> sea_query::SelectStatement {
    let table_roots = Alias::new("roots");
    let table_path_observations = Alias::new("path_observations");
    let aliases = file_appearances_aliases();
    let r = aliases.roots;
    let p = aliases.path_observations;

    Query::select()
        .expr(Expr::col((r.clone(), Alias::new("id"))))
        .expr(Expr::col((r.clone(), Alias::new("path"))))
        .expr(Expr::col((r.clone(), Alias::new("label"))))
        .expr(Expr::col((p.clone(), Alias::new("relative_path"))))
        .expr(Expr::col((p.clone(), Alias::new("size_bytes"))))
        .expr(Expr::col((p.clone(), Alias::new("modified_at"))))
        .expr(Expr::col((p.clone(), Alias::new("content_id"))))
        .from_as(table_path_observations, p.clone())
        .inner_join(
            TableRef::from(table_roots).alias(r.clone()),
            Expr::col((r.clone(), Alias::new("id"))).equals((p.clone(), Alias::new("root_id"))),
        )
        .order_by((r, Alias::new("path")), sea_query::Order::Asc)
        .order_by((p, Alias::new("relative_path")), sea_query::Order::Asc)
        .to_owned()
}

fn file_appearance_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileAppearanceRow> {
    let size: i64 = row.get(4)?;
    Ok(FileAppearanceRow {
        root_id: row.get(0)?,
        root_path: row.get(1)?,
        root_label: row.get(2)?,
        relative_path: row.get(3)?,
        size_bytes: size as u64,
        modified_at: row.get(5)?,
        content_id: row.get(6)?,
    })
}

pub fn hash_baselines_for_root(
    conn: &Connection,
    machine_id: &str,
    root_id: &str,
) -> rusqlite::Result<Vec<HashBaselineRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT p.relative_path, c.size_bytes, c.blake3, c.sha256
        FROM path_observations p
        JOIN content_objects c ON c.id = p.content_id
        WHERE p.machine_id = ?1
          AND p.root_id = ?2
          AND c.blake3 IS NOT NULL
          AND c.sha256 IS NOT NULL
        "#,
    )?;
    let rows = stmt.query_map(params![machine_id, root_id], |row| {
        let size: i64 = row.get(1)?;
        Ok(HashBaselineRow {
            relative_path: row.get(0)?,
            size_bytes: size as u64,
            blake3: row.get(2)?,
            sha256: row.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn roots(conn: &Connection) -> rusqlite::Result<Vec<RootRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT
            r.id,
            r.machine_id,
            r.path,
            r.label,
            r.current_size_bytes,
            (
                SELECT j.kind
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_kind,
            (
                SELECT j.status
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_status,
            (
                SELECT j.phase
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_phase
        FROM roots r
        ORDER BY r.created_at DESC
        "#,
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RootRow {
            id: row.get(0)?,
            machine_id: row.get(1)?,
            path: row.get(2)?,
            label: row.get(3)?,
            current_size_bytes: row.get(4)?,
            latest_job_kind: row.get(5)?,
            latest_job_status: row.get(6)?,
            latest_job_phase: row.get(7)?,
        })
    })?;
    rows.collect()
}

pub fn find_root_by_machine_path(
    conn: &Connection,
    machine_id: &str,
    path: &str,
) -> rusqlite::Result<Option<RootRow>> {
    conn.query_row(
        r#"
        SELECT
            r.id,
            r.machine_id,
            r.path,
            r.label,
            r.current_size_bytes,
            (
                SELECT j.kind
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_kind,
            (
                SELECT j.status
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_status,
            (
                SELECT j.phase
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_phase
        FROM roots r
        WHERE r.machine_id = ?1 AND r.path = ?2
        "#,
        params![machine_id, path],
        |row| {
            Ok(RootRow {
                id: row.get(0)?,
                machine_id: row.get(1)?,
                path: row.get(2)?,
                label: row.get(3)?,
                current_size_bytes: row.get(4)?,
                latest_job_kind: row.get(5)?,
                latest_job_status: row.get(6)?,
                latest_job_phase: row.get(7)?,
            })
        },
    )
    .optional()
}

pub fn root_by_id(conn: &Connection, root_id: &str) -> rusqlite::Result<Option<RootRow>> {
    conn.query_row(
        r#"
        SELECT
            r.id,
            r.machine_id,
            r.path,
            r.label,
            r.current_size_bytes,
            (
                SELECT j.kind
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_kind,
            (
                SELECT j.status
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_status,
            (
                SELECT j.phase
                FROM jobs j
                WHERE j.root_id = r.id
                ORDER BY j.created_at DESC
                LIMIT 1
            ) AS latest_job_phase
        FROM roots r
        WHERE r.id = ?1
        "#,
        params![root_id],
        |row| {
            Ok(RootRow {
                id: row.get(0)?,
                machine_id: row.get(1)?,
                path: row.get(2)?,
                label: row.get(3)?,
                current_size_bytes: row.get(4)?,
                latest_job_kind: row.get(5)?,
                latest_job_status: row.get(6)?,
                latest_job_phase: row.get(7)?,
            })
        },
    )
    .optional()
}

pub fn target_status(
    conn: &Connection,
    machine_id: &str,
    path: &str,
) -> rusqlite::Result<Option<TargetStatus>> {
    let Some(root) = find_root_by_machine_path(conn, machine_id, path)? else {
        return Ok(None);
    };
    let file_count: i64 = conn.query_row(
        r#"
        SELECT COUNT(*)
        FROM path_observations
        WHERE machine_id = ?1 AND root_id = ?2
        "#,
        params![machine_id, root.id],
        |row| row.get(0),
    )?;
    let content_count: i64 = conn.query_row(
        r#"
        SELECT COUNT(DISTINCT content_id)
        FROM path_observations
        WHERE machine_id = ?1 AND root_id = ?2 AND content_id IS NOT NULL
        "#,
        params![machine_id, root.id],
        |row| row.get(0),
    )?;
    let evidence = root_integrity_summary(conn, &root.id)?;
    let latest_job = conn
        .query_row(
            r#"
            SELECT id, kind, status, machine_id, root_id, created_at, started_at, completed_at,
                   params_json, phase, current_path, files_total, files_seen, files_done,
                   files_skipped, errors, cancel_requested
            FROM jobs
            WHERE root_id = ?1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            params![root.id],
            job_from_row,
        )
        .optional()?;
    let latest_event_at = conn
        .query_row(
            r#"
            SELECT MAX(e.created_at)
            FROM job_events e
            JOIN jobs j ON j.id = e.job_id
            WHERE j.root_id = ?1
            "#,
            params![root.id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let total_bytes = root.current_size_bytes;
    Ok(Some(TargetStatus {
        root,
        file_count,
        total_bytes,
        content_count,
        hashed_file_count: evidence.hashed_file_count,
        sha256_file_count: evidence.sha256_file_count,
        crc32_file_count: evidence.crc32_file_count,
        chunk_hashed_file_count: evidence.chunk_hashed_file_count,
        latest_job,
        latest_event_at,
    }))
}

pub fn recent_jobs_and_events(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<JobEventRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT j.id, j.kind, j.root_id, j.status, j.phase, j.current_path, j.files_seen,
               j.files_done, j.files_skipped, j.errors, j.cancel_requested,
               e.sequence, e.event_kind, e.payload_json, j.params_json
        FROM job_events e
        JOIN jobs j ON j.id = e.job_id
        ORDER BY e.created_at DESC, e.sequence DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(JobEventRow {
            job_id: row.get(0)?,
            job_kind: row.get(1)?,
            root_id: row.get(2)?,
            status: row.get(3)?,
            phase: row.get(4)?,
            current_path: row.get(5)?,
            files_seen: row.get(6)?,
            files_done: row.get(7)?,
            files_skipped: row.get(8)?,
            errors: row.get(9)?,
            cancel_requested: row.get::<_, i64>(10)? != 0,
            sequence: row.get(11)?,
            event_kind: row.get(12)?,
            payload_json: row.get(13)?,
            params_json: row.get(14)?,
        })
    })?;
    rows.collect()
}

pub fn recent_jobs_and_events_for_root(
    conn: &Connection,
    root_id: &str,
    limit: i64,
) -> rusqlite::Result<Vec<JobEventRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT j.id, j.kind, j.root_id, j.status, j.phase, j.current_path, j.files_seen,
               j.files_done, j.files_skipped, j.errors, j.cancel_requested,
               e.sequence, e.event_kind, e.payload_json, j.params_json
        FROM job_events e
        JOIN jobs j ON j.id = e.job_id
        WHERE j.root_id = ?1
        ORDER BY e.created_at DESC, e.sequence DESC
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![root_id, limit], |row| {
        Ok(JobEventRow {
            job_id: row.get(0)?,
            job_kind: row.get(1)?,
            root_id: row.get(2)?,
            status: row.get(3)?,
            phase: row.get(4)?,
            current_path: row.get(5)?,
            files_seen: row.get(6)?,
            files_done: row.get(7)?,
            files_skipped: row.get(8)?,
            errors: row.get(9)?,
            cancel_requested: row.get::<_, i64>(10)? != 0,
            sequence: row.get(11)?,
            event_kind: row.get(12)?,
            payload_json: row.get(13)?,
            params_json: row.get(14)?,
        })
    })?;
    rows.collect()
}

pub fn recent_jobs(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, kind, status, machine_id, root_id, created_at, started_at, completed_at,
               params_json, phase, current_path, files_total, files_seen, files_done,
               files_skipped, errors, cancel_requested
        FROM jobs
        ORDER BY created_at DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], job_from_row)?;
    rows.collect()
}

pub fn job_by_id(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<JobRow>> {
    conn.query_row(
        r#"
        SELECT id, kind, status, machine_id, root_id, created_at, started_at, completed_at,
               params_json, phase, current_path, files_total, files_seen, files_done,
               files_skipped, errors, cancel_requested
        FROM jobs
        WHERE id = ?1
        "#,
        params![job_id],
        job_from_row,
    )
    .optional()
}

pub fn events_for_job(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT job_id, sequence, event_kind, created_at, payload_json
        FROM job_events
        WHERE job_id = ?1
        ORDER BY sequence ASC
        "#,
    )?;
    let rows = stmt.query_map(params![job_id], |row| {
        Ok(EventRow {
            job_id: row.get(0)?,
            sequence: row.get(1)?,
            event_kind: row.get(2)?,
            created_at: row.get(3)?,
            payload_json: row.get(4)?,
        })
    })?;
    rows.collect()
}

fn job_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: row.get(2)?,
        machine_id: row.get(3)?,
        root_id: row.get(4)?,
        created_at: row.get(5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
        params_json: row.get(8)?,
        phase: row.get(9)?,
        current_path: row.get(10)?,
        files_total: row.get(11)?,
        files_seen: row.get(12)?,
        files_done: row.get(13)?,
        files_skipped: row.get(14)?,
        errors: row.get(15)?,
        cancel_requested: row.get::<_, i64>(16)? != 0,
    })
}

#[cfg(test)]
pub fn table_count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_database() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(table_count(&conn, "machines").unwrap(), 0);
    }

    #[test]
    fn initializes_query_indexes_for_tui_startup_paths() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();

        let indexes = conn
            .prepare(
                r#"
                SELECT name
                FROM sqlite_master
                WHERE type = 'index'
                  AND name IN (
                    'idx_job_events_recent',
                    'idx_job_events_job_recent',
                    'idx_jobs_root_recent',
                    'idx_transfer_plans_status_created',
                    'idx_transfer_plans_roots'
                  )
                ORDER BY name
                "#,
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();

        assert_eq!(
            indexes,
            vec![
                "idx_job_events_job_recent",
                "idx_job_events_recent",
                "idx_jobs_root_recent",
                "idx_transfer_plans_roots",
                "idx_transfer_plans_status_created"
            ]
        );
    }

    #[test]
    fn updates_job_progress_projection() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let job_id = create_job(&conn, "scan", None, None, serde_json::json!({})).unwrap();
        update_job_progress(
            &conn,
            &job_id,
            JobProgressInput {
                phase: "processing",
                current_path: Some("a.txt"),
                files_total: Some(3),
                files_seen: 2,
                files_done: 1,
                files_skipped: 0,
                errors: 1,
            },
        )
        .unwrap();

        let job = job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.phase.as_deref(), Some("processing"));
        assert_eq!(job.current_path.as_deref(), Some("a.txt"));
        assert_eq!(job.files_total, 3);
        assert_eq!(job.files_seen, 2);
        assert_eq!(job.files_done, 1);
        assert_eq!(job.errors, 1);
    }

    #[test]
    fn upserts_path_observation() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: "a.txt",
                basename: "a.txt",
                parent_path: ".",
                size_bytes: 1,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: "a.txt",
                basename: "a.txt",
                parent_path: ".",
                size_bytes: 2,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();
        assert_eq!(table_count(&conn, "path_observations").unwrap(), 1);
        assert_eq!(recent_files(&conn, 1).unwrap()[0].size_bytes, 2);
        let root = find_root_by_machine_path(&conn, &machine_id, "/tmp/root")
            .unwrap()
            .unwrap();
        assert_eq!(root.current_size_bytes, 2);
    }

    #[test]
    fn path_observations_are_current_per_root_path() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let other_machine_id =
            ensure_machine_record(&conn, "machine_other", "other", Some("linux")).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: "a.txt",
                basename: "a.txt",
                parent_path: ".",
                size_bytes: 1,
                modified_at: Some("2026-07-01T00:00:00Z"),
                content_id: None,
            },
        )
        .unwrap();
        let content_id = ensure_content_object_sha256(&conn, 2, "sha256").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &other_machine_id,
                root_id: &root_id,
                relative_path: "a.txt",
                basename: "a.txt",
                parent_path: ".",
                size_bytes: 2,
                modified_at: Some("2026-07-02T00:00:00Z"),
                content_id: Some(&content_id),
            },
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM path_observations WHERE root_id = ?1 AND relative_path = 'a.txt'",
                params![root_id],
                |row| row.get(0),
            )
            .unwrap();
        let row = path_observation_for_root_path(&conn, &root_id, "a.txt")
            .unwrap()
            .unwrap();

        assert_eq!(count, 1);
        assert_eq!(row.size_bytes, 2);
        assert_eq!(row.content_id.as_deref(), Some(content_id.as_str()));
    }

    #[test]
    fn init_schema_dedupes_legacy_path_observation_duplicates() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        conn.execute("DROP INDEX idx_path_observations_root_relative_path", [])
            .unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let other_machine_id =
            ensure_machine_record(&conn, "machine_other", "other", Some("linux")).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        let content_id = ensure_content_object_sha256(&conn, 1, "sha256").unwrap();
        conn.execute(
            r#"
            INSERT INTO path_observations
                (id, machine_id, root_id, relative_path, basename, parent_path, size_bytes,
                 modified_at, content_id, last_seen_at, status)
            VALUES
                ('path_fast', ?1, ?3, 'dup.txt', 'dup.txt', '.', 1, NULL, NULL, '2026-07-02T00:00:00Z', 'present'),
                ('path_hash', ?2, ?3, 'dup.txt', 'dup.txt', '.', 1, NULL, ?4, '2026-07-01T00:00:00Z', 'present')
            "#,
            params![machine_id, other_machine_id, root_id, content_id],
        )
        .unwrap();

        init_schema(&conn).unwrap();

        let rows = conn
            .prepare(
                "SELECT id, content_id FROM path_observations WHERE root_id = ?1 AND relative_path = 'dup.txt'",
            )
            .unwrap()
            .query_map(params![root_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        assert_eq!(rows, vec![("path_hash".to_string(), Some(content_id))]);
    }

    #[test]
    fn cached_directory_entries_show_immediate_children_with_directory_sizes() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        for (path, size) in [
            ("top.txt", 1_u64),
            ("dir/a.txt", 2),
            ("dir/nested/b.txt", 3),
            ("other/c.txt", 4),
        ] {
            insert_path_observation(
                &conn,
                PathObservationInput {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    relative_path: path,
                    basename: path.rsplit('/').next().unwrap(),
                    parent_path: ".",
                    size_bytes: size,
                    modified_at: None,
                    content_id: None,
                },
            )
            .unwrap();
        }

        let root_entries = cached_directory_entries(&conn, &root_id, ".").unwrap();
        assert_eq!(
            root_entries
                .iter()
                .map(|entry| (
                    entry.kind.as_str(),
                    entry.relative_path.as_str(),
                    entry.size_bytes
                ))
                .collect::<Vec<_>>(),
            vec![
                ("dir", "dir", 5),
                ("dir", "other", 4),
                ("file", "top.txt", 1)
            ]
        );

        let dir_entries = cached_directory_entries(&conn, &root_id, "dir").unwrap();
        assert_eq!(
            dir_entries
                .iter()
                .map(|entry| (
                    entry.kind.as_str(),
                    entry.relative_path.as_str(),
                    entry.size_bytes
                ))
                .collect::<Vec<_>>(),
            vec![("dir", "dir/nested", 3), ("file", "dir/a.txt", 2)]
        );
    }

    #[test]
    fn toggles_directory_selection_for_descendant_files() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        for (path, size) in [
            ("dir/a.txt", 2_u64),
            ("dir/nested/b.txt", 3),
            ("other/c.txt", 4),
        ] {
            insert_path_observation(
                &conn,
                PathObservationInput {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    relative_path: path,
                    basename: path.rsplit('/').next().unwrap(),
                    parent_path: ".",
                    size_bytes: size,
                    modified_at: None,
                    content_id: None,
                },
            )
            .unwrap();
        }

        let marked = toggle_selection_directory(&conn, &root_id, "dir").unwrap();
        assert!(marked.selected);
        assert_eq!(marked.files_changed, 2);
        assert_eq!(marked.bytes_changed, 5);
        assert_eq!(
            selected_paths_for_root(&conn, &root_id).unwrap(),
            BTreeSet::from(["dir/a.txt".to_string(), "dir/nested/b.txt".to_string()])
        );

        let unmarked = toggle_selection_directory(&conn, &root_id, "dir").unwrap();
        assert!(!unmarked.selected);
        assert_eq!(unmarked.files_changed, 2);
        assert!(selected_paths_for_root(&conn, &root_id).unwrap().is_empty());
    }

    #[test]
    fn toggles_selection_entries_and_summarizes_bytes() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: "a.txt",
                basename: "a.txt",
                parent_path: ".",
                size_bytes: 5,
                modified_at: None,
                content_id: None,
            },
        )
        .unwrap();

        assert!(toggle_selection_entry(&conn, &root_id, "a.txt").unwrap());
        let selected = selected_paths_for_root(&conn, &root_id).unwrap();
        assert!(selected.contains("a.txt"));
        let summary = selection_summary_for_root(&conn, &root_id).unwrap();
        assert_eq!(summary.marked_count, 1);
        assert_eq!(summary.marked_bytes, 5);

        assert!(!toggle_selection_entry(&conn, &root_id, "a.txt").unwrap());
        let summary = selection_summary_for_root(&conn, &root_id).unwrap();
        assert_eq!(summary.marked_count, 0);
        assert_eq!(summary.marked_bytes, 0);
    }

    #[test]
    fn selected_file_entries_group_by_parent_path() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_id = ensure_root(&conn, &machine_id, "/tmp/root").unwrap();
        for (path, parent, size) in [
            ("dir/a.txt", "dir", 2_u64),
            ("dir/nested/b.txt", "dir/nested", 3),
            ("root.txt", ".", 1),
        ] {
            insert_path_observation(
                &conn,
                PathObservationInput {
                    machine_id: &machine_id,
                    root_id: &root_id,
                    relative_path: path,
                    basename: path.rsplit('/').next().unwrap(),
                    parent_path: parent,
                    size_bytes: size,
                    modified_at: None,
                    content_id: None,
                },
            )
            .unwrap();
            assert!(toggle_selection_entry(&conn, &root_id, path).unwrap());
        }

        let entries = selected_file_entries_for_root(&conn, &root_id).unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.parent_path.as_str(), entry.relative_path.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (".", "root.txt"),
                ("dir", "dir/a.txt"),
                ("dir/nested", "dir/nested/b.txt")
            ]
        );
    }

    #[test]
    fn stores_and_filters_transfer_plans() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let source_id = ensure_root(&conn, &machine_id, "/tmp/source").unwrap();
        let dest_id = ensure_root(&conn, &machine_id, "/tmp/dest").unwrap();
        let set_id = ensure_default_selection_set(&conn, &source_id).unwrap();
        let plan_id = create_transfer_plan(
            &conn,
            None,
            &source_id,
            &dest_id,
            Some(&set_id),
            serde_json::json!({ "test": true }),
        )
        .unwrap();
        insert_transfer_plan_entry(
            &conn,
            TransferPlanEntryInput {
                plan_id: &plan_id,
                relative_path: "a.txt",
                dest_relative_path: None,
                size_bytes: 10,
                source_content_id: None,
                dest_content_id: None,
                action: "copy",
                reason: "missing",
                metadata_json: serde_json::json!({}),
            },
        )
        .unwrap();
        insert_transfer_plan_entry(
            &conn,
            TransferPlanEntryInput {
                plan_id: &plan_id,
                relative_path: "b.txt",
                dest_relative_path: None,
                size_bytes: 20,
                source_content_id: None,
                dest_content_id: None,
                action: "review",
                reason: "possible duplicate",
                metadata_json: serde_json::json!({}),
            },
        )
        .unwrap();

        let plans = recent_transfer_plans(&conn, 10).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].entry_count, 2);
        assert_eq!(plans[0].total_bytes, 30);
        let plan = transfer_plan_by_id(&conn, &plan_id).unwrap().unwrap();
        assert_eq!(plan.source_path, "/tmp/source");
        let copy_entries = transfer_plan_entries_filtered(&conn, &plan_id, Some("copy")).unwrap();
        assert_eq!(copy_entries.len(), 1);
        assert_eq!(copy_entries[0].relative_path, "a.txt");
        assert!(queued_transfer_plans(&conn, 10).unwrap().is_empty());
        update_transfer_plan_status(&conn, &plan_id, "queued").unwrap();
        let queued = queued_transfer_plans(&conn, 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, plan_id);
        update_transfer_plan_status(&conn, &plan_id, "planned").unwrap();

        assert!(decide_review_transfer_plan_entry(
            &conn,
            &plan_id,
            "b.txt",
            "skip",
            "review dropped by user",
            serde_json::json!({ "decision": "drop" }),
        )
        .unwrap());
        let skipped_entries =
            transfer_plan_entries_filtered(&conn, &plan_id, Some("skip")).unwrap();
        assert_eq!(skipped_entries.len(), 1);
        assert_eq!(skipped_entries[0].relative_path, "b.txt");
        assert!(!decide_review_transfer_plan_entry(
            &conn,
            &plan_id,
            "b.txt",
            "copy",
            "review accepted for copy",
            serde_json::json!({ "decision": "accept" }),
        )
        .unwrap());

        insert_transfer_plan_entry(
            &conn,
            TransferPlanEntryInput {
                plan_id: &plan_id,
                relative_path: "c.txt",
                dest_relative_path: None,
                size_bytes: 30,
                source_content_id: None,
                dest_content_id: None,
                action: "review",
                reason: "possible duplicate",
                metadata_json: serde_json::json!({}),
            },
        )
        .unwrap();
        assert!(
            retarget_review_transfer_plan_entry(&conn, &plan_id, "c.txt", "renamed/c.txt").unwrap()
        );
        let retargeted = transfer_plan_entries_filtered(&conn, &plan_id, Some("copy")).unwrap();
        assert!(retargeted
            .iter()
            .any(|entry| entry.relative_path == "c.txt"
                && entry.dest_relative_path == "renamed/c.txt"));
        assert!(
            retarget_review_transfer_plan_entry(&conn, &plan_id, "missing.txt", "../bad").is_err()
        );
    }

    #[test]
    fn queues_file_job_with_created_event() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let job_id = queue_file_job(&conn, "scan", dir.path(), None).unwrap();
        let job = job_by_id(&conn, &job_id).unwrap().unwrap();
        assert_eq!(job.status, "created");
        assert_eq!(
            events_for_job(&conn, &job_id).unwrap()[0].event_kind,
            "job_created"
        );
    }

    #[test]
    fn crc32_attaches_to_existing_sha256_content_identity() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let sha256 = "a".repeat(64);

        let first = ensure_content_object_sha256(&conn, 10, &sha256).unwrap();
        let second = ensure_content_object_sha256_crc(&conn, 10, &sha256, "CBF43926").unwrap();
        let row = content_object_by_id(&conn, &first).unwrap().unwrap();

        assert_eq!(first, second);
        assert_eq!(row.sha256.as_deref(), Some(sha256.as_str()));
        assert_eq!(row.crc32.as_deref(), Some("CBF43926"));
    }

    #[test]
    fn file_appearances_find_matches_by_content_or_stat_identity() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let machine_id = ensure_local_machine_with_label(&conn, None).unwrap();
        let root_a = ensure_root(&conn, &machine_id, "/tmp/root-a").unwrap();
        let root_b = ensure_root(&conn, &machine_id, "/tmp/root-b").unwrap();
        let content_id = ensure_content_object(&conn, 10, "b3", "sha").unwrap();
        for (root_id, path) in [(&root_a, "photos/foo.png"), (&root_b, "dupes/foo.png")] {
            insert_path_observation(
                &conn,
                PathObservationInput {
                    machine_id: &machine_id,
                    root_id,
                    relative_path: path,
                    basename: "foo.png",
                    parent_path: ".",
                    size_bytes: 10,
                    modified_at: Some("2026-07-08T12:00:00Z"),
                    content_id: Some(&content_id),
                },
            )
            .unwrap();
        }

        let content_matches =
            file_appearances(&conn, Some(&content_id), "foo.png", 10, None).unwrap();
        assert_eq!(content_matches.len(), 2);
        assert!(content_matches
            .iter()
            .any(|row| row.root_path == "/tmp/root-a" && row.relative_path == "photos/foo.png"));

        let root_c = ensure_root(&conn, &machine_id, "/tmp/root-c").unwrap();
        let root_d = ensure_root(&conn, &machine_id, "/tmp/root-d").unwrap();
        for (root_id, path) in [(&root_c, "one/bar.txt"), (&root_d, "two/bar.txt")] {
            insert_path_observation(
                &conn,
                PathObservationInput {
                    machine_id: &machine_id,
                    root_id,
                    relative_path: path,
                    basename: "bar.txt",
                    parent_path: ".",
                    size_bytes: 25,
                    modified_at: Some("2026-07-09T01:02:03Z"),
                    content_id: None,
                },
            )
            .unwrap();
        }

        let stat_matches =
            file_appearances(&conn, None, "bar.txt", 25, Some("2026-07-09T01:02:03Z")).unwrap();
        assert_eq!(stat_matches.len(), 2);
        assert!(stat_matches
            .iter()
            .all(|row| row.relative_path.ends_with("bar.txt")));
    }

    #[test]
    fn local_file_availability_keys_exclude_ssh_observations() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        init_schema(&conn).unwrap();
        let ssh_machine = ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
        let ssh_root = ensure_root(&conn, &ssh_machine, "nas01:/srv/archive").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &ssh_machine,
                root_id: &ssh_root,
                relative_path: "copy.bin",
                basename: "copy.bin",
                parent_path: ".",
                size_bytes: 99,
                modified_at: Some("2026-07-10T01:02:03Z"),
                content_id: None,
            },
        )
        .unwrap();

        let candidate = LocalFileCandidate {
            key: "copy.bin",
            content_id: None,
            basename: "copy.bin",
            size_bytes: 99,
            modified_at: Some("2026-07-10T01:02:03Z"),
        };
        assert!(local_file_availability_keys(&conn, &[candidate])
            .unwrap()
            .is_empty());

        let local_machine = ensure_local_machine_with_label(&conn, None).unwrap();
        let local_root = ensure_root(&conn, &local_machine, "/tmp/local").unwrap();
        insert_path_observation(
            &conn,
            PathObservationInput {
                machine_id: &local_machine,
                root_id: &local_root,
                relative_path: "copy.bin",
                basename: "copy.bin",
                parent_path: ".",
                size_bytes: 99,
                modified_at: Some("2026-07-10T01:02:03Z"),
                content_id: None,
            },
        )
        .unwrap();

        assert_eq!(
            local_file_availability_keys(&conn, &[candidate]).unwrap(),
            BTreeSet::from(["copy.bin".to_string()])
        );
    }
}
