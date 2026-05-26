use crate::config::{
    DEFAULT_IGNORES, DEFAULT_IGNORE_EXTENSIONS, DEFAULT_IGNORE_FILENAMES, MAX_FILE_BYTES,
};
use anyhow::Result;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub fn walk_repo(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(repo_root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .build();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();

        if entry.file_type().map(|ft| !ft.is_file()).unwrap_or(true) {
            continue;
        }
        if should_ignore_path(path, repo_root) {
            continue;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let ignored = DEFAULT_IGNORE_EXTENSIONS.iter().any(|&ig| {
                if ig.contains('.') {
                    filename.ends_with(&format!(".{}", ig))
                } else {
                    ext == ig
                }
            });
            if ignored {
                continue;
            }
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if DEFAULT_IGNORE_FILENAMES.contains(&name) {
                continue;
            }
        }
        if let Ok(meta) = path.metadata() {
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
        }
        if is_binary(path) {
            continue;
        }
        files.push(path.to_path_buf());
    }

    files.sort();
    Ok(files)
}

fn should_ignore_path(path: &Path, repo_root: &Path) -> bool {
    if let Ok(rel) = path.strip_prefix(repo_root) {
        for comp in rel.components() {
            if let std::path::Component::Normal(n) = comp {
                if let Some(s) = n.to_str() {
                    if DEFAULT_IGNORES.contains(&s) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_binary(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    buf[..n].contains(&0u8)
}
