//! Index-building pipeline: walk the repo, slice files into chunks, generate
//! semantic headers, embed them, and persist to SQLite.

pub mod deps;
pub mod embedder;
pub mod header;
pub mod indexer;
pub mod symbols;
pub mod ts_lang;
pub mod walker;
