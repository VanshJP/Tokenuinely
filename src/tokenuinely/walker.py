from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

import pathspec

from .config import DEFAULT_IGNORES, MAX_FILE_BYTES

EXT_TO_LANG = {
    ".py": "python",
    ".js": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".rs": "rust",
    ".go": "go",
    ".java": "java",
    ".kt": "kotlin",
    ".swift": "swift",
    ".rb": "ruby",
    ".php": "php",
    ".c": "c",
    ".h": "c",
    ".cpp": "cpp",
    ".cc": "cpp",
    ".hpp": "cpp",
    ".cs": "csharp",
    ".scala": "scala",
    ".sh": "shell",
    ".bash": "shell",
    ".zsh": "shell",
    ".sql": "sql",
    ".md": "markdown",
    ".yaml": "yaml",
    ".yml": "yaml",
    ".toml": "toml",
    ".json": "json",
    ".html": "html",
    ".css": "css",
    ".scss": "scss",
}


@dataclass
class WalkedFile:
    abs_path: Path
    rel_path: str
    content: str
    truncated: bool
    language: str | None


def _load_gitignore(root: Path) -> pathspec.PathSpec | None:
    gi = root / ".gitignore"
    if not gi.exists():
        return None
    with gi.open("r", encoding="utf-8", errors="ignore") as f:
        return pathspec.PathSpec.from_lines("gitwildmatch", f)


def _default_spec() -> pathspec.PathSpec:
    return pathspec.PathSpec.from_lines("gitwildmatch", DEFAULT_IGNORES)


def _looks_binary(sample: bytes) -> bool:
    if b"\x00" in sample:
        return True
    text_chars = bytes(range(32, 127)) + b"\n\r\t\f\b"
    nontext = sum(1 for b in sample if b not in text_chars)
    return (nontext / max(1, len(sample))) > 0.30


def _read_text(path: Path) -> tuple[str | None, bool]:
    """Return (content, truncated). content is None if file is binary/unreadable."""
    try:
        size = path.stat().st_size
    except OSError:
        return None, False
    truncated = False
    read_bytes = MAX_FILE_BYTES if size > MAX_FILE_BYTES else size
    try:
        with path.open("rb") as f:
            data = f.read(read_bytes)
    except OSError:
        return None, False
    if size > MAX_FILE_BYTES:
        truncated = True
    if _looks_binary(data[:4096]):
        return None, False
    try:
        return data.decode("utf-8"), truncated
    except UnicodeDecodeError:
        try:
            return data.decode("latin-1"), truncated
        except UnicodeDecodeError:
            return None, False


def walk(root: Path) -> Iterator[WalkedFile]:
    root = root.resolve()
    default = _default_spec()
    gitignore = _load_gitignore(root)

    for path in root.rglob("*"):
        if not path.is_file():
            continue
        try:
            rel = path.relative_to(root)
        except ValueError:
            continue
        rel_str = str(rel)
        rel_for_match = rel_str + ("/" if path.is_dir() else "")
        if default.match_file(rel_for_match):
            continue
        if gitignore and gitignore.match_file(rel_for_match):
            continue

        content, truncated = _read_text(path)
        if content is None:
            continue
        lang = EXT_TO_LANG.get(path.suffix.lower())
        yield WalkedFile(
            abs_path=path,
            rel_path=rel_str,
            content=content,
            truncated=truncated,
            language=lang,
        )
