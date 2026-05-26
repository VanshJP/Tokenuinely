#![allow(dead_code)]

use serde::Serialize;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Serialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,
    pub parent: Option<String>,
}

/// Map file extension to language name.
pub fn detect_language(file_path: &str) -> Option<String> {
    let ext = file_path.rsplit('.').next()?;
    match ext {
        "rs" => Some("rust".into()),
        "py" => Some("python".into()),
        "js" | "mjs" | "cjs" => Some("javascript".into()),
        "ts" | "tsx" => Some("typescript".into()),
        "go" => Some("go".into()),
        "java" => Some("java".into()),
        "c" | "h" => Some("c".into()),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp".into()),
        _ => None,
    }
}

fn make_parser(language: &str) -> Option<Parser> {
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

/// Extract symbol definitions from source code.
pub fn extract_symbols(source: &str, language: &str) -> Vec<SymbolInfo> {
    let mut parser = match make_parser(language) {
        Some(p) => p,
        None => return vec![],
    };
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return vec![],
    };
    let mut symbols = Vec::new();
    let src = source.as_bytes();
    collect_symbols(&tree.root_node(), src, language, None, &mut symbols);
    symbols
}

fn collect_symbols(
    node: &Node,
    src: &[u8],
    lang: &str,
    parent: Option<&str>,
    out: &mut Vec<SymbolInfo>,
) {
    if let Some(sym) = node_to_symbol(node, src, lang, parent) {
        out.push(sym);
    }

    let container = container_name(node, src, lang);
    let next_parent = container.as_deref().or(parent);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(&child, src, lang, next_parent, out);
    }
}

/// Return the name of a container node that provides parent context for children.
fn container_name(node: &Node, src: &[u8], lang: &str) -> Option<String> {
    match lang {
        "rust" => match node.kind() {
            "impl_item" => node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(src).ok())
                .map(|s| s.to_string()),
            "trait_item" | "struct_item" => field_text(node, "name", src),
            _ => None,
        },
        "python" => {
            if node.kind() == "class_definition" {
                field_text(node, "name", src)
            } else {
                None
            }
        }
        "javascript" | "typescript" => {
            if node.kind() == "class_declaration" {
                field_text(node, "name", src)
            } else {
                None
            }
        }
        "java" => match node.kind() {
            "class_declaration" | "interface_declaration" | "enum_declaration" => {
                field_text(node, "name", src)
            }
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Per-language symbol extraction
// ---------------------------------------------------------------------------

fn node_to_symbol(
    node: &Node,
    src: &[u8],
    lang: &str,
    parent: Option<&str>,
) -> Option<SymbolInfo> {
    match lang {
        "rust" => rust_symbol(node, src, parent),
        "python" => python_symbol(node, src, parent),
        "javascript" | "typescript" => js_symbol(node, src, parent),
        "go" => go_symbol(node, src, parent),
        "java" => java_symbol(node, src, parent),
        "c" | "cpp" => c_symbol(node, src, parent),
        _ => None,
    }
}

fn rust_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    let kind_str = match node.kind() {
        "function_item" => {
            if parent.is_some() {
                "method"
            } else {
                "function"
            }
        }
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "impl_item" => "impl",
        "const_item" => "const",
        "type_item" => "type",
        _ => return None,
    };

    let name = if node.kind() == "impl_item" {
        let type_name = node
            .child_by_field_name("type")
            .and_then(|n| n.utf8_text(src).ok())
            .unwrap_or("?");
        let trait_name = node
            .child_by_field_name("trait")
            .and_then(|n| n.utf8_text(src).ok());
        match trait_name {
            Some(t) => format!("{} for {}", t, type_name),
            None => type_name.to_string(),
        }
    } else {
        field_text(node, "name", src)?
    };

    Some(SymbolInfo {
        name,
        kind: kind_str.to_string(),
        line_start: node.start_position().row + 1,
        line_end: node.end_position().row + 1,
        signature: extract_signature(node, src),
        parent: parent.map(|s| s.to_string()),
    })
}

fn python_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    let kind_str = match node.kind() {
        "function_definition" => {
            if parent.is_some() {
                "method"
            } else {
                "function"
            }
        }
        "class_definition" => "class",
        _ => return None,
    };
    let name = field_text(node, "name", src)?;
    Some(SymbolInfo {
        name,
        kind: kind_str.to_string(),
        line_start: node.start_position().row + 1,
        line_end: node.end_position().row + 1,
        signature: extract_signature(node, src),
        parent: parent.map(|s| s.to_string()),
    })
}

fn js_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    match node.kind() {
        "function_declaration" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(
                name,
                "function",
                node,
                src,
                parent,
            ))
        }
        "class_declaration" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "class", node, src, parent))
        }
        "method_definition" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "method", node, src, parent))
        }
        // Named arrow functions: `const foo = () => { ... }`
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if value.kind() != "arrow_function" {
                return None;
            }
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "function", node, src, parent))
        }
        _ => None,
    }
}

fn go_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    match node.kind() {
        "function_declaration" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "function", node, src, parent))
        }
        "method_declaration" => {
            let name = field_text(node, "name", src)?;
            // Extract receiver type as parent
            let receiver = node
                .child_by_field_name("receiver")
                .and_then(|r| {
                    // receiver is a parameter_list like (x *Foo)
                    let mut cur = r.walk();
                    let result = r.named_children(&mut cur)
                        .next()
                        .and_then(|param| {
                            param
                                .child_by_field_name("type")
                                .and_then(|t| {
                                    let text = t.utf8_text(src).ok()?;
                                    Some(text.trim_start_matches('*').to_string())
                                })
                        });
                    result
                });
            Some(SymbolInfo {
                name,
                kind: "method".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: extract_signature(node, src),
                parent: receiver.or_else(|| parent.map(|s| s.to_string())),
            })
        }
        "type_spec" => {
            let name = field_text(node, "name", src)?;
            let type_node = node.child_by_field_name("type");
            let kind_str = match type_node.as_ref().map(|n| n.kind()) {
                Some("struct_type") => "struct",
                Some("interface_type") => "interface",
                _ => "type",
            };
            Some(make_sym(name, kind_str, node, src, parent))
        }
        _ => None,
    }
}

fn java_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    let kind_str = match node.kind() {
        "method_declaration" => "method",
        "class_declaration" => "class",
        "interface_declaration" => "interface",
        "enum_declaration" => "enum",
        _ => return None,
    };
    let name = field_text(node, "name", src)?;
    Some(make_sym(name, kind_str, node, src, parent))
}

fn c_symbol(node: &Node, src: &[u8], parent: Option<&str>) -> Option<SymbolInfo> {
    match node.kind() {
        "function_definition" => {
            let declarator = node.child_by_field_name("declarator")?;
            let name = c_declarator_name(&declarator, src)?;
            Some(make_sym(name, "function", node, src, parent))
        }
        "struct_specifier" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "struct", node, src, parent))
        }
        "enum_specifier" => {
            let name = field_text(node, "name", src)?;
            Some(make_sym(name, "enum", node, src, parent))
        }
        "type_definition" => {
            let name = typedef_name(node, src)?;
            Some(make_sym(name, "type", node, src, parent))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_sym(
    name: String,
    kind: &str,
    node: &Node,
    src: &[u8],
    parent: Option<&str>,
) -> SymbolInfo {
    SymbolInfo {
        name,
        kind: kind.to_string(),
        line_start: node.start_position().row + 1,
        line_end: node.end_position().row + 1,
        signature: extract_signature(node, src),
        parent: parent.map(|s| s.to_string()),
    }
}

fn field_text(node: &Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(src)
        .ok()
        .map(|s| s.to_string())
}

/// Extract the signature: text from node start up to the body, or first line (≤100 chars).
fn extract_signature(node: &Node, src: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let start = node.start_byte();
        let end = body.start_byte();
        if end > start {
            if let Ok(sig) = std::str::from_utf8(&src[start..end]) {
                let trimmed = sig.trim_end();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    // Fallback: first line, capped at 100 chars
    let start = node.start_byte();
    let end = (start + 100).min(node.end_byte()).min(src.len());
    std::str::from_utf8(&src[start..end])
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Recursively unwrap C declarator nodes to find the identifier name.
fn c_declarator_name(node: &Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(src).ok().map(|s| s.to_string()),
        "function_declarator" | "pointer_declarator" | "array_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| c_declarator_name(&n, src)),
        "parenthesized_declarator" => {
            let mut cur = node.walk();
            let result = node.named_children(&mut cur)
                .find_map(|child| c_declarator_name(&child, src));
            result
        }
        _ => None,
    }
}

/// Extract the typedef name from a C type_definition node.
fn typedef_name(node: &Node, src: &[u8]) -> Option<String> {
    if let Some(decl) = node.child_by_field_name("declarator") {
        return c_declarator_name(&decl, src);
    }
    // Fallback: last type_identifier or identifier child
    let mut cur = node.walk();
    node.named_children(&mut cur)
        .filter(|c| c.kind() == "type_identifier" || c.kind() == "identifier")
        .last()
        .and_then(|c| c.utf8_text(src).ok().map(|s| s.to_string()))
}
