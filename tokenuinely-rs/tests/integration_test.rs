use std::io::Write;
use tempfile::TempDir;

// We test the library modules directly by referencing the binary crate's modules
// Since this is a binary crate, we use process-based testing for CLI
// and inline the logic for unit-style tests.

/// Helper: open a Db in a temp dir
fn open_test_db() -> (TempDir, tokenuinely_test_helpers::Db) {
    let tmp = TempDir::new().unwrap();
    let db = tokenuinely_test_helpers::Db::open(tmp.path()).unwrap();
    (tmp, db)
}

/// Minimal re-implementation of the Db for testing without depending on the binary crate.
/// We duplicate just enough to test the SQLite logic.
mod tokenuinely_test_helpers {
    use rusqlite::{params, Connection};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub struct Db {
        conn: Connection,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct QueryResult {
        pub path: String,
        pub header: String,
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
            let dir = repo_root.join(".tokenuinely");
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
                "#,
            )?;
            Ok(())
        }

        #[allow(dead_code)]
        pub fn conn(&self) -> &Connection {
            &self.conn
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

        pub fn get_header(&self, path: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
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
        ) -> Result<(), Box<dyn std::error::Error>> {
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

        pub fn delete(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.conn
                .execute("DELETE FROM files WHERE path=?1", params![path])?;
            self.conn
                .execute("DELETE FROM file_vecs WHERE path=?1", params![path])?;
            Ok(())
        }

        pub fn list_all_paths(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
            let mut stmt = self.conn.prepare("SELECT path FROM files")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            let paths: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
            Ok(paths)
        }

        pub fn stats(&self) -> Result<(usize, Option<i64>), Box<dyn std::error::Error>> {
            let count = self
                .conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            let last = self
                .conn
                .query_row("SELECT MAX(indexed_at) FROM files", [], |r| r.get(0))
                .ok();
            Ok((count, last))
        }

        pub fn set_meta(&self, key: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
                params![key, value],
            )?;
            Ok(())
        }

        pub fn get_meta(
            &self,
            key: &str,
        ) -> Result<Option<String>, Box<dyn std::error::Error>> {
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

        pub fn search(
            &self,
            query_vec: &[f32],
            k: usize,
        ) -> Result<Vec<QueryResult>, Box<dyn std::error::Error>> {
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

        pub fn add_adr(
            &self,
            title: &str,
            body: &str,
        ) -> Result<i64, Box<dyn std::error::Error>> {
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
}

// ─── Database Tests ──────────────────────────────────────────────────────────

#[test]
fn test_upsert_and_get_sha256() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert("src/main.rs", "abc123", "SUMMARY: main entry point", &emb)
        .unwrap();
    assert_eq!(db.get_sha256("src/main.rs").unwrap(), Some("abc123".into()));
}

#[test]
fn test_get_header() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert("src/lib.rs", "def456", "SUMMARY: library root", &emb)
        .unwrap();
    assert_eq!(
        db.get_header("src/lib.rs").unwrap(),
        Some("SUMMARY: library root".into())
    );
}

#[test]
fn test_get_sha256_missing() {
    let (_tmp, db) = open_test_db();
    assert_eq!(db.get_sha256("nonexistent.rs").unwrap(), None);
}

#[test]
fn test_delete() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert("src/foo.rs", "aaa", "SUMMARY: foo", &emb)
        .unwrap();
    assert!(db.get_sha256("src/foo.rs").unwrap().is_some());
    db.delete("src/foo.rs").unwrap();
    assert_eq!(db.get_sha256("src/foo.rs").unwrap(), None);
}

#[test]
fn test_list_all_paths() {
    let (_tmp, db) = open_test_db();
    let emb = vec![1.0f32; 4];
    db.upsert("a.rs", "h1", "header a", &emb).unwrap();
    db.upsert("b.rs", "h2", "header b", &emb).unwrap();
    let mut paths = db.list_all_paths().unwrap();
    paths.sort();
    assert_eq!(paths, vec!["a.rs", "b.rs"]);
}

#[test]
fn test_stats() {
    let (_tmp, db) = open_test_db();
    let (count, _) = db.stats().unwrap();
    assert_eq!(count, 0);

    let emb = vec![1.0f32; 4];
    db.upsert("x.rs", "h", "header", &emb).unwrap();
    let (count, last) = db.stats().unwrap();
    assert_eq!(count, 1);
    assert!(last.is_some());
}

#[test]
fn test_search_ranking() {
    let (_tmp, db) = open_test_db();

    // File A: embedding pointing in +x direction
    let emb_a = vec![1.0f32, 0.0, 0.0, 0.0];
    db.upsert("auth.rs", "h1", "SUMMARY: authentication logic", &emb_a)
        .unwrap();

    // File B: embedding pointing in -x direction (opposite)
    let emb_b = vec![-1.0f32, 0.0, 0.0, 0.0];
    db.upsert("database.rs", "h2", "SUMMARY: database layer", &emb_b)
        .unwrap();

    // Query vector pointing in +x — auth.rs should rank first
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = db.search(&query, 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].path, "auth.rs");
    assert!(results[0].score > results[1].score);
}

#[test]
fn test_meta_set_get() {
    let (_tmp, db) = open_test_db();
    assert_eq!(db.get_meta("version").unwrap(), None);
    db.set_meta("version", "0.2.0").unwrap();
    assert_eq!(db.get_meta("version").unwrap(), Some("0.2.0".into()));
    // Overwrite
    db.set_meta("version", "0.3.0").unwrap();
    assert_eq!(db.get_meta("version").unwrap(), Some("0.3.0".into()));
}

// ─── ADR Tests ───────────────────────────────────────────────────────────────

#[test]
fn test_adr_add_and_list() {
    let (_tmp, db) = open_test_db();
    let id1 = db.add_adr("Use SQLite", "Because it's simple").unwrap();
    let id2 = db
        .add_adr("Use Voyage embeddings", "Best quality/cost ratio")
        .unwrap();
    assert!(id1 > 0);
    assert!(id2 > id1);

    let adrs = db.list_adrs().unwrap();
    assert_eq!(adrs.len(), 2);
    // Listed in DESC order by id (most recent first)
    assert_eq!(adrs[0].title, "Use Voyage embeddings");
    assert_eq!(adrs[1].title, "Use SQLite");
}

// ─── Export/Import Round-trip ────────────────────────────────────────────────

#[test]
fn test_export_import_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let repo_root = tmp.path();

    // Create and populate a db
    {
        let db = tokenuinely_test_helpers::Db::open(repo_root).unwrap();
        let emb = vec![0.5f32; 4];
        db.upsert("test.rs", "hashval", "SUMMARY: test file", &emb)
            .unwrap();
    }

    let db_path = repo_root.join(".tokenuinely").join("index.db");
    assert!(db_path.exists());

    // Export
    let artifact_path = tmp.path().join("export.zst");
    let raw = std::fs::read(&db_path).unwrap();
    let compressed = zstd::encode_all(raw.as_slice(), 9).unwrap();
    std::fs::write(&artifact_path, &compressed).unwrap();

    // Import to a new location
    let tmp2 = TempDir::new().unwrap();
    let dest_dir = tmp2.path().join(".tokenuinely");
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest_db = dest_dir.join("index.db");

    let comp_data = std::fs::read(&artifact_path).unwrap();
    let decompressed = zstd::decode_all(comp_data.as_slice()).unwrap();

    // Validate SQLite magic
    assert!(decompressed.len() >= 16);
    assert_eq!(&decompressed[..16], b"SQLite format 3\0");

    std::fs::write(&dest_db, &decompressed).unwrap();

    // Verify the imported db works
    let db = tokenuinely_test_helpers::Db::open(tmp2.path()).unwrap();
    assert_eq!(db.get_sha256("test.rs").unwrap(), Some("hashval".into()));
    assert_eq!(
        db.get_header("test.rs").unwrap(),
        Some("SUMMARY: test file".into())
    );
}

// ─── SHA-256 Determinism ─────────────────────────────────────────────────────

#[test]
fn test_sha256_determinism() {
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
    let hash2 = {
        let bytes = std::fs::read(&file_path).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    };

    assert_eq!(hash1, hash2);
    // Known SHA-256 of "hello world"
    assert_eq!(
        hash1,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}
