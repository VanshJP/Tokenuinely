use crate::config::{Config, EMBED_BATCH_MAX};
use crate::db::Db;
use crate::embedder::embed_batch;
use crate::hasher::sha256_file;
use crate::header::generate_header;
use crate::walker::walk_repo;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug, Default)]
pub struct IndexStats {
    pub scanned: usize,
    pub unchanged: usize,
    pub indexed: usize,
    pub deleted: usize,
    pub failed: Vec<(String, String)>,
}

struct PendingFile {
    rel_path: String,
    sha256: String,
    header: String,
}

pub async fn index_repo(repo_root: &Path, cfg: &Config) -> Result<IndexStats> {
    let db = Db::open(repo_root)?;
    let files = walk_repo(repo_root)?;
    let mut stats = IndexStats {
        scanned: files.len(),
        ..Default::default()
    };

    // Determine which files need (re-)indexing
    let mut pending_paths: Vec<(String, String)> = Vec::new(); // (rel_path, sha256)
    for file in &files {
        let rel = file
            .strip_prefix(repo_root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        match sha256_file(file) {
            Ok(hash) => {
                if let Ok(Some(existing)) = db.get_sha256(&rel) {
                    if existing == hash {
                        stats.unchanged += 1;
                        continue;
                    }
                }
                pending_paths.push((rel, hash));
            }
            Err(e) => {
                stats
                    .failed
                    .push((rel, format!("hash error: {}", e)));
            }
        }
    }

    // Delete entries for files no longer on disk
    let on_disk: std::collections::HashSet<String> = files
        .iter()
        .map(|f| {
            f.strip_prefix(repo_root)
                .unwrap_or(f)
                .to_string_lossy()
                .to_string()
        })
        .collect();
    for indexed_path in db.list_all_paths()? {
        if !on_disk.contains(&indexed_path) {
            db.delete(&indexed_path)?;
            stats.deleted += 1;
        }
    }

    if pending_paths.is_empty() {
        return Ok(stats);
    }

    // Phase 1: Generate headers in parallel
    let pb = ProgressBar::new(pending_paths.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} generating headers",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    let semaphore = Arc::new(Semaphore::new(cfg.header_concurrency));
    let api_key = cfg.require_anthropic_key()?.to_string();

    let mut handles = Vec::new();
    for (rel_path, sha256) in pending_paths {
        let sem = semaphore.clone();
        let key = api_key.clone();
        let root = repo_root.to_path_buf();
        let pb = pb.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let full_path = root.join(&rel_path);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(e) => {
                    pb.inc(1);
                    return Err((rel_path, format!("read error: {}", e)));
                }
            };
            match generate_header(&content, &key).await {
                Ok(header) => {
                    pb.inc(1);
                    Ok(PendingFile {
                        rel_path,
                        sha256,
                        header,
                    })
                }
                Err(e) => {
                    pb.inc(1);
                    Err((rel_path, format!("header error: {}", e)))
                }
            }
        });
        handles.push(handle);
    }

    let mut pending_files: Vec<PendingFile> = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(pf)) => pending_files.push(pf),
            Ok(Err((path, err))) => stats.failed.push((path, err)),
            Err(e) => stats.failed.push(("unknown".into(), format!("join error: {}", e))),
        }
    }
    pb.finish_and_clear();

    // Phase 2: Embed headers in batches
    let embed_pb = ProgressBar::new(pending_files.len() as u64);
    embed_pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} embedding headers",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    let voyage_key = cfg.require_voyage_key()?.to_string();
    for chunk in pending_files.chunks(EMBED_BATCH_MAX) {
        let texts: Vec<String> = chunk.iter().map(|pf| pf.header.clone()).collect();
        match embed_batch(&texts, &voyage_key, "document").await {
            Ok(embeddings) => {
                for (pf, emb) in chunk.iter().zip(embeddings.iter()) {
                    if let Err(e) = db.upsert(&pf.rel_path, &pf.sha256, &pf.header, emb) {
                        stats
                            .failed
                            .push((pf.rel_path.clone(), format!("upsert error: {}", e)));
                    } else {
                        stats.indexed += 1;
                    }
                    embed_pb.inc(1);
                }
            }
            Err(e) => {
                for pf in chunk {
                    stats
                        .failed
                        .push((pf.rel_path.clone(), format!("embed error: {}", e)));
                    embed_pb.inc(1);
                }
            }
        }
    }
    embed_pb.finish_and_clear();

    Ok(stats)
}
