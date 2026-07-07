use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

use crate::db;
use crate::error::GremlinError;
use crate::events::{EventEnvelope, EventPayload};

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
}
