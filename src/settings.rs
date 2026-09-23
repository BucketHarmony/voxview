//! What voxview remembers between runs.
//!
//! Window geometry, the overlay toggles, how the library was last sorted and
//! looked at, and the collections you built. A viewer that forgets all of that
//! every launch feels unfinished no matter how well it renders.
//!
//! The file is line-based `key = value` text rather than JSON or TOML. Two
//! reasons: it needs no dependency, and a settings file people can open and
//! fix by hand is a feature when something goes wrong. Unknown keys are
//! ignored and malformed values fall back to the default, so a file written by
//! a newer version -- or edited badly -- degrades instead of failing.

use crate::gfx::Background;
use crate::library::{Sort, View};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Everything carried across runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub window: Window,

    pub background: Background,
    pub grid: bool,
    pub bbox: bool,
    pub axes: bool,
    pub occlusion: bool,
    /// Multisample count for the 3D view: 1, 2, 4 or 8.
    pub msaa: u32,
    /// `true` for an orthographic camera, which is what most asset inspection
    /// wants; `false` for perspective.
    pub orthographic: bool,

    pub view: View,
    pub sort: Sort,
    pub descending: bool,
    pub stack_variants: bool,
    pub cell: f32,
    /// The directory the library was last pointed at, reopened when voxview is
    /// started with no argument.
    pub root: Option<PathBuf>,

    /// Collections, as names and the absolute paths that were in them.
    ///
    /// Paths rather than asset indices: indices only mean something relative
    /// to one scan, and the next scan may find a different set of files.
    pub collections: Vec<(String, Vec<PathBuf>)>,
}

/// Where and how big the window was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub width: u32,
    pub height: u32,
    /// Screen position, when it was known. Restored only if it still lands on
    /// a monitor -- see [`Window::sane`].
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

impl Default for Window {
    fn default() -> Window {
        Window {
            width: 1280,
            height: 800,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

impl Window {
    /// Reject geometry that would open the window somewhere unusable.
    ///
    /// A saved position is only good until the monitor it named goes away, and
    /// a window restored onto a display that no longer exists is invisible and
    /// looks exactly like a crash. Rather than enumerate monitors, this throws
    /// out what is obviously wrong and lets the compositor place the rest.
    pub fn sane(self) -> Window {
        let mut out = self;
        out.width = self.width.clamp(480, 16_384);
        out.height = self.height.clamp(360, 16_384);
        let off_screen = |v: Option<i32>| v.is_some_and(|v| !(-32_000..32_000).contains(&v));
        if off_screen(self.x) || off_screen(self.y) {
            out.x = None;
            out.y = None;
        }
        out
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            window: Window::default(),
            background: Background::Dark,
            grid: true,
            bbox: false,
            axes: true,
            occlusion: true,
            msaa: 4,
            orthographic: false,
            view: View::Grid,
            sort: Sort::Name,
            descending: false,
            stack_variants: true,
            cell: 128.0,
            root: None,
            collections: Vec::new(),
        }
    }
}

impl Settings {
    /// Read the settings file, or hand back the defaults.
    ///
    /// Never fails. A missing file is the first run, and an unreadable one is
    /// not worth refusing to start over.
    pub fn load() -> Settings {
        match path().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => Settings::parse(&text),
            None => Settings::default(),
        }
    }

    /// Write the settings file, reporting why if it could not be written.
    ///
    /// Called on the way out, where nothing can be done about a failure except
    /// say so on stderr -- which is still better than silence, because the
    /// symptom otherwise is "it keeps forgetting my window size".
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = path() else {
            return Err(std::io::Error::other(
                "no configuration directory on this platform",
            ));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write beside the target and rename, so an interrupted save cannot
        // leave a half-written settings file behind.
        let temp = path.with_extension("txt.new");
        std::fs::write(&temp, self.to_text())?;
        std::fs::rename(&temp, &path)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("# voxview settings\n# Edit freely: unknown keys are ignored.\n\n");

        let w = self.window;
        let _ = writeln!(out, "window.width = {}", w.width);
        let _ = writeln!(out, "window.height = {}", w.height);
        if let Some(x) = w.x {
            let _ = writeln!(out, "window.x = {x}");
        }
        if let Some(y) = w.y {
            let _ = writeln!(out, "window.y = {y}");
        }
        let _ = writeln!(out, "window.maximized = {}", w.maximized);

        let _ = writeln!(
            out,
            "\nview.background = {}",
            match self.background {
                Background::Dark => "dark",
                Background::Light => "light",
            }
        );
        let _ = writeln!(out, "view.grid = {}", self.grid);
        let _ = writeln!(out, "view.bbox = {}", self.bbox);
        let _ = writeln!(out, "view.axes = {}", self.axes);
        let _ = writeln!(out, "view.occlusion = {}", self.occlusion);
        let _ = writeln!(out, "view.msaa = {}", self.msaa);
        let _ = writeln!(out, "view.orthographic = {}", self.orthographic);

        let _ = writeln!(
            out,
            "\nlibrary.view = {}",
            match self.view {
                View::Grid => "grid",
                View::List => "list",
            }
        );
        let _ = writeln!(
            out,
            "library.sort = {}",
            match self.sort {
                Sort::Name => "name",
                Sort::Extent => "extent",
                Sort::Voxels => "voxels",
                Sort::Modified => "modified",
            }
        );
        let _ = writeln!(out, "library.descending = {}", self.descending);
        let _ = writeln!(out, "library.stack_variants = {}", self.stack_variants);
        let _ = writeln!(out, "library.cell = {}", self.cell);
        if let Some(root) = &self.root {
            let _ = writeln!(out, "library.root = {}", root.display());
        }

        for (name, members) in &self.collections {
            // A name with a newline in it would forge a key, so it is dropped
            // rather than escaped: nothing here is worth an escaping scheme.
            if name.contains('\n') || name.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\ncollection = {name}");
            for path in members {
                let line = path.display().to_string();
                if !line.contains('\n') {
                    let _ = writeln!(out, "member = {line}");
                }
            }
        }
        out
    }

    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        // Collections are written as a `collection` line followed by its
        // `member` lines, so parsing carries the open one along.
        let mut open: Option<(String, Vec<PathBuf>)> = None;

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            if key != "member" && key != "collection" {
                open = take(open, &mut s);
            }
            match key {
                "window.width" => set(&mut s.window.width, value),
                "window.height" => set(&mut s.window.height, value),
                "window.x" => s.window.x = value.parse().ok(),
                "window.y" => s.window.y = value.parse().ok(),
                "window.maximized" => set(&mut s.window.maximized, value),

                "view.background" => {
                    if let Some(b) = match value {
                        "dark" => Some(Background::Dark),
                        "light" => Some(Background::Light),
                        _ => None,
                    } {
                        s.background = b;
                    }
                }
                "view.grid" => set(&mut s.grid, value),
                "view.bbox" => set(&mut s.bbox, value),
                "view.axes" => set(&mut s.axes, value),
                "view.occlusion" => set(&mut s.occlusion, value),
                "view.msaa" => {
                    if let Ok(n) = value.parse::<u32>()
                        && matches!(n, 1 | 2 | 4 | 8)
                    {
                        s.msaa = n;
                    }
                }
                "view.orthographic" => set(&mut s.orthographic, value),

                "library.view" => {
                    if let Some(v) = match value {
                        "grid" => Some(View::Grid),
                        "list" => Some(View::List),
                        _ => None,
                    } {
                        s.view = v;
                    }
                }
                "library.sort" => {
                    if let Some(v) = match value {
                        "name" => Some(Sort::Name),
                        "extent" => Some(Sort::Extent),
                        "voxels" => Some(Sort::Voxels),
                        "modified" => Some(Sort::Modified),
                        _ => None,
                    } {
                        s.sort = v;
                    }
                }
                "library.descending" => set(&mut s.descending, value),
                "library.stack_variants" => set(&mut s.stack_variants, value),
                "library.cell" => {
                    if let Ok(v) = value.parse::<f32>()
                        && v.is_finite()
                    {
                        s.cell = v.clamp(48.0, 512.0);
                    }
                }
                "library.root" => {
                    if !value.is_empty() {
                        s.root = Some(PathBuf::from(value));
                    }
                }

                "collection" => {
                    open = take(open, &mut s);
                    if !value.is_empty() {
                        open = Some((value.to_string(), Vec::new()));
                    }
                }
                "member" => {
                    if let Some((_, members)) = &mut open
                        && !value.is_empty()
                    {
                        members.push(PathBuf::from(value));
                    }
                }
                _ => {}
            }
        }
        take(open, &mut s);
        s.window = s.window.sane();
        s
    }
}

/// Close off the collection being read, if there is one.
fn take(open: Option<(String, Vec<PathBuf>)>, s: &mut Settings) -> Option<(String, Vec<PathBuf>)> {
    if let Some(entry) = open {
        s.collections.push(entry);
    }
    None
}

/// Parse into `slot`, leaving the default in place if the value is nonsense.
fn set<T: std::str::FromStr>(slot: &mut T, value: &str) {
    if let Ok(parsed) = value.parse() {
        *slot = parsed;
    }
}

/// Where the settings file lives, following each platform's convention.
///
/// The same shape as the thumbnail cache in [`crate::thumb`], one directory
/// over: cache is data you can delete, configuration is data you would miss.
pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.txt"))
}

fn dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("voxview"))
}

/// Does this path still exist and look like something to browse?
pub fn usable_root(root: &Path) -> bool {
    root.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_keeps_every_field() {
        let mut s = Settings {
            window: Window {
                width: 1600,
                height: 900,
                x: Some(-12),
                y: Some(40),
                maximized: true,
            },
            background: Background::Light,
            grid: false,
            bbox: true,
            axes: false,
            occlusion: false,
            msaa: 8,
            orthographic: true,
            view: View::List,
            sort: Sort::Voxels,
            descending: true,
            stack_variants: false,
            cell: 96.0,
            root: Some(PathBuf::from("/some/assets")),
            collections: vec![
                ("Favourites".into(), vec!["/a.vox".into(), "/b.vox".into()]),
                ("Empty".into(), vec![]),
            ],
        };
        s.window = s.window.sane();
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
    }

    #[test]
    fn nonsense_values_leave_the_defaults_alone() {
        // The point of the format: a bad line costs that one setting, not the
        // whole file and not the launch.
        let s = Settings::parse(
            "window.width = banana\n\
             view.msaa = 3\n\
             view.grid = yes-please\n\
             library.sort = by-vibes\n\
             library.cell = NaN\n\
             totally.unknown.key = 7\n\
             a line with no equals sign\n\
             view.bbox = true\n",
        );
        let d = Settings::default();
        assert_eq!(s.window.width, d.window.width);
        assert_eq!(s.msaa, d.msaa, "3 is not a legal sample count");
        assert_eq!(s.grid, d.grid);
        assert_eq!(s.sort, d.sort);
        assert_eq!(s.cell, d.cell);
        assert!(s.bbox, "the one good line still applied");
    }

    #[test]
    fn a_window_off_the_edge_of_the_world_is_forgotten() {
        // A saved position outlives the monitor it named. Restoring it puts
        // the window somewhere invisible, which reads as a crash.
        let s = Settings::parse("window.x = 99999\nwindow.y = 10\n");
        assert_eq!(s.window.x, None);
        assert_eq!(s.window.y, None);

        let s = Settings::parse("window.width = 3\nwindow.height = 999999\n");
        assert_eq!(s.window.width, 480, "clamped up to something usable");
        assert_eq!(s.window.height, 16_384, "clamped down");
    }

    #[test]
    fn collections_survive_as_paths() {
        let s = Settings::parse(
            "collection = Trees\n\
             member = /forest/oak.vox\n\
             member = /forest/pine.vox\n\
             collection = Rocks\n\
             member = /quarry/granite.vox\n",
        );
        assert_eq!(s.collections.len(), 2);
        assert_eq!(s.collections[0].0, "Trees");
        assert_eq!(s.collections[0].1.len(), 2);
        assert_eq!(s.collections[1].0, "Rocks");
        assert_eq!(s.collections[1].1.len(), 1);
    }

    #[test]
    fn a_member_before_any_collection_is_dropped() {
        let s = Settings::parse("member = /orphan.vox\ncollection = Real\n");
        assert_eq!(s.collections.len(), 1);
        assert!(s.collections[0].1.is_empty());
    }

    #[test]
    fn a_name_with_a_newline_cannot_forge_a_key() {
        let s = Settings {
            collections: vec![("bad\nview.grid = false".into(), vec![])],
            ..Settings::default()
        };
        let text = s.to_text();
        assert!(!text.contains("bad\nview.grid"));
        assert!(Settings::parse(&text).grid, "grid was not turned off");
    }
}
