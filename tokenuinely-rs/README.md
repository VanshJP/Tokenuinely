# tokenuinely

**Semantic codebase index for AI coding agents.** One MCP tool call replaces 50+ grep/glob/read_file calls — saving 50-200x tokens per question.

tokenuinely generates compact, Claude-powered semantic headers for every file in your repo, embeds them with Voyage AI, and serves them through a JSON-RPC MCP server. Instead of burning tokens on file-by-file exploration, your agent calls `tokenuinely__query("authentication logic")` and gets exactly the right files in under 300ms.

## What Makes This Different

Most code intelligence tools do structural analysis (AST parsing, call graphs). tokenuinely does that too — but its core differentiator is **Claude-generated semantic headers with NOT HERE redirect hints**:

```
SUMMARY: Handles OAuth2 token refresh and session persistence
KEY SYMBOLS: refresh_token, SessionManager, validate_session
TOUCHES: Redis session store, /api/auth/* routes, SESSION_SECRET env var
NOT HERE: Initial login flow → src/auth/login.rs, password hashing → src/auth/crypto.rs
```

The `NOT HERE` line is the killer feature — it tells agents *where NOT to look* and *where to go instead*, preventing the expensive wrong-file-first pattern that burns thousands of tokens.

## Quick Start

### 1. Build

```bash
cd tokenuinely-rs
cargo build --release
```

The binary is at `target/release/tokenuinely`.

### 2. Set API Keys

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # Required for indexing (header generation)
export VOYAGE_API_KEY=pa-...          # Required for semantic search
```

Or create a `.env` file in your working directory. **Structural tools (find-symbol, architecture, detect-changes, etc.) work without any API keys.**

### 3. Index a Repo

```bash
tokenuinely index /path/to/your/repo
```

This walks the repo, generates semantic headers via Claude Haiku, embeds them with Voyage AI, and stores everything in `.tokenuinely/index.db`.

### 4. Search

```bash
tokenuinely query "authentication logic"
tokenuinely query "where are database migrations" -k 10
```

### 5. Connect to Your AI Agent

**One-command setup for Claude Code:**

```bash
tokenuinely setup /path/to/your/repo
```

This registers the MCP server, installs the PreToolUse hook, appends a hint to `CLAUDE.md`, and runs an initial index.

**Manual MCP registration:**

```bash
claude mcp add tokenuinely --transport stdio -- /path/to/tokenuinely mcp
```

## CLI Reference

| Command | Description | Requires API Keys |
|---------|-------------|-------------------|
| `index [path]` | Index or re-index a repo (default: `.`) | Yes |
| `query <text> [-k N] [--path P]` | Semantic search over the index | Yes (Voyage) |
| `status [path]` | Show index stats (file count, last indexed) | No |
| `mcp` | Start the JSON-RPC MCP stdio server | Depends on tool |
| `hook-augment` | Run as Claude Code PreToolUse hook | Yes (Voyage) |
| `export [path] [--output FILE]` | Export index as compressed `.zst` artifact | No |
| `import <artifact> [path]` | Import a compressed index artifact | No |
| `detect-changes [path]` | Show files with stale index entries | No |
| `adr add <title> <body>` | Add an Architecture Decision Record | No |
| `adr list [path]` | List all ADRs | No |
| `setup [path]` | Register MCP + hook with Claude Code | Yes (for index) |

**Setup flags:** `--skip-claude-md` (don't modify CLAUDE.md), `--skip-index` (skip initial indexing)

## MCP Tools

When running as an MCP server (`tokenuinely mcp`), these tools are available to your AI agent:

### `tokenuinely__query`
Semantic search over the index. Returns the top-k files whose semantic headers best match the query.

```json
{"text": "authentication logic", "k": 5}
```

**Use this BEFORE grepping or globbing** — it points you at the right files in one call.

### `tokenuinely__status`
Shows index health: file count, repo path, last indexed timestamp.

### `tokenuinely__reindex`
Re-indexes the repo. Incremental — only changed files (by SHA-256) are reprocessed.

## How It Works

### Indexing Pipeline

```
walker.walk_repo(root)
  │  filter: .gitignore, size, binary, extensions
  ▼
hasher.sha256_file()  →  db.get_sha256()  (skip if unchanged)
  │
  ▼  Phase 1: Header Generation (parallel, semaphore-limited)
header.generate_header(content, anthropic_key)
  │  Claude Haiku → compact 4-line semantic header
  ▼
  ▼  Phase 2: Embedding (batched)
embedder.embed_batch(headers, voyage_key)
  │  Voyage AI → 1024-dim float vectors
  ▼
db.upsert(path, sha256, header, embedding)
```

### Search

```
query text → Voyage embed_query() → 1024-dim vector
  → brute-force cosine similarity over all stored vectors
  → top-k results with path, header, score
```

Brute-force cosine similarity is fast enough for repos up to ~100k files. No vector DB extension needed.

### PreToolUse Hook

When registered as a Claude Code hook, tokenuinely silently injects semantic context into every `Grep` or `Glob` call:

```
Agent calls Grep("pattern")
  → tokenuinely hook-augment runs (stdin: hook payload)
  → embeds the pattern, searches top-3 matches
  → returns additionalContext with file hints
  → Agent sees relevant files before grep even runs
```

**Hard 300ms deadline** — the hook never blocks the agent.

## Architecture Decision Records

Store architectural decisions alongside your index:

```bash
tokenuinely adr add "Use SQLite for storage" "Single-file database, no server dependency, WAL mode for concurrent reads"
tokenuinely adr list
```

ADRs persist in the same `.tokenuinely/index.db` and survive export/import.

## Index Artifacts

Share pre-built indexes with your team so they skip the indexing step:

```bash
# Export (zstd level 9 compression)
tokenuinely export --output my-project-index.zst

# Import on another machine
tokenuinely import my-project-index.zst /path/to/repo
```

Commit the `.zst` file to your repo and teammates can import it immediately.

## Git-Aware Change Detection

See which indexed files are stale:

```bash
tokenuinely detect-changes
```

This cross-references `git status` and SHA-256 hashes to show files that have changed since last indexing.

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `ANTHROPIC_API_KEY` | For indexing | Claude API key for header generation |
| `VOYAGE_API_KEY` | For semantic search | Voyage AI key for embeddings |
| `TOKENUINELY_REPO` | No | Default repo path for MCP server |
| `RUST_LOG` | No | Log level (e.g. `tokenuinely=debug`) |

Keys can also be set in a `.env` file (loaded via dotenvy).

## Design Decisions

1. **NOT HERE redirect hints** — The header prompt includes a "NOT HERE" line that tells agents where to look instead. This is the single biggest token saver beyond basic semantic search.

2. **300ms hard deadline on hooks** — The PreToolUse hook must never block the agent. If the index is slow or unavailable, it exits silently.

3. **Incremental indexing by SHA-256** — Only changed files are reprocessed. Re-indexing after a small change takes seconds, not minutes.

4. **Brute-force cosine similarity** — Fast enough for <100k files. No sqlite-vec extension needed, zero deployment complexity.

5. **All logs to stderr** — stdout is the JSON-RPC channel in MCP mode. Log corruption would break the protocol.

6. **API keys are optional for structural tools** — Status, detect-changes, ADRs, export/import all work without any API keys.

## Project Structure

```
tokenuinely-rs/
├── Cargo.toml
├── src/
│   ├── main.rs            # CLI entry point (clap)
│   ├── config.rs           # Constants, Config struct, path helpers
│   ├── walker.rs           # Repo file walker with ignore rules
│   ├── hasher.rs           # SHA-256 file hashing
│   ├── db.rs               # SQLite database layer + cosine similarity
│   ├── header.rs           # Anthropic API header generation
│   ├── embedder.rs         # Voyage AI embedding (batch + single)
│   ├── indexer.rs           # Full indexing pipeline
│   ├── query.rs            # Search functions
│   ├── mcp.rs              # JSON-RPC 2.0 MCP stdio server
│   ├── hook_augment.rs     # Claude Code PreToolUse hook
│   ├── export.rs           # Zstd compressed export/import
│   ├── adr.rs              # Architecture Decision Records
│   └── detect_changes.rs   # Git-aware stale index detection
└── tests/
    ├── integration_test.rs # Unit tests (no API keys needed)
    ├── bench/
    │   └── benchmark.py    # Token savings benchmark harness
    └── fixtures/
        └── sample_repo/    # Test fixture files
```

## Testing

```bash
cargo test
```

All 11 tests pass without API keys — they test SQLite operations, cosine similarity ranking, export/import round-trips, ADRs, and SHA-256 determinism.

## Benchmarking

Compare token usage with and without tokenuinely:

```bash
python tests/bench/benchmark.py /path/to/repo
python tests/bench/benchmark.py /path/to/repo --output results.json
```

Runs 5 standard code-discovery questions through `claude` CLI and measures `input_tokens` with vs. without the MCP server active.

## License

MIT
