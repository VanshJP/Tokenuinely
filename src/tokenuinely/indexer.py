from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

from rich.console import Console
from rich.progress import (
    BarColumn,
    MofNCompleteColumn,
    Progress,
    TextColumn,
    TimeElapsedColumn,
)

from . import db
from .config import Config, index_db_path
from .embedder import Embedder
from .hasher import hash_bytes
from .header import HeaderGenerator
from .walker import WalkedFile, walk


@dataclass
class IndexStats:
    scanned: int = 0
    unchanged: int = 0
    indexed: int = 0
    deleted: int = 0
    failed: list[tuple[str, str]] = field(default_factory=list)


async def _process_one(
    wf: WalkedFile,
    content_hash: str,
    header_gen: HeaderGenerator,
    embedder: Embedder,
    sem: asyncio.Semaphore,
) -> tuple[str, str, list[float]] | tuple[None, str, str]:
    async with sem:
        try:
            header = await header_gen.generate(
                wf.rel_path, wf.language, wf.content, wf.truncated
            )
            embedding = await embedder.embed_document(header)
            return wf.rel_path, header, embedding
        except Exception as e:  # noqa: BLE001
            return None, wf.rel_path, f"{type(e).__name__}: {e}"


async def index_repo(repo_root: Path, cfg: Config, console: Console | None = None) -> IndexStats:
    console = console or Console()
    repo_root = repo_root.resolve()
    db_path = index_db_path(repo_root)
    conn = db.connect(db_path)
    stats = IndexStats()

    seen_paths: set[str] = set()
    work: list[tuple[WalkedFile, str]] = []

    console.print(f"[dim]scanning {repo_root}…[/dim]")
    for wf in walk(repo_root):
        stats.scanned += 1
        seen_paths.add(wf.rel_path)
        ch = hash_bytes(wf.content.encode("utf-8", errors="replace"))
        existing = db.get_hash(conn, wf.rel_path)
        if existing == ch:
            stats.unchanged += 1
            continue
        work.append((wf, ch))

    known = db.get_all_paths(conn)
    to_delete = known - seen_paths
    if to_delete:
        stats.deleted = db.delete_paths(conn, to_delete)

    if not work:
        console.print(
            f"[green]nothing to do[/green] — {stats.scanned} files scanned, "
            f"{stats.unchanged} unchanged, {stats.deleted} removed"
        )
        return stats

    header_gen = HeaderGenerator(cfg.anthropic_api_key)
    embedder = Embedder(cfg.voyage_api_key)
    sem = asyncio.Semaphore(cfg.concurrency)

    now = datetime.now(timezone.utc).isoformat()

    with Progress(
        TextColumn("[bold]indexing"),
        BarColumn(),
        MofNCompleteColumn(),
        TextColumn("•"),
        TimeElapsedColumn(),
        console=console,
    ) as progress:
        task_id = progress.add_task("indexing", total=len(work))

        async def runner(wf: WalkedFile, ch: str) -> None:
            result = await _process_one(wf, ch, header_gen, embedder, sem)
            if result[0] is None:
                _, path, err = result
                stats.failed.append((path, err))
            else:
                path, header, embedding = result
                db.upsert_file(
                    conn,
                    path=path,
                    content_hash=ch,
                    header=header,
                    language=wf.language,
                    generated_at=now,
                    embedding=embedding,
                )
                stats.indexed += 1
            progress.advance(task_id)

        await asyncio.gather(*(runner(wf, ch) for wf, ch in work))

    db.set_meta(conn, "last_index_at", now)
    conn.close()
    return stats
