use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

/// A chunk-level FTS hit. `chunk_id` joins back to `chunks`/`chunk_vecs`.
#[derive(Debug, Clone, Serialize)]
pub struct FtsChunkResult {
    pub chunk_id: i64,
    pub path: String,
    pub symbol: Option<String>,
    pub header: String,
    pub rank: f64,
}

/// Legacy file-shaped FTS result, used by code that still wants per-file hits.
#[derive(Debug, Clone, Serialize)]
pub struct FtsResult {
    pub path: String,
    pub header: String,
    pub rank: f64,
}

/// Create the FTS5 virtual table indexing chunk_id (UNINDEXED), path, header, and symbol.
pub fn create_fts_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts_index USING fts5(\
            chunk_id UNINDEXED, path, header, symbol);",
    )
    .context("Failed to create FTS5 virtual table")?;
    Ok(())
}

/// Wipe + repopulate the FTS table from the `chunks` table.
pub fn populate_fts(conn: &Connection) -> Result<usize> {
    conn.execute("DELETE FROM fts_index", [])
        .context("Failed to clear FTS table")?;

    let mut select = conn.prepare("SELECT id, path, symbol, header FROM chunks")?;
    let rows: Vec<(i64, String, Option<String>, String)> = select
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut insert = conn.prepare(
        "INSERT INTO fts_index (chunk_id, path, header, symbol) VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut count = 0usize;
    for (id, path, symbol, header) in rows {
        insert.execute(params![id, path, header, symbol.unwrap_or_default()])?;
        count += 1;
    }
    Ok(count)
}

/// FTS5 MATCH on the chunk index. Treats `query` as plain text (FTS5 'phrase'-ish).
pub fn fts_search_chunks(conn: &Connection, query: &str, k: usize) -> Result<Vec<FtsChunkResult>> {
    let safe_query = sanitize_fts_query(query);
    if safe_query.is_empty() {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT chunk_id, path, symbol, header, rank FROM fts_index \
         WHERE fts_index MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;
    let results = stmt
        .query_map(params![safe_query, k as i64], |row| {
            let sym: String = row.get(2).unwrap_or_default();
            Ok(FtsChunkResult {
                chunk_id: row.get(0)?,
                path: row.get(1)?,
                symbol: if sym.is_empty() { None } else { Some(sym) },
                header: row.get(3)?,
                rank: row.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(results)
}

/// Convenience wrapper that returns one row per file (best chunk wins).
pub fn fts_search(conn: &Connection, query: &str, k: usize) -> Result<Vec<FtsResult>> {
    let chunk_hits = fts_search_chunks(conn, query, k * 4)?;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for h in chunk_hits {
        if seen.insert(h.path.clone()) {
            out.push(FtsResult {
                path: h.path,
                header: h.header,
                rank: h.rank,
            });
            if out.len() >= k {
                break;
            }
        }
    }
    Ok(out)
}

/// Strip FTS5 syntax characters that would otherwise raise a syntax error.
/// Users pass natural-language queries; we want them tokenised, not parsed.
fn sanitize_fts_query(q: &str) -> String {
    let cleaned: String = q
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}
