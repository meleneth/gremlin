use std::fs::File;
use std::io::{BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::Context;
use sha2::{Digest, Sha256};

use super::protocol::{FileStat, HashRequest, HelperEvent};
use super::{DEFAULT_CHUNK_SIZE_BYTES, MAX_CHUNK_SIZE_BYTES, MIN_CHUNK_SIZE_BYTES};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RequestedHashes {
    crc32: bool,
    sha256: bool,
    blake3: bool,
    chunks: bool,
}

pub fn hash_request(
    request: HashRequest,
    mut emit: impl FnMut(HelperEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if request.op != "hash" {
        emit_error(
            &mut emit,
            request.id,
            Some(request.path),
            "unsupported_op",
            "unsupported op",
        )?;
        return Ok(());
    }
    let hashes = match requested_hashes(&request.hashes) {
        Ok(hashes) => hashes,
        Err(message) => {
            emit_error(
                &mut emit,
                request.id,
                Some(request.path),
                "unsupported_algorithm",
                &message,
            )?;
            return Ok(());
        }
    };
    if hashes == RequestedHashes::default() {
        emit_error(
            &mut emit,
            request.id,
            Some(request.path),
            "invalid_request",
            "no hashes requested",
        )?;
        return Ok(());
    }
    let chunk_size = request.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE_BYTES);
    if hashes.chunks && !(MIN_CHUNK_SIZE_BYTES..=MAX_CHUNK_SIZE_BYTES).contains(&chunk_size) {
        emit_error(
            &mut emit,
            request.id,
            Some(request.path),
            "invalid_chunk_size",
            "chunk size outside supported range",
        )?;
        return Ok(());
    }
    match hash_path(&request, hashes, chunk_size, &mut emit) {
        Ok(()) => Ok(()),
        Err(err) => {
            emit_error(
                &mut emit,
                request.id,
                Some(request.path),
                "read_failure",
                &err.to_string(),
            )?;
            Ok(())
        }
    }
}

fn hash_path(
    request: &HashRequest,
    hashes: RequestedHashes,
    chunk_size: u64,
    emit: &mut impl FnMut(HelperEvent) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let path = Path::new(&request.path);
    let before_meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            emit_error(
                emit,
                request.id.clone(),
                Some(request.path.clone()),
                "not_found",
                &err.to_string(),
            )?;
            return Ok(());
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            emit_error(
                emit,
                request.id.clone(),
                Some(request.path.clone()),
                "permission_denied",
                &err.to_string(),
            )?;
            return Ok(());
        }
        Err(err) => return Err(err).with_context(|| format!("stat {}", request.path)),
    };
    if !before_meta.file_type().is_file() || before_meta.file_type().is_symlink() {
        emit_error(
            emit,
            request.id.clone(),
            Some(request.path.clone()),
            "not_regular_file",
            "path is not a regular file",
        )?;
        return Ok(());
    }
    let before = file_stat(&before_meta);
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            emit_error(
                emit,
                request.id.clone(),
                Some(request.path.clone()),
                "permission_denied",
                &err.to_string(),
            )?;
            return Ok(());
        }
        Err(err) => return Err(err).with_context(|| format!("open {}", request.path)),
    };
    let mut reader = BufReader::new(file);
    let mut crc32 = hashes.crc32.then(crate::crc32::Hasher::new);
    let mut sha256 = hashes.sha256.then(Sha256::new);
    let mut blake3 = hashes.blake3.then(blake3::Hasher::new);
    let mut chunk_md5 = hashes.chunks.then(md5::Context::new);
    let mut chunks = Vec::new();
    let mut chunk_bytes = 0_u64;
    let mut bytes_read = 0_u64;
    let mut next_progress = 64 * 1024 * 1024_u64;
    let mut buf = [0_u8; 128 * 1024];

    loop {
        let read = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", request.path))?;
        if read == 0 {
            break;
        }
        let bytes = &buf[..read];
        if let Some(hasher) = crc32.as_mut() {
            hasher.update(bytes);
        }
        if let Some(hasher) = sha256.as_mut() {
            hasher.update(bytes);
        }
        if let Some(hasher) = blake3.as_mut() {
            hasher.update(bytes);
        }
        if let Some(hasher) = chunk_md5.as_mut() {
            let mut consumed = 0_usize;
            while consumed < bytes.len() {
                let take = ((chunk_size - chunk_bytes) as usize).min(bytes.len() - consumed);
                hasher.consume(&bytes[consumed..consumed + take]);
                consumed += take;
                chunk_bytes += take as u64;
                if chunk_bytes == chunk_size {
                    let digest = std::mem::take(hasher).finalize();
                    chunks.push(format!("{digest:x}"));
                    *hasher = md5::Context::new();
                    chunk_bytes = 0;
                }
            }
        }
        bytes_read += read as u64;
        if bytes_read >= next_progress || bytes_read == before.size {
            emit(HelperEvent::Progress {
                id: request.id.clone(),
                path: request.path.clone(),
                bytes_read,
                size: before.size,
            })?;
            next_progress = bytes_read.saturating_add(64 * 1024 * 1024);
        }
    }
    if let Some(hasher) = chunk_md5 {
        if chunk_bytes > 0 {
            chunks.push(format!("{:x}", hasher.finalize()));
        }
    }
    let after_meta = std::fs::metadata(path).with_context(|| format!("stat {}", request.path))?;
    let after = file_stat(&after_meta);
    let stable = before == after;
    emit(HelperEvent::Result {
        id: request.id.clone(),
        path: request.path.clone(),
        before,
        after,
        stable,
        crc32: crc32.map(|hasher| format!("{:08x}", hasher.finalize())),
        sha256: sha256.map(|hasher| crate_sha256_hex(hasher.finalize())),
        blake3: blake3.map(|hasher| hasher.finalize().to_hex().to_string()),
        chunks: hashes.chunks.then_some(chunks),
    })?;
    Ok(())
}

fn file_stat(meta: &std::fs::Metadata) -> FileStat {
    FileStat {
        size: meta.len(),
        mtime_ns: (meta.mtime() as i128 * 1_000_000_000_i128) + meta.mtime_nsec() as i128,
    }
}

fn requested_hashes(values: &[String]) -> Result<RequestedHashes, String> {
    let mut requested = RequestedHashes::default();
    for value in values {
        match value.as_str() {
            "crc32" => requested.crc32 = true,
            "sha256" => requested.sha256 = true,
            "blake3" => requested.blake3 = true,
            "chunks" | "md5_chunks" | "chunk_md5" => requested.chunks = true,
            other => return Err(format!("unsupported hash algorithm {other}")),
        }
    }
    Ok(requested)
}

fn emit_error(
    emit: &mut impl FnMut(HelperEvent) -> anyhow::Result<()>,
    id: serde_json::Value,
    path: Option<String>,
    code: &str,
    message: &str,
) -> anyhow::Result<()> {
    emit(HelperEvent::Error {
        id,
        path,
        code: code.to_string(),
        message: message.to_string(),
    })
}

fn crate_sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn hash_events(path: &Path, hashes: &[&str], chunk_size: Option<u64>) -> Vec<HelperEvent> {
        let request = HashRequest {
            id: json!(42),
            op: "hash".to_string(),
            path: path.to_string_lossy().to_string(),
            hashes: hashes.iter().map(|hash| hash.to_string()).collect(),
            chunk_size,
        };
        let mut events = Vec::new();
        hash_request(request, |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        events
    }

    fn result(events: &[HelperEvent]) -> &HelperEvent {
        events
            .iter()
            .find(|event| matches!(event, HelperEvent::Result { .. }))
            .expect("result event")
    }

    fn error(events: &[HelperEvent]) -> &HelperEvent {
        events
            .iter()
            .find(|event| matches!(event, HelperEvent::Error { .. }))
            .expect("error event")
    }

    #[test]
    fn computes_crc32_sha256_blake3_and_chunks_from_one_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("name with spaces, quotes ' \" and unicode 雪.bin");
        let data = b"123456789";
        fs::write(&path, data).unwrap();

        let events = hash_events(
            &path,
            &["crc32", "sha256", "blake3", "chunks"],
            Some(MIN_CHUNK_SIZE_BYTES),
        );

        match result(&events) {
            HelperEvent::Result {
                stable,
                crc32,
                sha256,
                blake3,
                chunks,
                before,
                after,
                ..
            } => {
                assert!(*stable);
                assert_eq!(before, after);
                assert_eq!(crc32.as_deref(), Some("cbf43926"));
                assert_eq!(
                    sha256.as_deref(),
                    Some("15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225")
                );
                assert_eq!(
                    blake3.as_deref(),
                    Some("b7d65b48420d1033cb2595293263b6f72eabee20d55e699d0df1973b3c9deed1")
                );
                assert_eq!(
                    chunks.as_ref().unwrap(),
                    &vec![format!("{:x}", md5::compute(data))]
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn empty_file_has_no_chunk_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        fs::write(&path, []).unwrap();

        let events = hash_events(&path, &["crc32", "chunks"], Some(MIN_CHUNK_SIZE_BYTES));

        match result(&events) {
            HelperEvent::Result {
                stable,
                crc32,
                chunks,
                ..
            } => {
                assert!(*stable);
                assert_eq!(crc32.as_deref(), Some("00000000"));
                assert_eq!(chunks.as_ref().unwrap(), &Vec::<String>::new());
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn exact_boundary_and_final_partial_chunks_match_md5_convention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chunks.bin");
        let mut data = vec![b'a'; MIN_CHUNK_SIZE_BYTES as usize];
        data.extend_from_slice(b"tail");
        fs::write(&path, &data).unwrap();

        let events = hash_events(&path, &["chunks"], Some(MIN_CHUNK_SIZE_BYTES));

        match result(&events) {
            HelperEvent::Result { chunks, .. } => {
                assert_eq!(
                    chunks.as_ref().unwrap(),
                    &vec![
                        format!("{:x}", md5::compute(&data[..MIN_CHUNK_SIZE_BYTES as usize])),
                        format!("{:x}", md5::compute(b"tail")),
                    ]
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn reports_missing_files_without_aborting_session_logic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let events = hash_events(&path, &["sha256"], None);

        match error(&events) {
            HelperEvent::Error { code, path: p, .. } => {
                assert_eq!(code, "not_found");
                assert_eq!(p.as_deref(), Some(path.to_string_lossy().as_ref()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_algorithms_and_invalid_chunk_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, b"data").unwrap();

        match error(&hash_events(&path, &["crc64"], None)) {
            HelperEvent::Error { code, .. } => assert_eq!(code, "unsupported_algorithm"),
            other => panic!("unexpected event: {other:?}"),
        }
        match error(&hash_events(&path, &["chunks"], Some(1))) {
            HelperEvent::Error { code, .. } => assert_eq!(code, "invalid_chunk_size"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn marks_file_unstable_when_metadata_changes_during_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("changing.bin");
        fs::write(&path, vec![b'a'; 1024]).unwrap();
        let request = HashRequest {
            id: json!(7),
            op: "hash".to_string(),
            path: path.to_string_lossy().to_string(),
            hashes: vec!["sha256".to_string()],
            chunk_size: None,
        };
        let mut changed = false;
        let mut events = Vec::new();

        hash_request(request, |event| {
            if matches!(event, HelperEvent::Progress { .. }) && !changed {
                fs::write(&path, vec![b'b'; 2048]).unwrap();
                changed = true;
            }
            events.push(event);
            Ok(())
        })
        .unwrap();

        match result(&events) {
            HelperEvent::Result { stable, .. } => assert!(!stable),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
