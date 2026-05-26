use super::deps::{extract_deps, DepInfo};
use super::embedder::embed_batch;
use super::header::{generate_chunk_header, generate_file_header};
use super::symbols::{detect_language, extract_symbols, SymbolInfo};
use super::walker::walk_repo;
use crate::config::{Config, EMBED_BATCH_MAX};
use crate::db::{Db, PendingChunk};
use crate::hasher::sha256_file;
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
    pub chunks: usize,
    pub deleted: usize,
    pub failed: Vec<(String, String)>,
}

/// One pending file: its hash + raw content + the chunks we sliced out of it.
struct FilePlan {
    rel_path: String,
    sha256: String,
    chunks: Vec<PendingChunk>,
    symbols: Vec<SymbolInfo>,
    deps: Vec<DepInfo>,
}

/// One pending chunk after header generation, before embedding.
struct ChunkWithHeader {
    file_idx: usize,  // index into FilePlan list
    chunk_idx: usize, // index within that file's `chunks`
    header: String,
}

pub async fn index_repo(repo_root: &Path, cfg: &Config) -> Result<IndexStats> {
    let mut db = Db::open(repo_root)?;
    let files = walk_repo(repo_root)?;
    let mut stats = IndexStats {
        scanned: files.len(),
        ..Default::default()
    };

    // Phase 0: hash + skip unchanged + delete orphans
    let mut plans: Vec<FilePlan> = Vec::new();
    let on_disk: std::collections::HashSet<String> = files
        .iter()
        .map(|f| {
            f.strip_prefix(repo_root)
                .unwrap_or(f)
                .to_string_lossy()
                .to_string()
        })
        .collect();
    for indexed in db.list_all_paths()? {
        if !on_disk.contains(&indexed) {
            db.delete_file(&indexed)?;
            stats.deleted += 1;
        }
    }

    for file in &files {
        let rel = file
            .strip_prefix(repo_root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        let hash = match sha256_file(file) {
            Ok(h) => h,
            Err(e) => {
                stats.failed.push((rel, format!("hash error: {}", e)));
                continue;
            }
        };
        if let Ok(Some(existing)) = db.get_sha256(&rel) {
            if existing == hash {
                stats.unchanged += 1;
                continue;
            }
        }
        // Build plan: read content, slice into chunks, also extract symbols+deps for graph tables.
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) => {
                stats.failed.push((rel, format!("read error: {}", e)));
                continue;
            }
        };
        let language = detect_language(&rel);
        let symbols = language
            .as_deref()
            .map(|l| extract_symbols(&content, l))
            .unwrap_or_default();
        let deps = language
            .as_deref()
            .map(|l| extract_deps(&content, l))
            .unwrap_or_default();
        let chunks = build_chunks(&content, &symbols);
        plans.push(FilePlan {
            rel_path: rel,
            sha256: hash,
            chunks,
            symbols,
            deps,
        });
    }

    if plans.is_empty() {
        return Ok(stats);
    }

    // Phase 1: per-chunk header generation, bounded concurrency.
    let total_chunks: usize = plans.iter().map(|p| p.chunks.len()).sum();
    let pb = ProgressBar::new(total_chunks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} generating chunk headers",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    let semaphore = Arc::new(Semaphore::new(cfg.header_concurrency));
    let api_key = cfg.require_anthropic_key()?.to_string();

    let mut handles = Vec::new();
    for (file_idx, plan) in plans.iter().enumerate() {
        for (chunk_idx, chunk) in plan.chunks.iter().enumerate() {
            let sem = semaphore.clone();
            let key = api_key.clone();
            let pb = pb.clone();
            let source = chunk.source.clone();
            let symbol_only = chunk.symbol.is_some();
            let context = match &chunk.symbol {
                Some(s) => format!("{} {} in {}", chunk.kind, s, plan.rel_path),
                None => plan.rel_path.clone(),
            };

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = if symbol_only {
                    generate_chunk_header(&source, &context, &key).await
                } else {
                    generate_file_header(&source, &key).await
                };
                pb.inc(1);
                match result {
                    Ok(h) => Ok(ChunkWithHeader {
                        file_idx,
                        chunk_idx,
                        header: h,
                    }),
                    Err(e) => Err((file_idx, chunk_idx, format!("header error: {}", e))),
                }
            });
            handles.push(handle);
        }
    }

    // Bucket headers back by file_idx so we know which file is fully ready.
    let mut headers_by_file: Vec<Vec<Option<String>>> =
        plans.iter().map(|p| vec![None; p.chunks.len()]).collect();
    for handle in handles {
        match handle.await {
            Ok(Ok(cwh)) => {
                headers_by_file[cwh.file_idx][cwh.chunk_idx] = Some(cwh.header);
            }
            Ok(Err((fi, _ci, err))) => {
                stats.failed.push((plans[fi].rel_path.clone(), err));
            }
            Err(e) => stats
                .failed
                .push(("unknown".into(), format!("join error: {}", e))),
        }
    }
    pb.finish_and_clear();

    // Phase 2: embed chunk headers in batches across files.
    // Flatten into (file_idx, chunk_idx, header) for batching.
    let mut flat: Vec<(usize, usize, String)> = Vec::new();
    for (fi, headers) in headers_by_file.iter().enumerate() {
        for (ci, h) in headers.iter().enumerate() {
            if let Some(h) = h {
                flat.push((fi, ci, h.clone()));
            }
        }
    }

    let embed_pb = ProgressBar::new(flat.len() as u64);
    embed_pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} embedding chunks",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    let voyage_key = cfg.require_voyage_key()?.to_string();
    let mut embeddings_by_file: Vec<Vec<Option<Vec<f32>>>> =
        plans.iter().map(|p| vec![None; p.chunks.len()]).collect();

    for chunk in flat.chunks(EMBED_BATCH_MAX) {
        let texts: Vec<String> = chunk.iter().map(|(_, _, h)| h.clone()).collect();
        match embed_batch(&texts, &voyage_key, "document").await {
            Ok(embs) => {
                for ((fi, ci, _), emb) in chunk.iter().zip(embs) {
                    embeddings_by_file[*fi][*ci] = Some(emb);
                    embed_pb.inc(1);
                }
            }
            Err(e) => {
                for (fi, _, _) in chunk {
                    stats
                        .failed
                        .push((plans[*fi].rel_path.clone(), format!("embed error: {}", e)));
                    embed_pb.inc(1);
                }
            }
        }
    }
    embed_pb.finish_and_clear();

    // Phase 3: upsert per file atomically + write symbols/deps for graph queries.
    for (fi, plan) in plans.iter().enumerate() {
        let mut pending: Vec<(PendingChunk, String, Vec<f32>)> = Vec::new();
        for (ci, chunk) in plan.chunks.iter().enumerate() {
            let header = headers_by_file[fi][ci].clone();
            let emb = embeddings_by_file[fi][ci].clone();
            if let (Some(h), Some(e)) = (header, emb) {
                pending.push((
                    PendingChunk {
                        symbol: chunk.symbol.clone(),
                        kind: chunk.kind.clone(),
                        line_start: chunk.line_start,
                        line_end: chunk.line_end,
                        source: chunk.source.clone(),
                        parent: chunk.parent.clone(),
                    },
                    h,
                    e,
                ));
            }
        }
        if pending.is_empty() {
            continue;
        }
        if let Err(e) = db.upsert_file_chunks(&plan.rel_path, &plan.sha256, &pending) {
            stats
                .failed
                .push((plan.rel_path.clone(), format!("upsert error: {}", e)));
            continue;
        }
        stats.indexed += 1;
        stats.chunks += pending.len();

        // Mirror symbol + dep rows for the graph tools.
        let sym_rows: Vec<_> = plan
            .symbols
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.kind.clone(),
                    s.line_start,
                    s.line_end,
                    s.signature.clone(),
                    s.parent.clone(),
                )
            })
            .collect();
        let _ = db.replace_symbols(&plan.rel_path, &sym_rows);
        let dep_rows: Vec<_> = plan
            .deps
            .iter()
            .map(|d| {
                (
                    d.source_symbol.clone(),
                    d.target_symbol.clone(),
                    d.target_path.clone(),
                    d.kind.clone(),
                )
            })
            .collect();
        let _ = db.replace_deps(&plan.rel_path, &dep_rows);
    }

    Ok(stats)
}

/// Slice `content` into PendingChunks. One per top-level symbol, with whole-file fallback.
fn build_chunks(content: &str, symbols: &[SymbolInfo]) -> Vec<PendingChunk> {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Filter to symbols we'd actually want to return as standalone hits.
    let mut top_level: Vec<&SymbolInfo> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind.as_str(),
                "function"
                    | "method"
                    | "struct"
                    | "class"
                    | "trait"
                    | "interface"
                    | "enum"
                    | "type"
            )
        })
        .collect();
    top_level.sort_by_key(|s| s.line_start);

    if top_level.is_empty() || total_lines == 0 {
        return vec![PendingChunk {
            symbol: None,
            kind: "file".to_string(),
            line_start: 1,
            line_end: total_lines.max(1),
            source: content.to_string(),
            parent: None,
        }];
    }

    let mut chunks = Vec::with_capacity(top_level.len());
    for sym in &top_level {
        let start = sym
            .line_start
            .saturating_sub(1)
            .min(total_lines.saturating_sub(1));
        let end = sym.line_end.min(total_lines);
        if end <= start {
            continue;
        }
        let source = lines[start..end].join("\n");
        chunks.push(PendingChunk {
            symbol: Some(sym.name.clone()),
            kind: sym.kind.clone(),
            line_start: sym.line_start,
            line_end: sym.line_end,
            source,
            parent: sym.parent.clone(),
        });
    }

    // If nothing usable came out, fall back to a single file chunk.
    if chunks.is_empty() {
        return vec![PendingChunk {
            symbol: None,
            kind: "file".to_string(),
            line_start: 1,
            line_end: total_lines,
            source: content.to_string(),
            parent: None,
        }];
    }
    chunks
}
