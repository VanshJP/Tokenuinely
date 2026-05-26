use crate::config::EMBED_MODEL;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

#[derive(Serialize)]
struct VoyageRequest {
    model: String,
    input: Vec<String>,
    input_type: String,
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

pub async fn embed_batch(
    texts: &[String],
    api_key: &str,
    input_type: &str,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }

    let body = VoyageRequest {
        model: EMBED_MODEL.to_string(),
        input: texts.to_vec(),
        input_type: input_type.to_string(),
    };

    let client = reqwest::Client::new();

    for attempt in 0..3u32 {
        let resp = client
            .post("https://api.voyageai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send embedding request")?;

        let status = resp.status();
        if status == 429 || status.is_server_error() {
            let delay = Duration::from_millis(500 * 2u64.pow(attempt));
            tracing::warn!(
                "Voyage API returned {}, retrying in {:?} (attempt {}/3)",
                status,
                delay,
                attempt + 1
            );
            sleep(delay).await;
            continue;
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("Voyage API error {}: {}", status, text);
        }

        let parsed: VoyageResponse = resp
            .json()
            .await
            .context("Failed to parse embedding response")?;

        return Ok(parsed.data.into_iter().map(|d| d.embedding).collect());
    }

    bail!("Embedding failed after 3 retries")
}

pub async fn embed_query(text: &str, api_key: &str) -> Result<Vec<f32>> {
    let results = embed_batch(&[text.to_string()], api_key, "query").await?;
    results
        .into_iter()
        .next()
        .context("Empty embedding response")
}
