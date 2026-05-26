use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

/// A single FTS search result with path, header text, and FTS5 rank score.
#[derive(Debug, Clone, Serialize)]
pub struct FtsResult {
    pub path: String,
    pub header: String,
    pub rank: f64,
}

/// Create the FTS5 virtual table for full-text search over file paths, headers,
/// and aggregated symbol names.
pub fn create_fts_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts_index USING fts5(path, header, symbols);",
    )
    .context("Failed to create FTS5 virtual table")?;
    Ok(())
}

/// Clear and repopulate the FTS table from the `files` and `symbols` tables.
///
/// For each file in the `files` table, aggregates symbol names from the `symbols`
/// table and inserts (path, header, symbols) into `fts_index`.
///
/// Returns the number of rows inserted.
pub fn populate_fts(conn: &Connection) -> Result<usize> {
    conn.execute("DELETE FROM fts_index", [])
        .context("Failed to clear FTS table")?;

    let mut file_stmt = conn.prepare("SELECT path, header FROM files")?;
    let files: Vec<(String, String)> = file_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let mut insert_stmt =
        conn.prepare("INSERT INTO fts_index (path, header, symbols) VALUES (?1, ?2, ?3)")?;

    let mut count = 0usize;
    for (path, header) in &files {
        // Aggregate symbol names for this file path
        let symbols: String = match conn
            .prepare_cached("SELECT name FROM symbols WHERE path = ?1")
        {
            Ok(mut sym_stmt) => sym_stmt
                .query_map(params![path], |row| row.get::<_, String>(0))
                .ok()
                .map(|rows| {
                    rows.filter_map(|r| r.ok())
                        .collect::<Vec<String>>()
                        .join(" ")
                })
                .unwrap_or_default(),
            Err(_) => {
                // symbols table may not exist yet; just use empty string
                String::new()
            }
        };

        insert_stmt.execute(params![path, header, symbols])?;
        count += 1;
    }

    Ok(count)
}

/// Query the FTS table using FTS5 MATCH syntax.
///
/// Results are ordered by FTS5 rank (lower rank = better match) and limited to `k` results.
pub fn fts_search(conn: &Connection, query: &str, k: usize) -> Result<Vec<FtsResult>> {
    let mut stmt = conn.prepare(
        "SELECT path, header, rank FROM fts_index \
         WHERE fts_index MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;

    let results = stmt
        .query_map(params![query, k as i64], |row| {
            Ok(FtsResult {
                path: row.get(0)?,
                header: row.get(1)?,
                rank: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}
