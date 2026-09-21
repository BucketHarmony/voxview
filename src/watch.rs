//! Watching the open file so edits show up without a reload.

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

/// How long the file has to stay quiet before we re-read it. Editors and
/// exporters write in several bursts; 100 ms is long enough to coalesce those
/// and short enough to feel immediate.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// A debounced watcher for a single file.
pub struct FileWatcher {
    /// Kept alive: dropping the watcher stops the notifications.
    _watcher: notify::RecommendedWatcher,
    events: Receiver<()>,
    path: PathBuf,
    /// When the most recent event arrived, if one is still settling.
    pending: Option<Instant>,
}

impl FileWatcher {
    /// Start watching `path`.
    ///
    /// The *directory* is watched rather than the file, because a great many
    /// tools save by writing a temporary file and renaming it over the
    /// original. That replaces the inode, and a watch on the file itself would
    /// be left pointing at the old one.
    pub fn new(path: &Path) -> Result<FileWatcher> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let (tx, events) = channel();
        let target = path.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            if event.paths.iter().any(|p| same_file(p, &target)) {
                // A full channel or a closed receiver just means the viewer is
                // shutting down; there is nothing useful to do about it.
                let _ = tx.send(());
            }
        })
        .context("could not start the file watcher")?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("could not watch {}", dir.display()))?;

        Ok(FileWatcher {
            _watcher: watcher,
            events,
            path,
            pending: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Call once a frame. Returns true when the file has changed and then
    /// stayed still for [`DEBOUNCE`].
    pub fn poll(&mut self) -> bool {
        loop {
            match self.events.try_recv() {
                Ok(()) => self.pending = Some(Instant::now()),
                Err(TryRecvError::Empty) => break,
                // The watcher thread is gone; nothing more will ever arrive.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        match self.pending {
            Some(at) if at.elapsed() >= DEBOUNCE => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

/// Compare paths tolerantly: events may or may not arrive canonicalised, and
/// on Windows they may differ in case or in the `\\?\` prefix.
fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(a) == canon(b) {
        return true;
    }
    // A rename-into-place may report the temporary name; fall back to the
    // final component so those still trigger a reload.
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_matches_itself_and_not_a_sibling() {
        assert!(same_file(Path::new("a/b.vox"), Path::new("a/b.vox")));
        assert!(same_file(Path::new("a/b.vox"), Path::new("c/b.vox")));
        assert!(!same_file(Path::new("a/b.vox"), Path::new("a/c.vox")));
    }

    #[test]
    fn watching_a_missing_directory_is_an_error_not_a_panic() {
        let missing = Path::new("definitely/not/here/model.vox");
        assert!(FileWatcher::new(missing).is_err());
    }
}
