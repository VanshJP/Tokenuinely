from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from mcp.server.fastmcp import FastMCP

from . import db
from .config import Config, index_db_path
from .indexer import index_repo
from .query import query_repo

mcp = FastMCP("tokenuinely")


def _resolve_repo(path: str | None) -> Path:
    if path:
        return Path(path).expanduser().resolve()
    env_root = os.environ.get("TOKENUINELY_REPO")
    if env_root:
        return Path(env_root).expanduser().resolve()
    return Path.cwd().resolve()


@mcp.tool()
async def query(text: str, k: int = 5, path: str | None = None) -> list[dict[str, Any]]:
    """Semantic search over the repo's tokenuinely index.

    Returns the top-k files whose semantic headers best match the query, with
    each file's path, header (SUMMARY/KEY SYMBOLS/TOUCHES/NOT HERE), and
    distance. Use this BEFORE grepping or globbing — it points you at the right
    files in one call.

    Args:
        text: natural-language description of what you're looking for.
        k: number of results (default 5).
        path: repository root. Defaults to TOKENUINELY_REPO env var or CWD.
    """
    cfg = Config.load()
    repo = _resolve_repo(path)
    hits = await query_repo(repo, text, k, cfg)
    return [
        {
            "path": h.path,
            "header": h.header,
            "distance": h.distance,
            "language": h.language,
        }
        for h in hits
    ]


@mcp.tool()
def index_status(path: str | None = None) -> dict[str, Any]:
    """Show whether a repo is indexed, how many files, and when it was last updated."""
    repo = _resolve_repo(path)
    db_path = index_db_path(repo)
    if not db_path.exists():
        return {"indexed": False, "repo": str(repo), "db_path": str(db_path)}
    conn = db.connect(db_path)
    n = db.count_files(conn)
    last = db.get_meta(conn, "last_index_at")
    conn.close()
    return {
        "indexed": True,
        "repo": str(repo),
        "db_path": str(db_path),
        "files_indexed": n,
        "last_index_at": last,
    }


@mcp.tool()
async def reindex(path: str | None = None) -> dict[str, Any]:
    """Rescan the repo and update headers for any files whose contents changed.

    Incremental — only files whose content hash differs from the index get
    re-headered and re-embedded. Cheap to call.
    """
    cfg = Config.load()
    repo = _resolve_repo(path)
    stats = await index_repo(repo, cfg)
    return {
        "scanned": stats.scanned,
        "unchanged": stats.unchanged,
        "indexed": stats.indexed,
        "deleted": stats.deleted,
        "failed": [{"path": p, "error": e} for p, e in stats.failed],
    }


def run() -> None:
    mcp.run()


if __name__ == "__main__":
    run()
