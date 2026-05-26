use crate::config::{INDEX_DIRNAME, INDEX_FILENAME};
use anyhow::{bail, Context, Result};
use std::path::Path;

pub fn export_index(repo_root: &Path, output: &Path) -> Result<()> {
    let db_path = repo_root.join(INDEX_DIRNAME).join(INDEX_FILENAME);
    if !db_path.exists() {
        bail!("No index found at {}", db_path.display());
    }

    let raw = std::fs::read(&db_path).context("Failed to read index database")?;
    let raw_len = raw.len();

    let compressed = zstd::encode_all(raw.as_slice(), 9).context("zstd compression failed")?;
    let compressed_len = compressed.len();

    std::fs::write(output, &compressed).context("Failed to write export artifact")?;

    let ratio = if compressed_len > 0 {
        raw_len as f64 / compressed_len as f64
    } else {
        0.0
    };
    eprintln!(
        "Exported: {} -> {} ({:.1}x compression)",
        raw_len, compressed_len, ratio
    );

    Ok(())
}

pub fn import_index(artifact: &Path, repo_root: &Path) -> Result<()> {
    let compressed = std::fs::read(artifact).context("Failed to read artifact")?;
    let raw = zstd::decode_all(compressed.as_slice()).context("zstd decompression failed")?;

    // Validate SQLite magic bytes
    if raw.len() < 16 || &raw[..16] != b"SQLite format 3\0" {
        bail!("Decompressed data is not a valid SQLite database");
    }

    let dir = repo_root.join(INDEX_DIRNAME);
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join(INDEX_FILENAME);
    std::fs::write(&db_path, &raw).context("Failed to write index database")?;

    eprintln!(
        "Imported {} bytes to {}",
        raw.len(),
        db_path.display()
    );

    Ok(())
}
