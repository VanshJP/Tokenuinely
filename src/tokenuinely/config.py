from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

from dotenv import load_dotenv

load_dotenv()

EMBED_MODEL = "voyage-3"
EMBED_DIM = 1024
HEADER_MODEL = "claude-haiku-4-5-20251001"

MAX_FILE_BYTES = 100_000
HEADER_INPUT_CHAR_LIMIT = 40_000

EMBED_BATCH_MAX = 128
EMBED_TOKEN_BUDGET = 280_000

INDEX_DIRNAME = ".tokenuinely"
INDEX_FILENAME = "index.db"

DEFAULT_IGNORES = [
    ".git/",
    ".hg/",
    ".svn/",
    "node_modules/",
    ".venv/",
    "venv/",
    "env/",
    "__pycache__/",
    ".mypy_cache/",
    ".pytest_cache/",
    ".ruff_cache/",
    "dist/",
    "build/",
    "target/",
    ".next/",
    ".nuxt/",
    "out/",
    "vendor/",
    ".tokenuinely/",
    ".onetoken/",
    "*.lock",
    "*.min.js",
    "*.min.css",
    "*.map",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "uv.lock",
    "Cargo.lock",
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.ico",
    "*.svg",
    "*.pdf",
    "*.zip",
    "*.tar",
    "*.gz",
    "*.bin",
    "*.so",
    "*.dylib",
    "*.dll",
    "*.exe",
    "*.o",
    "*.a",
    "*.class",
    "*.pyc",
    "*.pyo",
    "*.db",
    "*.sqlite",
    "*.sqlite3",
    ".DS_Store",
]


@dataclass
class Config:
    anthropic_api_key: str
    voyage_api_key: str
    header_concurrency: int = 16
    embed_workers: int = 2
    concurrency: int | None = None

    def __post_init__(self) -> None:
        if self.concurrency is not None:
            self.header_concurrency = self.concurrency
        self.concurrency = self.header_concurrency

    @classmethod
    def load(cls) -> "Config":
        ak = os.environ.get("ANTHROPIC_API_KEY")
        vk = os.environ.get("VOYAGE_API_KEY")
        if not ak:
            raise RuntimeError("ANTHROPIC_API_KEY not set (use .env or export it)")
        if not vk:
            raise RuntimeError("VOYAGE_API_KEY not set (use .env or export it)")
        return cls(anthropic_api_key=ak, voyage_api_key=vk)


def index_db_path(repo_root: Path) -> Path:
    return repo_root / INDEX_DIRNAME / INDEX_FILENAME
