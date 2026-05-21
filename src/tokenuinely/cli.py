from __future__ import annotations

import asyncio
import os
import shutil
import subprocess
import sys
from pathlib import Path

import typer
from rich.console import Console
from rich.panel import Panel
from rich.prompt import Confirm, Prompt
from rich.table import Table

from . import db
from .config import Config, index_db_path
from .indexer import index_repo
from .query import query_repo

app = typer.Typer(
    add_completion=False,
    help="Per-file semantic headers + embedding index for agentic code retrieval.",
    no_args_is_help=True,
)
console = Console()


def _load_cfg() -> Config:
    try:
        return Config.load()
    except RuntimeError as e:
        console.print(f"[red]error:[/red] {e}")
        raise typer.Exit(1)


@app.command(help="Index or update a repository.")
def index(
    path: Path = typer.Argument(Path("."), help="Path to repository root"),
) -> None:
    cfg = _load_cfg()
    if not path.exists() or not path.is_dir():
        console.print(f"[red]error:[/red] {path} is not a directory")
        raise typer.Exit(1)

    stats = asyncio.run(index_repo(path, cfg, console=console))

    summary = Table.grid(padding=(0, 2))
    summary.add_column(justify="right", style="dim")
    summary.add_column()
    summary.add_row("scanned", str(stats.scanned))
    summary.add_row("unchanged", str(stats.unchanged))
    summary.add_row("indexed", f"[green]{stats.indexed}[/green]")
    summary.add_row("deleted", str(stats.deleted))
    if stats.failed:
        summary.add_row("failed", f"[red]{len(stats.failed)}[/red]")
    console.print(Panel(summary, title="tokenuinely index", border_style="cyan"))

    if stats.failed:
        console.print("[red]failures:[/red]")
        for p, err in stats.failed[:10]:
            console.print(f"  • {p}: {err}")
        if len(stats.failed) > 10:
            console.print(f"  …and {len(stats.failed) - 10} more")


@app.command(help="Semantic search over indexed headers.")
def query(
    text: str = typer.Argument(..., help="Search query"),
    path: Path = typer.Option(Path("."), "--path", "-p", help="Repository root"),
    k: int = typer.Option(5, "--k", "-k", help="Number of results"),
    show_headers: bool = typer.Option(
        True, "--headers/--no-headers", help="Show full headers in results"
    ),
) -> None:
    cfg = _load_cfg()
    try:
        hits = asyncio.run(query_repo(path, text, k, cfg))
    except RuntimeError as e:
        console.print(f"[red]error:[/red] {e}")
        raise typer.Exit(1)

    if not hits:
        console.print("[yellow]no results[/yellow]")
        return

    for i, h in enumerate(hits, 1):
        title = f"[bold cyan]{i}.[/bold cyan] [bold]{h.path}[/bold]  [dim](d={h.distance:.3f})[/dim]"
        if show_headers:
            console.print(Panel(h.header, title=title, border_style="dim"))
        else:
            console.print(title)


@app.command(help="Show index status.")
def status(
    path: Path = typer.Argument(Path("."), help="Repository root"),
) -> None:
    db_path = index_db_path(path.resolve())
    if not db_path.exists():
        console.print(f"[yellow]no index at {db_path}[/yellow]")
        raise typer.Exit(0)
    conn = db.connect(db_path)
    n = db.count_files(conn)
    last = db.get_meta(conn, "last_index_at") or "never"
    conn.close()
    t = Table.grid(padding=(0, 2))
    t.add_column(justify="right", style="dim")
    t.add_column()
    t.add_row("repo", str(path.resolve()))
    t.add_row("index", str(db_path))
    t.add_row("files indexed", str(n))
    t.add_row("last index", last)
    console.print(Panel(t, title="tokenuinely status", border_style="cyan"))


@app.command(help="Run the MCP server over stdio (for Claude Code and other harnesses).")
def mcp() -> None:
    from .mcp_server import run

    run()


CLAUDE_MD_SNIPPET = """\
## Code retrieval via tokenuinely

This repo has a `tokenuinely` semantic index. Before running `grep`/`glob`/`find`
to discover files, call the `tokenuinely__query` MCP tool first with a natural-
language description of what you're looking for. It returns the top-k relevant
files with semantic headers describing what each file does, what it touches,
and pointers to related code. Fall back to text search only if semantic
results don't cover the question.
"""


def _has_command(name: str) -> bool:
    return shutil.which(name) is not None


def _tokenuinely_path() -> str:
    return shutil.which("tokenuinely") or sys.argv[0]


@app.command(help="One-shot setup: keys, Claude Code MCP registration, initial index.")
def setup(
    path: Path = typer.Option(Path("."), "--path", "-p", help="Repository to index"),
    scope: str = typer.Option(
        "user", "--scope", help="claude mcp scope: user | project | local"
    ),
    skip_index: bool = typer.Option(False, "--skip-index", help="Skip initial indexing"),
    skip_claude_md: bool = typer.Option(
        False, "--skip-claude-md", help="Don't offer to update CLAUDE.md"
    ),
) -> None:
    console.print(Panel.fit("[bold cyan]tokenuinely setup[/bold cyan]", border_style="cyan"))

    ak = os.environ.get("ANTHROPIC_API_KEY")
    vk = os.environ.get("VOYAGE_API_KEY")
    if not ak:
        console.print(
            "[yellow]ANTHROPIC_API_KEY not set.[/yellow] "
            "Get one at https://console.anthropic.com/settings/keys"
        )
        ak = Prompt.ask("Paste your Anthropic API key (or leave blank to skip)", default="")
    if not vk:
        console.print(
            "[yellow]VOYAGE_API_KEY not set.[/yellow] "
            "Get one at https://dash.voyageai.com/api-keys"
        )
        vk = Prompt.ask("Paste your Voyage API key (or leave blank to skip)", default="")

    if ak or vk:
        shell_rc = _detect_shell_rc()
        if shell_rc and Confirm.ask(
            f"Append API keys to {shell_rc}?", default=True
        ):
            lines = []
            if ak:
                lines.append(f'export ANTHROPIC_API_KEY="{ak}"\n')
                os.environ["ANTHROPIC_API_KEY"] = ak
            if vk:
                lines.append(f'export VOYAGE_API_KEY="{vk}"\n')
                os.environ["VOYAGE_API_KEY"] = vk
            with shell_rc.open("a") as f:
                f.write("\n# tokenuinely\n")
                f.writelines(lines)
            console.print(f"[green]✓[/green] keys written to {shell_rc}")
        else:
            if ak:
                os.environ["ANTHROPIC_API_KEY"] = ak
            if vk:
                os.environ["VOYAGE_API_KEY"] = vk
            console.print(
                "[dim]Set these in your shell profile to persist:[/dim]\n"
                f"  export ANTHROPIC_API_KEY=...\n  export VOYAGE_API_KEY=..."
            )

    if _has_command("claude"):
        if Confirm.ask(
            "Register tokenuinely MCP server with Claude Code now?", default=True
        ):
            bin_path = _tokenuinely_path()
            cmd = [
                "claude", "mcp", "add", "tokenuinely",
                "--scope", scope,
                "--", bin_path, "mcp",
            ]
            console.print(f"[dim]$ {' '.join(cmd)}[/dim]")
            result = subprocess.run(cmd, capture_output=True, text=True)
            if result.returncode == 0:
                console.print("[green]✓[/green] registered with Claude Code")
            else:
                console.print(f"[red]claude mcp add failed:[/red] {result.stderr.strip()}")
                console.print(
                    f"[dim]Try manually:[/dim] claude mcp add tokenuinely -- {bin_path} mcp"
                )
    else:
        console.print(
            "[yellow]`claude` CLI not found.[/yellow] Install Claude Code, "
            "then run:\n"
            f"  claude mcp add tokenuinely -- {_tokenuinely_path()} mcp"
        )

    repo = path.resolve()
    if not skip_claude_md:
        claude_md = repo / "CLAUDE.md"
        if Confirm.ask(
            f"Append usage hint to {claude_md}?", default=True
        ):
            existing = claude_md.read_text() if claude_md.exists() else ""
            if "tokenuinely__query" not in existing:
                with claude_md.open("a") as f:
                    if existing and not existing.endswith("\n"):
                        f.write("\n")
                    f.write("\n" + CLAUDE_MD_SNIPPET)
                console.print(f"[green]✓[/green] appended hint to {claude_md}")
            else:
                console.print(f"[dim]hint already present in {claude_md}[/dim]")

    if not skip_index:
        if not os.environ.get("ANTHROPIC_API_KEY") or not os.environ.get("VOYAGE_API_KEY"):
            console.print(
                "[yellow]Skipping initial index — API keys not in current env.[/yellow] "
                "Open a new shell and run: tokenuinely index"
            )
        elif Confirm.ask(f"Index {repo} now?", default=True):
            cfg = Config.load()
            stats = asyncio.run(index_repo(repo, cfg, console=console))
            console.print(
                f"[green]✓[/green] indexed {stats.indexed} files "
                f"({stats.unchanged} unchanged)"
            )

    console.print(Panel(
        "[green]Setup complete.[/green]\n\n"
        "In Claude Code, the [bold]tokenuinely__query[/bold] tool is now available.\n"
        "Try asking: [italic]'what file handles X?'[/italic] — Claude will use it.",
        border_style="green",
    ))


def _detect_shell_rc() -> Path | None:
    shell = os.environ.get("SHELL", "")
    home = Path.home()
    if "zsh" in shell:
        return home / ".zshrc"
    if "bash" in shell:
        for name in (".bashrc", ".bash_profile", ".profile"):
            p = home / name
            if p.exists():
                return p
        return home / ".bashrc"
    if "fish" in shell:
        return home / ".config" / "fish" / "config.fish"
    return None


if __name__ == "__main__":
    app()
