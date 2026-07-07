use thiserror::Error;

#[derive(Debug, Error)]
pub enum GremlinError {
    #[error("database does not exist at {0}; run `gremlin init --db {0}` first")]
    MissingDatabase(String),
    #[error("path has no file name: {0}")]
    MissingFileName(String),
    #[error("path is not under root: {path} (root: {root})")]
    PathOutsideRoot { path: String, root: String },
    #[error("invalid JSONL event at line {line}: {source}")]
    JsonLine {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}
