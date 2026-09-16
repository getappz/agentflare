use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::config::InsightsConfig;
use crate::ingest::IngestManager;
use crate::store::InsightsStore;

/// DRY watcher for Claude JSONL + OpenCode SQLite (from agentsview/codedash notify+poll)
#[derive(Debug, Clone)]
pub enum WatchEvent {
    Changed(PathBuf),
    RescanNeeded,
    Tick,
}

pub struct InsightsWatcher {
    pub watched: Vec<PathBuf>,
    pub debounce_ms: u64,
}

impl InsightsWatcher {
    pub fn from_config(config: &InsightsConfig) -> Self {
        let watched = config.sources.values().cloned().collect();
        Self {
            watched,
            debounce_ms: 500,
        }
    }

    pub fn new(watched: Vec<PathBuf>) -> Self {
        Self {
            watched,
            debounce_ms: 500,
        }
    }

    /// Spawn notify watcher + poll fallback, call `on_event` debounced
    pub fn spawn<F>(self, on_event: F) -> notify::Result<()>
    where
        F: FnMut(WatchEvent) + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let tx_clone = tx.clone();

        // Notify watcher -> channel
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(ev) = res {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
        })?;

        for p in &self.watched {
            let watch_path = if p.is_file() {
                p.parent().unwrap_or(Path::new("/tmp")).to_path_buf()
            } else {
                p.clone()
            };
            if watch_path.exists() {
                let _ = watcher.watch(&watch_path, RecursiveMode::Recursive);
            }
        }

        // Poll fallback -> same channel (DRY)
        let poll_watched = self.watched.clone();
        std::thread::spawn(move || {
            let mut last_mtimes: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();
            loop {
                std::thread::sleep(Duration::from_secs(5));
                for p in &poll_watched {
                    if let Ok(meta) = std::fs::metadata(p) {
                        if let Ok(mtime) = meta.modified() {
                            let prev = last_mtimes.get(p);
                            if prev.is_none() || prev.unwrap() != &mtime {
                                last_mtimes.insert(p.clone(), mtime);
                                let _ = tx_clone.send(p.clone());
                            }
                        }
                    }
                }
            }
        });

        // Single consumer with debounce (DRY)
        let debounce = Duration::from_millis(self.debounce_ms);
        std::thread::spawn(move || {
            let mut on_event = on_event;
            let mut last_emit = std::time::Instant::now() - debounce;
            for path in rx {
                if last_emit.elapsed() < debounce {
                    continue;
                }
                last_emit = std::time::Instant::now();
                on_event(WatchEvent::Changed(path));
            }
        });

        std::mem::forget(watcher);
        Ok(())
    }

    /// DRY one-shot rescan helper used by `insights sync --watch` and `serve`
    pub fn rescan_and_store(
        config: &InsightsConfig,
        store: &InsightsStore,
    ) -> (usize, usize, usize) {
        let mgr = IngestManager::new();
        let mut sess = 0;
        let mut turns = 0;
        let mut tools = 0;
        for (source, res) in mgr.scan_all(config, store, |_, _, _| {}) {
            let Ok(bundle) = res else { continue };
            sess += bundle.sessions.len();
            turns += bundle.turns.len();
            tools += bundle.tool_calls.len();
            let _ = store.upsert_sessions_batch(&bundle.sessions);
            let _ = store.upsert_turns_batch(&bundle.turns);
            let _ = store.upsert_tool_calls_batch(&bundle.tool_calls);
            let _ = store.upsert_file_events_batch(&bundle.file_events);
            let _ = store.upsert_subagents_batch(&bundle.subagents);
            if !bundle.file_cursors.is_empty() {
                let _ = store.upsert_file_cursors_batch(&source, &bundle.file_cursors);
            }
        }
        (sess, turns, tools)
    }
}
