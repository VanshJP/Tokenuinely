use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct ArchOverview {
    pub total_files: usize,
    pub total_symbols: usize,
    pub languages: Vec<LangStats>,
    pub top_directories: Vec<DirStats>,
    pub entry_points: Vec<String>,
    pub key_symbols: Vec<KeySymbol>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LangStats {
    pub language: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirStats {
    pub directory: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeySymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub callers: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeadSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: usize,
}

/// Map a file extension to a human-readable language name.
fn ext_to_language(ext: &str) -> &str {
    match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" => "TypeScript (JSX)",
        "jsx" => "JavaScript (JSX)",
        "go" => "Go",
        "java" => "Java",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "cs" => "C#",
        "scala" => "Scala",
        "sh" | "bash" | "zsh" => "Shell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" => "CSS",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        _ => "Other",
    }
}

/// Compute an architecture overview by querying the `files`, `symbols`, and `deps` tables.
pub fn get_architecture(conn: &Connection) -> Result<ArchOverview> {
    // Total files
    let total_files: usize = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .context("Failed to count files")?;

    // Total symbols (gracefully handle missing table)
    let total_symbols: usize = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap_or(0);

    // Language breakdown by file extension
    let mut lang_map: HashMap<String, usize> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path FROM files")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok());
        for path in paths {
            let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
            let lang = ext_to_language(&ext).to_string();
            *lang_map.entry(lang).or_insert(0) += 1;
        }
    }
    let mut languages: Vec<LangStats> = lang_map
        .into_iter()
        .map(|(language, file_count)| LangStats {
            language,
            file_count,
        })
        .collect();
    languages.sort_by_key(|l| std::cmp::Reverse(l.file_count));

    // Top directories: group files by their first path component
    let mut dir_map: HashMap<String, usize> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT path FROM files")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok());
        for path in paths {
            // Use the first path segment; if no separator, use the filename itself
            let first = path.split('/').next().unwrap_or(&path).to_string();
            *dir_map.entry(first).or_insert(0) += 1;
        }
    }
    let mut top_directories: Vec<DirStats> = dir_map
        .into_iter()
        .map(|(directory, file_count)| DirStats {
            directory,
            file_count,
        })
        .collect();
    top_directories.sort_by_key(|d| std::cmp::Reverse(d.file_count));
    top_directories.truncate(10);

    // Entry points: symbols named "main" or decorated with entry-point patterns
    let entry_points: Vec<String> = conn
        .prepare(
            "SELECT name || ' (' || kind || ') in ' || path FROM symbols \
             WHERE name = 'main' OR name LIKE '%entry%' OR name LIKE '%start%'",
        )
        .and_then(|mut stmt| {
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        })
        .unwrap_or_default();

    // Key symbols: top 10 most-called symbols from deps table
    let key_symbols: Vec<KeySymbol> = conn
        .prepare(
            "SELECT s.name, s.kind, s.path, COUNT(*) AS caller_count \
             FROM symbols s \
             JOIN deps d ON d.target_symbol = s.name AND d.kind = 'calls' \
             GROUP BY s.name, s.kind, s.path \
             ORDER BY caller_count DESC \
             LIMIT 10",
        )
        .and_then(|mut stmt| {
            let rows = stmt
                .query_map([], |r| {
                    Ok(KeySymbol {
                        name: r.get(0)?,
                        kind: r.get(1)?,
                        path: r.get(2)?,
                        callers: r.get(3)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        })
        .unwrap_or_default();

    Ok(ArchOverview {
        total_files,
        total_symbols,
        languages,
        top_directories,
        entry_points,
        key_symbols,
    })
}

/// Find functions/methods in `symbols` that have zero incoming `calls` edges in `deps`.
///
/// Excludes:
/// - Symbols named `main`, `new`, or `default`
/// - Symbols in test files (path contains "test")
/// - Symbols whose kind is not "function" or "method"
pub fn find_dead_code(conn: &Connection) -> Result<Vec<DeadSymbol>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, s.kind, s.path, s.line_start \
         FROM symbols s \
         WHERE (s.kind = 'function' OR s.kind = 'method') \
           AND s.name NOT IN ('main', 'new', 'default') \
           AND s.path NOT LIKE '%test%' \
           AND s.name NOT IN ( \
               SELECT d.target_symbol FROM deps d WHERE d.kind = 'calls' \
           ) \
         ORDER BY s.path, s.line_start",
    )?;

    let results = stmt
        .query_map([], |r| {
            Ok(DeadSymbol {
                name: r.get(0)?,
                kind: r.get(1)?,
                path: r.get(2)?,
                line: r.get::<_, usize>(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}
