//! Shared tree-sitter helpers used by `symbols.rs` and `deps.rs`.
//!
//! Both modules need the same parser construction and the same handful of
//! node-walking primitives; keeping a single copy here prevents the two from
//! drifting (e.g. one adding support for a new language and the other forgetting).

use tree_sitter::{Node, Parser};

/// Build a tree-sitter parser for a supported language identifier.
///
/// Returns `None` for unknown languages so callers can no-op gracefully on
/// unsupported file types.
pub fn make_parser(language: &str) -> Option<Parser> {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = match language {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "cpp" => tree_sitter_c::LANGUAGE.into(),
        _ => return None,
    };
    parser.set_language(&lang).ok()?;
    Some(parser)
}

/// Read the UTF-8 text of a named field on `node`.
pub fn field_text(node: &Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(src)
        .ok()
        .map(|s| s.to_string())
}

/// Recursively unwrap C declarator nodes to find the identifier name.
///
/// Handles `pointer_declarator`, `array_declarator`, `function_declarator`,
/// and `parenthesized_declarator` wrappers.
pub fn c_declarator_name(node: &Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(src).ok().map(|s| s.to_string()),
        "function_declarator" | "pointer_declarator" | "array_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| c_declarator_name(&n, src)),
        "parenthesized_declarator" => {
            let mut cur = node.walk();
            let result = node
                .named_children(&mut cur)
                .find_map(|child| c_declarator_name(&child, src));
            result
        }
        _ => None,
    }
}
