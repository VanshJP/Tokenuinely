use crate::config::{HEADER_INPUT_CHAR_LIMIT, HEADER_MODEL};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

const CHUNK_PROMPT: &str = r#"You are a code-indexing assistant. You are given ONE code chunk (a function, class, struct, or whole short file). Produce a compact retrieval header.

Output ONLY this block — no markdown fences, no extra text:

WHY: <one short sentence: the role this chunk plays>
EFFECTS: <comma-separated external touch-points it actually reaches: DB tables, API routes, env vars, files written, network calls — or "none" if pure>
CALLS: <up to 5 comma-separated names of other functions/types this chunk depends on>

Rules:
- WHY ≤ 18 words. Describe purpose, not mechanics.
- EFFECTS: only real side effects. Skip internal helpers and stdlib calls.
- CALLS: only names another file would recognise (skip locals, loop vars, primitives).
- Total output ≤ 80 tokens.
"#;

const FILE_PROMPT: &str = r#"You are a code-indexing assistant. Produce a compact retrieval header for this whole file (it has no extractable symbols — e.g. markdown, config, plain script).

Output ONLY this block — no markdown fences, no extra text:

WHY: <one short sentence: what this file is for>
EFFECTS: <comma-separated external touch-points: DB tables, API routes, env vars, services — or "none">
NOT HERE: <what readers might wrongly look for here; omit the line if nothing applies>

Rules:
- WHY ≤ 20 words.
- Total output ≤ 90 tokens.
"#;

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

/// Generate a per-chunk header. `context` is a short hint like "fn parse_rust_use in src/deps.rs".
pub async fn generate_chunk_header(
    chunk_source: &str,
    context: &str,
    api_key: &str,
) -> Result<String> {
    let truncated = truncate(chunk_source);
    let user_content = format!(
        "{}\n\n---CHUNK ({})---\n{}",
        CHUNK_PROMPT, context, truncated
    );
    call_anthropic(&user_content, 160, api_key).await
}

/// Generate a whole-file header for files with no extractable symbols.
pub async fn generate_file_header(file_content: &str, api_key: &str) -> Result<String> {
    let truncated = truncate(file_content);
    let user_content = format!("{}\n\n---FILE CONTENT---\n{}", FILE_PROMPT, truncated);
    call_anthropic(&user_content, 180, api_key).await
}

fn truncate(s: &str) -> &str {
    if s.len() > HEADER_INPUT_CHAR_LIMIT {
        let end = HEADER_INPUT_CHAR_LIMIT;
        // step back to a char boundary so we don't slice through a multi-byte char
        let mut cut = end;
        while !s.is_char_boundary(cut) && cut > 0 {
            cut -= 1;
        }
        &s[..cut]
    } else {
        s
    }
}

async fn call_anthropic(user_content: &str, max_tokens: u32, api_key: &str) -> Result<String> {
    let body = AnthropicRequest {
        model: HEADER_MODEL.to_string(),
        max_tokens,
        messages: vec![Message {
            role: "user".to_string(),
            content: user_content.to_string(),
        }],
    };

    let client = reqwest::Client::new();

    for attempt in 0..3u32 {
        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send header request")?;

        let status = resp.status();
        if status == 429 || status.is_server_error() {
            let delay = Duration::from_millis(500 * 2u64.pow(attempt));
            tracing::warn!(
                "Header API returned {}, retrying in {:?} (attempt {}/3)",
                status,
                delay,
                attempt + 1
            );
            sleep(delay).await;
            continue;
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error {}: {}", status, text);
        }

        let parsed: AnthropicResponse = resp
            .json()
            .await
            .context("Failed to parse header response")?;

        let text = parsed
            .content
            .into_iter()
            .find_map(|b| b.text)
            .unwrap_or_default();

        return Ok(text.trim().to_string());
    }

    bail!("Header generation failed after 3 retries")
}
