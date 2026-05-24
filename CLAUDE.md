# CLAUDE.md

Contributor guide for Claude Code when working *inside* the `tokenuinely`
source tree. End-user install/usage docs live in `README.md`.

## What this is

`tokenuinely` builds a per-repo semantic index: for every text file it writes a
compact natural-language header (the "why" + key symbols + external touches +
redirects), embeds the header with Voyage, and stores both in a local
SQLite + `sqlite-vec` database at `.tokenuinely/index.db`. A coding agent
queries that index via the `tokenuinely__query` MCP tool instead of grepping.

## Dev environment

- Python ≥3.10, but the checked-in `.venv` uses 3.12. Always invoke
  `.venv/bin/python` and `.venv/bin/tokenuinely` directly — there's no
  `python` on PATH.
- No test suite, no lint config, no CI in-tree. Don't fabricate `pytest`
  invocations.
- Build backend is `hatchling`; source layout is `src/tokenuinely/`.
- Add or remove deps with `uv add` / `uv remove` (do **not** hand-edit
  `pyproject.toml` `dependencies`).

## Module map

| File | Role |
| --- | --- |
| `cli.py` | Typer app: `index`, `query`, `status`, `mcp`, `setup`. |
| `mcp_server.py` | FastMCP server exposing `query`, `index_status`, `reindex` tools over stdio. |
| `indexer.py` | Walk → hash-diff → header-gen (producer) → batched embed (consumer queue) → SQLite upsert. |
| `header.py` | `HeaderGenerator`: Anthropic call that returns the five-line header. |
| `embedder.py` | `Embedder`: `voyageai.AsyncClient` wrapper with `embed_query` + batched `embed_documents`. |
| `db.py` | SQLite + `sqlite-vec` schema, connect (WAL pragmas), upsert/search/delete/meta. |
| `walker.py` | Filesystem walk honoring `.gitignore` + `DEFAULT_IGNORES`, binary/size filtering, language detection. |
| `hasher.py` | SHA-256 helper for content-hash dedupe. |
| `config.py` | `Config` dataclass, model names, byte/char limits, embed batch constants. |

## Invariants — don't break these

1. **Header label grammar is a public contract.** The five lines and labels
   (`WHY:`, `SUMMARY:`, `KEY SYMBOLS:`, `TOUCHES:`, `NOT HERE:`) are referenced
   by `CLAUDE_MD_SNIPPET` in `cli.py`, MCP tool docstrings, and the README.
   Don't rename or reorder them. Headers already written by older versions
   (four-line, no `WHY:`) remain in the DB until reindex — code that parses
   headers must tolerate either shape.
2. **SQLite schema is append-compatible.** Don't change column types or the
   `files_vec` dimension (`EMBED_DIM = 1024`, tied to `voyage-3`). A schema
   change requires a migration story; there is none today.
3. **Per-call `conn.commit()` was intentionally removed from `db.upsert_file`.**
   The indexer commits once per consumer batch. If you add new write helpers,
   either commit there (one-shot writes) or document that the caller commits.
4. **`Config.concurrency` is a back-compat alias** for `header_concurrency`.
   Leave the `__post_init__` reconciliation in place; external callers may
   still pass `concurrency=`.
5. **Voyage call shape.** Use `Embedder.embed_documents([...])` for bulk and
   `embed_query(text)` for single queries. Don't reintroduce one-text-per-call
   document embedding — it was the bottleneck the batched path replaced.
6. **`max_tokens=260`** in `header.py` is deliberately tight. Raise it only
   with a real reason; the prompt is built to fit.
7. **Don't print secrets.** `ANTHROPIC_API_KEY` and `VOYAGE_API_KEY` flow
   through `Config`; never log them or echo them into errors.

## Data flow (indexer)

```
walker.walk(root)
   │  WalkedFile(rel_path, content, truncated, language)
   ▼
hasher.hash_bytes  ──►  db.get_hash  (skip if unchanged)
   │
   ▼
work[]  ──►  producer: _generate_header (under header_sem)
                       │  (WalkedFile, content_hash, header)
                       ▼
                  asyncio.Queue
                       │
                       ▼  cfg.embed_workers consumers
              drain up to EMBED_BATCH_MAX
              embedder.embed_documents(headers)
              db.upsert_file × N  →  conn.commit()
```

Tunables live on `Config`: `header_concurrency` (Anthropic-bound),
`embed_workers` (parallel batched-embed consumers). Embed batching constants
(`EMBED_BATCH_MAX`, `EMBED_TOKEN_BUDGET`) are module-level in `config.py`.

## Running things locally

```bash
# Status (no API calls)
.venv/bin/tokenuinely status .

# Full reindex (COSTS Anthropic + Voyage credits — ask the user first)
.venv/bin/tokenuinely index .

# Query (costs one Voyage embedding call)
.venv/bin/tokenuinely query "where do we batch embedding requests"

# MCP server (stdio; Claude Code launches this, you rarely run it by hand)
.venv/bin/tokenuinely mcp
```

The index DB lives at `.tokenuinely/index.db`; safe to delete and rebuild.

## Conventions

- `from __future__ import annotations` at the top of every module.
- Dataclasses for plain records (`Config`, `IndexStats`, `WalkedFile`,
  `FileRecord`, `QueryHit`).
- `async`/`await` end-to-end for anything touching Anthropic, Voyage, or the
  MCP server. SQLite calls stay sync (single event-loop thread).
- Use `rich.console.Console` and `rich.progress.Progress` for user-facing
  output — don't reach for `print`.
- Errors that should bubble to the CLI raise `RuntimeError` with a
  human-readable message; `cli.py` converts them to `typer.Exit(1)`.

## Things to avoid

- Don't run `tokenuinely index` or `tokenuinely query` against this repo
  without explicit user permission — both spend real API credits.
- Don't add new top-level packages. The `src/tokenuinely/` layout is
  intentional (matches `tool.hatch.build.targets.wheel`).
- Don't create `tests/` or `docs/` directories speculatively. There aren't any
  yet; adding them is a product decision, not a refactor.
- Don't hand-edit `.tokenuinely/index.db`. If you need to inspect it, use
  `sqlite3 .tokenuinely/index.db`.
