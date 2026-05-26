#!/usr/bin/env python3
"""
Benchmark harness for tokenuinely.

Runs the same 5 questions against a repo twice — once with tokenuinely MCP active,
once without — and compares token usage.

Requires: `claude` CLI installed and configured.

Usage:
    python benchmark.py /path/to/repo
    python benchmark.py /path/to/repo --output results.json
"""

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


BENCHMARK_TASKS = {
    "find_auth": "Where is the authentication and login logic handled?",
    "find_db_layer": "Which files handle database connections and queries?",
    "find_api_routes": "Where are the API routes or endpoints defined?",
    "find_config": "How is configuration managed? Where are environment variables loaded?",
    "find_error_handling": "Where is error handling and exception management implemented?",
}


@dataclass
class RunResult:
    task: str
    input_tokens: int
    output_tokens: int
    success: bool
    error: str = ""


def run_claude_query(prompt: str, repo: Path, use_mcp: bool) -> RunResult:
    """Run a single query through claude CLI and capture token usage."""
    cmd = [
        "claude",
        "--print",
        "--output-format", "json",
        "--no-session-persistence",
    ]

    if not use_mcp:
        cmd.extend(["--allowedTools", "Bash,Read,Write,Grep,Glob"])

    cmd.extend(["-p", prompt])

    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=120,
            cwd=str(repo),
        )

        if result.returncode != 0:
            return RunResult(
                task=prompt[:50],
                input_tokens=0,
                output_tokens=0,
                success=False,
                error=f"Exit code {result.returncode}: {result.stderr[:200]}",
            )

        data = json.loads(result.stdout)
        usage = data.get("usage", {})

        return RunResult(
            task=prompt[:50],
            input_tokens=usage.get("input_tokens", 0),
            output_tokens=usage.get("output_tokens", 0),
            success=True,
        )

    except subprocess.TimeoutExpired:
        return RunResult(
            task=prompt[:50],
            input_tokens=0,
            output_tokens=0,
            success=False,
            error="Timeout after 120s",
        )
    except json.JSONDecodeError as e:
        return RunResult(
            task=prompt[:50],
            input_tokens=0,
            output_tokens=0,
            success=False,
            error=f"JSON parse error: {e}",
        )


def run_benchmark(repo: Path) -> dict:
    """Run all benchmark tasks with and without tokenuinely."""
    results = {"baseline": {}, "with_tokenuinely": {}, "summary": {}}

    print("=" * 70)
    print("Running BASELINE (no tokenuinely MCP)")
    print("=" * 70)

    for name, prompt in BENCHMARK_TASKS.items():
        print(f"  [{name}] {prompt[:60]}...")
        result = run_claude_query(prompt, repo, use_mcp=False)
        results["baseline"][name] = {
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "success": result.success,
            "error": result.error,
        }
        if result.success:
            print(f"    → {result.input_tokens:,} input tokens")
        else:
            print(f"    → FAILED: {result.error}")

    print()
    print("=" * 70)
    print("Running WITH TOKENUINELY MCP")
    print("=" * 70)

    for name, prompt in BENCHMARK_TASKS.items():
        print(f"  [{name}] {prompt[:60]}...")
        result = run_claude_query(prompt, repo, use_mcp=True)
        results["with_tokenuinely"][name] = {
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "success": result.success,
            "error": result.error,
        }
        if result.success:
            print(f"    → {result.input_tokens:,} input tokens")
        else:
            print(f"    → FAILED: {result.error}")

    # Summary table
    print()
    print("=" * 70)
    print(f"{'Task':<25} {'Baseline':>12} {'With Tool':>12} {'Reduction':>12}")
    print("-" * 70)

    total_baseline = 0
    total_with = 0

    for name in BENCHMARK_TASKS:
        b = results["baseline"].get(name, {})
        w = results["with_tokenuinely"].get(name, {})
        b_tokens = b.get("input_tokens", 0)
        w_tokens = w.get("input_tokens", 0)

        total_baseline += b_tokens
        total_with += w_tokens

        if b_tokens > 0 and w_tokens > 0:
            ratio = f"{b_tokens / w_tokens:.1f}x"
        elif b_tokens > 0:
            ratio = "N/A"
        else:
            ratio = "N/A"

        print(f"{name:<25} {b_tokens:>12,} {w_tokens:>12,} {ratio:>12}")

    print("-" * 70)
    if total_with > 0:
        overall_ratio = f"{total_baseline / total_with:.1f}x"
    else:
        overall_ratio = "N/A"
    print(f"{'TOTAL':<25} {total_baseline:>12,} {total_with:>12,} {overall_ratio:>12}")
    print("=" * 70)

    results["summary"] = {
        "total_baseline_tokens": total_baseline,
        "total_with_tokenuinely_tokens": total_with,
        "overall_reduction_ratio": overall_ratio,
    }

    return results


def main():
    parser = argparse.ArgumentParser(
        description="Benchmark tokenuinely token savings"
    )
    parser.add_argument("repo", type=Path, help="Path to the repository to benchmark")
    parser.add_argument(
        "--output", type=Path, help="Save results to JSON file", default=None
    )
    args = parser.parse_args()

    if not args.repo.is_dir():
        print(f"Error: {args.repo} is not a directory", file=sys.stderr)
        sys.exit(1)

    results = run_benchmark(args.repo)

    if args.output:
        with open(args.output, "w") as f:
            json.dump(results, f, indent=2)
        print(f"\nResults saved to {args.output}")


if __name__ == "__main__":
    main()
