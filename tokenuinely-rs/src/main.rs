mod adr;
mod architecture;
mod config;
mod cypher;
mod db;
mod deps;
mod detect_changes;
mod embedder;
mod export;
mod fts;
mod graph_viz;
mod hasher;
mod header;
mod hook_augment;
mod indexer;
mod mcp;
mod query;
mod symbols;
mod walker;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "tokenuinely",
    version = "0.3.0",
    about = "Semantic codebase index for AI agents — 12 MCP tools, tree-sitter AST, zero-API-key structural search"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Index or re-index a repo (default: current directory)
    Index {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Semantic search over the index (requires VOYAGE_API_KEY)
    Query {
        /// Natural language search query
        text: String,
        /// Number of results to return
        #[arg(short, long, default_value = "5")]
        k: usize,
        /// Path to the repo root
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Full-text search over headers and symbols (no API key needed)
    SearchText {
        /// Search query
        text: String,
        /// Number of results
        #[arg(short, long, default_value = "10")]
        k: usize,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Find symbol definitions by name pattern (no API key needed)
    FindSymbol {
        /// Symbol name or pattern
        name: String,
        /// Filter by kind (function, struct, class, trait, enum, method)
        #[arg(long)]
        kind: Option<String>,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show what a symbol calls and what calls it
    TraceDeps {
        /// Symbol name to trace
        symbol: String,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show architecture overview (no API key needed)
    Architecture {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Find dead code — functions with zero callers
    DeadCode {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Run a Cypher-like graph query over symbols and deps
    CypherQuery {
        /// Cypher query string
        query: String,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show index status
    Status {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Start MCP stdio server
    Mcp,
    /// Run as Claude Code PreToolUse hook (reads stdin, writes stdout)
    HookAugment,
    /// Start 3D graph visualization server
    Viz {
        /// Port to serve on
        #[arg(long, default_value = "9749")]
        port: u16,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Export compressed index artifact
    Export {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output file path
        #[arg(long, default_value = "tokenuinely-index.zst")]
        output: PathBuf,
    },
    /// Import compressed index artifact
    Import {
        /// Path to the compressed artifact
        artifact: PathBuf,
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show stale index entries based on git status and hash comparison
    DetectChanges {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Architecture Decision Records
    Adr {
        #[command(subcommand)]
        command: AdrCommands,
    },
    /// Register MCP + hook with Claude Code, append CLAUDE.md hint, run initial index
    Setup {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Skip appending to CLAUDE.md
        #[arg(long)]
        skip_claude_md: bool,
        /// Skip initial indexing
        #[arg(long)]
        skip_index: bool,
    },
}

#[derive(Subcommand)]
enum AdrCommands {
    /// Add an Architecture Decision Record
    Add {
        /// Title of the ADR
        title: String,
        /// Body of the ADR
        body: String,
    },
    /// List all Architecture Decision Records
    List {
        /// Path to the repo root
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("tokenuinely=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Index { path } => {
            let path = std::fs::canonicalize(&path)?;
            let cfg = config::Config::load()?;
            let stats = indexer::index_repo(&path, &cfg).await?;
            eprintln!(
                "Done: {} scanned, {} unchanged, {} indexed, {} deleted, {} failed",
                stats.scanned,
                stats.unchanged,
                stats.indexed,
                stats.deleted,
                stats.failed.len()
            );
            for (file, err) in &stats.failed {
                eprintln!("  FAIL {}: {}", file, err);
            }
        }
        Commands::Query { text, k, path } => {
            let cfg = config::Config::load()?;
            let results = if let Some(p) = path {
                let p = std::fs::canonicalize(&p)?;
                query::search(&p, &text, k, &cfg).await?
            } else {
                query::search_auto(&text, k, &cfg).await?
            };
            for r in &results {
                println!("{:.4}  {}  {}", r.score, r.path, r.header.lines().next().unwrap_or(""));
            }
        }
        Commands::SearchText { text, k, path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            fts::create_fts_table(db.conn())?;
            fts::populate_fts(db.conn())?;
            let results = fts::fts_search(db.conn(), &text, k)?;
            for r in &results {
                println!("{:.4}  {}  {}", r.rank, r.path, r.header.lines().next().unwrap_or(""));
            }
        }
        Commands::FindSymbol { name, kind, path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let results = db.find_symbols(&name, kind.as_deref())?;
            if results.is_empty() {
                println!("No symbols found matching '{}'", name);
            } else {
                for s in &results {
                    println!("{}:{} [{}] {}", s.path, s.line_start, s.kind, s.signature);
                }
            }
        }
        Commands::TraceDeps { symbol, path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let callers = db.get_callers(&symbol)?;
            let callees = db.get_callees(&symbol)?;
            let imports = db.get_imports(&symbol)?;
            if !callers.is_empty() {
                println!("Called by:");
                for c in &callers {
                    println!("  {} ({})", c.source_symbol.as_deref().unwrap_or("?"), c.source_path);
                }
            }
            if !callees.is_empty() {
                println!("Calls:");
                for c in &callees {
                    println!("  {}", c.target_symbol);
                }
            }
            if !imports.is_empty() {
                println!("Imports:");
                for i in &imports {
                    println!("  {}", i.target_symbol);
                }
            }
            if callers.is_empty() && callees.is_empty() && imports.is_empty() {
                println!("No dependencies found for '{}'", symbol);
            }
        }
        Commands::Architecture { path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let arch = architecture::get_architecture(db.conn())?;
            println!("{}", serde_json::to_string_pretty(&arch)?);
        }
        Commands::DeadCode { path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let dead = architecture::find_dead_code(db.conn())?;
            if dead.is_empty() {
                println!("No dead code found.");
            } else {
                println!("{} potentially dead functions:", dead.len());
                for d in &dead {
                    println!("  {}:{} [{}] {}", d.path, d.line, d.kind, d.name);
                }
            }
        }
        Commands::CypherQuery { query: q, path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let results = cypher::execute_cypher(db.conn(), &q)?;
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        Commands::Status { path } => {
            let path = std::fs::canonicalize(&path)?;
            let db = db::Db::open(&path)?;
            let (count, last) = db.stats()?;
            println!("Repo: {}", path.display());
            println!("Files indexed: {}", count);
            if let Some(ts) = last {
                println!("Last indexed at: {}", ts);
            }
        }
        Commands::Mcp => {
            mcp::run_mcp_server().await?;
        }
        Commands::HookAugment => {
            hook_augment::run_hook_augment().await?;
        }
        Commands::Viz { port, path } => {
            let path = std::fs::canonicalize(&path)?;
            graph_viz::print_viz_banner(port);
            graph_viz::start_viz_server(path, port).await?;
        }
        Commands::Export { path, output } => {
            let path = std::fs::canonicalize(&path)?;
            export::export_index(&path, &output)?;
        }
        Commands::Import { artifact, path } => {
            let path = std::fs::canonicalize(&path)?;
            export::import_index(&artifact, &path)?;
        }
        Commands::DetectChanges { path } => {
            let path = std::fs::canonicalize(&path)?;
            let changes = detect_changes::detect_changes(&path)?;
            if changes.is_empty() {
                println!("No stale entries detected.");
            } else {
                for c in &changes {
                    println!("[{}] {}", c.status, c.path);
                    if let Some(header) = &c.stale_header {
                        for line in header.lines() {
                            println!("  {}", line);
                        }
                    }
                }
            }
        }
        Commands::Adr { command } => match command {
            AdrCommands::Add { title, body } => {
                let cwd = std::env::current_dir()?;
                let db = db::Db::open(&cwd)?;
                let id = adr::add_adr(&db, &title, &body)?;
                println!("ADR #{} created.", id);
            }
            AdrCommands::List { path } => {
                let path = std::fs::canonicalize(&path)?;
                let db = db::Db::open(&path)?;
                let adrs = adr::list_adrs(&db)?;
                if adrs.is_empty() {
                    println!("No ADRs found.");
                } else {
                    for a in &adrs {
                        println!("#{} [{}] {}", a.id, a.created_at, a.title);
                        println!("  {}", a.body);
                        println!();
                    }
                }
            }
        },
        Commands::Setup {
            path,
            skip_claude_md,
            skip_index,
        } => {
            let path = std::fs::canonicalize(&path)?;
            let bin_path = std::env::current_exe()?;
            let bin_str = bin_path.to_string_lossy();

            // Register MCP server with Claude Code
            eprintln!("Registering MCP server with Claude Code...");
            let mcp_status = std::process::Command::new("claude")
                .args([
                    "mcp",
                    "add",
                    "tokenuinely",
                    "--transport",
                    "stdio",
                    "--",
                    &bin_str,
                    "mcp",
                ])
                .status();
            match mcp_status {
                Ok(s) if s.success() => eprintln!("MCP server registered."),
                _ => eprintln!("Warning: Could not register MCP server (is `claude` CLI installed?)."),
            }

            // Register hook
            eprintln!("Registering PreToolUse hook...");
            let hook_status = std::process::Command::new("claude")
                .args([
                    "hooks",
                    "add",
                    "--event",
                    "PreToolUse",
                    "--matcher",
                    "Grep|Glob",
                    "--",
                    &bin_str,
                    "hook-augment",
                ])
                .status();
            match hook_status {
                Ok(s) if s.success() => eprintln!("Hook registered."),
                _ => eprintln!("Warning: Could not register hook."),
            }

            // Append CLAUDE.md snippet
            if !skip_claude_md {
                let claude_md = path.join("CLAUDE.md");
                let existing = std::fs::read_to_string(&claude_md).unwrap_or_default();
                if !existing.contains("tokenuinely") {
                    let mut content = existing;
                    content.push_str(config::CLAUDE_MD_SNIPPET);
                    std::fs::write(&claude_md, content)?;
                    eprintln!("Appended tokenuinely hint to CLAUDE.md");
                } else {
                    eprintln!("CLAUDE.md already contains tokenuinely hint.");
                }
            }

            // Run initial index
            if !skip_index {
                eprintln!("Running initial index...");
                let cfg = config::Config::load()?;
                let stats = indexer::index_repo(&path, &cfg).await?;
                eprintln!(
                    "Indexed: {} scanned, {} indexed, {} failed",
                    stats.scanned,
                    stats.indexed,
                    stats.failed.len()
                );
            }

            eprintln!("Setup complete!");
        }
    }

    Ok(())
}
