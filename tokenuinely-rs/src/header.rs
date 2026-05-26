use crate::config::{HEADER_INPUT_CHAR_LIMIT, HEADER_MODEL};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

const HEADER_PROMPT: &str = r#"You are a code-indexing assistant. Analyze the file and produce a compact semantic header.

Output ONLY this block — no markdown fences, no extra text:

SUMMARY: <one sentence: what this file does>
KEY SYMBOLS: <comma-separated list of the most important exported/public names>
TOUCHES: <comma-separated external dependencies: DB tables, API routes, env vars, external services>
NOT HERE: <what is explicitly NOT in this file; redirect hints, e.g. "OAuth flows → src/auth/oauth.py">

Rules:
- SUMMARY must be ≤ 20 words
- KEY SYMBOLS: list only names that another file would import or call
- TOUCHES: only real external touch-points (skip internal helpers)
- NOT HERE: only add if there is a natural confusion point; omit the line if nothing applies
- Total output must be ≤ 120 tokens"#;

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

pub async fn generate_header(file_content: &str, api_key: &str) -> Result<String> {
    let truncated = if file_content.len() > HEADER_INPUT_CHAR_LIMIT {
        &file_content[..HEADER_INPUT_CHAR_LIMIT]
    } else {
        file_content
    };

    let user_content = format!(
        "{}\n\n---FILE CONTENT---\n{}",
        HEADER_PROMPT, truncated
    );

    let body = AnthropicRequest {
        model: HEADER_MODEL.to_string(),
        max_tokens: 200,
        messages: vec![Message {
            role: "user".to_string(),
            content: user_content,
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
