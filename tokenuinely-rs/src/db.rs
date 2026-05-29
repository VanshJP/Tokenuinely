use crate::config::{INDEX_DIRNAME, SCHEMA_VERSION};
use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileRecord {
    pub path: String,
    pub sha256: String,
    pub indexed_at: i64,
}

/// A single retrievable unit: either one top-level symbol or a whole-file fallback.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkRecord {
    pub id: i64,
    pub path: String,
    pub symbol: Option<String>,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub header: String,
    pub source: String,
    pub parent: Option<String>,
}

/// Result of a fused-ranking search. `source` may be truncated; `truncated` flags it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryHit {
    pub path: String,
    pub symbol: Option<String>,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub header: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub truncated: bool,
    pub score: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolRecord {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DepRecord {
    pub source_path: String,
    pub source_symbol: Option<String>,
    pub target_path: Option<String>,
    pub target_symbol: String,
    pub kind: String,
}

/// `(name, kind, line_start, line_end, signature, parent)` — flat row shape used
/// when bulk-inserting into the `symbols` table.
pub type SymbolRow = (String, String, usize, usize, String, Option<String>);

/// `(source_symbol, target_symbol, target_path, kind)` — flat row shape used when
/// bulk-inserting into the `deps` table.
pub type DepRow = (Option<String>, String, Option<String>, String);

/// Map from `(symbol, body_sha256)` to a previously-computed `(header, embedding)`.
/// Lets the indexer reuse work for chunks whose source body didn't change.
pub type ReuseMap = std::collections::HashMap<(Option<String>, String), (String, Vec<f32>)>;

/// One chunk to upsert. Constructed by the indexer from tree-sitter spans.
pub struct PendingChunk {
    pub symbol: Option<String>,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub source: String,
    pub parent: Option<String>,
    /// SHA-256 of `source`. Lets a reindex skip header+embed for unchanged symbols.
    pub body_sha256: String,
}

impl Db {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let dir = repo_root.join(INDEX_DIRNAME);
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("index.db"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Self { conn };
        db.create_tables()?;
        db.assert_schema_version()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                sha256 TEXT NOT NULL,
                indexed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                symbol TEXT,
                kind TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                header TEXT NOT NULL,
                source TEXT NOT NULL,
                parent TEXT,
                -- SHA-256 of the chunk body, for per-symbol reuse on reindex. The ''
                -- default is just a placeholder for any pre-hash row; real chunks always
                -- write a hash (distinct from the ''-as-incomplete sentinel on files.sha256).
                body_sha256 TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
            CREATE INDEX IF NOT EXISTS idx_chunks_symbol ON chunks(symbol);
            CREATE TABLE IF NOT EXISTS chunk_vecs (
                chunk_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS adrs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                signature TEXT NOT NULL DEFAULT '',
                parent TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_path ON symbols(path);
            CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
            CREATE TABLE IF NOT EXISTS deps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_path TEXT NOT NULL,
                source_symbol TEXT,
                target_path TEXT,
                target_symbol TEXT NOT NULL,
                kind TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_deps_source ON deps(source_path, source_symbol);
            CREATE INDEX IF NOT EXISTS idx_deps_target ON deps(target_symbol);
            "#,
        )?;
        Ok(())
    }

    /// Wipe everything if the stored schema version doesn't match. Cheap because the
    /// only safe migration story is "throw away and rebuild" — see CLAUDE.md.
    fn assert_schema_version(&self) -> Result<()> {
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .ok();
        match current.as_deref() {
            Some(v) if v == SCHEMA_VERSION => Ok(()),
            Some(_) => {
                self.conn.execute_batch(
                    "DELETE FROM files; DELETE FROM chunks; DELETE FROM chunk_vecs; \
                     DELETE FROM symbols; DELETE FROM deps; \
                     DELETE FROM meta WHERE key LIKE 'cache:%';",
                )?;
                self.set_meta("schema_version", SCHEMA_VERSION)?;
                Ok(())
            }
            None => {
                self.set_meta("schema_version", SCHEMA_VERSION)?;
                Ok(())
            }
        }
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn get_sha256(&self, path: &str) -> Result<Option<String>> {
        match self
            .conn
            .prepare_cached("SELECT sha256 FROM files WHERE path=?1")?
            .query_row(params![path], |r| r.get(0))
        {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Replace all chunks for a file atomically. `pending` is (chunk, header, embedding).
    pub fn upsert_file_chunks(
        &mut self,
        path: &str,
        sha256: &str,
        pending: &[(PendingChunk, String, Vec<f32>)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let now = now_unix();

        // 1) file row
        tx.execute(
            "INSERT OR REPLACE INTO files (path, sha256, indexed_at) VALUES (?1, ?2, ?3)",
            params![path, sha256, now],
        )?;

        // 2) drop any old chunks (and their vecs) for this file
        let old_ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM chunks WHERE path=?1")?;
            let mut ids = Vec::new();
            let mut rows = stmt.query(params![path])?;
            while let Some(row) = rows.next()? {
                ids.push(row.get::<_, i64>(0)?);
            }
            ids
        };
        for id in &old_ids {
            tx.execute("DELETE FROM chunk_vecs WHERE chunk_id=?1", params![id])?;
        }
        tx.execute("DELETE FROM chunks WHERE path=?1", params![path])?;

        // 3) insert new chunks + vecs
        for (chunk, header, embedding) in pending {
            tx.execute(
                "INSERT INTO chunks (path, symbol, kind, line_start, line_end, header, source, parent, body_sha256) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    path,
                    &chunk.symbol,
                    &chunk.kind,
                    chunk.line_start as i64,
                    chunk.line_end as i64,
                    header,
                    &chunk.source,
                    &chunk.parent,
                    &chunk.body_sha256,
                ],
            )?;
            let id = tx.last_insert_rowid();
            let blob = encode_embedding(embedding);
            tx.execute(
                "INSERT INTO chunk_vecs (chunk_id, embedding) VALUES (?1, ?2)",
                params![id, blob],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn delete_file(&self, path: &str) -> Result<()> {
        let ids: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT id FROM chunks WHERE path=?1")?;
            let mut acc = Vec::new();
            let mut rows = stmt.query(params![path])?;
            while let Some(row) = rows.next()? {
                acc.push(row.get::<_, i64>(0)?);
            }
            acc
        };
        for id in ids {
            self.conn
                .execute("DELETE FROM chunk_vecs WHERE chunk_id=?1", params![id])?;
        }
        self.conn
            .execute("DELETE FROM chunks WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM files WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM deps WHERE source_path=?1", params![path])?;
        Ok(())
    }

    pub fn replace_symbols(&self, path: &str, symbols: &[SymbolRow]) -> Result<()> {
        self.conn
            .execute("DELETE FROM symbols WHERE path=?1", params![path])?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO symbols (path, name, kind, line_start, line_end, signature, parent) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (name, kind, ls, le, sig, parent) in symbols {
            stmt.execute(params![
                path, name, kind, *ls as i64, *le as i64, sig, parent
            ])?;
        }
        Ok(())
    }

    pub fn replace_deps(&self, source_path: &str, deps: &[DepRow]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM deps WHERE source_path=?1",
            params![source_path],
        )?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO deps (source_path, source_symbol, target_path, target_symbol, kind) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (src_sym, tgt_sym, tgt_path, kind) in deps {
            stmt.execute(params![source_path, src_sym, tgt_path, tgt_sym, kind])?;
        }
        Ok(())
    }

    pub fn find_symbols(
        &self,
        name_pattern: &str,
        kind_filter: Option<&str>,
    ) -> Result<Vec<SymbolRecord>> {
        let sql = if kind_filter.is_some() {
            "SELECT path, name, kind, line_start, line_end, signature, parent \
             FROM symbols WHERE name LIKE ?1 AND kind = ?2 ORDER BY path, line_start"
        } else {
            "SELECT path, name, kind, line_start, line_end, signature, parent \
             FROM symbols WHERE name LIKE ?1 ORDER BY path, line_start"
        };
        let like_pat = format!("%{}%", name_pattern);
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(kind) = kind_filter {
            stmt.query_map(params![like_pat, kind], map_symbol_row)?
        } else {
            stmt.query_map(params![like_pat], map_symbol_row)?
        };
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_callers(&self, symbol_name: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE target_symbol = ?1 AND kind = 'calls'",
        )?;
        let rows = stmt.query_map(params![symbol_name], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_callees(&self, symbol_name: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE source_symbol = ?1 AND kind = 'calls'",
        )?;
        let rows = stmt.query_map(params![symbol_name], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_imports(&self, path: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE source_path = ?1 AND kind = 'imports'",
        )?;
        let rows = stmt.query_map(params![path], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    #[allow(dead_code)]
    pub fn get_importers(&self, path: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE target_path = ?1 AND kind = 'imports'",
        )?;
        let rows = stmt.query_map(params![path], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn list_all_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let paths: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
        Ok(paths)
    }

    pub fn stats(&self) -> Result<(usize, usize, Option<i64>)> {
        let files: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let chunks: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .unwrap_or(0);
        let last = self
            .conn
            .query_row("SELECT MAX(indexed_at) FROM files", [], |r| r.get(0))
            .ok();
        Ok((files, chunks, last))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            params![key, value],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self
            .conn
            .query_row("SELECT value FROM meta WHERE key=?1", params![key], |r| {
                r.get(0)
            }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Return a synthesized file-level header by joining the first few chunk headers.
    /// Used by detect_changes and hook_augment for display only.
    pub fn get_header(&self, path: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT symbol, header FROM chunks WHERE path=?1 ORDER BY line_start LIMIT 5",
        )?;
        let rows: Vec<(Option<String>, String)> = stmt
            .query_map(params![path], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        let joined: String = rows
            .iter()
            .map(|(sym, h)| match sym {
                Some(s) => format!("[{}] {}", s, h.lines().next().unwrap_or("")),
                None => h.lines().next().unwrap_or("").to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(joined))
    }

    /// Fetch the chunk row for a given symbol name (best-effort; first match).
    pub fn get_chunk_for_symbol(&self, symbol: &str) -> Result<Option<ChunkRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, symbol, kind, line_start, line_end, header, source, parent \
             FROM chunks WHERE symbol = ?1 LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![symbol], |r| {
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
            })
            .ok();
        Ok(row)
    }

    /// Return every chunk plus its embedding so the caller can score them in-memory.
    /// Cheap until the corpus grows past ~50k chunks; can be swapped for sqlite-vec later.
    pub fn all_chunks_with_vecs(&self) -> Result<Vec<(ChunkRecord, Vec<f32>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.path, c.symbol, c.kind, c.line_start, c.line_end, \
                    c.header, c.source, c.parent, cv.embedding \
             FROM chunks c JOIN chunk_vecs cv ON cv.chunk_id = c.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    ChunkRecord {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        symbol: r.get(2)?,
                        kind: r.get(3)?,
                        line_start: r.get::<_, i64>(4)? as usize,
                        line_end: r.get::<_, i64>(5)? as usize,
                        header: r.get(6)?,
                        source: r.get(7)?,
                        parent: r.get(8)?,
                    },
                    r.get::<_, Vec<u8>>(9)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .map(|(rec, blob)| (rec, decode_embedding(&blob)))
            .collect();
        Ok(rows)
    }

    /// Map `(symbol, body_sha256) → (header, embedding)` for a file's currently-stored
    /// chunks. The indexer uses this to carry an unchanged symbol's header+embedding
    /// forward across a reindex instead of paying for fresh Anthropic + Voyage calls.
    pub fn existing_chunks_for_reuse(&self, path: &str) -> Result<ReuseMap> {
        let mut stmt = self.conn.prepare(
            "SELECT c.symbol, c.body_sha256, c.header, cv.embedding \
             FROM chunks c JOIN chunk_vecs cv ON cv.chunk_id = c.id \
             WHERE c.path = ?1 AND c.body_sha256 != ''",
        )?;
        let mut out = std::collections::HashMap::new();
        let mut rows = stmt.query(params![path])?;
        while let Some(row) = rows.next()? {
            let symbol: Option<String> = row.get(0)?;
            let body_sha: String = row.get(1)?;
            let header: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            out.insert((symbol, body_sha), (header, decode_embedding(&blob)));
        }
        Ok(out)
    }

    pub fn delete_meta(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE key=?1", params![key])?;
        Ok(())
    }

    /// Drop every cached query result. Called on reindex (chunk IDs are reassigned)
    /// and by `tokenuinely cache clear`.
    pub fn clear_query_cache(&self) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM meta WHERE key LIKE 'cache:%'", [])?;
        Ok(n)
    }
}

fn map_symbol_row(r: &rusqlite::Row) -> rusqlite::Result<SymbolRecord> {
    Ok(SymbolRecord {
        path: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        line_start: r.get::<_, i64>(3)? as usize,
        line_end: r.get::<_, i64>(4)? as usize,
        signature: r.get(5)?,
        parent: r.get(6)?,
    })
}

fn map_dep_row(r: &rusqlite::Row) -> rusqlite::Result<DepRecord> {
    Ok(DepRecord {
        source_path: r.get(0)?,
        source_symbol: r.get(1)?,
        target_path: r.get(2)?,
        target_symbol: r.get(3)?,
        kind: r.get(4)?,
    })
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// On-disk embedding encoding: little-endian f32s. Tied to the `EMBED_DIM`/`voyage-3`
/// invariant — keep encode/decode as the single pair of functions that know the layout.
pub fn encode_embedding(emb: &[f32]) -> Vec<u8> {
    emb.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn decode_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Seconds since the Unix epoch, as the i64 used by `indexed_at` / ADR timestamps /
/// the query cache. Falls back to 0 if the clock is before the epoch.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
