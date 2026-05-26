use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Directory names to ignore when watching for file changes.
#[allow(dead_code)]
pub const WATCH_IGNORES: &[&str] = &[
    ".git",
    "node_modules",
    ".tokenuinely",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    "vendor",
];

const DEBOUNCE_MS: u64 = 500;

/// Returns `true` if any component of `path` matches a WATCH_IGNORES entry.
#[allow(dead_code)]
pub fn should_ignore_watch_path(path: &Path) -> bool {
    path.components().any(|c| {
        if let std::path::Component::Normal(s) = c {
            if let Some(s) = s.to_str() {
                return WATCH_IGNORES.contains(&s);
            }
        }
        false
    })
}

/// Start a recursive file watcher on `repo_root`.
///
/// Changed file paths are sent through `tx`. Events for the same file within
/// 500 ms are coalesced (debounced) so the receiver only sees one notification.
///
/// The returned `RecommendedWatcher` must be kept alive for watching to continue;
/// dropping it stops the watcher.
#[allow(dead_code)]
pub fn start_watcher(
    repo_root: PathBuf,
    tx: tokio::sync::mpsc::Sender<PathBuf>,
) -> Result<notify::RecommendedWatcher> {
    let debounce: std::sync::Arc<Mutex<HashMap<PathBuf, Instant>>> =
        std::sync::Arc::new(Mutex::new(HashMap::new()));

    let mut watcher = notify::recommended_watcher(move |res: std::result::Result<notify::Event, notify::Error>| {
        let event = match res {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Watch error: {e}");
                return;
            }
        };

        // Only care about creates, modifications, and removals.
        use notify::EventKind;
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
            _ => return,
        }

        let now = Instant::now();
        let debounce_dur = Duration::from_millis(DEBOUNCE_MS);

        for path in event.paths {
            if should_ignore_watch_path(&path) {
                continue;
            }

            // Debounce: skip if we sent this path recently.
            {
                let mut map = debounce.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(last) = map.get(&path) {
                    if now.duration_since(*last) < debounce_dur {
                        continue;
                    }
                }
                map.insert(path.clone(), now);
            }

            // blocking_send is safe here because notify calls us on its own
            // background thread, outside the tokio runtime.
            if let Err(e) = tx.blocking_send(path) {
                tracing::warn!("Watch channel send failed: {e}");
            }
        }
    })?;

    watcher.watch(&repo_root, RecursiveMode::Recursive)?;
    tracing::info!("File watcher started on {}", repo_root.display());

    Ok(watcher)
}
