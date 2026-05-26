//! Ways to expose the index to clients: MCP stdio server for Claude Code,
//! the PreToolUse hook, and the loopback HTTP graph viewer.

pub mod hook_augment;
pub mod mcp;
pub mod viz;
