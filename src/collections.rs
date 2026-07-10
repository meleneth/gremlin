use std::collections::BTreeSet;

use anyhow::Context;
use rusqlite::Connection;
use serde::Serialize;

use crate::db;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionVerifyKind {
    Ok,
    SizeOnly,
    Missing,
    SizeMismatch,
    HashMismatch,
    Unverified,
}

impl CollectionVerifyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::SizeOnly => "size_only",
            Self::Missing => "missing",
            Self::SizeMismatch => "size_mismatch",
            Self::HashMismatch => "hash_mismatch",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionVerifyFinding {
    pub kind: CollectionVerifyKind,
    pub relative_path: String,
    pub expected_size_bytes: u64,
    pub actual_size_bytes: Option<u64>,
    pub expected_blake3: Option<String>,
    pub actual_blake3: Option<String>,
    pub expected_sha256: Option<String>,
    pub actual_sha256: Option<String>,
    pub expected_crc32: Option<String>,
    pub actual_crc32: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionVerifyExtra {
    pub relative_path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CollectionVerifySummary {
    pub collection_id: String,
    pub collection_name: String,
    pub root_id: String,
    pub root_path: String,
    pub entries: usize,
    pub ok: usize,
    pub size_only: usize,
    pub missing: usize,
    pub size_mismatch: usize,
    pub hash_mismatch: usize,
    pub unverified: usize,
    pub extras: usize,
    pub findings: Vec<CollectionVerifyFinding>,
    pub extra_files: Vec<CollectionVerifyExtra>,
}

pub fn verify_collection_against_root(
    conn: &Connection,
    collection_id: &str,
    root: &db::RootRow,
) -> anyhow::Result<CollectionVerifySummary> {
    let collection = db::checksum_collection_by_id(conn, collection_id)?
        .ok_or_else(|| anyhow::anyhow!("checksum collection not found: {collection_id}"))?;
    let entries = db::checksum_entries_for_collection(conn, collection_id)
        .with_context(|| format!("loading checksum collection {collection_id}"))?;
    let observations = db::checksum_observations_for_root(conn, &root.id)
        .with_context(|| format!("loading root observations for {}", root.path))?;
    let observed_paths = observations.keys().cloned().collect::<BTreeSet<_>>();
    let expected_paths = entries
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect::<BTreeSet<_>>();

    let mut summary = CollectionVerifySummary {
        collection_id: collection.id,
        collection_name: collection.name,
        root_id: root.id.clone(),
        root_path: root.path.clone(),
        entries: entries.len(),
        ok: 0,
        size_only: 0,
        missing: 0,
        size_mismatch: 0,
        hash_mismatch: 0,
        unverified: 0,
        extras: 0,
        findings: Vec::new(),
        extra_files: Vec::new(),
    };

    for entry in entries {
        let observation = observations.get(&entry.relative_path);
        let finding = match observation {
            None => CollectionVerifyFinding {
                kind: CollectionVerifyKind::Missing,
                relative_path: entry.relative_path,
                expected_size_bytes: entry.size_bytes,
                actual_size_bytes: None,
                expected_blake3: entry.blake3,
                actual_blake3: None,
                expected_sha256: entry.sha256,
                actual_sha256: None,
                expected_crc32: entry.crc32,
                actual_crc32: None,
            },
            Some(observation) => classify_entry(&entry, observation),
        };
        increment_summary(&mut summary, &finding.kind);
        summary.findings.push(finding);
    }

    for extra_path in observed_paths.difference(&expected_paths) {
        if let Some(observation) = observations.get(extra_path) {
            summary.extra_files.push(CollectionVerifyExtra {
                relative_path: observation.relative_path.clone(),
                size_bytes: observation.size_bytes,
            });
        }
    }
    summary.extras = summary.extra_files.len();
    Ok(summary)
}

fn classify_entry(
    entry: &db::ChecksumEntryRow,
    observation: &db::ChecksumObservationRow,
) -> CollectionVerifyFinding {
    let kind = if entry.size_bytes != 0 && entry.size_bytes != observation.size_bytes {
        CollectionVerifyKind::SizeMismatch
    } else if let Some(expected) = entry.blake3.as_deref() {
        match observation.blake3.as_deref() {
            Some(actual) if actual == expected => CollectionVerifyKind::Ok,
            Some(_) => CollectionVerifyKind::HashMismatch,
            None => CollectionVerifyKind::Unverified,
        }
    } else if let Some(expected) = entry.sha256.as_deref() {
        match observation.sha256.as_deref() {
            Some(actual) if actual == expected => CollectionVerifyKind::Ok,
            Some(_) => CollectionVerifyKind::HashMismatch,
            None => CollectionVerifyKind::Unverified,
        }
    } else if let Some(expected) = entry.crc32.as_deref() {
        match observation.crc32.as_deref() {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => CollectionVerifyKind::Ok,
            Some(_) => CollectionVerifyKind::HashMismatch,
            None => CollectionVerifyKind::Unverified,
        }
    } else {
        CollectionVerifyKind::SizeOnly
    };

    CollectionVerifyFinding {
        kind,
        relative_path: entry.relative_path.clone(),
        expected_size_bytes: entry.size_bytes,
        actual_size_bytes: Some(observation.size_bytes),
        expected_blake3: entry.blake3.clone(),
        actual_blake3: observation.blake3.clone(),
        expected_sha256: entry.sha256.clone(),
        actual_sha256: observation.sha256.clone(),
        expected_crc32: entry.crc32.clone(),
        actual_crc32: observation.crc32.clone(),
    }
}

fn increment_summary(summary: &mut CollectionVerifySummary, kind: &CollectionVerifyKind) {
    match kind {
        CollectionVerifyKind::Ok => summary.ok += 1,
        CollectionVerifyKind::SizeOnly => summary.size_only += 1,
        CollectionVerifyKind::Missing => summary.missing += 1,
        CollectionVerifyKind::SizeMismatch => summary.size_mismatch += 1,
        CollectionVerifyKind::HashMismatch => summary.hash_mismatch += 1,
        CollectionVerifyKind::Unverified => summary.unverified += 1,
    }
}
