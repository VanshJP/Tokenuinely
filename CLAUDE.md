# CLAUDE.md

Contributor guide for Claude Code when working *inside* the `tokenuinely`
source tree. End-user install/usage docs live in
[`tokenuinely-rs/README.md`](tokenuinely-rs/README.md).

## What this is

`tokenuinely` builds a per-repo semantic index. It walks a repo, slices each
file into chunks (one per top-level symbol via tree-sitter), writes a compact
natural-language header per chunk with Claude, embeds the headers with Voyage,
and stores chunks + vectors + a symbol/dependency graph in a local SQLite
database at `.tokenuinely/v2/index.db`. A coding agent queries the index through
MCP tools instead of grepping.

The project is a single Rust crate in `tokenuinely-rs/` (binary `tokenuinely`,
edition 2021). The git repo root (`Tokenuinely/`) holds release tooling
(`dist-workspace.toml`, `.github/workflows/release.yml`).

## Dev environment

- Rust stable (the crate targets edition 2021). Build with `cargo build`
  (binary at `tokenuinely-rs/target/debug/tokenuinely`) or `cargo build
  --release`. Run all cargo commands from inside `tokenuinely-rs/`.
- `cargo test` — 33 tests, none require API keys. `cargo clippy --all-targets`
  and `cargo fmt` should stay clean.
- Add/remove deps with `cargo add` / `cargo remove`.
- API keys flow through env (`ANTHROPIC_API_KEY`, `VOYAGE_API_KEY`) or a `.env`
  file loaded by dotenvy.

## Module map (`tokenuinely-rs/src/`)

| Path | Role |
| --- | --- |
| `main.rs` | clap CLI: `index`, `query`, `search-text`, `find-symbol`, `trace-deps`, `architecture`, `dead-code`, `cypher-query`, `eval`, `status`, `cache`, `mcp`, `hook-augment`, `viz`, `export`/`import`, `detect-changes`, `adr`, `setup`. |
| `config.rs` | `Config`, model names, byte/char limits, `SCHEMA_VERSION`, `INDEX_DIRNAME`, `find_repo_root`. |
| `db.rs` | SQLite schema + connection (WAL), chunk/symbol/dep upserts, FTS-free queries, `encode_embedding`/`decode_embedding`, `cosine_similarity`, `now_unix`, query-cache helpers. |
| `hasher.rs` | `sha256_file` (whole-file) and `sha256_str` (per-chunk body). |
| `index/` | `walker` (gitignore/size/binary filtering), `symbols` + `deps` + `ts_lang` (tree-sitter), `header` (Anthropic call), `embedder` (Voyage batch), `indexer` (the pipeline). |
| `search/` | `query` (fused vec+BM25+exact ranking + cache), `fts` (BM25 over headers), `cypher`, `architecture`. |
| `server/` | `mcp` (JSON-RPC stdio), `hook_augment` (PreToolUse hook), `viz`. |
| `eval.rs` | retrieval eval harness (`tokenuinely eval`). |
| `adr.rs`, `export.rs`, `detect_changes.rs`, `watcher.rs` | ADRs, zstd export/import, git-aware staleness, fs-watch primitive. |

## Invariants — don't break these

1. **MCP surface is three tools.** `tokenuinely__query`,
   `tokenuinely__inspect_symbol`, `tokenuinely__repo_overview` (in
   `server/mcp.rs`). Their descriptions sit in the agent's context every turn,
   so keep them tight; niche functionality stays a CLI subcommand.
2. **Header grammar is a contract.** Chunk headers are `WHY: / EFFECTS: /
   CALLS:`; whole-file headers (no extractable symbols) are `WHY: / EFFECTS: /
   NOT HERE:` (see `index/header.rs`). The embedded header is what search
   matches, so don't rename/reorder labels casually.
3. **Schema change = wipe and rebuild.** Bump `SCHEMA_VERSION` in `config.rs`
   and `assert_schema_version` wipes the data tables on next open. There is no
   migration layer by design. `EMBED_DIM = 1024` is tied to `voyage-3`.
4. **Per-symbol hash diff.** `chunks.body_sha256` lets a reindex reuse a
   chunk's stored header+embedding when its body is unchanged — don't drop it;
   it's the main cost saver. Pair it with the partial-failure path (#5).
5. **Partial-failure recovery.** A file with some failed chunks is stored with
   the surviving chunks but a `PARTIAL_INDEX_SENTINEL` (empty) hash, so the next
   `index` reprocesses it. `file_unchanged()` is the single place that decides a
   skip — route skip logic through it.
6. **Don't print secrets.** `ANTHROPIC_API_KEY` / `VOYAGE_API_KEY` never get
   logged or echoed into errors.
7. **stdout is the JSON-RPC channel in MCP mode.** All logs go to stderr
   (`tracing` is configured for stderr). Don't `println!` diagnostics.

## Data flow (indexer)

```
walker::walk_repo(root)                       # gitignore + size/binary/ext filters
  └─ per file: hasher::sha256_file → file_unchanged()  (skip if unchanged)
       └─ tree-sitter: extract_symbols + extract_deps → build_chunks (per-symbol body hash)
            └─ db.existing_chunks_for_reuse → reuse header+embedding if body hash matches
                 ├─ Phase 1  index/header::generate_chunk_header  (Anthropic, header_concurrency)
                 ├─ Phase 2  index/embedder::embed_batch          (Voyage, 1024-dim, EMBED_BATCH_MAX)
                 └─ Phase 3  db.upsert_file_chunks + replace_symbols/replace_deps  (atomic per file)
```

`Config.header_concurrency` bounds the Anthropic calls; embed batching constants
live in `config.rs`. A successful write clears the query cache (chunk IDs are
reassigned).

## Running things locally (from `tokenuinely-rs/`)

```bash
cargo run -- status .          # no API calls
cargo run -- index .           # COSTS Anthropic + Voyage credits — ask the user first
cargo run -- query "where do we batch embedding requests"   # one Voyage call
cargo run -- eval .            # runs the query pipeline over evals/queries.jsonl — costs Voyage
cargo run -- mcp               # stdio MCP server; Claude Code launches this
```

The index DB lives at `.tokenuinely/v2/index.db`; safe to delete and rebuild.
Inspect it read-only with `sqlite3 .tokenuinely/v2/index.db`.

## Conventions

- Plain records are structs (`Config`, `IndexStats`, `ChunkRecord`,
  `QueryHit`, `PendingChunk`, …).
- `async`/`await` for anything touching Anthropic, Voyage, or the MCP server;
  SQLite calls stay sync.
- Errors bubble as `anyhow::Result`; `main.rs` surfaces them.
- User-facing CLI output uses `indicatif` progress bars + `eprintln!`/`println!`
  per command; structured logs use `tracing` (stderr only).

## Things to avoid

- Don't run `index`, `query`, or `eval` against a repo without explicit user
  permission — they spend real Anthropic/Voyage credits.
- Don't hand-edit `.tokenuinely/v2/index.db`; use `sqlite3` to inspect.
- Don't add a migration layer — schema bumps wipe and rebuild on purpose.
- Don't widen the MCP tool surface or loosen header `max_tokens` without a real
  reason; both are deliberately tight.
