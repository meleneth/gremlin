#[allow(dead_code)]
pub mod engine;
#[allow(dead_code)]
pub mod framing;
pub mod protocol;
#[allow(dead_code)]
pub mod session;
#[allow(dead_code)]
pub mod ssh;

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_CHUNK_SIZE_BYTES: u64 = 64 * 1024 * 1024;
pub const MIN_CHUNK_SIZE_BYTES: u64 = 1024 * 1024;
pub const MAX_CHUNK_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
