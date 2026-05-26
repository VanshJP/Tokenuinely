#![allow(dead_code)]

use serde::Serialize;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Serialize)]
pub struct DepInfo {
    pub source_symbol: Option<String>,
    pub target_symbol: String,
    pub target_path: Option<String>,
    pub kind: String, // "imports" or "calls"
}

/// Extract import and call dependencies from source code.
pub fn extract_deps(source: &str, language: &str) -> Vec<DepInfo> {
    let mut parser = match make_parser(language) {
        Some(p) => p,
        None => return vec![],
    };
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return vec![],
    };
    let mut deps = Vec::new();
    let src = source.as_bytes();
    collect_deps(&tree.root_node(), src, language, None, &mut deps);
    deps
}

// ---------------------------------------------------------------------------
// Parser construction (mirrors symbols.rs; kept local to avoid cross-module
// coupling so the user can wire modules independently)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// AST walk
// ---------------------------------------------------------------------------

fn collect_deps(
    node: &Node,
    src: &[u8],
    lang: &str,
    enclosing_fn: Option<&str>,
    out: &mut Vec<DepInfo>,
) {
    // Check imports
    for mut dep in extract_imports(node, src, lang) {
        dep.source_symbol = enclosing_fn.map(|s| s.to_string());
        out.push(dep);
    }

    // Check calls
    if let Some(mut dep) = extract_call(node, src, lang) {
        dep.source_symbol = enclosing_fn.map(|s| s.to_string());
        out.push(dep);
    }

    // Update enclosing function context
    let fn_name = function_name(node, src, lang);
    let next_fn = if fn_name.is_some() {
        fn_name.as_deref()
    } else {
        enclosing_fn
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_deps(&child, src, lang, next_fn, out);
    }
}

/// If this node is a function definition, return its name.
fn function_name(node: &Node, src: &[u8], lang: &str) -> Option<String> {
    match lang {
        "rust" => {
            if node.kind() == "function_item" {
                field_text(node, "name", src)
            } else {
                None
            }
        }
        "python" => {
            if node.kind() == "function_definition" {
                field_text(node, "name", src)
            } else {
                None
            }
        }
        "javascript" | "typescript" => match node.kind() {
            "function_declaration" => field_text(node, "name", src),
            "method_definition" => field_text(node, "name", src),
            _ => None,
        },
        "go" => match node.kind() {
            "function_declaration" | "method_declaration" => field_text(node, "name", src),
            _ => None,
        },
        "java" => {
            if node.kind() == "method_declaration" {
                field_text(node, "name", src)
            } else {
                None
            }
        }
        "c" | "cpp" => {
            if node.kind() == "function_definition" {
                let decl = node.child_by_field_name("declarator")?;
                c_declarator_name(&decl, src)
            } else {
                None
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Import extraction
// ---------------------------------------------------------------------------

fn extract_imports(node: &Node, src: &[u8], lang: &str) -> Vec<DepInfo> {
    match lang {
        "rust" => rust_imports(node, src),
        "python" => python_imports(node, src),
        "javascript" | "typescript" => js_imports(node, src),
        "go" => go_imports(node, src),
        "java" => java_imports(node, src),
        "c" | "cpp" => c_imports(node, src),
        _ => vec![],
    }
}

fn rust_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    if node.kind() != "use_declaration" {
        return vec![];
    }
    let full = match node.utf8_text(src) {
        Ok(t) => t.to_string(),
        Err(_) => return vec![],
    };
    let path = full
        .strip_prefix("use ")
        .unwrap_or(&full)
        .trim_end_matches(';')
        .trim();
    parse_rust_use(path)
}

fn parse_rust_use(path: &str) -> Vec<DepInfo> {
    let mut out = Vec::new();
    if let Some(brace) = path.find('{') {
        let prefix = path[..brace].trim_end_matches("::").to_string();
        let list = path[brace + 1..].trim_end_matches('}');
        for item in list.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let name = item
                .split(" as ")
                .next()
                .unwrap_or(item)
                .rsplit("::")
                .next()
                .unwrap_or(item)
                .trim();
            if !name.is_empty() && name != "*" {
                out.push(DepInfo {
                    source_symbol: None,
                    target_symbol: name.to_string(),
                    target_path: Some(prefix.clone()),
                    kind: "imports".to_string(),
                });
            }
        }
    } else {
        let effective = path.split(" as ").next().unwrap_or(path).trim();
        if let Some(pos) = effective.rfind("::") {
            let module = &effective[..pos];
            let name = &effective[pos + 2..];
            if !name.is_empty() && name != "*" {
                out.push(DepInfo {
                    source_symbol: None,
                    target_symbol: name.to_string(),
                    target_path: Some(module.to_string()),
                    kind: "imports".to_string(),
                });
            }
        } else if !effective.is_empty() {
            out.push(DepInfo {
                source_symbol: None,
                target_symbol: effective.to_string(),
                target_path: None,
                kind: "imports".to_string(),
            });
        }
    }
    out
}

fn python_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    match node.kind() {
        "import_statement" => {
            // `import foo` or `import foo.bar`
            let text = node.utf8_text(src).unwrap_or("").to_string();
            let path = text.strip_prefix("import ").unwrap_or("").trim();
            if path.is_empty() {
                return vec![];
            }
            let parts: Vec<&str> = path.split('.').collect();
            let name = parts.last().unwrap_or(&"").to_string();
            let module = if parts.len() > 1 {
                Some(parts[..parts.len() - 1].join("."))
            } else {
                None
            };
            vec![DepInfo {
                source_symbol: None,
                target_symbol: name,
                target_path: module,
                kind: "imports".to_string(),
            }]
        }
        "import_from_statement" => {
            // `from foo import bar, baz`
            let text = node.utf8_text(src).unwrap_or("").to_string();
            let rest = text.strip_prefix("from ").unwrap_or("").trim();
            let parts: Vec<&str> = rest.splitn(2, " import ").collect();
            if parts.len() != 2 {
                return vec![];
            }
            let module = parts[0].trim().to_string();
            let mut out = Vec::new();
            for name in parts[1].split(',') {
                let name = name
                    .split(" as ")
                    .next()
                    .unwrap_or(name)
                    .trim()
                    .to_string();
                if !name.is_empty() && name != "*" {
                    out.push(DepInfo {
                        source_symbol: None,
                        target_symbol: name,
                        target_path: Some(module.clone()),
                        kind: "imports".to_string(),
                    });
                }
            }
            out
        }
        _ => vec![],
    }
}

fn js_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    if node.kind() != "import_statement" {
        return vec![];
    }
    let source_path = node
        .child_by_field_name("source")
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| s.trim_matches(|c| c == '\'' || c == '"').to_string());

    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "import_clause" => {
                let mut inner = child.walk();
                for imp in child.named_children(&mut inner) {
                    match imp.kind() {
                        "identifier" => {
                            if let Ok(name) = imp.utf8_text(src) {
                                out.push(DepInfo {
                                    source_symbol: None,
                                    target_symbol: name.to_string(),
                                    target_path: source_path.clone(),
                                    kind: "imports".to_string(),
                                });
                            }
                        }
                        "named_imports" => {
                            let mut nc = imp.walk();
                            for spec in imp.named_children(&mut nc) {
                                if spec.kind() == "import_specifier" {
                                    let name = field_text(&spec, "name", src)
                                        .or_else(|| {
                                            spec.utf8_text(src)
                                                .ok()
                                                .map(|s| s.to_string())
                                        });
                                    if let Some(n) = name {
                                        out.push(DepInfo {
                                            source_symbol: None,
                                            target_symbol: n,
                                            target_path: source_path.clone(),
                                            kind: "imports".to_string(),
                                        });
                                    }
                                }
                            }
                        }
                        "namespace_import" => {
                            if let Ok(text) = imp.utf8_text(src) {
                                let alias = text
                                    .strip_prefix("* as ")
                                    .unwrap_or(text)
                                    .trim()
                                    .to_string();
                                if !alias.is_empty() {
                                    out.push(DepInfo {
                                        source_symbol: None,
                                        target_symbol: alias,
                                        target_path: source_path.clone(),
                                        kind: "imports".to_string(),
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Fallback: side-effect import like `import 'polyfill'`
    if out.is_empty() {
        if let Some(p) = &source_path {
            let name = p.rsplit('/').next().unwrap_or(p).to_string();
            if !name.is_empty() {
                out.push(DepInfo {
                    source_symbol: None,
                    target_symbol: name,
                    target_path: source_path,
                    kind: "imports".to_string(),
                });
            }
        }
    }
    out
}

fn go_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    if node.kind() != "import_spec" {
        return vec![];
    }
    let path_node = node.child_by_field_name("path");
    let path_text = path_node
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_default();
    if path_text.is_empty() {
        return vec![];
    }
    let name = path_text
        .rsplit('/')
        .next()
        .unwrap_or(&path_text)
        .to_string();
    vec![DepInfo {
        source_symbol: None,
        target_symbol: name,
        target_path: Some(path_text),
        kind: "imports".to_string(),
    }]
}

fn java_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    if node.kind() != "import_declaration" {
        return vec![];
    }
    let text = node.utf8_text(src).unwrap_or("").to_string();
    let path = text
        .strip_prefix("import ")
        .unwrap_or("")
        .trim_start_matches("static ")
        .trim_end_matches(';')
        .trim()
        .to_string();
    if path.is_empty() {
        return vec![];
    }
    let parts: Vec<&str> = path.rsplitn(2, '.').collect();
    let name = parts[0].to_string();
    let module = if parts.len() > 1 {
        Some(parts[1].to_string())
    } else {
        None
    };
    vec![DepInfo {
        source_symbol: None,
        target_symbol: name,
        target_path: module,
        kind: "imports".to_string(),
    }]
}

fn c_imports(node: &Node, src: &[u8]) -> Vec<DepInfo> {
    if node.kind() != "preproc_include" {
        return vec![];
    }
    let path_node = node.child_by_field_name("path");
    let text = path_node
        .and_then(|n| n.utf8_text(src).ok())
        .unwrap_or("");
    let name = text
        .trim_matches(|c: char| c == '<' || c == '>' || c == '"')
        .to_string();
    if name.is_empty() {
        return vec![];
    }
    vec![DepInfo {
        source_symbol: None,
        target_symbol: name,
        target_path: None,
        kind: "imports".to_string(),
    }]
}

// ---------------------------------------------------------------------------
// Call extraction
// ---------------------------------------------------------------------------

fn extract_call(node: &Node, src: &[u8], lang: &str) -> Option<DepInfo> {
    match lang {
        "rust" | "javascript" | "typescript" | "go" | "c" | "cpp" => {
            if node.kind() != "call_expression" {
                return None;
            }
            let fn_node = node.child_by_field_name("function")?;
            let target = fn_node.utf8_text(src).ok()?.to_string();
            Some(DepInfo {
                source_symbol: None,
                target_symbol: target,
                target_path: None,
                kind: "calls".to_string(),
            })
        }
        "python" => {
            if node.kind() != "call" {
                return None;
            }
            let fn_node = node.child_by_field_name("function")?;
            let target = fn_node.utf8_text(src).ok()?.to_string();
            Some(DepInfo {
                source_symbol: None,
                target_symbol: target,
                target_path: None,
                kind: "calls".to_string(),
            })
        }
        "java" => {
            if node.kind() != "method_invocation" {
                return None;
            }
            let name_node = node.child_by_field_name("name")?;
            let name = name_node.utf8_text(src).ok()?;
            let target = if let Some(obj) = node.child_by_field_name("object") {
                let obj_text = obj.utf8_text(src).ok()?;
                format!("{}.{}", obj_text, name)
            } else {
                name.to_string()
            };
            Some(DepInfo {
                source_symbol: None,
                target_symbol: target,
                target_path: None,
                kind: "calls".to_string(),
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn field_text(node: &Node, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(src)
        .ok()
        .map(|s| s.to_string())
}

/// Recursively unwrap C declarator nodes to find the identifier name.
fn c_declarator_name(node: &Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(src).ok().map(|s| s.to_string()),
        "function_declarator" | "pointer_declarator" | "array_declarator" => node
            .child_by_field_name("declarator")
            .and_then(|n| c_declarator_name(&n, src)),
        _ => None,
    }
}
