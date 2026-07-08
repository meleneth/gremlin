use super::*;
pub(super) fn run_command(command: &mut Command) -> anyhow::Result<()> {
    let output = command
        .output()
        .with_context(|| format!("running {:?}", command))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("{:?} failed: {}", command, stderr.trim());
}

pub(super) fn copy_with_hash(
    source_path: &Path,
    dest_path: &Path,
    on_progress: Option<&mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>>,
) -> anyhow::Result<CopyHashResult> {
    let mut source = std::fs::File::open(source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;
    let total = source
        .metadata()
        .with_context(|| format!("reading metadata for {}", source_path.display()))?
        .len();
    let mut dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest_path)
        .with_context(|| format!("creating destination {}", dest_path.display()))?;
    hash_stream_to_writer(
        &mut source,
        Some(&mut dest),
        source_path,
        total,
        on_progress,
    )
}

pub(super) fn hash_existing_file(path: &Path) -> anyhow::Result<CopyHashResult> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let total = file
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .len();
    hash_stream_to_writer(&mut file, None, path, total, None)
}

pub(super) fn sync_for_paranoid_readback(
    dest_path: &Path,
    parent: Option<PathBuf>,
) -> anyhow::Result<()> {
    let file = std::fs::File::open(dest_path)
        .with_context(|| format!("opening {}", dest_path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", dest_path.display()))?;
    if let Some(parent) = parent {
        let dir = std::fs::File::open(&parent)
            .with_context(|| format!("opening directory {}", parent.display()))?;
        dir.sync_all()
            .with_context(|| format!("syncing directory {}", parent.display()))?;
    }
    Ok(())
}

pub(super) fn source_modified_at(
    entry: &db::TransferPlanEntryRow,
) -> anyhow::Result<Option<String>> {
    let metadata: serde_json::Value = serde_json::from_str(&entry.metadata_json)
        .with_context(|| format!("parsing transfer metadata for {}", entry.relative_path))?;
    Ok(metadata
        .get("source_modified_at")
        .and_then(|value| value.as_str())
        .map(str::to_string))
}

pub(super) fn set_local_file_mtime(path: &Path, modified_at: Option<&str>) -> anyhow::Result<()> {
    let Some(modified_at) = modified_at else {
        return Ok(());
    };
    let time = file_time_from_rfc3339(modified_at)?;
    filetime::set_file_mtime(path, time)
        .with_context(|| format!("setting mtime on {}", path.display()))
}

pub(super) fn set_remote_file_mtime(
    host: &str,
    path: &str,
    modified_at: Option<&str>,
) -> anyhow::Result<()> {
    let Some(modified_at) = modified_at else {
        return Ok(());
    };
    run_command(Command::new("ssh").arg(host).arg(format!(
        "touch -d {} {}",
        shell_quote(modified_at),
        remote_shell_path(path)
    )))
    .with_context(|| format!("setting remote mtime on {host}:{path}"))
}

pub(super) fn file_time_from_rfc3339(value: &str) -> anyhow::Result<FileTime> {
    let dt = DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("invalid RFC3339 timestamp: {value}"))?;
    Ok(FileTime::from_unix_time(
        dt.timestamp(),
        dt.timestamp_subsec_nanos(),
    ))
}

pub(super) fn hash_stream_to_writer(
    reader: &mut std::fs::File,
    mut writer: Option<&mut std::fs::File>,
    path: &Path,
    total: u64,
    mut on_progress: Option<&mut dyn FnMut(u64, u64, f64) -> anyhow::Result<()>>,
) -> anyhow::Result<CopyHashResult> {
    use std::io::{Read, Write};

    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buf = [0_u8; 64 * 1024];
    let started_at = Instant::now();

    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        if let Some(writer) = writer.as_deref_mut() {
            writer
                .write_all(chunk)
                .with_context(|| format!("writing copy for {}", path.display()))?;
        }
        bytes += read as u64;
        if let Some(callback) = on_progress.as_deref_mut() {
            callback(bytes, total, rate_per_second(bytes, started_at))?;
        }
    }
    if let Some(writer) = writer {
        writer
            .sync_all()
            .with_context(|| format!("syncing copy for {}", path.display()))?;
    }

    Ok(CopyHashResult {
        bytes,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: bytes_to_hex(sha256_hasher.finalize()),
    })
}

pub(super) fn verify_copy_hash(
    conn: &Connection,
    entry: &db::TransferPlanEntryRow,
    actual: &CopyHashResult,
) -> anyhow::Result<()> {
    let Some(source_content_id) = entry.source_content_id.as_deref() else {
        return Ok(());
    };
    let Some(expected) = db::content_object_by_id(conn, source_content_id)? else {
        anyhow::bail!("planned source content object not found: {source_content_id}");
    };
    if expected.size_bytes != actual.bytes {
        anyhow::bail!(
            "source content size mismatch for {}: expected {}, copied {}",
            entry.relative_path,
            expected.size_bytes,
            actual.bytes
        );
    }
    if let Some(expected_blake3) = expected.blake3.as_deref() {
        if expected_blake3 != actual.blake3 {
            anyhow::bail!("BLAKE3 mismatch while copying {}", entry.relative_path);
        }
    }
    if let Some(expected_sha256) = expected.sha256.as_deref() {
        if expected_sha256 != actual.sha256 {
            anyhow::bail!("SHA-256 mismatch while copying {}", entry.relative_path);
        }
    }
    Ok(())
}

pub(super) fn bytes_to_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
