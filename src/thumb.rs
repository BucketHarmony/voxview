//! Thumbnails for the library grid.
//!
//! Every cell wants a picture of its model, and there can be four thousand
//! cells. Three things keep that affordable:
//!
//! * **Only what you can see.** The grid asks for a thumbnail while it draws a
//!   cell; nothing off-screen is ever requested.
//! * **Off the main thread.** Parsing and meshing happen on workers. Only the
//!   draw itself has to be on the thread that owns the device, and that is
//!   rationed to a couple per frame so scrolling never stalls.
//! * **On disk.** The result is a PNG keyed by the file's path, size and
//!   modification time, so the second visit to a folder is a decode rather
//!   than a parse, a mesh and a render.
//!
//! They are rendered by the viewer's own pipeline at one fixed angle, which is
//! the point: a folder of a hundred assets should read as one set, not as a
//! hundred separately framed photographs.

use crate::gfx::Renderer;
use crate::loader;
use crate::mesh;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

/// Edge of a thumbnail in pixels. Two of these fit a 6-column grid on a
/// 1440-wide window with room to spare, and 64 KB of readback is nothing.
pub const SIZE: u32 = 128;

/// Thumbnails rendered per frame. Each costs a GPU round trip, so this is the
/// knob that trades fill-in speed against a smooth scroll.
const PER_FRAME: usize = 2;

/// Requests outstanding at once. Enough to keep the workers busy through a
/// fast scroll without queueing a folder you have already scrolled past.
const IN_FLIGHT: usize = 8;

/// Textures held on the GPU before the least recently drawn are released.
const RESIDENT: usize = 1024;

enum Ready {
    /// Decoded straight from the disk cache: upload and done.
    Cached {
        path: PathBuf,
        rgba: Vec<u8>,
    },
    /// Parsed and meshed; still needs a draw on the device thread.
    Meshed {
        path: PathBuf,
        scene: Box<loader::VoxScene>,
        meshes: Vec<mesh::Mesh>,
    },
    Failed {
        path: PathBuf,
    },
}

pub struct Thumbs {
    jobs: Sender<PathBuf>,
    done: Receiver<Ready>,
    cache_dir: Option<PathBuf>,
    textures: HashMap<PathBuf, egui::TextureHandle>,
    last_used: HashMap<PathBuf, u64>,
    failed: HashMap<PathBuf, ()>,
    in_flight: Vec<PathBuf>,
    wanted: Vec<PathBuf>,
    frame: u64,
}

impl Thumbs {
    pub fn new() -> Thumbs {
        let cache_dir = cache_dir();
        let (jobs, job_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (done_tx, done) = std::sync::mpsc::channel();

        // One worker is enough: it is competing with the library scan for the
        // same disk, and the device thread is the real bottleneck anyway.
        let dir = cache_dir.clone();
        std::thread::Builder::new()
            .name("voxview-thumbs".into())
            .spawn(move || {
                while let Ok(path) = job_rx.recv() {
                    let ready = prepare(&path, dir.as_deref());
                    if done_tx.send(ready).is_err() {
                        return;
                    }
                }
            })
            .ok();

        Thumbs {
            jobs,
            done,
            cache_dir,
            textures: HashMap::new(),
            last_used: HashMap::new(),
            failed: HashMap::new(),
            in_flight: Vec::new(),
            wanted: Vec::new(),
            frame: 0,
        }
    }

    /// Ask for a thumbnail, and hand back whatever is available now.
    ///
    /// Call it while drawing the cell: the request is what marks the asset as
    /// on-screen, and the answer is `None` until it has been through the
    /// worker and the device.
    pub fn get(&mut self, path: &Path) -> Option<egui::TextureHandle> {
        if let Some(handle) = self.textures.get(path) {
            self.last_used.insert(path.to_path_buf(), self.frame);
            return Some(handle.clone());
        }
        if !self.failed.contains_key(path)
            && !self.in_flight.iter().any(|p| p == path)
            && !self.wanted.iter().any(|p| p == path)
        {
            self.wanted.push(path.to_path_buf());
        }
        None
    }

    /// True once a file has been tried and could not be drawn.
    pub fn is_failed(&self, path: &Path) -> bool {
        self.failed.contains_key(path)
    }

    /// Drop what we know about one file, so a hot reload redraws it.
    pub fn forget(&mut self, path: &Path) {
        self.textures.remove(path);
        self.failed.remove(path);
    }

    /// Collect finished work, draw a couple, and dispatch new requests.
    ///
    /// Returns true if anything changed, so the caller knows to redraw.
    pub fn pump(&mut self, ctx: &egui::Context, renderer: &mut Renderer) -> bool {
        self.frame += 1;
        let mut changed = false;
        let mut drawn = 0;

        loop {
            match self.done.try_recv() {
                Ok(Ready::Cached { path, rgba }) => {
                    self.in_flight.retain(|p| *p != path);
                    self.upload(ctx, &path, &rgba);
                    changed = true;
                }
                Ok(Ready::Meshed {
                    path,
                    scene,
                    meshes,
                }) => {
                    self.in_flight.retain(|p| *p != path);
                    if drawn >= PER_FRAME {
                        // Out of budget: put it back at the front of the queue
                        // so the next frame picks it up without re-parsing.
                        self.requeue(path);
                        break;
                    }
                    match renderer.thumbnail(&scene, &meshes, SIZE) {
                        Ok(rgba) => {
                            self.write_cache(&path, &rgba);
                            self.upload(ctx, &path, &rgba);
                            drawn += 1;
                            changed = true;
                        }
                        Err(e) => {
                            eprintln!("voxview: could not draw {}: {e}", path.display());
                            self.failed.insert(path, ());
                        }
                    }
                }
                Ok(Ready::Failed { path }) => {
                    self.in_flight.retain(|p| *p != path);
                    self.failed.insert(path, ());
                    changed = true;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }

        while self.in_flight.len() < IN_FLIGHT && !self.wanted.is_empty() {
            let path = self.wanted.remove(0);
            if self.jobs.send(path.clone()).is_err() {
                break;
            }
            self.in_flight.push(path);
        }
        // Anything still wanted was beyond this frame's budget. Drop it: the
        // cell will ask again next frame if it is still on screen, and if it
        // is not, we have saved the work.
        self.wanted.clear();

        self.evict();
        changed
    }

    /// Put a meshed-but-undrawn model back where the worker output arrives.
    fn requeue(&mut self, path: PathBuf) {
        if !self.wanted.contains(&path) {
            self.wanted.insert(0, path);
        }
    }

    fn upload(&mut self, ctx: &egui::Context, path: &Path, rgba: &[u8]) {
        let expected = (SIZE * SIZE * 4) as usize;
        if rgba.len() != expected {
            self.failed.insert(path.to_path_buf(), ());
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([SIZE as usize, SIZE as usize], rgba);
        let name = path.to_string_lossy().into_owned();
        let handle = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
        self.last_used.insert(path.to_path_buf(), self.frame);
        self.textures.insert(path.to_path_buf(), handle);
    }

    fn write_cache(&self, path: &Path, rgba: &[u8]) {
        let Some(file) = self.cache_path(path) else {
            return;
        };
        if let Some(image) = image::RgbaImage::from_raw(SIZE, SIZE, rgba.to_vec()) {
            // A cache that cannot be written is a slow browser, not a broken
            // one, so a failure here is worth no more than a silent shrug.
            image.save(&file).ok();
        }
    }

    fn cache_path(&self, path: &Path) -> Option<PathBuf> {
        Some(self.cache_dir.as_ref()?.join(cache_name(path)?))
    }

    /// Release the textures that have gone longest without being drawn.
    fn evict(&mut self) {
        if self.textures.len() <= RESIDENT {
            return;
        }
        let mut ages: Vec<(u64, PathBuf)> = self
            .textures
            .keys()
            .map(|p| (self.last_used.get(p).copied().unwrap_or(0), p.clone()))
            .collect();
        ages.sort();
        for (_, path) in ages.into_iter().take(self.textures.len() - RESIDENT) {
            self.textures.remove(&path);
            self.last_used.remove(&path);
        }
    }
}

impl Default for Thumbs {
    fn default() -> Self {
        Thumbs::new()
    }
}

/// Everything that can be done without the GPU: read the cache, or parse and
/// mesh the file.
fn prepare(path: &Path, cache_dir: Option<&Path>) -> Ready {
    if let Some(dir) = cache_dir
        && let Some(name) = cache_name(path)
        && let Ok(image) = image::open(dir.join(name))
    {
        let rgba = image.to_rgba8();
        if rgba.width() == SIZE && rgba.height() == SIZE {
            return Ready::Cached {
                path: path.to_path_buf(),
                rgba: rgba.into_raw(),
            };
        }
    }

    match loader::load_file(path) {
        Ok(scene) => {
            let meshes = mesh::mesh_models(&scene.models);
            Ready::Meshed {
                path: path.to_path_buf(),
                scene: Box::new(scene),
                meshes,
            }
        }
        Err(_) => Ready::Failed {
            path: path.to_path_buf(),
        },
    }
}

/// Cache file name for one asset: its path, length and mtime, hashed.
///
/// Length and mtime are both in the key, so editing a model in MagicaVoxel
/// invalidates its thumbnail without anything having to notice the change.
fn cache_name(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash = fnv(hash, *byte);
    }
    for byte in meta.len().to_le_bytes() {
        hash = fnv(hash, byte);
    }
    for byte in stamp.to_le_bytes() {
        hash = fnv(hash, byte);
    }
    Some(format!("{hash:016x}-{SIZE}.png"))
}

fn fnv(hash: u64, byte: u8) -> u64 {
    (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3)
}

/// Where thumbnails live between runs, following each platform's convention.
fn cache_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
    .unwrap_or_else(std::env::temp_dir);

    let dir = base.join("voxview").join("thumbnails");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_changes_when_the_file_does() {
        let path = std::env::temp_dir().join(format!("voxview-key-{}.vox", std::process::id()));
        std::fs::write(&path, b"one").unwrap();
        let first = cache_name(&path).unwrap();

        // Same length, different content: only the mtime can tell them apart.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, b"two").unwrap();
        let second = cache_name(&path).unwrap();

        std::fs::write(&path, b"a longer body").unwrap();
        let third = cache_name(&path).unwrap();

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert!(first.ends_with(&format!("-{SIZE}.png")));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_missing_file_has_no_cache_key() {
        assert_eq!(cache_name(Path::new("nothing/here.vox")), None);
    }
}
