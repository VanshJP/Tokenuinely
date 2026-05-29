use super::fts;
use crate::config::{find_repo_root, Config, QUERY_CACHE_TTL_SECS, QUERY_SOURCE_CHAR_LIMIT};
use crate::db::{cosine_similarity, now_unix, ChunkRecord, Db, QueryHit};
use crate::hasher::sha256_str;
use crate::index::embedder::embed_query;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Search options. `include_source = false` returns just headers (cheapest mode).
#[derive(Debug, Clone)]
pub struct SearchOpts {
    pub k: usize,
    pub include_source: bool,
    pub max_chars: usize,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            k: 5,
            include_source: true,
            max_chars: QUERY_SOURCE_CHAR_LIMIT,
        }
    }
}

const WEIGHT_VEC: f32 = 0.55;
const WEIGHT_FTS: f32 = 0.30;
const WEIGHT_EXACT: f32 = 0.15;
const RECALL_POOL: usize = 50;

pub async fn search(
    repo_root: &Path,
    query_text: &str,
    opts: SearchOpts,
    cfg: &Config,
) -> Result<Vec<QueryHit>> {
    let db = Db::open(repo_root)?;

    // (0) Query cache: identical (query, k, mode) within the TTL replays the prior
    // chunk IDs and skips the Voyage embedding call. Rows are re-fetched by ID so
    // even a cache hit reflects the latest indexed source.
    let semantic = cfg.voyage_api_key.is_some();
    let cache_key = query_cache_key(query_text, opts.k, semantic);
    if let Some(hits) = read_query_cache(&db, &cache_key, &opts)? {
        tracing::info!(query = query_text, k = opts.k, "query cache hit");
        return Ok(hits);
    }

    // (1) FTS over chunk headers + symbol names (no API key needed).
    fts::create_fts_table(db.conn())?;
    fts::populate_fts(db.conn())?;
    let fts_pool = fts::fts_search_chunks(db.conn(), query_text, RECALL_POOL).unwrap_or_default();
    let fts_scores: HashMap<i64, f32> = normalize_fts(&fts_pool);

    // (2) Vector cosine — skip if no Voyage key (fallback to FTS-only ranking).
    let vec_scores: HashMap<i64, f32> = match cfg.voyage_api_key.as_deref() {
        Some(key) => {
            let qvec = embed_query(query_text, key).await?;
            let all = db.all_chunks_with_vecs()?;
            let mut scores: Vec<(i64, f32)> = all
                .into_iter()
                .map(|(c, e)| (c.id, cosine_similarity(&qvec, &e)))
                .collect();
            // Normalize to 0..1 by min-max so blending with BM25 is meaningful.
            normalize_scores(&mut scores);
            scores.into_iter().collect()
        }
        None => HashMap::new(),
    };

    // (3) Exact symbol-name boost: any chunk whose symbol matches a token in the query.
    let query_tokens: Vec<String> = tokenize(query_text);
    let exact_boost = exact_symbol_scores(&db, &query_tokens)?;

    // Union of all candidate chunk_ids.
    let mut candidate_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    candidate_ids.extend(vec_scores.keys());
    candidate_ids.extend(fts_scores.keys());
    candidate_ids.extend(exact_boost.keys());
    if candidate_ids.is_empty() {
        return Ok(vec![]);
    }

    // Fetch chunk rows for the candidates only (still small even at RECALL_POOL ≈ 50).
    let chunks = fetch_chunks_by_ids(&db, &candidate_ids)?;

    // Fuse scores.
    let mut scored: Vec<(ChunkRecord, f32)> = chunks
        .into_iter()
        .map(|c| {
            let v = vec_scores.get(&c.id).copied().unwrap_or(0.0);
            let f = fts_scores.get(&c.id).copied().unwrap_or(0.0);
            let e = exact_boost.get(&c.id).copied().unwrap_or(0.0);
            let score = WEIGHT_VEC * v + WEIGHT_FTS * f + WEIGHT_EXACT * e;
            (c, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(opts.k);

    // Cache the winning chunk IDs (+ fused scores) for replay within the TTL.
    let ordered: Vec<(i64, f32)> = scored.iter().map(|(c, s)| (c.id, *s)).collect();
    write_query_cache(&db, &cache_key, &ordered)?;

    Ok(build_hits(scored, &opts))
}

/// Turn ranked `(chunk, score)` pairs into `QueryHit`s, applying the source cap.
fn build_hits(scored: Vec<(ChunkRecord, f32)>, opts: &SearchOpts) -> Vec<QueryHit> {
    scored
        .into_iter()
        .map(|(c, score)| {
            let (source, truncated) = if opts.include_source {
                let (s, t) = cap_source(&c.source, opts.max_chars);
                (Some(s), t)
            } else {
                (None, false)
            };
            QueryHit {
                path: c.path,
                symbol: c.symbol,
                kind: c.kind,
                line_start: c.line_start,
                line_end: c.line_end,
                header: c.header,
                source,
                truncated,
                score,
            }
        })
        .collect()
}

pub async fn search_auto(
    query_text: &str,
    opts: SearchOpts,
    cfg: &Config,
) -> Result<Vec<QueryHit>> {
    let cwd = std::env::current_dir()?;
    let repo_root =
        find_repo_root(&cwd).or_else(|| std::env::var("TOKENUINELY_REPO").ok().map(Into::into));
    match repo_root {
        Some(root) => search(&root, query_text, opts, cfg).await,
        None => bail!("No tokenuinely index found. Run `tokenuinely index` first."),
    }
}

// ---- query cache ----

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    ts: i64,
    /// Ranked `(chunk_id, fused_score)` from the cached search.
    hits: Vec<(i64, f32)>,
}

/// Cache key folds in `k` and whether semantic (vec) ranking was active, so an
/// FTS-only result never masquerades as a semantic one.
fn query_cache_key(query: &str, k: usize, semantic: bool) -> String {
    format!(
        "cache:{}",
        sha256_str(&format!("{}\u{0}{}\u{0}{}", query, k, semantic))
    )
}

/// Return cached hits if a fresh entry exists. Expired or dangling entries are
/// dropped so the cache self-prunes at read time.
fn read_query_cache(db: &Db, key: &str, opts: &SearchOpts) -> Result<Option<Vec<QueryHit>>> {
    let raw = match db.get_meta(key)? {
        Some(r) => r,
        None => return Ok(None),
    };
    let entry: CacheEntry = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(_) => {
            db.delete_meta(key)?;
            return Ok(None);
        }
    };
    if now_unix() - entry.ts >= QUERY_CACHE_TTL_SECS {
        db.delete_meta(key)?;
        return Ok(None);
    }
    let ids: std::collections::HashSet<i64> = entry.hits.iter().map(|(id, _)| *id).collect();
    let mut by_id: HashMap<i64, ChunkRecord> = fetch_chunks_by_ids(db, &ids)?
        .into_iter()
        .map(|c| (c.id, c))
        .collect();
    // Preserve the cached ranking; skip any chunk that vanished since (e.g. reindex).
    // `remove` moves each record out of the throwaway map rather than cloning it.
    let ordered: Vec<(ChunkRecord, f32)> = entry
        .hits
        .iter()
        .filter_map(|(id, score)| by_id.remove(id).map(|c| (c, *score)))
        .collect();
    if ordered.is_empty() {
        db.delete_meta(key)?;
        return Ok(None);
    }
    Ok(Some(build_hits(ordered, opts)))
}

fn write_query_cache(db: &Db, key: &str, ordered: &[(i64, f32)]) -> Result<()> {
    if ordered.is_empty() {
        return Ok(());
    }
    let entry = CacheEntry {
        ts: now_unix(),
        hits: ordered.to_vec(),
    };
    db.set_meta(key, &serde_json::to_string(&entry)?)?;
    Ok(())
}

// ---- helpers ----

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_string())
        .collect()
}

fn normalize_scores(scores: &mut [(i64, f32)]) {
    if scores.is_empty() {
        return;
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for (_, s) in scores.iter() {
        if *s < lo {
            lo = *s;
        }
        if *s > hi {
            hi = *s;
        }
    }
    let span = (hi - lo).max(1e-6);
    for (_, s) in scores.iter_mut() {
        *s = (*s - lo) / span;
    }
}

/// FTS5 returns negative ranks (lower = better). Normalize to 0..1 with best→1.
fn normalize_fts(hits: &[fts::FtsChunkResult]) -> HashMap<i64, f32> {
    if hits.is_empty() {
        return HashMap::new();
    }
    // Smaller (more negative) rank = better. Invert sign so larger = better, then min-max.
    let pos: Vec<(i64, f32)> = hits.iter().map(|h| (h.chunk_id, -h.rank as f32)).collect();
    let mut arr = pos;
    normalize_scores(&mut arr);
    arr.into_iter().collect()
}

fn exact_symbol_scores(db: &Db, tokens: &[String]) -> Result<HashMap<i64, f32>> {
    if tokens.is_empty() {
        return Ok(HashMap::new());
    }
    let mut out: HashMap<i64, f32> = HashMap::new();
    let mut stmt = db
        .conn()
        .prepare("SELECT id FROM chunks WHERE symbol = ?1")?;
    for t in tokens {
        let rows = stmt.query_map([t], |r| r.get::<_, i64>(0))?;
        for id in rows.flatten() {
            // exact match is a hard signal — saturate at 1.0
            out.insert(id, 1.0);
        }
    }
    Ok(out)
}

fn fetch_chunks_by_ids(db: &Db, ids: &std::collections::HashSet<i64>) -> Result<Vec<ChunkRecord>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT id, path, symbol, kind, line_start, line_end, header, source, parent \
         FROM chunks WHERE id IN ({})",
        placeholders.join(",")
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let id_vec: Vec<i64> = ids.iter().copied().collect();
    let params: Vec<&dyn rusqlite::ToSql> =
        id_vec.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), |r| {
        Ok(ChunkRecord {
            id: r.get(0)?,
            path: r.get(1)?,
            symbol: r.get(2)?,
            kind: r.get(3)?,
            line_start: r.get::<_, i64>(4)? as usize,
            line_end: r.get::<_, i64>(5)? as usize,
            header: r.get(6)?,
            source: r.get(7)?,
            parent: r.get(8)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn cap_source(s: &str, max_chars: usize) -> (String, bool) {
    if s.len() <= max_chars {
        return (s.to_string(), false);
    }
    let mut cut = max_chars;
    while !s.is_char_boundary(cut) && cut > 0 {
        cut -= 1;
    }
    let mut out = s[..cut].to_string();
    out.push_str("\n…");
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use tempfile::TempDir;

    fn db_with_chunk() -> (TempDir, Db, i64) {
        let tmp = TempDir::new().unwrap();
        let db = Db::open(tmp.path()).unwrap();
        db.conn()
            .execute(
                "INSERT INTO chunks (path, symbol, kind, line_start, line_end, header, source, parent, body_sha256) \
                 VALUES ('src/x.rs','foo','function',1,5,'WHY: x','fn foo() {}',NULL,'abc')",
                [],
            )
            .unwrap();
        let id = db.conn().last_insert_rowid();
        (tmp, db, id)
    }

    #[test]
    fn cache_key_is_stable_and_discriminates() {
        let base = query_cache_key("hello", 5, true);
        assert_eq!(base, query_cache_key("hello", 5, true));
        assert_ne!(base, query_cache_key("hello", 5, false)); // semantic vs FTS
        assert_ne!(base, query_cache_key("hello", 6, true)); // different k
        assert_ne!(base, query_cache_key("world", 5, true)); // different query
        assert!(base.starts_with("cache:"));
    }

    #[test]
    fn cache_round_trips_ranked_hits() {
        let (_tmp, db, id) = db_with_chunk();
        let opts = SearchOpts::default();
        let key = query_cache_key("q", 5, true);
        assert!(read_query_cache(&db, &key, &opts).unwrap().is_none());

        write_query_cache(&db, &key, &[(id, 0.9)]).unwrap();
        let hits = read_query_cache(&db, &key, &opts)
            .unwrap()
            .expect("expected a cache hit");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.as_deref(), Some("foo"));
        assert!((hits[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn expired_entry_is_pruned_on_read() {
        let (_tmp, db, id) = db_with_chunk();
        let key = query_cache_key("q", 5, true);
        let stale = CacheEntry {
            ts: now_unix() - QUERY_CACHE_TTL_SECS - 1,
            hits: vec![(id, 0.5)],
        };
        db.set_meta(&key, &serde_json::to_string(&stale).unwrap())
            .unwrap();

        assert!(read_query_cache(&db, &key, &SearchOpts::default())
            .unwrap()
            .is_none());
        assert!(db.get_meta(&key).unwrap().is_none(), "stale entry not pruned");
    }

    #[test]
    fn dangling_chunk_ids_drop_entry() {
        let (_tmp, db, _id) = db_with_chunk();
        let key = query_cache_key("q", 5, true);
        write_query_cache(&db, &key, &[(999_999, 0.5)]).unwrap();
        assert!(read_query_cache(&db, &key, &SearchOpts::default())
            .unwrap()
            .is_none());
    }
}
