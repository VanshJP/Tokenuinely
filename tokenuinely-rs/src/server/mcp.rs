use crate::config::{find_repo_root, Config};
use crate::db::Db;
use crate::search::architecture;
use crate::search::query::{search, SearchOpts};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(json!({"code": code, "message": message})),
        }
    }
}

fn resolve_repo(params: &Option<Value>) -> Result<PathBuf> {
    if let Some(params) = params {
        if let Some(path) = params.get("path").and_then(|v| v.as_str()) {
            return Ok(PathBuf::from(path));
        }
    }
    if let Ok(repo) = std::env::var("TOKENUINELY_REPO") {
        return Ok(PathBuf::from(repo));
    }
    let cwd = std::env::current_dir()?;
    find_repo_root(&cwd).ok_or_else(|| anyhow::anyhow!("No tokenuinely index found"))
}

/// Three tools. Every tool description lives in the agent's context every turn,
/// so each one earns its keep — anything niche stays as a CLI subcommand.
fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "tokenuinely__query",
                "description": "Semantic + keyword + exact-symbol fused search over this repo. Returns top-k matching CHUNKS (function/struct/class spans) with file path, line range, header, and the actual source. Use this BEFORE Grep/Glob/Read — it returns code, not just pointers. Works with or without VOYAGE_API_KEY (falls back to BM25).",
                "inputSchema": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "Natural-language query"},
                    "k": {"type": "integer", "description": "Number of chunks to return (default 5, max 20)"},
                    "include_source": {"type": "boolean", "description": "Include source code in results (default true). Set false for path-only results."},
                    "max_chars": {"type": "integer", "description": "Per-chunk source cap (default 2400)"},
                    "path": {"type": "string", "description": "Repo root (optional)"}
                }, "required": ["text"]}
            },
            {
                "name": "tokenuinely__inspect_symbol",
                "description": "One-shot lookup for a symbol: definition (file:line + source), callers, callees, and imports. Use to answer 'what would break if I change X' or 'where is X defined'. On a miss, returns a `suggestions` list of similarly-named symbols (did-you-mean) — retry with one of those. No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "symbol": {"type": "string", "description": "Symbol name (exact match preferred)"},
                    "path": {"type": "string", "description": "Repo root (optional)"}
                }, "required": ["symbol"]}
            },
            {
                "name": "tokenuinely__repo_overview",
                "description": "Compact repo snapshot: languages, top directories, entry points, most-called symbols, index freshness, and dead-code count. Use once at the start of a session for orientation.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root (optional)"}
                }}
            }
        ]
    })
}

fn text_result(data: &impl serde::Serialize) -> Result<Value> {
    Ok(json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(data)? }] }))
}

async fn handle_tool_call(name: &str, args: &Value, cfg: &Config) -> Result<Value> {
    let repo = resolve_repo(&Some(args.clone()))?;
    match name {
        "tokenuinely__query" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let k = args
                .get("k")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 20) as usize;
            let include_source = args
                .get("include_source")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let max_chars = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(SearchOpts::default().max_chars);
            let opts = SearchOpts {
                k,
                include_source,
                max_chars,
            };
            let hits = search(&repo, text, opts, cfg).await?;
            text_result(&hits)
        }
        "tokenuinely__inspect_symbol" => {
            let sym = args.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            let db = Db::open(&repo)?;
            let definition = db.get_chunk_for_symbol(sym)?;
            // Even if there's no chunk row (e.g. only seen via symbol-table imports),
            // fall back to the symbols table for location info.
            let symbol_rows = db.find_symbols(sym, None)?;
            let callers = db.get_callers(sym)?;
            let callees = db.get_callees(sym)?;
            // Did-you-mean: when nothing matched the symbol exactly, surface up to 5
            // fuzzy candidates so the agent can retry instead of getting an empty hit.
            let exact_hit = definition.is_some()
                || symbol_rows.iter().any(|s| s.name == sym);
            let suggestions: Vec<Value> = if exact_hit {
                Vec::new()
            } else {
                let mut seen = std::collections::HashSet::new();
                symbol_rows
                    .iter()
                    .filter(|s| seen.insert(s.name.clone()))
                    .take(5)
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "kind": s.kind,
                            "path": s.path,
                            "line": s.line_start,
                        })
                    })
                    .collect()
            };
            text_result(&json!({
                "definition": definition,
                "symbol_rows": symbol_rows,
                "callers": callers,
                "callees": callees,
                "suggestions": suggestions,
            }))
        }
        "tokenuinely__repo_overview" => {
            let db = Db::open(&repo)?;
            let (files, chunks, last) = db.stats()?;
            let arch = architecture::get_architecture(db.conn())?;
            let dead = architecture::find_dead_code(db.conn())?;
            text_result(&json!({
                "repo": repo.to_string_lossy(),
                "files_indexed": files,
                "chunks_indexed": chunks,
                "last_indexed_at": last,
                "architecture": arch,
                "dead_code_count": dead.len(),
            }))
        }
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
}

pub async fn run_mcp_server() -> Result<()> {
    let cfg = Config::load()?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Invalid JSON-RPC: {}", e);
                continue;
            }
        };

        let _ = req.jsonrpc;
        let is_notification = req.id.is_none();

        match req.method.as_str() {
            "initialize" => {
                if !is_notification {
                    let resp = JsonRpcResponse::success(
                        req.id.unwrap_or(Value::Null),
                        json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {"tools": {}},
                            "serverInfo": {
                                "name": "tokenuinely",
                                "version": "0.4.0"
                            }
                        }),
                    );
                    writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                    stdout.flush()?;
                }
            }
            "notifications/initialized" => continue,
            "tools/list" => {
                if !is_notification {
                    let resp =
                        JsonRpcResponse::success(req.id.unwrap_or(Value::Null), tools_list());
                    writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                    stdout.flush()?;
                }
            }
            "tools/call" => {
                let id = req.id.unwrap_or(Value::Null);
                let params = req.params.unwrap_or(json!({}));
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let resp = match handle_tool_call(tool_name, &arguments, &cfg).await {
                    Ok(result) => JsonRpcResponse::success(id, result),
                    Err(e) => JsonRpcResponse::error(id, -32000, &e.to_string()),
                };
                writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                stdout.flush()?;
            }
            _ => {
                if !is_notification {
                    let resp = JsonRpcResponse::error(
                        req.id.unwrap_or(Value::Null),
                        -32601,
                        &format!("Method not found: {}", req.method),
                    );
                    writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                    stdout.flush()?;
                }
            }
        }
    }

    Ok(())
}
