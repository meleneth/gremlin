use super::*;
pub(super) fn copy_local_to_local(
    ctx: CopyContext<'_>,
    entry: &db::TransferPlanEntryRow,
    source_path: &Path,
    dest_path: &Path,
    paranoid: bool,
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

    if let Ok(dest_meta) = std::fs::metadata(dest_path) {
        if dest_meta.is_file() && dest_meta.len() == entry.size_bytes {
            let verified_content_id = if paranoid {
                sync_for_paranoid_readback(dest_path, None)?;
                let readback_hash = hash_existing_file(dest_path)?;
                verify_copy_hash(ctx.conn, entry, &readback_hash)?;
                Some(db::ensure_content_object(
                    ctx.conn,
                    readback_hash.bytes,
                    &readback_hash.blake3,
                    &readback_hash.sha256,
                )?)
            } else {
                None
            };
            let dest_modified_at = dest_meta.modified().ok().map(system_time_rfc3339);
            insert_dest_observation(
                ctx.conn,
                ctx.dest_root,
                entry,
                verified_content_id.as_deref(),
                dest_modified_at.as_deref(),
            )?;
            persist_transfer_file_event(
                ctx.conn,
                ctx.job_id,
                TransferFileEventInput {
                    event_kind: EventKind::TransferSkipped,
                    relative_path: &entry.relative_path,
                    source_path: &source_path.display().to_string(),
                    dest_path: &dest_path.display().to_string(),
                    size_bytes: entry.size_bytes,
                    action: "skip",
                    message: Some("destination already has planned size"),
                    error: None,
                },
            )?;
            return Ok(CopyOutcome::Skipped);
        }
        anyhow::bail!("destination exists and differs: {}", dest_path.display());
    }

    let parent_created = ensure_dest_parent(dest_path)?;
    let mut progress = |done: u64, total: u64, rate: f64| on_progress(done, total, rate, None);
    let copy_hash = copy_with_hash(source_path, dest_path, Some(&mut progress))?;
    if copy_hash.bytes != entry.size_bytes {
        anyhow::bail!(
            "copied byte count mismatch for {}: planned {}, copied {}",
            entry.relative_path,
            entry.size_bytes,
            copy_hash.bytes
        );
    }
    verify_copy_hash(ctx.conn, entry, &copy_hash)?;
    set_local_file_mtime(dest_path, source_modified_at.as_deref())?;
    if paranoid {
        sync_for_paranoid_readback(dest_path, parent_created)?;
        let readback_hash = hash_existing_file(dest_path)?;
        if readback_hash.bytes != copy_hash.bytes
            || readback_hash.blake3 != copy_hash.blake3
            || readback_hash.sha256 != copy_hash.sha256
        {
            anyhow::bail!(
                "paranoid readback hash mismatch for {}",
                dest_path.display()
            );
        }
    }

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
    persist_transfer_file_event(
        ctx.conn,
        ctx.job_id,
        TransferFileEventInput {
            event_kind: EventKind::TransferCompleted,
            relative_path: &entry.relative_path,
            source_path: &source_path.display().to_string(),
            dest_path: &dest_path.display().to_string(),
            size_bytes: entry.size_bytes,
            action: "copy",
            message: Some("copied"),
            error: None,
        },
    )?;
    Ok(CopyOutcome::Copied(copy_hash.bytes))
}
