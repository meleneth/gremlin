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
    JobCompleted,
    JobFailed,
    DirectorySeen,
    FileSeen,
    HashStarted,
    HashCompleted,
    HashFailed,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JobCreated => "job_created",
            Self::JobStarted => "job_started",
            Self::JobCompleted => "job_completed",
            Self::JobFailed => "job_failed",
            Self::DirectorySeen => "directory_seen",
            Self::FileSeen => "file_seen",
            Self::HashStarted => "hash_started",
            Self::HashCompleted => "hash_completed",
            Self::HashFailed => "hash_failed",
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
    },
    HashFailed {
        relative_path: Option<String>,
        path: String,
        error: String,
    },
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
            },
        };

        let json = event.to_json_line().unwrap();
        let decoded: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }
}
