use crate::config::{find_repo_root, Config};
use crate::db::{Db, QueryResult};
use crate::embedder::embed_query;
use anyhow::{bail, Result};
use std::path::Path;

pub async fn search(
    repo_root: &Path,
    query_text: &str,
    k: usize,
    cfg: &Config,
) -> Result<Vec<QueryResult>> {
    let db = Db::open(repo_root)?;
    let voyage_key = cfg.require_voyage_key()?;
    let query_vec = embed_query(query_text, voyage_key).await?;
    db.search(&query_vec, k)
}

pub async fn search_auto(
    query_text: &str,
    k: usize,
    cfg: &Config,
) -> Result<Vec<QueryResult>> {
    let cwd = std::env::current_dir()?;
    let repo_root = find_repo_root(&cwd)
        .or_else(|| std::env::var("TOKENUINELY_REPO").ok().map(Into::into));
    match repo_root {
        Some(root) => search(&root, query_text, k, cfg).await,
        None => bail!("No tokenuinely index found. Run `tokenuinely index` first."),
    }
}
