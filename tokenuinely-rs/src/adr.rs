use crate::db::{now_unix, Db};
use anyhow::Result;
use rusqlite::params;

#[derive(Debug, Clone)]
pub struct Adr {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: String,
}

pub fn add_adr(db: &Db, title: &str, body: &str) -> Result<i64> {
    let now = now_unix();
    db.conn().execute(
        "INSERT INTO adrs (title, body, created_at) VALUES (?1, ?2, ?3)",
        params![title, body, now],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn list_adrs(db: &Db) -> Result<Vec<Adr>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT id, title, body, created_at FROM adrs ORDER BY created_at DESC")?;
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
