use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

use crate::db;
use crate::error::GremlinError;
use crate::events::{EventEnvelope, EventKind, EventPayload};
use crate::util::{basename, now_rfc3339, parent_path};

#[derive(Debug, Clone)]
pub struct EventImportTarget {
    pub machine_id: String,
    pub root_id: String,
    pub root_path: String,
}

pub fn import_events_file(conn: &Connection, input: &Path) -> anyhow::Result<()> {
    import_events_file_for_target(conn, input, None)
}

pub fn import_events_file_for_target(
    conn: &Connection,
    input: &Path,
    target: Option<&EventImportTarget>,
) -> anyhow::Result<()> {
    import_events_file_for_target_inner(conn, input, target, true)
}

pub fn import_events_file_for_target_silent(
    conn: &Connection,
    input: &Path,
    target: Option<&EventImportTarget>,
) -> anyhow::Result<()> {
    import_events_file_for_target_inner(conn, input, target, false)
}

fn import_events_file_for_target_inner(
    conn: &Connection,
    input: &Path,
    target: Option<&EventImportTarget>,
    print_summary: bool,
) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let file = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let reader = BufReader::new(file);
    let source = input.display().to_string();
    let import_job_id = create_import_job(conn, &source, target)?;
    let collection_id =
        db::create_checksum_collection(conn, &source, "jsonl_import", Some(&import_job_id))?;
    if let Some(target) = target {
        db::attach_checksum_collection_target(
            conn,
            &collection_id,
            &target.machine_id,
            &target.root_id,
        )?;
    }

    let mut imported = 0_u64;
    let mut checksums = 0_u64;
    let mut projected = 0_u64;
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
            parent_path,
            size_bytes,
            modified_at,
            blake3,
            sha256,
            ..
        } = &event.payload
        {
            let projected_relative_path = target
                .filter(|_| relative_path == ".")
                .map(|target| remote_or_path_basename(&target.root_path))
                .unwrap_or_else(|| relative_path.clone());
            let projected_basename = if projected_relative_path == *relative_path {
                basename.clone()
            } else {
                projected_relative_path.clone()
            };
            let projected_parent_path = if projected_relative_path == *relative_path {
                parent_path.clone()
            } else {
                crate::util::parent_path(&projected_relative_path)
            };
            db::insert_checksum_entry(
                conn,
                db::ChecksumEntryInput {
                    collection_id: &collection_id,
                    relative_path: &projected_relative_path,
                    basename: &projected_basename,
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
            if let Some(target) = target {
                let content_id = db::ensure_content_object(conn, *size_bytes, blake3, sha256)?;
                db::insert_path_observation(
                    conn,
                    db::PathObservationInput {
                        machine_id: &target.machine_id,
                        root_id: &target.root_id,
                        relative_path: &projected_relative_path,
                        basename: &projected_basename,
                        parent_path: &projected_parent_path,
                        size_bytes: *size_bytes,
                        modified_at: modified_at.as_deref(),
                        content_id: Some(&content_id),
                    },
                )?;
                projected += 1;
            }
            checksums += 1;
        }
    }

    db::complete_job(conn, &import_job_id, "completed")?;
    if print_summary {
        if let Some(target) = target {
            println!(
                "import job {import_job_id}: {imported} events, {checksums} checksum entries, {projected} projected files, collection {collection_id}, root {} {}",
                target.root_id, target.root_path
            );
        } else {
            println!(
                "import job {import_job_id}: {imported} events, {checksums} checksum entries, collection {collection_id}"
            );
        }
    }
    Ok(())
}

fn remote_or_path_basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && *name != "~")
        .unwrap_or(path)
        .to_string()
}

fn create_import_job(
    conn: &Connection,
    source: &str,
    target: Option<&EventImportTarget>,
) -> rusqlite::Result<String> {
    let job_id = if let Some(target) = target {
        db::create_job(
            conn,
            "import_events",
            Some(&target.machine_id),
            Some(&target.root_id),
            serde_json::json!({
                "source": source,
                "target_root_id": target.root_id,
                "target_root_path": target.root_path,
            }),
        )?
    } else {
        return db::ensure_import_job(conn, source);
    };
    db::start_job(conn, &job_id)?;
    Ok(job_id)
}

pub fn import_manifest_file(conn: &Connection, input: &Path) -> anyhow::Result<()> {
    db::init_schema(conn)?;
    let format = manifest_format(input)?;
    if format == "par2" {
        return import_par2_file_list(conn, input);
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

fn import_par2_file_list(conn: &Connection, input: &Path) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    File::open(input)
        .with_context(|| format!("opening {}", input.display()))?
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", input.display()))?;
    let entries = parse_par2_file_descriptions(&bytes);
    let source = input.display().to_string();
    let job_id = db::create_job(
        conn,
        "import_manifest",
        None,
        None,
        serde_json::json!({ "path": source, "format": "par2" }),
    )?;
    db::start_job(conn, &job_id)?;
    let collection_id =
        db::create_checksum_collection(conn, &source, "par2_manifest", Some(&job_id))?;
    let mut sequence = db::next_sequence(conn, &job_id)?;
    persist_import_job_event(
        conn,
        &job_id,
        &mut sequence,
        EventKind::JobStarted,
        EventPayload::Job {
            kind: "import_manifest".to_string(),
            path: Some(source.clone()),
            message: Some("par2".to_string()),
            files_seen: None,
            errors: None,
        },
    )?;

    for (idx, entry) in entries.iter().enumerate() {
        let base = basename(Path::new(&entry.relative_path))
            .unwrap_or_else(|_| entry.relative_path.clone());
        db::insert_checksum_entry(
            conn,
            db::ChecksumEntryInput {
                collection_id: &collection_id,
                relative_path: &entry.relative_path,
                basename: &base,
                size_bytes: entry.size_bytes,
                modified_at: None,
                blake3: None,
                sha256: None,
                metadata_json: serde_json::json!({
                    "format": "par2",
                    "parent_path": parent_path(&entry.relative_path),
                    "file_id": entry.file_id,
                    "md5": entry.md5,
                    "md5_16k": entry.md5_16k
                }),
            },
        )?;
        db::update_job_progress(
            conn,
            &job_id,
            db::JobProgressInput {
                phase: "processing",
                current_path: Some(&entry.relative_path),
                files_total: Some(entries.len() as u64),
                files_seen: idx as u64 + 1,
                files_done: idx as u64 + 1,
                files_skipped: 0,
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
            files_total: Some(entries.len() as u64),
            files_seen: entries.len() as u64,
            files_done: entries.len() as u64,
            files_skipped: 0,
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
                "entries={} collection={collection_id}",
                entries.len()
            )),
            files_seen: Some(entries.len() as u64),
            errors: Some(0),
        },
    )?;
    db::complete_job(conn, &job_id, "completed")?;
    println!(
        "par2 import job {job_id}: {} file entries, collection {collection_id}",
        entries.len()
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SfvEntry {
    relative_path: String,
    crc32: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Par2FileEntry {
    relative_path: String,
    size_bytes: u64,
    file_id: String,
    md5: String,
    md5_16k: String,
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

fn parse_par2_file_descriptions(bytes: &[u8]) -> Vec<Par2FileEntry> {
    const MAGIC: &[u8; 8] = b"PAR2\0PKT";
    const FILE_DESC: &[u8; 16] = b"PAR 2.0\0FileDesc";
    let mut entries = Vec::new();
    let mut offset = 0_usize;
    while offset + 64 <= bytes.len() {
        if &bytes[offset..offset + 8] != MAGIC {
            offset += 1;
            continue;
        }
        let mut len_bytes = [0_u8; 8];
        len_bytes.copy_from_slice(&bytes[offset + 8..offset + 16]);
        let packet_len = u64::from_le_bytes(len_bytes) as usize;
        if packet_len < 64 || offset + packet_len > bytes.len() {
            offset += 8;
            continue;
        }
        let packet_type = &bytes[offset + 48..offset + 64];
        if packet_type == FILE_DESC {
            if let Some(entry) =
                parse_par2_file_description_body(&bytes[offset + 64..offset + packet_len])
            {
                entries.push(entry);
            }
        }
        offset += packet_len;
    }
    entries
}

fn parse_par2_file_description_body(body: &[u8]) -> Option<Par2FileEntry> {
    if body.len() < 56 {
        return None;
    }
    let mut size_bytes = [0_u8; 8];
    size_bytes.copy_from_slice(&body[48..56]);
    let name_bytes = body[56..]
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .collect::<Vec<_>>();
    let relative_path = String::from_utf8_lossy(&name_bytes).to_string();
    if relative_path.is_empty() {
        return None;
    }
    Some(Par2FileEntry {
        relative_path,
        size_bytes: u64::from_le_bytes(size_bytes),
        file_id: hex_bytes(&body[0..16]),
        md5: hex_bytes(&body[16..32]),
        md5_16k: hex_bytes(&body[32..48]),
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    fn imports_hash_events_into_target_root_projection() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("remote.jsonl");
        let event = EventEnvelope {
            event_kind: EventKind::HashCompleted,
            job_id: Some("job_remote".to_string()),
            sequence: Some(1),
            created_at: now_rfc3339(),
            payload: EventPayload::HashCompleted {
                relative_path: "folder/a.txt".to_string(),
                basename: "a.txt".to_string(),
                parent_path: "folder".to_string(),
                size_bytes: 5,
                modified_at: Some("2026-07-07T00:00:00Z".to_string()),
                blake3: "b".repeat(64),
                sha256: "s".repeat(64),
            },
        };
        std::fs::write(&jsonl, format!("{}\n", event.to_json_line().unwrap())).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        let machine_id = db::ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
        let root_id = db::ensure_root(&conn, &machine_id, "/srv/archive").unwrap();
        let target = EventImportTarget {
            machine_id: machine_id.clone(),
            root_id: root_id.clone(),
            root_path: "/srv/archive".to_string(),
        };

        import_events_file_for_target(&conn, &jsonl, Some(&target)).unwrap();

        assert_eq!(db::table_count(&conn, "checksum_collections").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "checksum_entries").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "content_objects").unwrap(), 1);
        let status = db::target_status(&conn, &machine_id, "/srv/archive")
            .unwrap()
            .unwrap();
        assert_eq!(status.file_count, 1);
        assert_eq!(status.content_count, 1);
        assert_eq!(status.total_bytes, 5);
        assert_eq!(status.latest_job.unwrap().kind, "import_events");
        let observation = db::path_observation_for_root_path(&conn, &root_id, "folder/a.txt")
            .unwrap()
            .unwrap();
        assert_eq!(observation.size_bytes, 5);
        assert!(observation.content_id.is_some());
    }

    #[test]
    fn target_import_maps_root_file_hash_dot_to_basename() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("remote-file.jsonl");
        let event = EventEnvelope {
            event_kind: EventKind::HashCompleted,
            job_id: Some("job_remote".to_string()),
            sequence: Some(1),
            created_at: now_rfc3339(),
            payload: EventPayload::HashCompleted {
                relative_path: ".".to_string(),
                basename: "ignored".to_string(),
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
        let machine_id = db::ensure_machine_hint(&conn, "nas01", Some("ssh")).unwrap();
        let root_id = db::ensure_root(&conn, &machine_id, "/srv/archive/foo.png").unwrap();
        let target = EventImportTarget {
            machine_id,
            root_id: root_id.clone(),
            root_path: "/srv/archive/foo.png".to_string(),
        };

        import_events_file_for_target(&conn, &jsonl, Some(&target)).unwrap();

        let observation = db::path_observation_for_root_path(&conn, &root_id, "foo.png")
            .unwrap()
            .unwrap();
        assert_eq!(observation.size_bytes, 5);
        assert!(observation.content_id.is_some());
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

    #[test]
    fn parses_par2_file_description_packets() {
        let packet = test_par2_file_description("folder/movie.mkv", 12345);
        let entries = parse_par2_file_descriptions(&packet);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "folder/movie.mkv");
        assert_eq!(entries[0].size_bytes, 12345);
    }

    #[test]
    fn imports_par2_file_list_into_collection() {
        let dir = tempfile::tempdir().unwrap();
        let par2 = dir.path().join("sample.par2");
        std::fs::write(&par2, test_par2_file_description("folder/movie.mkv", 12345)).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        import_manifest_file(&conn, &par2).unwrap();
        assert_eq!(db::table_count(&conn, "checksum_collections").unwrap(), 1);
        assert_eq!(db::table_count(&conn, "checksum_entries").unwrap(), 1);
        let job = db::recent_jobs(&conn, 1).unwrap().remove(0);
        assert_eq!(job.kind, "import_manifest");
        assert_eq!(job.files_done, 1);
    }

    fn test_par2_file_description(name: &str, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend([0x11; 16]);
        body.extend([0x22; 16]);
        body.extend([0x33; 16]);
        body.extend(size.to_le_bytes());
        body.extend(name.as_bytes());
        while body.len() % 4 != 0 {
            body.push(0);
        }

        let packet_len = 64 + body.len();
        let mut packet = Vec::new();
        packet.extend(b"PAR2\0PKT");
        packet.extend((packet_len as u64).to_le_bytes());
        packet.extend([0x44; 16]);
        packet.extend([0x55; 16]);
        packet.extend(b"PAR 2.0\0FileDesc");
        packet.extend(body);
        packet
    }
}
