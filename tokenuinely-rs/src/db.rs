use crate::config::INDEX_DIRNAME;
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
    pub header: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub path: String,
    pub header: String,
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

impl Db {
    pub fn open(repo_root: &Path) -> Result<Self> {
        let dir = repo_root.join(INDEX_DIRNAME);
        std::fs::create_dir_all(&dir)?;
        let conn = Connection::open(dir.join("index.db"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Self { conn };
        db.create_tables()?;
        Ok(db)
    }

    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY,
                sha256 TEXT NOT NULL,
                header TEXT NOT NULL,
                indexed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS file_vecs (
                path TEXT PRIMARY KEY,
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

    pub fn get_header(&self, path: &str) -> Result<Option<String>> {
        match self
            .conn
            .prepare_cached("SELECT header FROM files WHERE path=?1")?
            .query_row(params![path], |r| r.get(0))
        {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn upsert(
        &self,
        path: &str,
        sha256: &str,
        header: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.conn.execute(
            "INSERT OR REPLACE INTO files (path,sha256,header,indexed_at) VALUES(?1,?2,?3,?4)",
            params![path, sha256, header, now],
        )?;
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "INSERT OR REPLACE INTO file_vecs (path,embedding) VALUES(?1,?2)",
            params![path, blob],
        )?;
        Ok(())
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM file_vecs WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE path=?1", params![path])?;
        self.conn
            .execute("DELETE FROM deps WHERE source_path=?1", params![path])?;
        Ok(())
    }

    /// Insert extracted symbols for a file (clears old symbols for that path first).
    #[allow(dead_code)]
    pub fn insert_symbols(
        &self,
        path: &str,
        symbols: &[(String, String, usize, usize, String, Option<String>)],
    ) -> Result<()> {
        self.conn
            .execute("DELETE FROM symbols WHERE path=?1", params![path])?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO symbols (path, name, kind, line_start, line_end, signature, parent) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (name, kind, ls, le, sig, parent) in symbols {
            stmt.execute(params![path, name, kind, *ls as i64, *le as i64, sig, parent])?;
        }
        Ok(())
    }

    /// Insert extracted dependencies for a file (clears old deps for that path first).
    #[allow(dead_code)]
    pub fn insert_deps(
        &self,
        source_path: &str,
        deps: &[(Option<String>, String, Option<String>, String)],
    ) -> Result<()> {
        self.conn
            .execute("DELETE FROM deps WHERE source_path=?1", params![source_path])?;
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO deps (source_path, source_symbol, target_path, target_symbol, kind) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (src_sym, tgt_sym, tgt_path, kind) in deps {
            stmt.execute(params![source_path, src_sym, tgt_path, tgt_sym, kind])?;
        }
        Ok(())
    }

    /// Find symbols matching a name pattern and optional kind filter.
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

    /// Get callers of a symbol (what calls this?).
    pub fn get_callers(&self, symbol_name: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE target_symbol = ?1 AND kind = 'calls'",
        )?;
        let rows = stmt.query_map(params![symbol_name], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get callees of a symbol (what does this call?).
    pub fn get_callees(&self, symbol_name: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE source_symbol = ?1 AND kind = 'calls'",
        )?;
        let rows = stmt.query_map(params![symbol_name], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get imports for a file.
    pub fn get_imports(&self, path: &str) -> Result<Vec<DepRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_path, source_symbol, target_path, target_symbol, kind \
             FROM deps WHERE source_path = ?1 AND kind = 'imports'",
        )?;
        let rows = stmt.query_map(params![path], map_dep_row)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get files that import this file's symbols.
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

    pub fn stats(&self) -> Result<(usize, Option<i64>)> {
        let count = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let last = self
            .conn
            .query_row("SELECT MAX(indexed_at) FROM files", [], |r| r.get(0))
            .ok();
        Ok((count, last))
    }

    #[allow(dead_code)]
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            params![key, value],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM meta WHERE key=?1",
            params![key],
            |r| r.get(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn search(&self, query_vec: &[f32], k: usize) -> Result<Vec<QueryResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT fv.path, f.header, fv.embedding \
             FROM file_vecs fv JOIN files f ON fv.path=f.path",
        )?;
        let mut results: Vec<(String, String, f32)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .map(|(p, h, b)| {
                let emb: Vec<f32> = b
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let score = cosine_similarity(query_vec, &emb);
                (p, h, score)
            })
            .collect();
        results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        Ok(results
            .into_iter()
            .map(|(path, header, score)| QueryResult {
                path,
                header,
                score,
            })
            .collect())
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

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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
