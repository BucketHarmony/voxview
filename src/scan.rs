//! Reading a whole asset tree without making the window wait.
//!
//! The library needs two different things from disk, at two different speeds:
//! the *names*, which it must have before it can draw anything at all, and the
//! *stats* -- extent, voxel count, palette -- which are worth a full parse per
//! file and cannot hold up the first frame. So the walk runs first and reports
//! in one message, then a small pool of workers parses the files it found and
//! reports each result as it lands. The UI drains whatever has arrived at the
//! top of each frame and redraws; a cold tree fills in under your eyes.
//!
//! A scan that is no longer wanted is cancelled by dropping it. The workers
//! check the flag between files, so the worst case is one more parse.

use crate::library::{AssetStats, Load, PALETTE_HEAD};
use crate::loader;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

/// Most workers to put on the parse; more than this is disk-bound anyway.
const MAX_WORKERS: usize = 4;

/// News from the scan thread.
pub enum Msg {
    /// The walk finished. Sent once, before any `Stats`.
    Found {
        files: Vec<PathBuf>,
        skipped: Vec<(PathBuf, String)>,
        truncated: bool,
    },
    Stats {
        path: PathBuf,
        load: Load,
    },
    /// Every file has been parsed.
    Done,
}

pub struct Scan {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

impl Scan {
    /// Start walking and parsing `root` on background threads.
    pub fn start(root: PathBuf) -> Scan {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        // Detached: the cancel flag and a closed channel are enough to stop it,
        // and nothing downstream needs to block on the thread ending.
        std::thread::Builder::new()
            .name("voxview-scan".into())
            .spawn(move || run(root, tx, flag))
            .ok();
        Scan {
            rx,
            cancel,
            finished: false,
        }
    }

    /// Everything that has arrived since the last call. Never blocks.
    pub fn drain(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(msg) => {
                    if matches!(msg, Msg::Done) {
                        self.finished = true;
                    }
                    out.push(msg);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        out
    }

    pub fn finished(&self) -> bool {
        self.finished
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn run(root: PathBuf, tx: Sender<Msg>, cancel: Arc<AtomicBool>) {
    let walk = loader::walk_vox_dir(&root);
    let files = walk.files.clone();
    if tx
        .send(Msg::Found {
            files: walk.files,
            skipped: walk.skipped,
            truncated: walk.truncated,
        })
        .is_err()
    {
        return;
    }

    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(1, MAX_WORKERS))
        .unwrap_or(1);

    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let (files, next, tx, cancel) = (
            Arc::clone(&files),
            Arc::clone(&next),
            tx.clone(),
            Arc::clone(&cancel),
        );
        let handle = std::thread::Builder::new()
            .name("voxview-parse".into())
            .spawn(move || {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = files.get(i) else { return };
                    let load = stats_of(path);
                    if tx
                        .send(Msg::Stats {
                            path: path.clone(),
                            load,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        if let Ok(handle) = handle {
            handles.push(handle);
        }
    }
    for handle in handles {
        handle.join().ok();
    }
    tx.send(Msg::Done).ok();
}

/// Parse one file for its stats. Errors become a `Failed` load, never a panic:
/// a malformed asset should cost that one cell, not the browser.
pub fn stats_of(path: &std::path::Path) -> Load {
    let meta = std::fs::metadata(path).ok();
    let scene = match loader::load_file(path) {
        Ok(scene) => scene,
        // Only the innermost reason is worth a cell's worth of space; the
        // outer context is the file name, which the cell already shows.
        Err(e) => return Load::Failed(root_cause(&e)),
    };
    let dims = scene.dimensions();
    Load::Ready(AssetStats {
        dims: [dims.x, dims.y, dims.z],
        voxels: scene.voxel_count,
        models: scene.models.len(),
        instances: scene.instances.len(),
        palette_from_file: scene.palette_from_file,
        palette_head: (0..PALETTE_HEAD)
            .map(|i| {
                let c = scene.palette.color(i as u8).to_array();
                [c[0], c[1], c[2]]
            })
            .collect(),
        bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        modified: meta.and_then(|m| m.modified().ok()),
    })
}

fn root_cause(error: &anyhow::Error) -> String {
    error
        .chain()
        .last()
        .map(|e| e.to_string())
        .unwrap_or_else(|| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_file_reports_a_failure_rather_than_panicking() {
        let path = std::env::temp_dir().join(format!("voxview-bad-{}.vox", std::process::id()));
        std::fs::write(&path, b"definitely not a vox file").unwrap();
        assert!(matches!(stats_of(&path), Load::Failed(_)));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scanning_an_empty_tree_still_finishes() {
        let root = std::env::temp_dir().join(format!("voxview-empty-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut scan = Scan::start(root.clone());
        let mut found = false;
        for _ in 0..200 {
            for msg in scan.drain() {
                match msg {
                    Msg::Found { files, .. } => {
                        assert!(files.is_empty());
                        found = true;
                    }
                    Msg::Done => {
                        assert!(found);
                        std::fs::remove_dir_all(&root).ok();
                        return;
                    }
                    Msg::Stats { .. } => unreachable!("no files to parse"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the scan never reported Done");
    }
}
