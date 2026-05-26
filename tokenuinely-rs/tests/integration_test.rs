use std::io::Write;
use tempfile::TempDir;

// The binary crate's `Db` is not exported as a library, so these tests duplicate
// just enough of the v2 schema (chunks + chunk_vecs + files) to exercise the
// SQLite shape end-to-end. The real production schema lives in src/db.rs.

fn open_test_db() -> (TempDir, helpers::Db) {
    let tmp = TempDir::new().unwrap();
    let db = helpers::Db::open(tmp.path()).unwrap();
    (tmp, db)
}

mod helpers {
    use rusqlite::{params, Connection};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub struct Db {
        pub conn: Connection,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct ChunkHit {
        pub path: String,
        pub symbol: Option<String>,
        pub header: String,
        pub source: String,
        pub score: f32,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct Adr {
        pub id: i64,
        pub title: String,
        pub body: String,
        pub created_at: String,
    }

    impl Db {
        pub fn open(repo_root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
            let dir = repo_root.join(".tokenuinely/v2");
            std::fs::create_dir_all(&dir)?;
            let conn = Connection::open(dir.join("index.db"))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
            let db = Self { conn };
            db.create_tables()?;
            Ok(db)
        }

        fn create_tables(&self) -> Result<(), Box<dyn std::error::Error>> {
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
                    parent TEXT
                );
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
                "#,
            )?;
            Ok(())
        }

        pub fn get_sha256(&self, path: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
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

        /// Insert one chunk + embedding (file row created/updated).
        #[allow(clippy::too_many_arguments)]
        pub fn upsert_chunk(
            &self,
            path: &str,
            sha256: &str,
            symbol: Option<&str>,
            kind: &str,
            line_start: usize,
            line_end: usize,
            header: &str,
            source: &str,
            embedding: &[f32],
        ) -> Result<i64, Box<dyn std::error::Error>> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            self.conn.execute(
                "INSERT OR REPLACE INTO files (path, sha256, indexed_at) VALUES (?1, ?2, ?3)",
                params![path, sha256, now],
            )?;
            self.conn.execute(
                "INSERT INTO chunks (path, symbol, kind, line_start, line_end, header, source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    path,
                    symbol,
                    kind,
                    line_start as i64,
                    line_end as i64,
                    header,
                    source
                ],
            )?;
            let id = self.conn.last_insert_rowid();
            let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
            self.conn.execute(
                "INSERT INTO chunk_vecs (chunk_id, embedding) VALUES (?1, ?2)",
                params![id, blob],
            )?;
            Ok(id)
        }

        pub fn delete_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
            let mut stmt = self.conn.prepare("SELECT id FROM chunks WHERE path=?1")?;
            let mut ids = Vec::new();
            let mut rows = stmt.query(params![path])?;
            while let Some(row) = rows.next()? {
                ids.push(row.get::<_, i64>(0)?);
            }
            drop(rows);
            drop(stmt);
            for id in ids {
                self.conn
                    .execute("DELETE FROM chunk_vecs WHERE chunk_id=?1", params![id])?;
            }
            self.conn
                .execute("DELETE FROM chunks WHERE path=?1", params![path])?;
            self.conn
                .execute("DELETE FROM files WHERE path=?1", params![path])?;
            Ok(())
        }

        pub fn list_all_paths(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            let mut stmt = self.conn.prepare("SELECT path FROM files")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        }

        /// (files, chunks, last_indexed_at)
        pub fn stats(&self) -> Result<(usize, usize, Option<i64>), Box<dyn std::error::Error>> {
            let files: usize = self
                .conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            let chunks: usize = self
                .conn
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
            let last = self
                .conn
                .query_row("SELECT MAX(indexed_at) FROM files", [], |r| r.get(0))
                .ok();
            Ok((files, chunks, last))
        }

        pub fn set_meta(&self, key: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
                params![key, value],
            )?;
            Ok(())
        }

        pub fn get_meta(&self, key: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
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

        /// Cosine-only chunk search (the real query.rs adds FTS + exact-symbol boosts).
        pub fn cosine_search(
            &self,
            query_vec: &[f32],
            k: usize,
        ) -> Result<Vec<ChunkHit>, Box<dyn std::error::Error>> {
            let mut stmt = self.conn.prepare(
                "SELECT c.path, c.symbol, c.header, c.source, cv.embedding \
                 FROM chunks c JOIN chunk_vecs cv ON cv.chunk_id = c.id",
            )?;
            let mut hits: Vec<ChunkHit> = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .map(|(p, sym, h, src, blob)| {
                    let emb: Vec<f32> = blob
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    let score = cosine(query_vec, &emb);
                    ChunkHit {
                        path: p,
                        symbol: sym,
                        header: h,
                        source: src,
                        score,
                    }
                })
                .collect();
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(k);
            Ok(hits)
        }

        pub fn add_adr(&self, title: &str, body: &str) -> Result<i64, Box<dyn std::error::Error>> {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            self.conn.execute(
                "INSERT INTO adrs (title, body, created_at) VALUES (?1, ?2, ?3)",
                params![title, body, now],
            )?;
            Ok(self.conn.last_insert_rowid())
        }

        pub fn list_adrs(&self) -> Result<Vec<Adr>, Box<dyn std::error::Error>> {
            let mut stmt = self
                .conn
                .prepare("SELECT id, title, body, created_at FROM adrs ORDER BY id DESC")?;
            let rows = stmt.query_map([], |r| {
                Ok(Adr {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    body: r.get(2)?,
                    created_at: r.get::<_, i64>(3)?.to_string(),
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        }
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
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
}

// ── Chunk schema tests ──────────────────────────────────────────────────────

#[test]
fn upsert_chunk_creates_file_row_and_returns_hash() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert_chunk(
        "src/main.rs",
        "abc123",
        Some("main"),
        "function",
        1,
        10,
        "WHY: entrypoint",
        "fn main() {}",
        &emb,
    )
    .unwrap();
    assert_eq!(db.get_sha256("src/main.rs").unwrap(), Some("abc123".into()));
}

#[test]
fn multiple_chunks_per_file_are_stored() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert_chunk(
        "a.rs",
        "h1",
        Some("foo"),
        "function",
        1,
        5,
        "h",
        "fn foo() {}",
        &emb,
    )
    .unwrap();
    db.upsert_chunk(
        "a.rs",
        "h1",
        Some("bar"),
        "function",
        7,
        12,
        "h",
        "fn bar() {}",
        &emb,
    )
    .unwrap();
    let (files, chunks, _) = db.stats().unwrap();
    assert_eq!(files, 1);
    assert_eq!(chunks, 2);
}

#[test]
fn delete_file_cascades_to_chunks_and_vecs() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert_chunk(
        "x.rs",
        "h",
        Some("f"),
        "function",
        1,
        3,
        "h",
        "fn f(){}",
        &emb,
    )
    .unwrap();
    db.upsert_chunk(
        "x.rs",
        "h",
        Some("g"),
        "function",
        5,
        7,
        "h",
        "fn g(){}",
        &emb,
    )
    .unwrap();
    db.delete_file("x.rs").unwrap();
    let (files, chunks, _) = db.stats().unwrap();
    assert_eq!(files, 0);
    assert_eq!(chunks, 0);
}

#[test]
fn missing_sha256_returns_none() {
    let (_tmp, db) = open_test_db();
    assert_eq!(db.get_sha256("nonexistent.rs").unwrap(), None);
}

#[test]
fn list_all_paths_returns_indexed_files() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert_chunk("a.rs", "h", None, "file", 1, 1, "h", "x", &emb)
        .unwrap();
    db.upsert_chunk("b.rs", "h", None, "file", 1, 1, "h", "y", &emb)
        .unwrap();
    let mut paths = db.list_all_paths().unwrap();
    paths.sort();
    assert_eq!(paths, vec!["a.rs", "b.rs"]);
}

#[test]
fn stats_counts_chunks_separately_from_files() {
    let (_tmp, db) = open_test_db();
    let (f, c, _) = db.stats().unwrap();
    assert_eq!(f, 0);
    assert_eq!(c, 0);

    let emb = vec![1.0f32; 4];
    db.upsert_chunk(
        "x.rs",
        "h",
        Some("a"),
        "function",
        1,
        2,
        "h",
        "fn a(){}",
        &emb,
    )
    .unwrap();
    db.upsert_chunk(
        "x.rs",
        "h",
        Some("b"),
        "function",
        4,
        5,
        "h",
        "fn b(){}",
        &emb,
    )
    .unwrap();
    let (f, c, last) = db.stats().unwrap();
    assert_eq!(f, 1);
    assert_eq!(c, 2);
    assert!(last.is_some());
}

#[test]
fn cosine_search_ranks_by_similarity_and_returns_source() {
    let (_tmp, db) = open_test_db();

    // Chunk A: embedding in +x direction
    let emb_a = vec![1.0f32, 0.0, 0.0, 0.0];
    db.upsert_chunk(
        "auth.rs",
        "h",
        Some("login"),
        "function",
        10,
        20,
        "WHY: log a user in",
        "fn login() { /* ... */ }",
        &emb_a,
    )
    .unwrap();

    // Chunk B: opposite direction
    let emb_b = vec![-1.0f32, 0.0, 0.0, 0.0];
    db.upsert_chunk(
        "db.rs",
        "h",
        Some("connect"),
        "function",
        1,
        5,
        "WHY: connect to db",
        "fn connect() {}",
        &emb_b,
    )
    .unwrap();

    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let hits = db.cosine_search(&query, 2).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].path, "auth.rs");
    assert_eq!(hits[0].symbol.as_deref(), Some("login"));
    assert!(hits[0].source.contains("fn login"));
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn meta_round_trips_schema_version() {
    let (_tmp, db) = open_test_db();
    assert_eq!(db.get_meta("schema_version").unwrap(), None);
    db.set_meta("schema_version", "2").unwrap();
    assert_eq!(db.get_meta("schema_version").unwrap(), Some("2".into()));
    db.set_meta("schema_version", "3").unwrap();
    assert_eq!(db.get_meta("schema_version").unwrap(), Some("3".into()));
}

// ── ADR tests ────────────────────────────────────────────────────────────────

#[test]
fn adr_add_and_list_in_reverse_id_order() {
    let (_tmp, db) = open_test_db();
    let id1 = db.add_adr("Use SQLite", "Because it's simple").unwrap();
    let id2 = db
        .add_adr("Use Voyage embeddings", "Best quality/cost")
        .unwrap();
    assert!(id1 > 0);
    assert!(id2 > id1);
    let adrs = db.list_adrs().unwrap();
    assert_eq!(adrs.len(), 2);
    assert_eq!(adrs[0].title, "Use Voyage embeddings");
    assert_eq!(adrs[1].title, "Use SQLite");
}

// ── Export/Import round-trip ─────────────────────────────────────────────────

#[test]
fn export_import_roundtrip_preserves_chunk_data() {
    let tmp = TempDir::new().unwrap();
    let repo_root = tmp.path();

    {
        let db = helpers::Db::open(repo_root).unwrap();
        let emb = vec![0.5f32; 4];
        db.upsert_chunk(
            "test.rs",
            "hashval",
            Some("answer"),
            "function",
            1,
            3,
            "WHY: returns 42",
            "fn answer() -> i32 { 42 }",
            &emb,
        )
        .unwrap();
    }

    let db_path = repo_root.join(".tokenuinely/v2").join("index.db");
    assert!(db_path.exists());

    let artifact_path = tmp.path().join("export.zst");
    let raw = std::fs::read(&db_path).unwrap();
    let compressed = zstd::encode_all(raw.as_slice(), 9).unwrap();
    std::fs::write(&artifact_path, &compressed).unwrap();

    let tmp2 = TempDir::new().unwrap();
    let dest_dir = tmp2.path().join(".tokenuinely/v2");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_db = dest_dir.join("index.db");

    let comp_data = std::fs::read(&artifact_path).unwrap();
    let decompressed = zstd::decode_all(comp_data.as_slice()).unwrap();
    assert!(decompressed.len() >= 16);
    assert_eq!(&decompressed[..16], b"SQLite format 3\0");
    std::fs::write(&dest_db, &decompressed).unwrap();

    let db = helpers::Db::open(tmp2.path()).unwrap();
    assert_eq!(db.get_sha256("test.rs").unwrap(), Some("hashval".into()));
    let (f, c, _) = db.stats().unwrap();
    assert_eq!(f, 1);
    assert_eq!(c, 1);
}

// ── SHA-256 determinism ──────────────────────────────────────────────────────

#[test]
fn sha256_is_deterministic_and_matches_known_value() {
    use sha2::{Digest, Sha256};

    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("test.txt");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(b"hello world").unwrap();
    }
    let hash1 = {
        let bytes = std::fs::read(&file_path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    };
    assert_eq!(
        hash1,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}
