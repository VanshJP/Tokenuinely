# tokenuinely

**Semantic codebase index for AI coding agents.** One MCP tool call replaces a
flurry of grep/glob/read_file calls — your agent asks a question in plain
English and gets back the exact functions and structs that answer it, with
source, in well under a second.

tokenuinely slices every file into chunks (one per top-level symbol via
tree-sitter), generates a compact Claude-powered header for each, embeds the
headers with Voyage AI, and serves the whole thing through a JSON-RPC MCP
server backed by local SQLite. Every query **fuses** semantic similarity, BM25
keyword match, and exact-symbol hits into one ranking, over an index that also
carries a real symbol/dependency graph (callers, callees, imports, dead-code).

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

Or build from source:

```bash
git clone https://github.com/VanshJP/Tokenuinely
cd Tokenuinely/tokenuinely-rs
cargo build --release      # binary at target/release/tokenuinely
```

## Quick start

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # indexing (header generation)
export VOYAGE_API_KEY=pa-...          # semantic search (embeddings)

# One-command onboarding for Claude Code, run inside the repo you want indexed:
tokenuinely setup .
```

`setup` registers the MCP server with Claude Code, installs the `Grep|Glob`
PreToolUse hook, appends a hint to `CLAUDE.md`, adds `.tokenuinely/` to your
`.gitignore`, and runs the initial index. Your agent then has the
`tokenuinely__query`, `tokenuinely__inspect_symbol`, and
`tokenuinely__repo_overview` tools.

## Documentation

The crate lives in [`tokenuinely-rs/`](tokenuinely-rs). **Full documentation —
MCP tools, the complete CLI reference, indexing/search internals, and
configuration — is in [`tokenuinely-rs/README.md`](tokenuinely-rs/README.md).**

## License

MIT — see [LICENSE](tokenuinely-rs/LICENSE).
