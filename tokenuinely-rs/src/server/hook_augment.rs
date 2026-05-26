use crate::config::{find_repo_root, Config};
use crate::search::query::{search, SearchOpts};
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
            let pattern = payload
                .tool_input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cleaned = pattern
                .replace("**", " ")
                .replace(['*', '/', '.'], " ")
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
    // 600ms budget: per-chunk fused search + (maybe) one Voyage call.
    let result = tokio::time::timeout(Duration::from_millis(600), run_hook_inner()).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => { /* fall through silently */ }
    }
    Ok(())
}

async fn run_hook_inner() -> Result<()> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;

    let payload: HookPayload = serde_json::from_str(&input)?;
    let query =
        extract_search_query(&payload).ok_or_else(|| anyhow::anyhow!("No query extractable"))?;

    let cfg = Config::load()?;
    let cwd = std::env::current_dir()?;
    let repo_root = find_repo_root(&cwd)
        .or_else(|| std::env::var("TOKENUINELY_REPO").ok().map(Into::into))
        .ok_or_else(|| anyhow::anyhow!("No index found"))?;

    let opts = SearchOpts {
        k: 3,
        include_source: true,
        // Hook context is shared with the model on every tool call — be stingy.
        max_chars: 800,
    };
    let hits = search(&repo_root, &query, opts, &cfg).await?;

    if hits.is_empty() {
        return Ok(());
    }

    let blocks: Vec<String> = hits
        .iter()
        .map(|h| {
            let loc = match &h.symbol {
                Some(s) => format!(
                    "{}:{}-{} [{}] {}",
                    h.path, h.line_start, h.line_end, h.kind, s
                ),
                None => format!("{}:{}-{}", h.path, h.line_start, h.line_end),
            };
            let src = h.source.as_deref().unwrap_or("");
            format!(
                "▌ {} (score {:.2})\n{}\n```\n{}\n```",
                loc, h.score, h.header, src
            )
        })
        .collect();

    let context = format!(
        "tokenuinely matches (use these BEFORE re-grepping the same area):\n\n{}",
        blocks.join("\n\n")
    );

    let output = json!({
        "hookSpecificOutput": {
            "additionalContext": context
        }
    });

    print!("{}", serde_json::to_string(&output)?);
    Ok(())
}
