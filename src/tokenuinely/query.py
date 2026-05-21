from __future__ import annotations

import asyncio
from dataclasses import dataclass
from pathlib import Path

from . import db
from .config import Config, index_db_path
from .embedder import Embedder


@dataclass
class QueryHit:
    path: str
    header: str
    distance: float
    language: str | None


async def query_repo(repo_root: Path, text: str, k: int, cfg: Config) -> list[QueryHit]:
    db_path = index_db_path(repo_root.resolve())
    if not db_path.exists():
        raise RuntimeError(
            f"No index found at {db_path}. Run `tokenuinely index` first."
        )
    conn = db.connect(db_path)
    embedder = Embedder(cfg.voyage_api_key)
    qvec = await embedder.embed_query(text)
    results = db.search(conn, qvec, k=k)
    conn.close()
    return [
        QueryHit(path=r.path, header=r.header, distance=d, language=r.language)
        for r, d in results
    ]


def query_repo_sync(repo_root: Path, text: str, k: int, cfg: Config) -> list[QueryHit]:
    return asyncio.run(query_repo(repo_root, text, k, cfg))
