# tokenuinely

Per-file semantic headers + embedding index for agentic code retrieval.

`tokenuinely` generates a compact natural-language header for every file in a repo
(what it does, key symbols, what it touches, where related things live), embeds
those headers, and stores them in a local SQLite vector index. Claude Code (or
any MCP-compatible agent) can then semantically retrieve the right files in one
call — instead of burning thousands of tokens on `grep` / `glob` / `find` until
it stumbles on the relevant code.

Content-hash tracking makes re-indexing incremental: only files whose contents
changed get a new header and a new embedding.

---

## Try it yourself

Just want to kick the tires? Three commands:

```bash
# 1. Install from GitHub
uv tool install --python 3.12 git+https://github.com/VanshJP/tokenuinely

# 2. Grab API keys (free tiers are plenty for personal use):
#    - Anthropic: https://console.anthropic.com/settings/keys
#    - Voyage:    https://dash.voyageai.com/api-keys
export ANTHROPIC_API_KEY=sk-ant-...
export VOYAGE_API_KEY=pa-...

# 3. cd into any repo you want to play with, then:
tokenuinely setup
```

That registers the MCP server with Claude Code and indexes the current repo. Restart Claude Code, then ask it something about your codebase — it'll call `tokenuinely__query` instead of grepping around.

To try it on another repo later: `cd` in and run `tokenuinely index`. Indexing is incremental (content-hashed), so re-runs are cheap.

---

## Quickstart with Claude Code

**Three commands.** Run these in any terminal:

```bash
# 1. Install tokenuinely globally
uv tool install --python 3.12 git+https://github.com/VanshJP/tokenuinely
# (once published to PyPI, this will simply be:  uv tool install tokenuinely)

# 2. Run the guided setup
tokenuinely setup
```

`tokenuinely setup` will:
1. Prompt you for an Anthropic API key (https://console.anthropic.com/settings/keys) and a Voyage API key (https://dash.voyageai.com/api-keys), and offer to write them to your shell profile.
2. Register the MCP server with Claude Code via `claude mcp add tokenuinely -- tokenuinely mcp` (user-scope, so it's available in every repo).
3. Append a usage hint to `CLAUDE.md` in the current repo so Claude knows to reach for `tokenuinely__query` before grepping.
4. Build the initial index of the current repo.

```bash
# 3. Re-open Claude Code (or restart it) and the tokenuinely__query tool will be available.
```

That's it. Ask Claude about your codebase and watch it use semantic retrieval
instead of fishing around with text search.

### Per-repo indexing

The setup step indexes whichever repo you ran it in. For other repos, just `cd`
in and run:

```bash
tokenuinely index
```

It's incremental — only changed files cost API calls.

---

## Manual install (from source)

Until `tokenuinely` is published to PyPI, install from the GitHub repo:

```bash
# Direct from git (no clone needed)
uv tool install --python 3.12 git+https://github.com/VanshJP/tokenuinely

# Or from a local clone
git clone https://github.com/VanshJP/tokenuinely.git && cd tokenuinely
uv tool install --python 3.12 .
```

Then set your API keys and run setup:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
export VOYAGE_API_KEY=pa-...
tokenuinely setup
```

If `tokenuinely setup` can't find the `claude` CLI, register manually:

```bash
claude mcp add tokenuinely --scope user -- $(which tokenuinely) mcp
```

And add this snippet to your repo's `CLAUDE.md`:

> Before running `grep`/`glob`/`find` to discover files, call the
> `tokenuinely__query` MCP tool first with a natural-language description of
> what you're looking for. Fall back to text search only if semantic results
> don't cover the question.

---

## Commands

| Command | What it does |
| --- | --- |
| `tokenuinely setup` | Guided one-shot install: keys, MCP registration, initial index. |
| `tokenuinely index [path]` | Index or incrementally update a repo. Defaults to `.`. |
| `tokenuinely query "<text>"` | Semantic search. `--k 5` to change result count. |
| `tokenuinely status` | Show how many files are indexed and when last updated. |
| `tokenuinely mcp` | Run the MCP server (stdio). Used by Claude Code; you don't call this directly. |

The index lives at `.tokenuinely/index.db` inside each repo.

---

## How it works

1. Walk the repo, respecting `.gitignore` and a sane default ignorelist.
2. SHA-256 each text file. If the hash matches the indexed copy, skip.
3. Otherwise send the file to Claude Haiku 4.5 with a fixed schema prompt → get
   a compact header (~5 lines).
4. Embed the header with Voyage `voyage-3` (1024-d).
5. Upsert into SQLite + `sqlite-vec`.
6. Files removed from disk are removed from the index.

Queries embed with `input_type="query"` and do a `vec0` ANN search.

### Header format

```
SUMMARY: One sentence on what this file does.
KEY SYMBOLS: parseJWT, refreshToken, rotateSession, ...
TOUCHES: db.sessions, redis.cache, /auth/refresh endpoint, env.JWT_SECRET
NOT HERE: OAuth flows → src/auth/oauth/, session storage → src/session/store.ts
```

The `NOT HERE` line is the secret sauce: it stops the agent from following the
wrong scent into a related-but-not-this-file direction.

---

## Requirements

- **Python 3.10+**, built with SQLite extension loading. macOS users: the
  python.org installer disables this — use Homebrew (`brew install python@3.12`)
  or `pyenv`. tokenuinely will print a clear error if your Python can't load
  `sqlite-vec`.
- **Anthropic API key** for header generation (uses Claude Haiku 4.5; ~$0.001 per file at indexing time).
- **Voyage API key** for embeddings (free tier covers most personal codebases).

---

## Not in v0.1

- Pre-commit hook for auto-regeneration on file changes.
- Audit pass that re-checks header semantic accuracy.
- Function-level chunking (currently file-level).

These are the natural next steps. PRs welcome.

## License

MIT.
