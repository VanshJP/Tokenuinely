use crate::adr;
use crate::architecture;
use crate::config::{find_repo_root, Config};
use crate::cypher;
use crate::db::Db;
use crate::detect_changes;
use crate::fts;
use crate::indexer::index_repo;
use crate::query::search;
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
    find_repo_root(&cwd)
        .ok_or_else(|| anyhow::anyhow!("No tokenuinely index found"))
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "tokenuinely__query",
                "description": "Semantic search over this repo's tokenuinely index. Returns top-k files whose semantic headers best match the query. Use this BEFORE grepping or globbing — saves 50-200x tokens. Requires VOYAGE_API_KEY.",
                "inputSchema": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "Natural language search query"},
                    "k": {"type": "integer", "description": "Number of results (default 5)"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["text"]}
            },
            {
                "name": "tokenuinely__search_text",
                "description": "BM25 full-text search over file headers and symbol names. No API key needed. Use when VOYAGE_API_KEY is unavailable.",
                "inputSchema": {"type": "object", "properties": {
                    "text": {"type": "string", "description": "Search query"},
                    "k": {"type": "integer", "description": "Number of results (default 10)"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["text"]}
            },
            {
                "name": "tokenuinely__find_symbol",
                "description": "Find function/class/struct/trait definitions by name pattern. Returns file, line, signature. No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "name": {"type": "string", "description": "Symbol name or pattern to search for"},
                    "kind": {"type": "string", "description": "Filter by kind: function, struct, class, trait, enum, method"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["name"]}
            },
            {
                "name": "tokenuinely__trace_dependencies",
                "description": "Show what calls a symbol and what it calls. Answers 'what would break if I change this?'. No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "symbol": {"type": "string", "description": "Symbol name to trace"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["symbol"]}
            },
            {
                "name": "tokenuinely__get_architecture",
                "description": "Single-call architecture overview: languages, file counts, entry points, top directories, most-called symbols. No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }}
            },
            {
                "name": "tokenuinely__find_dead_code",
                "description": "Find functions/methods with zero callers (excluding main, new, default, test files). No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }}
            },
            {
                "name": "tokenuinely__detect_changes",
                "description": "Show files with stale index entries based on git status and hash comparison. No API key needed.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }}
            },
            {
                "name": "tokenuinely__manage_adr",
                "description": "Manage Architecture Decision Records. Use action='add' with title+body, or action='list' to see all.",
                "inputSchema": {"type": "object", "properties": {
                    "action": {"type": "string", "description": "'add' or 'list'"},
                    "title": {"type": "string", "description": "ADR title (required for add)"},
                    "body": {"type": "string", "description": "ADR body (required for add)"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["action"]}
            },
            {
                "name": "tokenuinely__cypher_query",
                "description": "Run a Cypher-like graph query over symbols and deps. Example: MATCH (f:Function)-[:CALLS]->(g) WHERE f.name = 'main' RETURN g.name",
                "inputSchema": {"type": "object", "properties": {
                    "query": {"type": "string", "description": "Cypher query string"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }, "required": ["query"]}
            },
            {
                "name": "tokenuinely__status",
                "description": "Show the status of this repo's tokenuinely index.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }}
            },
            {
                "name": "tokenuinely__reindex",
                "description": "Re-index this repo. Requires ANTHROPIC_API_KEY + VOYAGE_API_KEY.",
                "inputSchema": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "Repo root path (optional)"}
                }}
            },
            {
                "name": "tokenuinely__start_viz",
                "description": "Start 3D graph visualization server at localhost:9749. Open in browser to explore the code graph.",
                "inputSchema": {"type": "object", "properties": {
                    "port": {"type": "integer", "description": "Port (default 9749)"},
                    "path": {"type": "string", "description": "Repo root path (optional)"}
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
            let k = args.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let results = search(&repo, text, k, cfg).await?;
            let items: Vec<Value> = results.iter().map(|r| json!({"path": r.path, "header": r.header, "score": r.score})).collect();
            text_result(&items)
        }
        "tokenuinely__search_text" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let k = args.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let db = Db::open(&repo)?;
            fts::create_fts_table(db.conn())?;
            fts::populate_fts(db.conn())?;
            let results = fts::fts_search(db.conn(), text, k)?;
            text_result(&results)
        }
        "tokenuinely__find_symbol" => {
            let name_pat = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let kind = args.get("kind").and_then(|v| v.as_str());
            let db = Db::open(&repo)?;
            let results = db.find_symbols(name_pat, kind)?;
            text_result(&results)
        }
        "tokenuinely__trace_dependencies" => {
            let sym = args.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            let db = Db::open(&repo)?;
            let callers = db.get_callers(sym)?;
            let callees = db.get_callees(sym)?;
            text_result(&json!({"callers": callers, "callees": callees}))
        }
        "tokenuinely__get_architecture" => {
            let db = Db::open(&repo)?;
            let arch = architecture::get_architecture(db.conn())?;
            text_result(&arch)
        }
        "tokenuinely__find_dead_code" => {
            let db = Db::open(&repo)?;
            let dead = architecture::find_dead_code(db.conn())?;
            text_result(&dead)
        }
        "tokenuinely__detect_changes" => {
            let changes = detect_changes::detect_changes(&repo)?;
            let items: Vec<Value> = changes.iter().map(|c| json!({
                "path": c.path, "status": c.status.to_string(), "stale_header": c.stale_header
            })).collect();
            text_result(&items)
        }
        "tokenuinely__manage_adr" => {
            let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");
            let db = Db::open(&repo)?;
            match action {
                "add" => {
                    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
                    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let id = adr::add_adr(&db, title, body)?;
                    text_result(&json!({"created": id}))
                }
                _ => {
                    let adrs = adr::list_adrs(&db)?;
                    let items: Vec<Value> = adrs.iter().map(|a| json!({"id": a.id, "title": a.title, "body": a.body, "created_at": a.created_at})).collect();
                    text_result(&items)
                }
            }
        }
        "tokenuinely__cypher_query" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let db = Db::open(&repo)?;
            let results = cypher::execute_cypher(db.conn(), q)?;
            text_result(&results)
        }
        "tokenuinely__status" => {
            let db = Db::open(&repo)?;
            let (count, last) = db.stats()?;
            text_result(&json!({"indexed": true, "repo": repo.to_string_lossy(), "files_indexed": count, "last_indexed_at": last}))
        }
        "tokenuinely__reindex" => {
            let stats = index_repo(&repo, cfg).await?;
            text_result(&json!({"scanned": stats.scanned, "unchanged": stats.unchanged, "indexed": stats.indexed, "deleted": stats.deleted, "failed": stats.failed.len()}))
        }
        "tokenuinely__start_viz" => {
            let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(9749) as u16;
            // Spawn viz server as background task
            let repo_clone = repo.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::graph_viz::start_viz_server(repo_clone, port).await {
                    eprintln!("Viz server error: {}", e);
                }
            });
            text_result(&json!({"status": "started", "url": format!("http://localhost:{}", port)}))
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

        let _ = req.jsonrpc; // consumed but not checked

        // Notifications have no id — don't respond
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
                                "version": "0.3.0"
                            }
                        }),
                    );
                    let out = serde_json::to_string(&resp)?;
                    writeln!(stdout, "{}", out)?;
                    stdout.flush()?;
                }
            }
            "notifications/initialized" => {
                // No response for notifications
                continue;
            }
            "tools/list" => {
                if !is_notification {
                    let resp = JsonRpcResponse::success(
                        req.id.unwrap_or(Value::Null),
                        tools_list(),
                    );
                    let out = serde_json::to_string(&resp)?;
                    writeln!(stdout, "{}", out)?;
                    stdout.flush()?;
                }
            }
            "tools/call" => {
                let id = req.id.unwrap_or(Value::Null);
                let params = req.params.unwrap_or(json!({}));
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(json!({}));

                let resp = match handle_tool_call(tool_name, &arguments, &cfg).await {
                    Ok(result) => JsonRpcResponse::success(id, result),
                    Err(e) => JsonRpcResponse::error(id, -32000, &e.to_string()),
                };
                let out = serde_json::to_string(&resp)?;
                writeln!(stdout, "{}", out)?;
                stdout.flush()?;
            }
            _ => {
                if !is_notification {
                    let resp = JsonRpcResponse::error(
                        req.id.unwrap_or(Value::Null),
                        -32601,
                        &format!("Method not found: {}", req.method),
                    );
                    let out = serde_json::to_string(&resp)?;
                    writeln!(stdout, "{}", out)?;
                    stdout.flush()?;
                }
            }
        }
    }

    Ok(())
}
