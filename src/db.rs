use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
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
pub struct FileRow {
    pub relative_path: String,
    pub size_bytes: i64,
    pub modified_at: Option<String>,
    pub content_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct RootSummary {
    pub file_count: i64,
    pub content_count: i64,
}

#[derive(Debug, Clone)]
pub struct PathObservationRow {
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
            metadata_json TEXT
        );
        "#,
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
        ON CONFLICT(machine_id, root_id, relative_path) DO UPDATE SET
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

pub fn ensure_content_object(
    conn: &Connection,
    size_bytes: u64,
    blake3: &str,
    sha256: &str,
) -> rusqlite::Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM content_objects WHERE size_bytes = ?1 AND blake3 = ?2 AND sha256 = ?3",
            params![size_bytes as i64, blake3, sha256],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }

    let id = new_id("content");
    conn.execute(
        "INSERT INTO content_objects (id, size_bytes, blake3, sha256, first_seen_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, size_bytes as i64, blake3, sha256, now_rfc3339()],
    )?;
    Ok(id)
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

pub fn insert_checksum_entry(
    conn: &Connection,
    input: ChecksumEntryInput<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        r#"
        INSERT INTO checksum_entries
            (id, collection_id, relative_path, basename, size_bytes, modified_at, blake3, sha256, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
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
            serde_json::to_string(&input.metadata_json).unwrap_or_else(|_| "{}".to_string())
        ],
    )?;
    Ok(())
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
        SELECT relative_path, size_bytes, modified_at, content_id, status
        FROM path_observations
        ORDER BY last_seen_at DESC
        LIMIT ?1
        "#,
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            status: row.get(4)?,
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
        SELECT relative_path, size_bytes, modified_at, content_id, status
        FROM path_observations
        WHERE root_id = ?1
        ORDER BY last_seen_at DESC
        LIMIT ?2
        "#,
    )?;
    let rows = stmt.query_map(params![root_id, limit], |row| {
        Ok(FileRow {
            relative_path: row.get(0)?,
            size_bytes: row.get(1)?,
            modified_at: row.get(2)?,
            content_id: row.get(3)?,
            status: row.get(4)?,
        })
    })?;
    rows.collect()
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
    Ok(RootSummary {
        file_count,
        content_count,
    })
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
        latest_job,
        latest_event_at,
    }))
}

pub fn recent_jobs_and_events(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<JobEventRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT j.id, j.kind, j.status, j.phase, j.current_path, j.files_seen,
               j.files_done, j.files_skipped, j.errors, j.cancel_requested,
               e.sequence, e.event_kind, e.payload_json
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
            status: row.get(2)?,
            phase: row.get(3)?,
            current_path: row.get(4)?,
            files_seen: row.get(5)?,
            files_done: row.get(6)?,
            files_skipped: row.get(7)?,
            errors: row.get(8)?,
            cancel_requested: row.get::<_, i64>(9)? != 0,
            sequence: row.get(10)?,
            event_kind: row.get(11)?,
            payload_json: row.get(12)?,
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
        SELECT j.id, j.kind, j.status, j.phase, j.current_path, j.files_seen,
               j.files_done, j.files_skipped, j.errors, j.cancel_requested,
               e.sequence, e.event_kind, e.payload_json
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
            status: row.get(2)?,
            phase: row.get(3)?,
            current_path: row.get(4)?,
            files_seen: row.get(5)?,
            files_done: row.get(6)?,
            files_skipped: row.get(7)?,
            errors: row.get(8)?,
            cancel_requested: row.get::<_, i64>(9)? != 0,
            sequence: row.get(10)?,
            event_kind: row.get(11)?,
            payload_json: row.get(12)?,
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
}
