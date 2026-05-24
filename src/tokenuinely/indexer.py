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
from .config import EMBED_BATCH_MAX, Config, index_db_path
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


async def _generate_header(
    wf: WalkedFile,
    header_gen: HeaderGenerator,
    sem: asyncio.Semaphore,
) -> tuple[WalkedFile, str | None, str | None]:
    async with sem:
        try:
            header = await header_gen.generate(
                wf.rel_path, wf.language, wf.content, wf.truncated
            )
            return wf, header, None
        except Exception as e:  # noqa: BLE001
            return wf, None, f"{type(e).__name__}: {e}"


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
    header_sem = asyncio.Semaphore(cfg.header_concurrency)

    now = datetime.now(timezone.utc).isoformat()

    DONE: object = object()
    embed_queue: asyncio.Queue[object] = asyncio.Queue()

    with Progress(
        TextColumn("[bold]indexing"),
        BarColumn(),
        MofNCompleteColumn(),
        TextColumn("•"),
        TimeElapsedColumn(),
        console=console,
    ) as progress:
        task_id = progress.add_task("indexing", total=len(work))

        async def producer() -> None:
            async def one(wf: WalkedFile, ch: str) -> None:
                wf2, header, err = await _generate_header(wf, header_gen, header_sem)
                if err or header is None:
                    stats.failed.append((wf2.rel_path, err or "empty header"))
                    progress.advance(task_id)
                else:
                    await embed_queue.put((wf2, ch, header))

            await asyncio.gather(*(one(wf, ch) for wf, ch in work))
            for _ in range(cfg.embed_workers):
                await embed_queue.put(DONE)

        async def consumer() -> None:
            while True:
                item = await embed_queue.get()
                if item is DONE:
                    return
                batch: list[tuple[WalkedFile, str, str]] = [item]  # type: ignore[list-item]
                while len(batch) < EMBED_BATCH_MAX:
                    try:
                        nxt = embed_queue.get_nowait()
                    except asyncio.QueueEmpty:
                        break
                    if nxt is DONE:
                        await embed_queue.put(DONE)
                        break
                    batch.append(nxt)  # type: ignore[arg-type]
                headers = [h for _, _, h in batch]
                vecs = await embedder.embed_documents(headers)
                for (wf2, ch, header), vec in zip(batch, vecs):
                    db.upsert_file(
                        conn,
                        path=wf2.rel_path,
                        content_hash=ch,
                        header=header,
                        language=wf2.language,
                        generated_at=now,
                        embedding=vec,
                    )
                    stats.indexed += 1
                    progress.advance(task_id)
                conn.commit()

        await asyncio.gather(
            producer(), *[consumer() for _ in range(cfg.embed_workers)]
        )

    db.set_meta(conn, "last_index_at", now)
    conn.close()
    return stats
