use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// SHA-256 of an in-memory string. Used for per-chunk body hashing (P0 hash-diff)
/// and for deriving query-cache keys.
pub fn sha256_str(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}
