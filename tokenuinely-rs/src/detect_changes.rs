use crate::db::Db;
use crate::hasher::sha256_file;
use anyhow::Result;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub enum ChangeStatus {
    Modified,
    Added,
    Deleted,
    HashMismatch,
}

impl std::fmt::Display for ChangeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeStatus::Modified => write!(f, "modified"),
            ChangeStatus::Added => write!(f, "added"),
            ChangeStatus::Deleted => write!(f, "deleted"),
            ChangeStatus::HashMismatch => write!(f, "hash-mismatch"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: ChangeStatus,
    pub stale_header: Option<String>,
}

pub fn detect_changes(repo_root: &Path) -> Result<Vec<ChangedFile>> {
    let db = Db::open(repo_root)?;
    let mut changes = Vec::new();

    // 1. Parse git status --porcelain
    let output = Command::new("git")
        .args(["-C", &repo_root.to_string_lossy(), "status", "--porcelain"])
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.len() < 4 {
                continue;
            }
            let status_code = &line[..2];
            let file_path = line[3..].trim().to_string();

            let change_status = match status_code.trim() {
                "M" | "MM" | "AM" => ChangeStatus::Modified,
                "A" | "??" => ChangeStatus::Added,
                "D" => ChangeStatus::Deleted,
                _ => ChangeStatus::Modified,
            };

            let stale_header = db.get_header(&file_path).ok().flatten();
            changes.push(ChangedFile {
                path: file_path,
                status: change_status,
                stale_header,
            });
        }
    }

    // 2. For indexed files not in git status, compare sha256
    let indexed_paths = db.list_all_paths()?;
    let git_changed: std::collections::HashSet<String> =
        changes.iter().map(|c| c.path.clone()).collect();

    for indexed_path in indexed_paths {
        if git_changed.contains(&indexed_path) {
            continue;
        }
        let full_path = repo_root.join(&indexed_path);
        if !full_path.exists() {
            let stale_header = db.get_header(&indexed_path).ok().flatten();
            changes.push(ChangedFile {
                path: indexed_path,
                status: ChangeStatus::Deleted,
                stale_header,
            });
            continue;
        }
        if let Ok(current_hash) = sha256_file(&full_path) {
            if let Ok(Some(stored_hash)) = db.get_sha256(&indexed_path) {
                if current_hash != stored_hash {
                    let stale_header = db.get_header(&indexed_path).ok().flatten();
                    changes.push(ChangedFile {
                        path: indexed_path,
                        status: ChangeStatus::HashMismatch,
                        stale_header,
                    });
                }
            }
        }
    }

    Ok(changes)
}
