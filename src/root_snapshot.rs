use std::path::{Path, PathBuf};

use anyhow::Context;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::util::{now_rfc3339, parent_path};

const FORMAT: &str = "gremlin.root_snapshot";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootSnapshot {
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub machine: SnapshotMachine,
    pub root: SnapshotRoot,
    pub files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMachine {
    pub id: String,
    pub label: String,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRoot {
    pub path: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub relative_path: String,
    pub basename: String,
    pub parent_path: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub status: String,
    pub blake3: Option<String>,
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crc32: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExportResult {
    pub path: PathBuf,
    pub file_count: usize,
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    pub root_id: String,
    pub root_path: String,
    pub file_count: usize,
}

pub fn looks_like_snapshot_file(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        && path.is_file()
}

pub fn read_snapshot(path: &Path) -> anyhow::Result<RootSnapshot> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading root snapshot {}", path.display()))?;
    let snapshot: RootSnapshot = serde_json::from_str(&text)
        .with_context(|| format!("parsing root snapshot {}", path.display()))?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn export_root(conn: &Connection, root: &db::RootRow) -> anyhow::Result<ExportResult> {
    let path = PathBuf::from(format!("{}.json", safe_file_stem(&short_root_name(root))));
    export_root_to_path(conn, root, path)
}

pub fn export_root_to_path(
    conn: &Connection,
    root: &db::RootRow,
    path: impl AsRef<Path>,
) -> anyhow::Result<ExportResult> {
    let snapshot = build_snapshot(conn, root)?;
    let path = path.as_ref().to_path_buf();
    let text = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(&path, format!("{text}\n"))
        .with_context(|| format!("writing root snapshot {}", path.display()))?;
    Ok(ExportResult {
        path,
        file_count: snapshot.files.len(),
    })
}

pub fn import_snapshot_file(conn: &Connection, path: &Path) -> anyhow::Result<ImportResult> {
    let snapshot = read_snapshot(path)?;
    import_snapshot(conn, &snapshot)
}

pub fn import_snapshot(conn: &Connection, snapshot: &RootSnapshot) -> anyhow::Result<ImportResult> {
    validate_snapshot(snapshot)?;
    let machine_id = db::ensure_machine_record(
        conn,
        &snapshot.machine.id,
        &snapshot.machine.label,
        snapshot.machine.platform.as_deref(),
    )?;
    let root_id = db::ensure_root(conn, &machine_id, &snapshot.root.path)?;
    if let Some(label) = snapshot.root.label.as_deref() {
        db::set_root_label(conn, &root_id, label)?;
    }
    db::clear_root_file_metadata(conn, &root_id)?;
    let collection_id = db::create_checksum_collection(
        conn,
        &format!("root snapshot {}", snapshot.root.path),
        "root_snapshot",
        None,
    )?;
    db::attach_checksum_collection_target(conn, &collection_id, &machine_id, &root_id)?;
    for file in &snapshot.files {
        let content_id = match (file.blake3.as_deref(), file.sha256.as_deref()) {
            (Some(blake3), Some(sha256)) => Some(match file.crc32.as_deref() {
                Some(crc32) => {
                    db::ensure_content_object_crc(conn, file.size_bytes, blake3, sha256, crc32)?
                }
                None => db::ensure_content_object(conn, file.size_bytes, blake3, sha256)?,
            }),
            (None, Some(sha256)) => Some(db::ensure_content_object_sha256(
                conn,
                file.size_bytes,
                sha256,
            )?),
            _ => None,
        };
        db::insert_path_observation(
            conn,
            db::PathObservationInput {
                machine_id: &machine_id,
                root_id: &root_id,
                relative_path: &file.relative_path,
                basename: &file.basename,
                parent_path: &file.parent_path,
                size_bytes: file.size_bytes,
                modified_at: file.modified_at.as_deref(),
                content_id: content_id.as_deref(),
            },
        )?;
        if file.blake3.is_some() || file.sha256.is_some() || file.crc32.is_some() {
            db::insert_checksum_entry(
                conn,
                db::ChecksumEntryInput {
                    collection_id: &collection_id,
                    relative_path: &file.relative_path,
                    basename: &file.basename,
                    size_bytes: file.size_bytes,
                    modified_at: file.modified_at.as_deref(),
                    blake3: file.blake3.as_deref(),
                    sha256: file.sha256.as_deref(),
                    crc32: file.crc32.as_deref(),
                    metadata_json: serde_json::json!({
                        "source": "root_snapshot",
                        "root_path": snapshot.root.path,
                    }),
                },
            )?;
        }
    }
    Ok(ImportResult {
        root_id,
        root_path: snapshot.root.path.clone(),
        file_count: snapshot.files.len(),
    })
}

fn build_snapshot(conn: &Connection, root: &db::RootRow) -> anyhow::Result<RootSnapshot> {
    let machine = db::machine_by_id(conn, &root.machine_id)?
        .ok_or_else(|| anyhow::anyhow!("root machine {} is missing", root.machine_id))?;
    let files = db::export_observations_for_root(conn, &root.id)?
        .into_iter()
        .map(|row| SnapshotFile {
            relative_path: row.relative_path,
            basename: row.basename,
            parent_path: row.parent_path,
            size_bytes: row.size_bytes,
            modified_at: row.modified_at,
            status: row.status,
            blake3: row.blake3,
            sha256: row.sha256,
            crc32: row.crc32,
        })
        .collect();
    Ok(RootSnapshot {
        format: FORMAT.to_string(),
        version: VERSION,
        exported_at: now_rfc3339(),
        machine: SnapshotMachine {
            id: machine.id,
            label: machine.label,
            platform: machine.platform,
        },
        root: SnapshotRoot {
            path: root.path.clone(),
            label: root.label.clone(),
        },
        files,
    })
}

fn validate_snapshot(snapshot: &RootSnapshot) -> anyhow::Result<()> {
    if snapshot.format != FORMAT {
        anyhow::bail!("not a Gremlin root snapshot");
    }
    if snapshot.version != VERSION {
        anyhow::bail!(
            "unsupported Gremlin root snapshot version {}",
            snapshot.version
        );
    }
    if snapshot.machine.id.trim().is_empty() {
        anyhow::bail!("root snapshot machine id is empty");
    }
    if snapshot.machine.label.trim().is_empty() {
        anyhow::bail!("root snapshot machine label is empty");
    }
    if snapshot.root.path.trim().is_empty() {
        anyhow::bail!("root snapshot path is empty");
    }
    for file in &snapshot.files {
        if file.relative_path.trim().is_empty() {
            anyhow::bail!("root snapshot contains an empty relative path");
        }
        if file.basename.trim().is_empty() {
            anyhow::bail!("root snapshot contains an empty basename");
        }
        let expected_parent = parent_path(&file.relative_path);
        if file.parent_path != expected_parent {
            anyhow::bail!(
                "root snapshot parent mismatch for {}: {} != {}",
                file.relative_path,
                file.parent_path,
                expected_parent
            );
        }
    }
    Ok(())
}

pub(crate) fn short_root_name(root: &db::RootRow) -> String {
    if let Some(label) = root
        .label
        .as_deref()
        .filter(|label| !label.trim().is_empty() && *label != root.path)
    {
        return label.to_string();
    }
    root.path
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .filter(|name| !name.is_empty() && *name != "~")
        .unwrap_or("root")
        .to_string()
}

pub(crate) fn safe_file_stem(name: &str) -> String {
    let stem = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if stem.is_empty() {
        "root".to_string()
    } else {
        stem
    }
}
