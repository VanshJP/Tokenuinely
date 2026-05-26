use crate::config::{find_repo_root, Config};
use crate::db::Db;
use crate::embedder::embed_query;
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Deserialize)]
struct HookPayload {
    #[serde(rename = "toolName")]
    tool_name: String,
    #[serde(rename = "toolInput")]
    tool_input: serde_json::Value,
}

fn extract_search_query(payload: &HookPayload) -> Option<String> {
    match payload.tool_name.as_str() {
        "Grep" => payload
            .tool_input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        "Glob" => {
            // Clean up glob pattern into natural language
            let pattern = payload
                .tool_input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cleaned = pattern
                .replace("**", " ")
                .replace("*", " ")
                .replace("/", " ")
                .replace(".", " ")
                .trim()
                .to_string();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        }
        _ => None,
    }
}

pub async fn run_hook_augment() -> Result<()> {
    let result = tokio::time::timeout(Duration::from_millis(300), async {
        run_hook_inner().await
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => {
            // Timeout or error: exit silently with no stdout
        }
    }
    Ok(())
}

async fn run_hook_inner() -> Result<()> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;

    let payload: HookPayload = serde_json::from_str(&input)?;
    let query = extract_search_query(&payload)
        .ok_or_else(|| anyhow::anyhow!("No query extractable"))?;

    let cfg = Config::load()?;
    let cwd = std::env::current_dir()?;
    let repo_root = find_repo_root(&cwd)
        .or_else(|| std::env::var("TOKENUINELY_REPO").ok().map(Into::into))
        .ok_or_else(|| anyhow::anyhow!("No index found"))?;

    let db = Db::open(&repo_root)?;
    let voyage_key = cfg.require_voyage_key()?;
    let query_vec = embed_query(&query, voyage_key).await?;
    let results = db.search(&query_vec, 3)?;

    if results.is_empty() {
        return Ok(());
    }

    let context_lines: Vec<String> = results
        .iter()
        .map(|r| format!("• {} (score: {:.2})\n  {}", r.path, r.score, r.header))
        .collect();
    let context = format!(
        "tokenuinely semantic matches:\n{}",
        context_lines.join("\n")
    );

    let output = json!({
        "hookSpecificOutput": {
            "additionalContext": context
        }
    });

    print!("{}", serde_json::to_string(&output)?);
    Ok(())
}
