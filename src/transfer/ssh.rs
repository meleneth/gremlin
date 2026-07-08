use super::*;
pub(super) fn copy_ssh_to_local(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source: &TransferEndpoint,
    dest_path: &Path,
    on_progress: &mut TransferProgressCallback<'_>,
) -> anyhow::Result<CopyOutcome> {
    if std::fs::metadata(dest_path).is_ok() {
        anyhow::bail!("destination exists: {}", dest_path.display());
    }
    let parent = ensure_dest_parent(dest_path)?;
    let temp_path = transfer_temp_path(dest_path);
    let copy_result = copy_ssh_to_local_chunked(
        ctx.conn,
        ctx.job_id,
        ctx.plan_id,
        entry,
        source,
        &temp_path,
        on_progress,
    )
    .with_context(|| {
        format!(
            "copying {} to {}",
            source.display_path(),
            dest_path.display()
        )
    });
    if let Err(err) = copy_result {
        return Err(err);
    }
    let copy_hash = copy_result?;
    if copy_hash.bytes != entry.size_bytes {
        let _ = std::fs::remove_file(&temp_path);
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            copy_hash.bytes
        );
    }
    if let Err(err) = verify_copy_hash(ctx.conn, entry, &copy_hash) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    std::fs::rename(&temp_path, dest_path)
        .with_context(|| format!("installing copy at {}", dest_path.display()))?;
    let source_modified_at = source_modified_at(entry)?;
    set_local_file_mtime(dest_path, source_modified_at.as_deref())?;
    sync_for_paranoid_readback(dest_path, parent)?;
    let content_id = db::ensure_content_object(
        ctx.conn,
        copy_hash.bytes,
        &copy_hash.blake3,
        &copy_hash.sha256,
    )?;
    insert_dest_observation(
        ctx.conn,
        ctx.dest_root,
        entry,
        Some(&content_id),
        source_modified_at.as_deref(),
    )?;
    let source_display = source.display_path();
    let dest_display = dest_path.display().to_string();
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_display,
            dest_path: &dest_display,
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied over ssh"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(copy_hash.bytes))
}

pub(super) fn copy_local_to_ssh(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest: &TransferEndpoint,
    on_progress: &mut TransferProgressCallback<'_>,
) -> anyhow::Result<CopyOutcome> {
    let source_meta = std::fs::metadata(source_path)
        .with_context(|| format!("reading source {}", source_path.display()))?;
    if !source_meta.is_file() {
        anyhow::bail!("source is not a regular file: {}", source_path.display());
    }
    if source_meta.len() != entry.size_bytes {
        anyhow::bail!(
            "source size changed for {}: planned {} bytes, found {} bytes",
            entry.relative_path,
            entry.size_bytes,
            source_meta.len()
        );
    }
    let source_modified_at = source_meta.modified().ok().map(system_time_rfc3339);
    let source_hash = hash_existing_file(source_path)?;
    verify_copy_hash(ctx.conn, entry, &source_hash)?;

    let TransferEndpoint::Ssh { host, path } = dest else {
        anyhow::bail!("destination is not SSH");
    };
    let parent = remote_parent(path);
    if remote_path_exists(host, path)? {
        let checkpoint_count = db::transfer_copy_chunk_count_for_entry(
            ctx.conn,
            ctx.plan_id,
            &entry.relative_path,
            &entry.dest_relative_path,
        )?;
        if checkpoint_count == 0 {
            anyhow::bail!("remote destination exists: {host}:{path}");
        }
    }
    run_command(Command::new("ssh").arg(host).arg(format!(
        "test -f {} || mkdir -p {}",
        remote_shell_path(path),
        remote_shell_path(&parent)
    )))
    .with_context(|| format!("preparing remote destination {host}:{path}"))?;
    copy_local_to_ssh_chunked(
        ctx.conn,
        ctx.job_id,
        ctx.plan_id,
        entry,
        source_path,
        dest,
        on_progress,
    )
    .with_context(|| {
        format!(
            "copying {} to {}",
            source_path.display(),
            dest.display_path()
        )
    })?;
    set_remote_file_mtime(host, path, source_modified_at.as_deref())?;
    let content_id = db::ensure_content_object(
        ctx.conn,
        source_hash.bytes,
        &source_hash.blake3,
        &source_hash.sha256,
    )?;
    insert_dest_observation(
        ctx.conn,
        ctx.dest_root,
        entry,
        Some(&content_id),
        source_modified_at.as_deref(),
    )?;
    let source_display = source_path.display().to_string();
    let dest_display = dest.display_path();
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_display,
            dest_path: &dest_display,
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied over ssh"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(source_hash.bytes))
}

pub(super) fn copy_ssh_to_local_chunked(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    source: &TransferEndpoint,
    temp_path: &Path,
    on_progress: &mut TransferProgressCallback<'_>,
) -> anyhow::Result<CopyHashResult> {
    let TransferEndpoint::Ssh { host, path } = source else {
        anyhow::bail!("source is not SSH");
    };
    let mut dest = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(temp_path)
        .with_context(|| format!("creating {}", temp_path.display()))?;
    dest.set_len(entry.size_bytes)
        .with_context(|| format!("sizing {}", temp_path.display()))?;
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut copied = 0_u64;
    let started_at = Instant::now();
    let chunks = transfer_chunks(entry.size_bytes);
    let chunk_total = chunks.len() as u64;
    for chunk in chunks {
        let mut chunk_state = "fetching and verifying remote chunk";
        let bytes = if let Some(checkpoint) =
            matching_copy_chunk_checkpoint(conn, plan_id, entry, chunk)?
        {
            match local_chunk_bytes(temp_path, chunk) {
                Ok(bytes) if format!("{:x}", md5::compute(&bytes)) == checkpoint.digest => {
                    chunk_state = "reused local checkpoint after MD5 verify";
                    bytes
                }
                _ => {
                    chunk_state = "checkpoint miss; fetched and MD5 verified remote chunk";
                    fetch_verified_remote_chunk(host, path, chunk)?
                }
            }
        } else {
            fetch_verified_remote_chunk(host, path, chunk)?
        };
        if bytes.len() as u64 != chunk.size {
            anyhow::bail!("chunk {} size changed while copying {}", chunk.index, path);
        }
        let local_md5 = format!("{:x}", md5::compute(&bytes));
        dest.seek(SeekFrom::Start(chunk.offset))
            .with_context(|| format!("seeking {}", temp_path.display()))?;
        dest.write_all(&bytes)
            .with_context(|| format!("writing {}", temp_path.display()))?;
        persist_copy_chunk_checkpoint(conn, job_id, plan_id, entry, chunk, &local_md5)?;
        blake3_hasher.update(&bytes);
        sha256_hasher.update(&bytes);
        copied += bytes.len() as u64;
        on_progress(
            copied,
            entry.size_bytes,
            rate_per_second(copied, started_at),
            Some(&chunk_progress_message(chunk, chunk_total, chunk_state)),
        )?;
    }
    dest.sync_all()
        .with_context(|| format!("syncing {}", temp_path.display()))?;
    Ok(CopyHashResult {
        bytes: copied,
        blake3: blake3_hasher.finalize().to_hex().to_string(),
        sha256: bytes_to_hex(sha256_hasher.finalize()),
    })
}

pub(super) fn copy_local_to_ssh_chunked(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest: &TransferEndpoint,
    on_progress: &mut TransferProgressCallback<'_>,
) -> anyhow::Result<()> {
    let TransferEndpoint::Ssh { host, path } = dest else {
        anyhow::bail!("destination is not SSH");
    };
    if entry.size_bytes == 0 {
        run_command(
            Command::new("ssh")
                .arg(host)
                .arg(format!(": > {}", remote_shell_path(path))),
        )
        .with_context(|| format!("creating empty remote file {host}:{path}"))?;
        on_progress(0, 0, 0.0, Some("created empty remote file"))?;
        return Ok(());
    }
    let mut source = std::fs::File::open(source_path)
        .with_context(|| format!("opening source {}", source_path.display()))?;
    let mut copied = 0_u64;
    let started_at = Instant::now();
    let chunks = transfer_chunks(entry.size_bytes);
    let chunk_total = chunks.len() as u64;
    for chunk in chunks {
        let mut bytes = vec![0_u8; chunk.size as usize];
        source
            .seek(SeekFrom::Start(chunk.offset))
            .with_context(|| format!("seeking {}", source_path.display()))?;
        source
            .read_exact(&mut bytes)
            .with_context(|| format!("reading {}", source_path.display()))?;
        let local_md5 = format!("{:x}", md5::compute(&bytes));
        let checkpoint = matching_copy_chunk_checkpoint(conn, plan_id, entry, chunk)?;
        let mut chunk_state = "wrote and MD5 verified remote chunk";
        let remote_md5 = if checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.digest == local_md5)
        {
            let remote_md5 = remote_chunk_md5(host, path, chunk.index)?;
            if remote_md5 == local_md5 {
                chunk_state = "reused remote checkpoint after MD5 verify";
                remote_md5
            } else {
                chunk_state = "checkpoint miss; rewrote and MD5 verified remote chunk";
                write_remote_chunk(host, path, chunk.index, &bytes)?;
                remote_chunk_md5(host, path, chunk.index)?
            }
        } else {
            write_remote_chunk(host, path, chunk.index, &bytes)?;
            remote_chunk_md5(host, path, chunk.index)?
        };
        if remote_md5 != local_md5 {
            anyhow::bail!(
                "MD5 chunk mismatch after SSH write for {} chunk {}: local {}, remote {}",
                path,
                chunk.index,
                local_md5,
                remote_md5
            );
        }
        persist_copy_chunk_checkpoint(conn, job_id, plan_id, entry, chunk, &local_md5)?;
        copied += chunk.size;
        on_progress(
            copied,
            entry.size_bytes,
            rate_per_second(copied, started_at),
            Some(&chunk_progress_message(chunk, chunk_total, chunk_state)),
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TransferChunk {
    pub(super) index: u64,
    pub(super) offset: u64,
    pub(super) size: u64,
}

pub(super) fn transfer_chunks(total: u64) -> Vec<TransferChunk> {
    let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    let mut index = 0_u64;
    while offset < total {
        let size = (total - offset).min(chunk_size);
        chunks.push(TransferChunk {
            index,
            offset,
            size,
        });
        offset += size;
        index += 1;
    }
    chunks
}

pub(super) fn chunk_progress_message(
    chunk: TransferChunk,
    chunk_total: u64,
    state: &str,
) -> String {
    format!(
        "{}/{} {} offset={} size={}",
        chunk.index + 1,
        chunk_total,
        state,
        chunk.offset,
        chunk.size
    )
}

pub(super) fn matching_copy_chunk_checkpoint(
    conn: &Connection,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    chunk: TransferChunk,
) -> rusqlite::Result<Option<db::TransferCopyChunkRow>> {
    db::transfer_copy_chunk(
        conn,
        plan_id,
        &entry.relative_path,
        &entry.dest_relative_path,
        crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
        chunk.index,
        "md5",
    )
    .map(|checkpoint| {
        checkpoint.filter(|checkpoint| {
            checkpoint.offset_bytes == chunk.offset
                && checkpoint.size_bytes == chunk.size
                && checkpoint.chunk_size_bytes == crate::fswork::DEFAULT_CHUNK_SIZE_BYTES
                && checkpoint.chunk_index == chunk.index
                && checkpoint.algorithm == "md5"
        })
    })
}

pub(super) fn persist_copy_chunk_checkpoint(
    conn: &Connection,
    job_id: &str,
    plan_id: &str,
    entry: &db::TransferPlanEntryRow,
    chunk: TransferChunk,
    md5_digest: &str,
) -> rusqlite::Result<()> {
    db::upsert_transfer_copy_chunk(
        conn,
        db::TransferCopyChunkInput {
            plan_id,
            relative_path: &entry.relative_path,
            dest_relative_path: &entry.dest_relative_path,
            chunk_size_bytes: crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
            chunk_index: chunk.index,
            offset_bytes: chunk.offset,
            size_bytes: chunk.size,
            algorithm: "md5",
            digest: md5_digest,
            job_id,
        },
    )
}

pub(super) fn local_chunk_bytes(path: &Path, chunk: TransferChunk) -> anyhow::Result<Vec<u8>> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(chunk.offset))
        .with_context(|| format!("seeking {}", path.display()))?;
    let mut bytes = vec![0_u8; chunk.size as usize];
    file.read_exact(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(bytes)
}

pub(super) fn fetch_verified_remote_chunk(
    host: &str,
    path: &str,
    chunk: TransferChunk,
) -> anyhow::Result<Vec<u8>> {
    let remote_md5 = remote_chunk_md5(host, path, chunk.index)?;
    let bytes = remote_chunk_bytes(host, path, chunk.index)?;
    if bytes.len() as u64 != chunk.size {
        anyhow::bail!(
            "remote chunk size mismatch for {} chunk {}: expected {}, got {}",
            path,
            chunk.index,
            chunk.size,
            bytes.len()
        );
    }
    let local_md5 = format!("{:x}", md5::compute(&bytes));
    if local_md5 != remote_md5 {
        anyhow::bail!(
            "MD5 chunk mismatch for {} chunk {}: remote {}, copied {}",
            path,
            chunk.index,
            remote_md5,
            local_md5
        );
    }
    Ok(bytes)
}

pub(super) fn remote_chunk_md5(host: &str, path: &str, chunk_index: u64) -> anyhow::Result<String> {
    let output = remote_chunk_command(host, path, chunk_index, " | md5sum")?;
    let text = String::from_utf8(output).context("remote md5sum output was not UTF-8")?;
    text.split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("remote md5sum produced no digest for {host}:{path}"))
}

pub(super) fn remote_chunk_bytes(
    host: &str,
    path: &str,
    chunk_index: u64,
) -> anyhow::Result<Vec<u8>> {
    remote_chunk_command(host, path, chunk_index, "")
}

pub(super) fn remote_chunk_command(
    host: &str,
    path: &str,
    chunk_index: u64,
    suffix: &str,
) -> anyhow::Result<Vec<u8>> {
    let chunk_size = crate::fswork::DEFAULT_CHUNK_SIZE_BYTES;
    let command = format!(
        "dd if={} bs={} skip={} count=1 iflag=fullblock status=none{}",
        remote_shell_path(path),
        chunk_size,
        chunk_index,
        suffix
    );
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .output()
        .with_context(|| format!("reading remote chunk {chunk_index} from {host}:{path}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        anyhow::bail!(
            "remote chunk command failed for {host}:{path} chunk {}: {}",
            chunk_index,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

pub(super) fn write_remote_chunk(
    host: &str,
    path: &str,
    chunk_index: u64,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let command = format!(
        "dd of={} bs={} seek={} count=1 conv=notrunc status=none",
        remote_shell_path(path),
        crate::fswork::DEFAULT_CHUNK_SIZE_BYTES,
        chunk_index
    );
    let mut child = Command::new("ssh")
        .arg(host)
        .arg(command)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting remote chunk write to {host}:{path}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to open ssh stdin"))?;
        stdin
            .write_all(bytes)
            .with_context(|| format!("writing chunk {} to {host}:{path}", chunk_index))?;
    }
    let status = child
        .wait()
        .with_context(|| format!("waiting for remote chunk write to {host}:{path}"))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("remote chunk write failed for {host}:{path} chunk {chunk_index}");
    }
}
