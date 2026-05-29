# Roadmap

What's missing before `tokenuinely` is something I'd actually reach for over
`Grep`/`Read` on a real codebase. Ordered by what would change my behavior most.

Each item lists: **why**, **rough shape**, **how to know it worked**.

---

## P0 — adoption blockers

### 1. Per-symbol hash diff — ✅ DONE

**Why.** Today the indexer hashes whole files. Changing one line in a 30-symbol
file re-runs 30 Anthropic header calls and 30 Voyage embeddings. On a hot file
in active development that's a real bill — and the slow loop discourages
running `index` often enough to keep results fresh.

**Shape.**
- Add `chunks.body_sha256` (SHA-256 of the chunk's source text).
- In `index/indexer.rs`, before queueing a chunk for header generation: hash
  its source slice, look up existing `(path, symbol)` → `body_sha256`, skip
  if unchanged.
- Carry the previous header/embedding forward into the new file row instead.
- Bump `SCHEMA_VERSION` to `3`; the existing wipe-and-rebuild path handles it.

**Validation.** Re-index after a one-line edit to `src/db.rs`. Expect ≤2 chunks
re-embedded (the touched fn + maybe its neighbour if line spans shifted), not
the whole file.

---

### 2. Reranker pass on top-50 → top-5

**Why.** The fused score (vec 0.55 + BM25 0.30 + exact 0.15) gets close, but
without a reranker the agent still wastes one or two `Read` calls on
plausible-but-wrong chunks per query. A single `voyage/rerank-2` call on the
50-chunk recall pool typically halves false positives for one extra API
round-trip — strictly worth it.

**Shape.**
- Extend `index/embedder.rs` with `rerank(query, &[doc]) -> Vec<f32>`.
- In `search/query.rs::search`, after fusing scores but before truncating to
  `opts.k`: send the top-50 chunk headers to rerank, replace scores with
  rerank output, re-sort.
- Add `SearchOpts::rerank: bool` (default `true` when `VOYAGE_API_KEY` is
  present, `false` otherwise so FTS-only fallback stays free).

**Validation.** Hand-label 20 queries across a real repo with the "right"
chunk; measure top-1 hit rate before/after. Target: ≥30% reduction in
"correct chunk is rank 2–5".

---

### 3. Eval harness — ✅ DONE (`tokenuinely eval`)

**Why.** Right now changes to the ranking pipeline are vibes. We have no way to
know whether bumping `WEIGHT_VEC` from 0.55 to 0.65 made things better or
worse, or whether the new chunk-aware header prompt actually beats the old
file-level one. Without this, every future tuning decision is guesswork —
and this is the single most important missing piece.

**Shape.**
- New `evals/` directory (excluded from `walker.rs` via `DEFAULT_IGNORES`).
- `evals/queries.jsonl`: `{"query": "...", "expected_path": "...", "expected_symbol": "..."}`.
- `evals/run.rs` binary (or `cargo test --test evals` gated behind an env
  var so it doesn't run in CI by default): loads queries, calls
  `search::query::search` against a pre-built test index, reports
  top-1/top-3/top-5 hit rate, mean reciprocal rank, mean latency.
- Seed with 20–30 queries against this repo as a starter.

**Validation.** Run baseline; check in the numbers. Every subsequent ranking
change has to move at least one of those numbers in the right direction.

---

## P1 — scale + freshness

### 4. Swap in-memory cosine for `sqlite-vec`

**Why.** `db::all_chunks_with_vecs` loads every embedding into RAM on every
query. Fine at ~5k chunks (this repo), painful at ~50k, broken at ~500k.
`sqlite-vec` does the KNN inside SQLite with an index.

**Shape.**
- Add `sqlite-vec` to `Cargo.toml`.
- Replace `chunk_vecs` with a `vec0` virtual table; embedding stays 1024-dim.
- `search/query.rs` drops the in-memory scoring loop, runs a `MATCH` query
  against the vec table for the top-50 candidates.
- Drop the now-unused `db::cosine_similarity` helper (keep in tests).

**Validation.** Synthetic load test: 100k chunks, query latency p50/p95
before/after. Target: p95 under 100ms on a laptop.

---

### 5. Wire `watcher.rs` into incremental reindex

**Why.** `src/watcher.rs` exists and works in isolation — but nothing calls it.
A long-running `tokenuinely watch` mode would keep the index fresh as you
edit, so `query` results never lag behind reality. This pairs naturally with
P0-#1 (per-symbol hash diff) — without that, watch-mode would be a steady
trickle of API spend.

**Shape.**
- New CLI subcommand: `tokenuinely watch [PATH]`.
- Start the existing `watcher::start_watcher` with a debounced channel.
- On each batched event: read the affected files, run the same chunk →
  header → embed → upsert path as `index_repo`, but only for the touched
  files.
- Print a one-line status to stderr per batch.

**Validation.** Edit a file, wait 1s, run a query that matches the new code.
The chunk should appear with the new content, not the stale version.

---

### 6. Incremental FTS instead of rebuild-per-query

**Why.** `search/query.rs` calls `fts::create_fts_table` + `populate_fts` on
every query. That's a full `DELETE FROM fts_index; INSERT …` cycle — fine
at 5k chunks, lazy at 50k. The FTS table should be maintained as chunks are
upserted.

**Shape.**
- Move FTS row writes into `db::upsert_file_chunks` (alongside `chunk_vecs`).
- Drop the per-query `populate_fts` call.
- Add a `--rebuild-fts` flag to `tokenuinely index` for the rare case where
  the virtual table needs rebuilding (e.g. after a schema bump).

**Validation.** Query latency on a 50k-chunk index drops by the time
currently spent in `populate_fts`. Measure via the eval harness (P0-#3).

---

## P2 — agent ergonomics

### 7. Fuzzy fallback in `inspect_symbol` — ✅ DONE

**Why.** `tokenuinely__inspect_symbol` requires an exact symbol-name match
today. Agents routinely guess "loginUser" when the actual symbol is
"login_user" — and get nothing back instead of a "did you mean…?".

**Shape.**
- In `server/mcp.rs::handle_tool_call` for `inspect_symbol`: if exact lookup
  returns nothing, fall back to `db.find_symbols(name, None)` (which already
  does `LIKE %name%`) and return up to 5 candidates with a `suggestions:`
  field so the agent can pick.

**Validation.** Manual test: `inspect_symbol("upsertChunk")` against this
repo returns `upsert_file_chunks` as a suggestion.

---

### 8. Query result caching — ✅ DONE (`tokenuinely cache clear`)

**Why.** Agents ask the same question multiple times in a session ("show me
the auth code", then 10 minutes later "where's the auth code again"). Each
costs a Voyage embedding call. A small LRU keyed on `(repo, query)` with a
~5-minute TTL would absorb most of that.

**Shape.**
- New `meta` rows: `cache:<sha256(query)>` → JSON of `(timestamp, top_k_chunk_ids)`.
- In `search::query::search`: check cache first; on hit, re-fetch the chunk
  rows by ID and return (so even a stale cache reflects the latest source).
- TTL enforced at read time (drop rows older than 5min).
- New CLI: `tokenuinely cache clear`.

**Validation.** Second identical query in the same session should skip the
Voyage call. Verify via `tracing` logs.

---

### 9. Drop v1 backward-compat

**Why.** `config::find_repo_root` currently accepts both `.tokenuinely/v2/index.db`
*and* the legacy `.tokenuinely/index.db`. That's a kindness for users mid-upgrade
from v0.3 → v0.4 but it's load-bearing nothing forever. Should be removed once
the next minor version ships.

**Shape.**
- Delete the second `if` branch in `config::find_repo_root`.
- Add a one-line note to release notes telling stragglers to re-run `index`.

**Validation.** `cargo test` still green; a repo with only `.tokenuinely/index.db`
should now fail with "no index found" instead of silently using the stale v1 DB.

---

## P3 — nice-to-haves

- **Metrics endpoint**. A `tokenuinely stats` subcommand showing index size,
  per-query latency p50/p95 (from a rolling buffer in `meta`), cache hit
  rate. Useful for users to know whether the tool is actually helping them.
- **Multi-repo MCP**. Today MCP resolves to one repo per invocation via
  `TOKENUINELY_REPO` or arg. A monorepo / workspace use case would want
  one MCP server serving multiple indexes.
- **`.tokenuineignore`**. Power users want to exclude specific paths
  (generated code, vendored libs) without editing `DEFAULT_IGNORES`.
- **Symbol-graph chunks**. A chunk currently maps 1:1 to a top-level symbol.
  Closely-coupled symbol clusters (a struct + its impl block + its tests)
  might be better returned as one chunk. Worth an A/B in the eval harness.

---

## Out of scope

Things deliberately *not* on the roadmap:

- **Local embedding model.** Voyage + Anthropic latency is fine; the cost
  optimisation that matters is reducing *how many* embeddings we generate
  (P0-#1), not who generates them.
- **Migration story.** Schema bumps wipe and rebuild. The rebuild cost is
  bounded by the corpus size and one-shot per upgrade; building a real
  migration layer would cost more engineering than it saves users.
- **More languages.** The 7 tree-sitter languages cover the realistic use
  cases. Adding more is mechanical; do it on demand, not speculatively.
