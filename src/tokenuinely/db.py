from __future__ import annotations

import json
import sqlite3
import struct
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

import sqlite_vec

from .config import EMBED_DIM


@dataclass
class FileRecord:
    path: str
    content_hash: str
    header: str
    language: str | None
    generated_at: str


def _pack(vec: list[float]) -> bytes:
    return struct.pack(f"{len(vec)}f", *vec)


def connect(db_path: Path) -> sqlite3.Connection:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    if not hasattr(conn, "enable_load_extension"):
        raise RuntimeError(
            "This Python build was compiled without SQLite extension loading "
            "(common on the macOS python.org installer). Use a Homebrew Python "
            "(e.g. `/opt/homebrew/bin/python3.12 -m venv .venv`) or a pyenv build."
        )
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA temp_store=MEMORY")
    _init_schema(conn)
    return conn


def _init_schema(conn: sqlite3.Connection) -> None:
    conn.executescript(
        f"""
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT UNIQUE NOT NULL,
            content_hash TEXT NOT NULL,
            header TEXT NOT NULL,
            language TEXT,
            generated_at TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS files_vec USING vec0(
            embedding FLOAT[{EMBED_DIM}]
        );

        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        """
    )
    conn.commit()


def get_hash(conn: sqlite3.Connection, path: str) -> str | None:
    row = conn.execute("SELECT content_hash FROM files WHERE path = ?", (path,)).fetchone()
    return row["content_hash"] if row else None


def get_all_paths(conn: sqlite3.Connection) -> set[str]:
    rows = conn.execute("SELECT path FROM files").fetchall()
    return {r["path"] for r in rows}


def upsert_file(
    conn: sqlite3.Connection,
    path: str,
    content_hash: str,
    header: str,
    language: str | None,
    generated_at: str,
    embedding: list[float],
) -> None:
    cur = conn.execute("SELECT id FROM files WHERE path = ?", (path,))
    row = cur.fetchone()
    if row:
        file_id = row["id"]
        conn.execute(
            """
            UPDATE files SET content_hash = ?, header = ?, language = ?, generated_at = ?
            WHERE id = ?
            """,
            (content_hash, header, language, generated_at, file_id),
        )
        conn.execute("DELETE FROM files_vec WHERE rowid = ?", (file_id,))
    else:
        cur = conn.execute(
            """
            INSERT INTO files (path, content_hash, header, language, generated_at)
            VALUES (?, ?, ?, ?, ?)
            """,
            (path, content_hash, header, language, generated_at),
        )
        file_id = cur.lastrowid

    conn.execute(
        "INSERT INTO files_vec (rowid, embedding) VALUES (?, ?)",
        (file_id, _pack(embedding)),
    )


def delete_paths(conn: sqlite3.Connection, paths: Iterable[str]) -> int:
    ids: list[int] = []
    for p in paths:
        row = conn.execute("SELECT id FROM files WHERE path = ?", (p,)).fetchone()
        if row:
            ids.append(row["id"])
    if not ids:
        return 0
    placeholders = ",".join("?" * len(ids))
    conn.execute(f"DELETE FROM files_vec WHERE rowid IN ({placeholders})", ids)
    conn.execute(f"DELETE FROM files WHERE id IN ({placeholders})", ids)
    conn.commit()
    return len(ids)


def search(
    conn: sqlite3.Connection, query_embedding: list[float], k: int = 5
) -> list[tuple[FileRecord, float]]:
    rows = conn.execute(
        """
        SELECT f.path, f.content_hash, f.header, f.language, f.generated_at, v.distance
        FROM files_vec v
        JOIN files f ON f.id = v.rowid
        WHERE v.embedding MATCH ? AND k = ?
        ORDER BY v.distance
        """,
        (_pack(query_embedding), k),
    ).fetchall()
    return [
        (
            FileRecord(
                path=r["path"],
                content_hash=r["content_hash"],
                header=r["header"],
                language=r["language"],
                generated_at=r["generated_at"],
            ),
            float(r["distance"]),
        )
        for r in rows
    ]


def count_files(conn: sqlite3.Connection) -> int:
    row = conn.execute("SELECT COUNT(*) AS n FROM files").fetchone()
    return int(row["n"])


def set_meta(conn: sqlite3.Connection, key: str, value: str) -> None:
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )
    conn.commit()


def get_meta(conn: sqlite3.Connection, key: str) -> str | None:
    row = conn.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
    return row["value"] if row else None
