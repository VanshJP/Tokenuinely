//! Retrieval eval harness (ROADMAP P0-#3).
//!
//! Loads `evals/queries.jsonl`, runs each query through the real `search` pipeline
//! against the already-built index, and reports top-1/3/5 hit rate, mean reciprocal
//! rank, and mean latency. Runs with or without `VOYAGE_API_KEY` (FTS-only fallback).
//!
//! This is the baseline every future ranking change has to beat — check the numbers
//! in before tuning weights or prompts.

use crate::config::Config;
use crate::db::QueryHit;
use crate::search::query::{search, SearchOpts};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::Instant;

/// One labelled query. `expected_symbol` is optional — omit it to match any chunk
/// in `expected_path`.
#[derive(Deserialize)]
struct EvalQuery {
    query: String,
    expected_path: String,
    #[serde(default)]
    expected_symbol: Option<String>,
}

/// Search this deep so top-5 / MRR are well-defined regardless of the caller's `k`.
const EVAL_POOL: usize = 10;

fn is_match(h: &QueryHit, case: &EvalQuery) -> bool {
    if h.path != case.expected_path {
        return false;
    }
    match &case.expected_symbol {
        Some(sym) => h.symbol.as_deref() == Some(sym.as_str()),
        None => true,
    }
}

pub async fn run_eval(repo: &Path, queries_file: &Path, cfg: &Config) -> Result<()> {
    let raw = std::fs::read_to_string(queries_file)
        .with_context(|| format!("reading {}", queries_file.display()))?;
    let cases: Vec<EvalQuery> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parsing line: {}", l)))
        .collect::<Result<_>>()?;
    if cases.is_empty() {
        anyhow::bail!("no queries found in {}", queries_file.display());
    }

    let (mut hit1, mut hit3, mut hit5) = (0usize, 0usize, 0usize);
    let mut rr_sum = 0.0f64;
    let mut latencies = Vec::with_capacity(cases.len());
    let mut misses: Vec<&str> = Vec::new();

    for case in &cases {
        let opts = SearchOpts {
            k: EVAL_POOL,
            include_source: false,
            max_chars: 0,
        };
        let start = Instant::now();
        let hits = search(repo, &case.query, opts, cfg).await?;
        latencies.push(start.elapsed().as_secs_f64() * 1000.0);

        match hits.iter().position(|h| is_match(h, case)) {
            Some(idx) => {
                let rank = idx + 1;
                if rank <= 1 {
                    hit1 += 1;
                }
                if rank <= 3 {
                    hit3 += 1;
                }
                if rank <= 5 {
                    hit5 += 1;
                }
                rr_sum += 1.0 / rank as f64;
            }
            None => misses.push(&case.query),
        }
    }

    let n = cases.len() as f64;
    let mean_latency = latencies.iter().sum::<f64>() / latencies.len() as f64;
    let mode = if cfg.voyage_api_key.is_some() {
        "semantic (vec+BM25+exact)"
    } else {
        "FTS-only (no VOYAGE_API_KEY)"
    };

    println!("Eval over {} queries — {}", cases.len(), mode);
    println!("  top-1 hit rate: {:.1}%", 100.0 * hit1 as f64 / n);
    println!("  top-3 hit rate: {:.1}%", 100.0 * hit3 as f64 / n);
    println!("  top-5 hit rate: {:.1}%", 100.0 * hit5 as f64 / n);
    println!("  MRR:            {:.3}", rr_sum / n);
    println!("  mean latency:   {:.1} ms", mean_latency);
    if !misses.is_empty() {
        println!("  misses ({}):", misses.len());
        for m in &misses {
            println!("    - {}", m);
        }
    }
    Ok(())
}
