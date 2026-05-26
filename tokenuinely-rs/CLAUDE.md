
## Code retrieval via tokenuinely

This repo has a `tokenuinely` semantic index. Before running `grep`/`glob`/`find`
to discover files, call the `tokenuinely__query` MCP tool first with a natural-
language description of what you're looking for. It returns the top-k relevant
files with semantic headers describing what each file does, what it touches,
and pointers to related code. Fall back to text search only if semantic
results don't cover the question.
