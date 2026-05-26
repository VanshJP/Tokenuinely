use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub const EMBED_DIM: usize = 1024;
pub const HEADER_MODEL: &str = "claude-haiku-4-5-20251001";
pub const EMBED_MODEL: &str = "voyage-3";
pub const MAX_FILE_BYTES: u64 = 100_000;
/// Per-chunk source slice fed to the header LLM. Chunks are small by design.
pub const HEADER_INPUT_CHAR_LIMIT: usize = 6_000;
/// Per-chunk source bytes returned inline in query results (cap to keep tokens bounded).
pub const QUERY_SOURCE_CHAR_LIMIT: usize = 2_400;
pub const EMBED_BATCH_MAX: usize = 128;
pub const HEADER_CONCURRENCY: usize = 16;
/// Bumped from ".tokenuinely" — v2 stores chunk-level rows instead of file-level.
/// Old indexes are abandoned; a fresh `tokenuinely index` rebuilds under the new dir.
pub const INDEX_DIRNAME: &str = ".tokenuinely/v2";
pub const INDEX_FILENAME: &str = "index.db";
/// Schema version stored in the `meta` table. Bumped → DB is wiped + rebuilt.
pub const SCHEMA_VERSION: &str = "2";

pub const DEFAULT_IGNORES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "out",
    "vendor",
    ".tokenuinely",
    ".onetoken",
    "tokenuinely-index.zst",
];

pub const DEFAULT_IGNORE_EXTENSIONS: &[&str] = &[
    "lock", "min.js", "min.css", "map", "png", "jpg", "jpeg", "gif", "ico", "svg", "pdf", "zip",
    "tar", "gz", "bin", "so", "dylib", "dll", "exe", "o", "a", "class", "pyc", "pyo", "db",
    "sqlite", "sqlite3",
];

pub const DEFAULT_IGNORE_FILENAMES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "uv.lock",
    "Cargo.lock",
    ".DS_Store",
];

pub const CLAUDE_MD_SNIPPET: &str = r#"
## tokenuinely — semantic code search

Before running `grep`/`glob`/`find` to discover files, call the `tokenuinely__query`
MCP tool first with a natural-language description of what you're looking for.
Fall back to text search only if semantic results don't cover the question.
"#;

#[derive(Clone, Debug)]
pub struct Config {
    pub anthropic_api_key: Option<String>,
    pub voyage_api_key: Option<String>,
    pub header_concurrency: usize,
    #[allow(dead_code)]
    pub embed_batch_max: usize,
}

impl Config {
    /// Load config with optional API keys. Structural tools work without keys.
    pub fn load() -> Result<Self> {
        let _ = dotenvy::dotenv();
        let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY").ok();
        let voyage_api_key = std::env::var("VOYAGE_API_KEY").ok();
        Ok(Self {
            anthropic_api_key,
            voyage_api_key,
            header_concurrency: HEADER_CONCURRENCY,
            embed_batch_max: EMBED_BATCH_MAX,
        })
    }

    /// Load config requiring API keys (for indexing/embedding operations).
    #[allow(dead_code)]
    pub fn load_require_keys() -> Result<Self> {
        let _ = dotenvy::dotenv();
        let anthropic_api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY not set (required for indexing)")?;
        let voyage_api_key = std::env::var("VOYAGE_API_KEY")
            .context("VOYAGE_API_KEY not set (required for indexing)")?;
        Ok(Self {
            anthropic_api_key: Some(anthropic_api_key),
            voyage_api_key: Some(voyage_api_key),
            header_concurrency: HEADER_CONCURRENCY,
            embed_batch_max: EMBED_BATCH_MAX,
        })
    }

    pub fn require_anthropic_key(&self) -> Result<&str> {
        self.anthropic_api_key.as_deref().ok_or_else(|| {
            anyhow::anyhow!("ANTHROPIC_API_KEY not set (required for header generation)")
        })
    }

    pub fn require_voyage_key(&self) -> Result<&str> {
        self.voyage_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("VOYAGE_API_KEY not set (required for semantic search). Use 'search-text' for API-free text search."))
    }
}

#[allow(dead_code)]
pub fn index_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(INDEX_DIRNAME).join(INDEX_FILENAME)
}

pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(INDEX_DIRNAME).join(INDEX_FILENAME).exists() {
            return Some(current);
        }
        // Also accept the legacy v1 location so users mid-upgrade aren't stranded.
        if current.join(".tokenuinely").join("index.db").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}
