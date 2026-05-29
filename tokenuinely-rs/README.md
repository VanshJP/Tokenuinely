# tokenuinely

**Semantic codebase index for AI coding agents.** One MCP tool call replaces a
flurry of grep/glob/read_file calls — your agent asks a question in plain
English and gets back the exact functions and structs that answer it, with
source, in well under a second.

tokenuinely slices every file into chunks (one per top-level symbol via
tree-sitter), generates a compact Claude-powered header for each, embeds the
headers with Voyage AI, and serves the whole thing through a JSON-RPC MCP
server backed by local SQLite. Instead of burning tokens on file-by-file
exploration, your agent calls `tokenuinely__query("auth token refresh")` and
gets the right chunks in one round-trip.

## What makes it different

Most code-intelligence tools do either structural analysis (AST, call graphs)
*or* vector search. tokenuinely does both and **fuses** them: every query blends
semantic similarity, BM25 keyword match, and exact-symbol hits into one ranking,
and the index also carries a real symbol/dependency graph (callers, callees,
imports, dead-code).

Each chunk gets a Claude-generated retrieval header:

```
WHY: Refreshes an expired OAuth2 access token and persists the new session
EFFECTS: Redis session store, POST /api/auth/refresh, SESSION_SECRET env var
CALLS: SessionManager, validate_session, redis_set
```

The header is what gets embedded — so search matches *intent*, not just tokens.

## Install

Once a release is tagged, install with any of:

```bash
# Prebuilt binary (macOS / Linux / Windows) — no toolchain needed
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/VanshJP/Tokenuinely/releases/latest/download/tokenuinely-installer.sh | sh

# Homebrew
brew install VanshJP/tap/tokenuinely

# npm
npm install -g tokenuinely

# From crates.io (needs a Rust toolchain)
cargo install tokenuinely
```

### Build from source

```bash
git clone https://github.com/VanshJP/Tokenuinely
cd Tokenuinely/tokenuinely-rs
cargo build --release      # binary at target/release/tokenuinely
```

## Quick start

```bash
# 1. API keys (indexing needs Anthropic; semantic search needs Voyage).
export ANTHROPIC_API_KEY=sk-ant-...
export VOYAGE_API_KEY=pa-...
# (or drop them in a .env file in the repo)

# 2. One-command onboarding for Claude Code, run inside the repo you want indexed:
tokenuinely setup .
```

`setup` registers the MCP server with Claude Code, installs the `Grep|Glob`
PreToolUse hook, appends a hint to `CLAUDE.md`, adds `.tokenuinely/` to your
`.gitignore`, and runs the initial index. After that your agent has the
`tokenuinely__*` tools.

Prefer to do it by hand?

```bash
tokenuinely index .                              # build the index
tokenuinely query "where do we batch embeddings" # search it
claude mcp add tokenuinely --transport stdio -- /path/to/tokenuinely mcp
```

### Using it across many repos

Register once at user scope and let the server resolve the repo from your
working directory:

```bash
tokenuinely setup . --scope user
```

Then `index` each repo you work in. The MCP server picks the right index based
on where Claude Code is running (or an explicit `path` arg / `TOKENUINELY_REPO`).

## MCP tools

When running as an MCP server (`tokenuinely mcp`), three tools are exposed —
each is described to the agent every turn, so the surface is deliberately small:

### `tokenuinely__query`
Fused semantic + keyword + exact-symbol search. Returns the top-k matching
**chunks** (function/struct/class spans) with path, line range, header, and the
actual source. Works without `VOYAGE_API_KEY` too (falls back to BM25).
```json
{"text": "auth token refresh", "k": 5, "include_source": true}
```

### `tokenuinely__inspect_symbol`
One-shot lookup for a symbol: definition (file:line + source), callers, callees,
and imports. On a miss it returns a `suggestions` list of similarly-named
symbols (did-you-mean). No API key needed.
```json
{"symbol": "upsert_file_chunks"}
```

### `tokenuinely__repo_overview`
Compact orientation snapshot: languages, top directories, entry points,
most-called symbols, index freshness, and dead-code count. No API key needed.

## CLI reference

| Command | Description | Needs keys |
|---|---|---|
| `index [path]` | Index or re-index a repo (default `.`) | Anthropic + Voyage |
| `query <text> [-k N] [--path P]` | Fused semantic search | Voyage |
| `search-text <text> [-k N] [path]` | BM25 full-text search over headers/symbols | No |
| `find-symbol <name> [--kind K] [path]` | Find symbol definitions by name pattern | No |
| `trace-deps <symbol> [path]` | What a symbol calls and what calls it | No |
| `architecture [path]` | Architecture overview | No |
| `dead-code [path]` | Functions with zero callers | No |
| `cypher-query <query> [path]` | Cypher-like graph query over symbols/deps | No |
| `eval [--queries F] [path]` | Retrieval eval harness (hit-rate / MRR / latency) | Voyage |
| `status [path]` | Index stats (files, chunks, last indexed) | No |
| `cache clear [path]` | Drop cached query results | No |
| `detect-changes [path]` | Files stale vs. git + hash | No |
| `export [path] [--output F]` / `import <F> [path]` | Share a prebuilt index as `.zst` | No |
| `adr add <title> <body>` / `adr list [path]` | Architecture Decision Records | No |
| `viz [--port N] [path]` | 3D dependency-graph visualization server | No |
| `mcp` | Start the JSON-RPC MCP stdio server | Depends on tool |
| `hook-augment` | Run as a Claude Code PreToolUse hook | Voyage |
| `setup [path]` | Register MCP + hook, gitignore, initial index | for index |

`setup` flags: `--skip-claude-md`, `--skip-index`, `--scope <local\|project\|user>`.

## How it works

### Indexing pipeline

```
walker.walk_repo(root)          # .gitignore + size/binary/extension filters
  └─ per file: sha256_file ──► skip if file hash unchanged
       └─ tree-sitter: extract symbols + deps, slice into chunks
            └─ per-symbol body hash ──► reuse stored header+embedding if unchanged
                 ├─ Phase 1  generate_chunk_header  (Claude Haiku, bounded concurrency)
                 ├─ Phase 2  embed_batch            (Voyage, 1024-dim, batched)
                 └─ Phase 3  upsert chunks + vecs + symbol/dep rows (atomic per file)
```

Two efficiency properties worth knowing:

- **Per-symbol hash diff.** Editing one function in a big file only re-generates
  and re-embeds that function's chunk; the rest carry their stored
  header+embedding forward — no API calls.
- **Partial-failure recovery.** If some chunks fail (e.g. a transient API
  error), the file is stored with what succeeded but flagged so the next
  `index` retries *only* the missing chunks.

### Search

```
query → (cache check) → embed_query (Voyage) ─┐
        BM25 over chunk headers/symbols ───────┼─► fuse: 0.55·vec + 0.30·BM25 + 0.15·exact
        exact symbol-name match ───────────────┘   → top-k chunks with source
```

In-memory cosine over the stored vectors is plenty fast for typical repos.
Identical `(query, k)` searches inside a 5-minute window are served from a local
cache, skipping the Voyage call.

### PreToolUse hook

Registered as a Claude Code hook, tokenuinely injects semantic context into
every `Grep`/`Glob` call under a hard 300 ms deadline — if the index is slow or
absent it exits silently and never blocks the agent.

## Storage

Everything lives in `.tokenuinely/v2/index.db` (SQLite, WAL). Safe to delete and
rebuild. ADRs and metadata share the same database, and a schema-version bump
wipes-and-rebuilds (there is no migration layer by design).

## Environment variables

| Variable | Required | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | for indexing | Claude key for header generation |
| `VOYAGE_API_KEY` | for semantic search | Voyage key for embeddings |
| `TOKENUINELY_REPO` | no | Default repo path for the MCP server |
| `RUST_LOG` | no | Log level, e.g. `tokenuinely=debug` |

Keys can also be set in a `.env` file (loaded via dotenvy). All logs go to
stderr — stdout is the JSON-RPC channel in MCP mode.

## Project structure

```
tokenuinely-rs/
├── Cargo.toml
├── src/
│   ├── main.rs              # CLI entry point (clap)
│   ├── config.rs            # constants, Config, path helpers
│   ├── db.rs                # SQLite layer, embedding codec, cosine similarity
│   ├── hasher.rs            # SHA-256 helpers
│   ├── eval.rs              # retrieval eval harness
│   ├── adr.rs               # Architecture Decision Records
│   ├── export.rs            # zstd export/import
│   ├── detect_changes.rs    # git-aware stale detection
│   ├── watcher.rs           # filesystem watch primitive
│   ├── index/               # walker, symbols, deps, header, embedder, indexer
│   ├── search/              # query (fused), fts (BM25), cypher, architecture
│   └── server/              # mcp (JSON-RPC), viz, hook_augment
├── evals/queries.jsonl      # labelled queries for `tokenuinely eval`
└── tests/                   # unit + integration tests (no API keys needed)
```

## Testing

```bash
cargo test     # 33 tests, none require API keys
```

## License

MIT — see [LICENSE](LICENSE).
