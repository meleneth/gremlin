use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

use crate::db;
use crate::error::GremlinError;
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{basename, now_rfc3339, parent_path};

pub fn import_events_file(conn: &Connection, input: &Path) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let reader = BufReader::new(file);
    let source = input.display().to_string();
    let import_job_id = db::ensure_import_job(conn, &source)?;
    let collection_id =
        db::create_checksum_collection(conn, &source, "jsonl_import", Some(&import_job_id))?;

    let mut imported = 0_u64;
    let mut checksums = 0_u64;
    let mut import_sequence = db::next_sequence(conn, &import_job_id)?;

    for (idx, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {} from {}", idx + 1, source))?;
        if line.trim().is_empty() {
            continue;
        }
        let mut event: EventEnvelope =
            serde_json::from_str(&line).map_err(|source| GremlinError::JsonLine {
                line: idx + 1,
                source,
            })?;

        if event.job_id.is_none() {
            event.job_id = Some(import_job_id.clone());
            event.sequence = Some(import_sequence);
            import_sequence += 1;
        }

        db::persist_event(conn, &event)?;
        imported += 1;

        if let EventPayload::HashCompleted {
            relative_path,
            basename,
            size_bytes,
            modified_at,
            blake3,
            sha256,
            ..
        } = &event.payload
        {
            db::insert_checksum_entry(
                conn,
                db::ChecksumEntryInput {
                    collection_id: &collection_id,
                    relative_path,
                    basename,
                    size_bytes: *size_bytes,
                    modified_at: modified_at.as_deref(),
                    blake3: Some(blake3),
                    sha256: Some(sha256),
                    metadata_json: serde_json::json!({
                        "import_event_job_id": event.job_id,
                        "import_event_sequence": event.sequence
                    }),
                },
            )?;
            checksums += 1;
        }
    }

    db::complete_job(conn, &import_job_id, "completed")?;
    println!(
        "import job {import_job_id}: {imported} events, {checksums} checksum entries, collection {collection_id}"
    );
    Ok(())
}

pub fn import_manifest_file(conn: &Connection, input: &Path) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let format = manifest_format(input)?;
    if format == "par2" {
        anyhow::bail!(
            "PAR2 file-list import needs a PAR2 parser/extractor; SFV/CFV import is supported now"
        );
    }

    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let reader = BufReader::new(file);
    let source = input.display().to_string();
    let job_id = db::create_job(
        conn,
        "import_manifest",
        None,
        None,
        serde_json::json!({ "path": source, "format": format }),
    )?;
    db::start_job(conn, &job_id)?;
    let collection_id = db::create_checksum_collection(
        conn,
        &source,
        &format!("{format}_manifest"),
        Some(&job_id),
    )?;
    let mut sequence = db::next_sequence(conn, &job_id)?;
    persist_import_job_event(
        conn,
        &job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "import_manifest".to_string(),
            path: Some(source.clone()),
            message: Some(format.to_string()),
            files_seen: None,
            errors: None,
        },
    )?;

    let mut entries = 0_u64;
    let mut skipped = 0_u64;
    for (idx, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("reading line {} from {}", idx + 1, source))?;
        let Some(entry) = parse_sfv_line(&line) else {
            if !line.trim().is_empty() && !is_manifest_comment(&line) {
                skipped += 1;
            }
            continue;
        };
        let base = basename(Path::new(&entry.relative_path))
            .unwrap_or_else(|_| entry.relative_path.clone());
        db::insert_checksum_entry(
            conn,
            db::ChecksumEntryInput {
                collection_id: &collection_id,
                relative_path: &entry.relative_path,
                basename: &base,
                size_bytes: 0,
                modified_at: None,
                blake3: None,
                sha256: None,
                metadata_json: serde_json::json!({
                    "format": format,
                    "crc32": entry.crc32,
                    "parent_path": parent_path(&entry.relative_path),
                    "line": idx + 1
                }),
            },
        )?;
        entries += 1;
        db::update_job_progress(
            conn,
            &job_id,
            db::JobProgressInput {
                phase: "processing",
                current_path: Some(&entry.relative_path),
                files_total: None,
                files_seen: entries + skipped,
                files_done: entries,
                files_skipped: skipped,
                errors: 0,
            },
        )?;
    }

    db::update_job_progress(
        conn,
        &job_id,
        db::JobProgressInput {
            phase: "finalizing",
            current_path: None,
            files_total: None,
            files_seen: entries + skipped,
            files_done: entries,
            files_skipped: skipped,
            errors: 0,
        },
    )?;
    persist_import_job_event(
        conn,
        &job_id,
        &mut sequence,
        EventKind::JobCompleted,
        EventPayload::Job {
            kind: "import_manifest".to_string(),
            path: Some(source),
            message: Some(format!(
                "entries={entries} skipped={skipped} collection={collection_id}"
            )),
            files_seen: Some(entries + skipped),
            errors: Some(0),
        },
    )?;
    db::complete_job(conn, &job_id, "completed")?;
    println!(
        "manifest import job {job_id}: {entries} entries, {skipped} skipped, collection {collection_id}"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SfvEntry {
    relative_path: String,
    crc32: String,
}

fn manifest_format(path: &Path) -> anyhow::Result<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("sfv") => Ok("sfv"),
        Some("cfv") => Ok("cfv"),
        Some("par2") => Ok("par2"),
        _ => anyhow::bail!("unsupported manifest format; expected .sfv, .cfv, or .par2"),
    }
}

fn parse_sfv_line(line: &str) -> Option<SfvEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_manifest_comment(trimmed) {
        return None;
    }
    let (path, crc) = trimmed.rsplit_once(char::is_whitespace)?;
    let crc = crc.trim();
    if crc.len() != 8 || !crc.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let relative_path = path.trim().trim_start_matches('*').to_string();
    if relative_path.is_empty() {
        return None;
    }
    Some(SfvEntry {
        relative_path,
        crc32: crc.to_ascii_uppercase(),
    })
}

fn is_manifest_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with(';') || trimmed.starts_with('#')
}

fn persist_import_job_event(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventKind, EventPayload};
    use crate::util::now_rfc3339;

    #[test]
    fn imports_hash_events_into_collection() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("checksums.jsonl");
        let event = EventEnvelope {
            event_kind: EventKind::HashCompleted,
            job_id: Some("job_remote".to_string()),
            sequence: Some(1),
            created_at: now_rfc3339(),
            payload: EventPayload::HashCompleted {
                relative_path: "a.txt".to_string(),
                basename: "a.txt".to_string(),
                parent_path: ".".to_string(),
                size_bytes: 5,
                modified_at: None,
                blake3: "b".repeat(64),
                sha256: "s".repeat(64),
            },
        };
        std::fs::write(&jsonl, format!("{}\n", event.to_json_line().unwrap())).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        import_events_file(&conn, &jsonl).unwrap();
        assert_eq!(db::table_count(&conn, "checksum_collections").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "checksum_entries").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "job_events").unwrap(), 1);
    }

    #[test]
    fn imports_sfv_manifest_into_collection() {
        let dir = tempfile::tempdir().unwrap();
        let sfv = dir.path().join("sample.sfv");
        std::fs::write(
            &sfv,
            "; generated elsewhere\nfolder/a.bin DEADBEEF\n*space name.txt 0123abcd\n",
        )
        .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        import_manifest_file(&conn, &sfv).unwrap();
        assert_eq!(db::table_count(&conn, "checksum_collections").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "checksum_entries").unwrap(), 2);
        let job = db::recent_jobs(&conn, 1).unwrap().remove(0);
        assert_eq!(job.kind, "import_manifest");
        assert_eq!(job.files_done, 2);
    }

    #[test]
    fn parses_sfv_lines() {
        assert_eq!(
            parse_sfv_line("dir/file.bin deadbeef"),
            Some(SfvEntry {
                relative_path: "dir/file.bin".to_string(),
                crc32: "DEADBEEF".to_string()
            })
        );
        assert_eq!(parse_sfv_line("; comment"), None);
        assert_eq!(parse_sfv_line("not a checksum"), None);
    }
}
