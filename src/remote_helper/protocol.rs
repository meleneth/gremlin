use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashRequest {
    pub id: serde_json::Value,
    pub op: String,
    pub path: String,
    #[serde(default)]
    pub hashes: Vec<String>,
    pub chunk_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime_ns: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HelperEvent {
    Hello {
        version: u32,
        capabilities: Vec<String>,
    },
    Progress {
        id: serde_json::Value,
        path: String,
        bytes_read: u64,
        size: u64,
    },
    Result {
        id: serde_json::Value,
        path: String,
        before: FileStat,
        after: FileStat,
        stable: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        crc32: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        blake3: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chunks: Option<Vec<String>>,
    },
    Error {
        id: serde_json::Value,
        path: Option<String>,
        code: String,
        message: String,
    },
}

pub fn capabilities() -> Vec<String> {
    ["crc32", "sha256", "blake3", "chunks"]
        .into_iter()
        .map(str::to_string)
        .collect()
}
