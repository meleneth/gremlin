use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventEnvelope {
    pub event_kind: EventKind,
    pub job_id: Option<String>,
    pub sequence: Option<i64>,
    pub created_at: String,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    JobCreated,
    JobStarted,
    JobProgress,
    JobCancelRequested,
    JobCanceled,
    JobCompleted,
    JobFailed,
    DirectorySeen,
    FileSeen,
    HashStarted,
    HashCompleted,
    HashFailed,
    VerifyStarted,
    VerifyFinding,
    VerifyCompleted,
    TransferStarted,
    TransferCompleted,
    TransferSkipped,
    TransferFailed,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JobCreated => "job_created",
            Self::JobStarted => "job_started",
            Self::JobProgress => "job_progress",
            Self::JobCancelRequested => "job_cancel_requested",
            Self::JobCanceled => "job_canceled",
            Self::JobCompleted => "job_completed",
            Self::JobFailed => "job_failed",
            Self::DirectorySeen => "directory_seen",
            Self::FileSeen => "file_seen",
            Self::HashStarted => "hash_started",
            Self::HashCompleted => "hash_completed",
            Self::HashFailed => "hash_failed",
            Self::VerifyStarted => "verify_started",
            Self::VerifyFinding => "verify_finding",
            Self::VerifyCompleted => "verify_completed",
            Self::TransferStarted => "transfer_started",
            Self::TransferCompleted => "transfer_completed",
            Self::TransferSkipped => "transfer_skipped",
            Self::TransferFailed => "transfer_failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    Job {
        kind: String,
        path: Option<String>,
        message: Option<String>,
        files_seen: Option<u64>,
        errors: Option<u64>,
    },
    JobProgress {
        phase: String,
        current_path: Option<String>,
        files_total: Option<u64>,
        files_seen: u64,
        files_done: u64,
        files_skipped: u64,
        errors: u64,
        bytes_done: Option<u64>,
        bytes_total: Option<u64>,
        file_bytes_done: Option<u64>,
        file_bytes_total: Option<u64>,
        bytes_per_second: Option<f64>,
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chunk_confidence: Option<TransferChunkConfidence>,
    },
    DirectorySeen {
        relative_path: String,
    },
    FileSeen {
        relative_path: String,
        basename: String,
        parent_path: String,
        size_bytes: u64,
        modified_at: Option<String>,
    },
    HashStarted {
        relative_path: String,
    },
    HashCompleted {
        relative_path: String,
        basename: String,
        parent_path: String,
        size_bytes: u64,
        modified_at: Option<String>,
        blake3: String,
        sha256: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crc32: Option<String>,
    },
    HashFailed {
        relative_path: Option<String>,
        path: String,
        error: String,
    },
    VerifyFinding {
        result: String,
        relative_path: String,
        basename: String,
        parent_path: String,
        size_bytes: u64,
        modified_at: Option<String>,
        expected_blake3: Option<String>,
        expected_sha256: Option<String>,
        actual_blake3: Option<String>,
        actual_sha256: Option<String>,
        error: Option<String>,
    },
    TransferFile {
        relative_path: String,
        source_path: String,
        dest_path: String,
        size_bytes: u64,
        action: String,
        message: Option<String>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TransferChunkConfidence {
    pub chunks_total: u64,
    pub chunks_done: u64,
    pub chunks_reused: u64,
    pub chunks_copied: u64,
    pub chunks_verified: u64,
    pub checkpoint_misses: u64,
}

impl EventEnvelope {
    pub fn to_json_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_json() {
        let event = EventEnvelope {
            event_kind: EventKind::HashCompleted,
            job_id: Some("job_1".to_string()),
            sequence: Some(2),
            created_at: "2026-07-07T00:00:00Z".to_string(),
            payload: EventPayload::HashCompleted {
                relative_path: "a.txt".to_string(),
                basename: "a.txt".to_string(),
                parent_path: ".".to_string(),
                size_bytes: 3,
                modified_at: None,
                blake3: "b3".to_string(),
                sha256: "s256".to_string(),
                crc32: Some("DEADBEEF".to_string()),
            },
        };

        let json = event.to_json_line().unwrap();
        let decoded: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }
}
