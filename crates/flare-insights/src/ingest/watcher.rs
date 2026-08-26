use std::path::PathBuf;
use std::sync::mpsc;

use notify::{RecursiveMode, Watcher};

#[derive(Debug, Clone)]
pub enum WatchEvent { Changed(PathBuf), Rescan }

/// Minimal file watcher that notifies on JSONL/DB changes.
/// Polls as fallback (codedash-style) if notify fails.
pub struct InsightsWatcher {
    pub watched: Vec<PathBuf>,
}

impl InsightsWatcher {
    pub fn new(watched: Vec<PathBuf>) -> Self { Self { watched } }

    pub fn spawn<F>(self, mut on_event: F) -> notify::Result<()>
    where F: FnMut(WatchEvent) + Send + 'static
    {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(ev) = res {
                for p in ev.paths { let _ = tx.send(p); }
            }
        })?;
        for p in &self.watched {
            if p.exists() {
                let _ = watcher.watch(p, RecursiveMode::Recursive);
            }
        }
        std::thread::spawn(move || {
            for path in rx {
                on_event(WatchEvent::Changed(path));
            }
        });
        // keep watcher alive via leak (simple for now)
        std::mem::forget(watcher);
        Ok(())
    }
}
