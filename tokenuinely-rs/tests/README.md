# tokenuinely Tests

## Unit / Integration Tests

```bash
cargo test
```

All tests run without API keys — they use in-memory SQLite databases and test
the core logic (db operations, cosine similarity, export/import round-trip, etc.).

## Benchmark

```bash
python tests/bench/benchmark.py /path/to/repo
python tests/bench/benchmark.py /path/to/repo --output results.json
```

Requires `claude` CLI installed. Runs 5 standard queries with and without
tokenuinely MCP and compares input token usage.

## Fixtures

`tests/fixtures/sample_repo/` contains minimal Rust source files for testing
the walker and indexer.
