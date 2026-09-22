//! What the library browser shows, and why.
//!
//! This module is deliberately free of egui and of wgpu: it is the answer to
//! "given 4,753 files on disk, which ones belong on screen right now, in what
//! order, and grouped how". The UI layer reads it and draws; the scanner
//! writes into it from a background thread's messages.
//!
//! Three ideas carry most of the weight:
//!
//! * **Folders are not the only grouping.** A *collection* cuts across the
//!   tree, because the thing you are working on -- a tavern, one creature --
//!   is almost never one directory.
//! * **Variant families.** Veloren ships `crate-0` through `crate-6` and
//!   `bed_cliff_head`/`_middle`/`_tail`. Seven cells for one crate is noise,
//!   so files sharing a name prefix collapse into one row you can open.
//! * **Facet counts are live.** Every filter shows how many assets it would
//!   leave, counted against everything *except* its own group, which is what
//!   makes a facet list worth reading before you click it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Anything under 24 hours old counts as "recently changed".
const RECENT: Duration = Duration::from_secs(24 * 60 * 60);

/// Palette entries kept per asset for the inspector's swatch strip.
pub const PALETTE_HEAD: usize = 32;

/// What the background scan learned about one file.
#[derive(Clone, Debug)]
pub struct AssetStats {
    pub dims: [i32; 3],
    pub voxels: usize,
    pub models: usize,
    pub instances: usize,
    /// False when the file carried no `RGBA` chunk and the MagicaVoxel
    /// default palette was substituted.
    pub palette_from_file: bool,
    pub palette_head: Vec<[u8; 3]>,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

impl AssetStats {
    /// Longest side in voxels; what the extent facets bucket on.
    pub fn longest_side(&self) -> i32 {
        self.dims[0].max(self.dims[1]).max(self.dims[2])
    }
}

/// How far the scan has got with one file.
#[derive(Clone, Debug)]
pub enum Load {
    Pending,
    Ready(AssetStats),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub path: PathBuf,
    /// File stem: `barrel_wood_water`, not `barrel_wood_water.vox`.
    pub name: String,
    /// Index into [`Library::folders`].
    pub folder: usize,
    /// Set when this asset shares a name prefix with a sibling.
    pub family: Option<String>,
    pub load: Load,
}

impl Asset {
    pub fn stats(&self) -> Option<&AssetStats> {
        match &self.load {
            Load::Ready(s) => Some(s),
            _ => None,
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self.load, Load::Failed(_))
    }
}

/// One directory in the rail's tree.
#[derive(Clone, Debug)]
pub struct Folder {
    pub name: String,
    pub path: PathBuf,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    /// `.vox` files sitting directly in this directory.
    pub direct: usize,
    /// `.vox` files in this directory and everything under it.
    pub total: usize,
    pub expanded: bool,
}

/// A hand-made set that cuts across the folder tree.
#[derive(Clone, Debug)]
pub struct Collection {
    pub name: String,
    /// Asset indices, in the order they were added.
    pub members: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Grid,
    List,
}

/// What the centre pane is showing the contents of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// A folder and everything beneath it.
    Folder(usize),
    Collection(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Name,
    Extent,
    Voxels,
    Modified,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "Name",
            Sort::Extent => "Extent",
            Sort::Voxels => "Voxels",
            Sort::Modified => "Modified",
        }
    }

    pub const ALL: [Sort; 4] = [Sort::Name, Sort::Extent, Sort::Voxels, Sort::Modified];
}

/// The filter checkboxes, as a flat set of flags.
///
/// All-off means "no filter from this group", which is what makes the counts
/// readable: nothing is hidden until you choose to hide it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facets {
    /// `<= 16`, `<= 32`, `> 32` on the longest side.
    pub extent: [bool; 3],
    pub palette_from_file: bool,
    pub palette_default: bool,
    pub recent: bool,
    pub failed: bool,
}

impl Facets {
    fn extent_any(&self) -> bool {
        self.extent.iter().any(|b| *b)
    }

    fn clear(&mut self) {
        *self = Facets::default();
    }

    pub fn any(&self) -> bool {
        self.extent_any()
            || self.palette_from_file
            || self.palette_default
            || self.recent
            || self.failed
    }
}

/// One entry in the centre pane: a single asset, or a stack of variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    One(usize),
    /// A collapsed or expanded variant family. When expanded, the `One` rows
    /// for its members follow immediately.
    Family {
        key: String,
        members: Vec<usize>,
    },
}

impl Row {
    /// The asset whose thumbnail and stats represent this row.
    pub fn lead(&self) -> usize {
        match self {
            Row::One(i) => *i,
            Row::Family { members, .. } => members[0],
        }
    }
}

/// One line of the rail's folder tree, after single-child chains are merged.
#[derive(Clone, Debug)]
pub struct TreeRow {
    pub folder: usize,
    /// `voxygen · voxel` when a chain of pass-through directories was merged.
    pub label: String,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
    pub total: usize,
}

/// A count for one facet checkbox.
#[derive(Clone, Copy, Debug)]
pub struct FacetCount {
    pub matching: usize,
    pub on: bool,
}

pub struct Library {
    pub root: PathBuf,
    pub folders: Vec<Folder>,
    pub assets: Vec<Asset>,
    pub collections: Vec<Collection>,

    pub scope: Scope,
    pub find: String,
    pub view: View,
    pub sort: Sort,
    pub descending: bool,
    pub stack_variants: bool,
    pub facets: Facets,
    /// Cell width in the grid, in points.
    pub cell: f32,

    /// Asset indices, in click order. The last one drives the inspector.
    pub selection: Vec<usize>,
    /// Where a shift-click range starts.
    pub anchor: Option<usize>,
    /// Row index the keyboard cursor sits on.
    pub cursor: usize,
    expanded_families: Vec<String>,

    /// Directories that could not be read during the walk.
    pub skipped: Vec<(PathBuf, String)>,
    pub truncated: bool,
    /// Cleared whenever anything that affects [`Library::rows`] changes.
    cache: Option<Vec<Row>>,
}

impl Library {
    /// Build the folder tree for a set of files already found on disk.
    pub fn new(root: PathBuf, files: Vec<PathBuf>) -> Library {
        let mut lib = Library {
            root: root.clone(),
            folders: vec![Folder {
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.to_string_lossy().into_owned()),
                path: root,
                parent: None,
                children: Vec::new(),
                depth: 0,
                direct: 0,
                total: 0,
                expanded: true,
            }],
            assets: Vec::new(),
            collections: Vec::new(),
            scope: Scope::Folder(0),
            find: String::new(),
            view: View::Grid,
            sort: Sort::Name,
            descending: false,
            stack_variants: true,
            facets: Facets::default(),
            cell: 150.0,
            selection: Vec::new(),
            anchor: None,
            cursor: 0,
            expanded_families: Vec::new(),
            skipped: Vec::new(),
            truncated: false,
            cache: None,
        };
        lib.add_files(files);
        lib
    }

    /// Insert files into the tree, creating folders as needed.
    fn add_files(&mut self, files: Vec<PathBuf>) {
        let mut by_dir: HashMap<PathBuf, usize> = HashMap::new();
        by_dir.insert(self.root.clone(), 0);

        for path in files {
            let Some(dir) = path.parent() else { continue };
            let folder = self.folder_for(dir, &mut by_dir);
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.folders[folder].direct += 1;
            let mut up = Some(folder);
            while let Some(f) = up {
                self.folders[f].total += 1;
                up = self.folders[f].parent;
            }
            self.assets.push(Asset {
                path,
                name,
                folder,
                family: None,
                load: Load::Pending,
            });
        }

        self.assign_families();
        self.cache = None;
    }

    /// Find or create the folder for `dir`, walking up to the root as needed.
    fn folder_for(&mut self, dir: &Path, by_dir: &mut HashMap<PathBuf, usize>) -> usize {
        if let Some(i) = by_dir.get(dir) {
            return *i;
        }
        // A file outside the root should still land somewhere sane.
        let Some(parent) = dir.parent() else {
            return 0;
        };
        if parent == dir {
            return 0;
        }
        let parent_index = self.folder_for(parent, by_dir);
        let index = self.folders.len();
        self.folders.push(Folder {
            name: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.to_string_lossy().into_owned()),
            path: dir.to_path_buf(),
            parent: Some(parent_index),
            children: Vec::new(),
            depth: self.folders[parent_index].depth + 1,
            direct: 0,
            total: 0,
            expanded: false,
        });
        self.folders[parent_index].children.push(index);
        by_dir.insert(dir.to_path_buf(), index);
        index
    }

    /// Tag every asset that shares a name prefix with a sibling.
    fn assign_families(&mut self) {
        let mut counts: HashMap<(usize, String), usize> = HashMap::new();
        for asset in &self.assets {
            if let Some(key) = family_key(&asset.name) {
                *counts.entry((asset.folder, key)).or_default() += 1;
            }
        }
        for asset in &mut self.assets {
            asset.family = family_key(&asset.name).filter(|key| {
                counts
                    .get(&(asset.folder, key.clone()))
                    .is_some_and(|n| *n > 1)
            });
        }
    }

    /// Record what the scan learned about one file.
    pub fn apply(&mut self, path: &Path, load: Load) {
        if let Some(asset) = self.assets.iter_mut().find(|a| a.path == path) {
            asset.load = load;
            self.cache = None;
        }
    }

    /// Path of `asset` relative to the library root, with `/` separators.
    pub fn relative(&self, asset: usize) -> String {
        let path = &self.assets[asset].path;
        relative_to(&self.root, path.parent().unwrap_or(path))
    }

    pub fn folder_relative(&self, folder: usize) -> String {
        relative_to(&self.root, &self.folders[folder].path)
    }

    // ---- the rail -------------------------------------------------------

    /// The folder tree, flattened for drawing.
    ///
    /// A directory that holds no `.vox` files of its own and has exactly one
    /// child is not worth a row: `voxygen` and `voxel` merge into one line,
    /// which is how the path reads out loud anyway.
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_tree_row(0, 0, &mut rows);
        rows
    }

    fn push_tree_row(&self, folder: usize, depth: usize, out: &mut Vec<TreeRow>) {
        let mut label = self.folders[folder].name.clone();
        let mut last = folder;
        while self.folders[last].direct == 0 && self.folders[last].children.len() == 1 {
            last = self.folders[last].children[0];
            label.push_str(" \u{b7} ");
            label.push_str(&self.folders[last].name);
        }
        let node = &self.folders[last];
        out.push(TreeRow {
            folder: last,
            label,
            depth,
            has_children: !node.children.is_empty(),
            expanded: node.expanded,
            total: node.total,
        });
        if node.expanded {
            for child in &node.children {
                self.push_tree_row(*child, depth + 1, out);
            }
        }
    }

    pub fn toggle_folder(&mut self, folder: usize) {
        self.folders[folder].expanded = !self.folders[folder].expanded;
    }

    /// Open every ancestor of `folder` so it is visible in the rail.
    pub fn reveal(&mut self, folder: usize) {
        let mut up = self.folders[folder].parent;
        while let Some(f) = up {
            self.folders[f].expanded = true;
            up = self.folders[f].parent;
        }
    }

    pub fn set_scope(&mut self, scope: Scope) {
        if self.scope != scope {
            self.scope = scope;
            self.selection.clear();
            self.anchor = None;
            self.cursor = 0;
            self.cache = None;
        }
    }

    /// Ancestors of the current folder, root first, for the breadcrumb.
    pub fn breadcrumb(&self) -> Vec<usize> {
        let Scope::Folder(mut f) = self.scope else {
            return Vec::new();
        };
        let mut crumbs = vec![f];
        while let Some(parent) = self.folders[f].parent {
            crumbs.push(parent);
            f = parent;
        }
        crumbs.reverse();
        crumbs
    }

    // ---- filtering ------------------------------------------------------

    /// Assets inside the current scope, before find and facets.
    fn in_scope(&self) -> Vec<usize> {
        match self.scope {
            Scope::Folder(f) => {
                let mut wanted = vec![false; self.folders.len()];
                mark_subtree(&self.folders, f, &mut wanted);
                (0..self.assets.len())
                    .filter(|i| wanted[self.assets[*i].folder])
                    .collect()
            }
            Scope::Collection(c) => self
                .collections
                .get(c)
                .map(|c| c.members.clone())
                .unwrap_or_default(),
        }
    }

    fn matches_find(&self, asset: usize) -> bool {
        if self.find.is_empty() {
            return true;
        }
        let needle = self.find.to_lowercase();
        // A find with a slash in it is about where the file lives, not what it
        // is called, so match the path instead.
        let hay = if needle.contains('/') {
            format!("{}/{}", self.relative(asset), self.assets[asset].name)
        } else {
            self.assets[asset].name.clone()
        };
        hay.to_lowercase().contains(&needle)
    }

    fn matches_extent(&self, asset: usize, facets: &Facets) -> bool {
        if !facets.extent_any() {
            return true;
        }
        let Some(stats) = self.assets[asset].stats() else {
            return false;
        };
        let side = stats.longest_side();
        let bucket = if side <= 16 {
            0
        } else if side <= 32 {
            1
        } else {
            2
        };
        facets.extent[bucket]
    }

    fn matches_palette(&self, asset: usize, facets: &Facets) -> bool {
        if !facets.palette_from_file && !facets.palette_default {
            return true;
        }
        let Some(stats) = self.assets[asset].stats() else {
            return false;
        };
        (facets.palette_from_file && stats.palette_from_file)
            || (facets.palette_default && !stats.palette_from_file)
    }

    fn matches_state(&self, asset: usize, facets: &Facets, now: SystemTime) -> bool {
        if !facets.recent && !facets.failed {
            return true;
        }
        let asset = &self.assets[asset];
        let recent = facets.recent
            && asset
                .stats()
                .and_then(|s| s.modified)
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|age| age < RECENT);
        (recent) || (facets.failed && asset.failed())
    }

    fn passes(&self, asset: usize, facets: &Facets, now: SystemTime) -> bool {
        self.matches_find(asset)
            && self.matches_extent(asset, facets)
            && self.matches_palette(asset, facets)
            && self.matches_state(asset, facets, now)
    }

    /// Every asset that survives scope, find and facets, sorted.
    pub fn filtered(&self) -> Vec<usize> {
        let now = SystemTime::now();
        let mut out: Vec<usize> = self
            .in_scope()
            .into_iter()
            .filter(|i| self.passes(*i, &self.facets, now))
            .collect();
        self.sort_assets(&mut out);
        out
    }

    fn sort_assets(&self, list: &mut [usize]) {
        list.sort_by(|a, b| {
            let (x, y) = (&self.assets[*a], &self.assets[*b]);
            let ord = match self.sort {
                Sort::Name => std::cmp::Ordering::Equal,
                Sort::Extent => key_or_zero(x, |s| s.longest_side() as i64)
                    .cmp(&key_or_zero(y, |s| s.longest_side() as i64)),
                Sort::Voxels => {
                    key_or_zero(x, |s| s.voxels as i64).cmp(&key_or_zero(y, |s| s.voxels as i64))
                }
                Sort::Modified => modified_key(x).cmp(&modified_key(y)),
            };
            // Name is always the tie-break, so the order is total and stable
            // however little the scan has filled in so far.
            ord.then_with(|| natural_cmp(&x.name, &y.name))
                .then_with(|| x.path.cmp(&y.path))
        });
        if self.descending {
            list.reverse();
        }
    }

    // ---- rows -----------------------------------------------------------

    /// The centre pane's contents, stacked into variant families if asked.
    pub fn rows(&mut self) -> &[Row] {
        if self.cache.is_none() {
            self.cache = Some(self.build_rows());
        }
        self.cache.as_deref().unwrap_or(&[])
    }

    fn build_rows(&self) -> Vec<Row> {
        let filtered = self.filtered();
        if !self.stack_variants {
            return filtered.into_iter().map(Row::One).collect();
        }

        let mut rows: Vec<Row> = Vec::with_capacity(filtered.len());
        let mut seen: Vec<String> = Vec::new();
        for asset in filtered.iter().copied() {
            let Some(key) = self.assets[asset].family.clone() else {
                rows.push(Row::One(asset));
                continue;
            };
            let scoped = format!("{}/{key}", self.assets[asset].folder);
            if seen.contains(&scoped) {
                continue;
            }
            seen.push(scoped.clone());
            let members: Vec<usize> = filtered
                .iter()
                .copied()
                .filter(|i| {
                    self.assets[*i].folder == self.assets[asset].folder
                        && self.assets[*i].family.as_deref() == Some(key.as_str())
                })
                .collect();
            // A family the filter has cut down to one member is just that file.
            if members.len() < 2 {
                rows.push(Row::One(asset));
                continue;
            }
            let expanded = self.expanded_families.contains(&scoped);
            rows.push(Row::Family {
                key,
                members: members.clone(),
            });
            if expanded {
                rows.extend(members.into_iter().map(Row::One));
            }
        }
        rows
    }

    fn family_id(&self, row: &Row) -> Option<String> {
        match row {
            Row::Family { key, members } => {
                Some(format!("{}/{key}", self.assets[members[0]].folder))
            }
            Row::One(_) => None,
        }
    }

    pub fn family_expanded(&self, row: &Row) -> bool {
        self.family_id(row)
            .is_some_and(|id| self.expanded_families.contains(&id))
    }

    pub fn toggle_family(&mut self, row: &Row) {
        let Some(id) = self.family_id(row) else {
            return;
        };
        match self.expanded_families.iter().position(|k| *k == id) {
            Some(i) => {
                self.expanded_families.remove(i);
            }
            None => self.expanded_families.push(id),
        }
        self.cache = None;
    }

    /// Mark the derived row list stale. Call after changing any filter.
    pub fn invalidate(&mut self) {
        self.cache = None;
    }

    // ---- facet counts ---------------------------------------------------

    /// How many assets each facet would leave, counted with that facet's own
    /// group switched off -- the count you actually want before clicking.
    pub fn facet_counts(&self) -> FacetCounts {
        let now = SystemTime::now();
        let scope = self.in_scope();

        let mut without_extent = self.facets;
        without_extent.extent = [false; 3];
        let mut extent = [FacetCount {
            matching: 0,
            on: false,
        }; 3];
        for (i, slot) in extent.iter_mut().enumerate() {
            slot.on = self.facets.extent[i];
        }

        let mut without_palette = self.facets;
        without_palette.palette_from_file = false;
        without_palette.palette_default = false;

        let mut without_state = self.facets;
        without_state.recent = false;
        without_state.failed = false;

        let mut counts = FacetCounts {
            extent,
            palette_from_file: FacetCount {
                matching: 0,
                on: self.facets.palette_from_file,
            },
            palette_default: FacetCount {
                matching: 0,
                on: self.facets.palette_default,
            },
            recent: FacetCount {
                matching: 0,
                on: self.facets.recent,
            },
            failed: FacetCount {
                matching: 0,
                on: self.facets.failed,
            },
            total: 0,
            pending: 0,
        };

        for asset in scope {
            let a = &self.assets[asset];
            counts.total += 1;
            if matches!(a.load, Load::Pending) {
                counts.pending += 1;
            }

            if self.matches_find(asset)
                && self.matches_palette(asset, &without_extent)
                && self.matches_state(asset, &without_extent, now)
                && let Some(stats) = a.stats()
            {
                let side = stats.longest_side();
                let bucket = if side <= 16 {
                    0
                } else if side <= 32 {
                    1
                } else {
                    2
                };
                counts.extent[bucket].matching += 1;
            }

            if self.matches_find(asset)
                && self.matches_extent(asset, &without_palette)
                && self.matches_state(asset, &without_palette, now)
                && let Some(stats) = a.stats()
            {
                if stats.palette_from_file {
                    counts.palette_from_file.matching += 1;
                } else {
                    counts.palette_default.matching += 1;
                }
            }

            if self.matches_find(asset)
                && self.matches_extent(asset, &without_state)
                && self.matches_palette(asset, &without_state)
            {
                if a.stats()
                    .and_then(|s| s.modified)
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|age| age < RECENT)
                {
                    counts.recent.matching += 1;
                }
                if a.failed() {
                    counts.failed.matching += 1;
                }
            }
        }
        counts
    }

    pub fn clear_filters(&mut self) {
        self.facets.clear();
        self.find.clear();
        self.cache = None;
    }

    // ---- selection ------------------------------------------------------

    /// The asset the inspector describes.
    pub fn focused(&self) -> Option<usize> {
        self.selection.last().copied()
    }

    /// The folder worth naming beside an asset, or `None` when everything on
    /// screen shares one.
    ///
    /// Veloren names a whole family of sprites `0.vox` .. `6.vox` inside a
    /// folder called `crate`, so browsing a subtree without this shows a grid
    /// of cells labelled `0`, `1`, `1`, `1`.
    pub fn folder_hint(&self, asset: usize) -> Option<&str> {
        let folder = self.assets[asset].folder;
        match self.scope {
            Scope::Folder(scope) if scope == folder => None,
            _ => Some(&self.folders[folder].name),
        }
    }

    pub fn is_selected(&self, asset: usize) -> bool {
        self.selection.contains(&asset)
    }

    /// Click handling for one asset, with the usual modifier conventions.
    pub fn click(&mut self, asset: usize, ctrl: bool, shift: bool) {
        if shift && let Some(anchor) = self.anchor {
            let order = self.filtered();
            let (Some(a), Some(b)) = (
                order.iter().position(|i| *i == anchor),
                order.iter().position(|i| *i == asset),
            ) else {
                return;
            };
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if !ctrl {
                self.selection.clear();
            }
            for i in &order[lo..=hi] {
                if !self.selection.contains(i) {
                    self.selection.push(*i);
                }
            }
            // Keep the clicked asset last so the inspector follows the click.
            self.selection.retain(|i| *i != asset);
            self.selection.push(asset);
            return;
        }

        if ctrl {
            match self.selection.iter().position(|i| *i == asset) {
                Some(i) => {
                    self.selection.remove(i);
                }
                None => self.selection.push(asset),
            }
        } else {
            self.selection.clear();
            self.selection.push(asset);
        }
        self.anchor = Some(asset);
    }

    /// Move the keyboard cursor `delta` rows and select what it lands on.
    ///
    /// Clamped rather than wrapped: arrowing off the end of a folder should
    /// stop there, not jump you back to the top of a list you just left.
    pub fn move_cursor(&mut self, delta: isize) -> Option<usize> {
        let len = self.rows().len();
        if len == 0 {
            self.cursor = 0;
            return None;
        }
        let next = (self.cursor as isize + delta).clamp(0, len as isize - 1) as usize;
        self.cursor = next;
        let asset = self.rows()[next].lead();
        self.select_only(asset);
        Some(next)
    }

    /// Put the cursor on the row holding `asset`, so the keyboard picks up
    /// where the mouse left off.
    pub fn focus_row(&mut self, asset: usize) {
        let found = self.rows().iter().position(|row| match row {
            Row::One(a) => *a == asset,
            Row::Family { members, .. } => members.contains(&asset),
        });
        if let Some(row) = found {
            self.cursor = row;
        }
    }

    pub fn select_only(&mut self, asset: usize) {
        self.selection.clear();
        self.selection.push(asset);
        self.anchor = Some(asset);
    }

    pub fn select_all(&mut self) {
        self.selection = self.filtered();
        self.anchor = self.selection.first().copied();
    }

    // ---- collections ----------------------------------------------------

    pub fn new_collection(&mut self, name: String) -> usize {
        self.collections.push(Collection {
            name,
            members: Vec::new(),
        });
        self.collections.len() - 1
    }

    /// Add assets to a collection, skipping ones already in it.
    pub fn add_to_collection(&mut self, collection: usize, assets: &[usize]) -> usize {
        let Some(target) = self.collections.get_mut(collection) else {
            return 0;
        };
        let mut added = 0;
        for asset in assets {
            if !target.members.contains(asset) {
                target.members.push(*asset);
                added += 1;
            }
        }
        self.cache = None;
        added
    }

    pub fn remove_from_collection(&mut self, collection: usize, asset: usize) {
        if let Some(target) = self.collections.get_mut(collection) {
            target.members.retain(|i| *i != asset);
            self.cache = None;
        }
    }

    /// Names of the collections holding `asset`.
    pub fn collections_of(&self, asset: usize) -> Vec<&str> {
        self.collections
            .iter()
            .filter(|c| c.members.contains(&asset))
            .map(|c| c.name.as_str())
            .collect()
    }

    /// Other members of `asset`'s variant family, in name order.
    pub fn family_of(&self, asset: usize) -> Vec<usize> {
        let Some(key) = self.assets[asset].family.clone() else {
            return Vec::new();
        };
        let folder = self.assets[asset].folder;
        let mut members: Vec<usize> = (0..self.assets.len())
            .filter(|i| {
                self.assets[*i].folder == folder
                    && self.assets[*i].family.as_deref() == Some(key.as_str())
            })
            .collect();
        members.sort_by(|a, b| natural_cmp(&self.assets[*a].name, &self.assets[*b].name));
        members
    }

    pub fn failures(&self) -> usize {
        self.assets.iter().filter(|a| a.failed()).count()
    }
}

/// Counts for one draw of the filter rail.
#[derive(Clone, Copy, Debug)]
pub struct FacetCounts {
    pub extent: [FacetCount; 3],
    pub palette_from_file: FacetCount,
    pub palette_default: FacetCount,
    pub recent: FacetCount,
    pub failed: FacetCount,
    /// Assets in scope, before find and facets.
    pub total: usize,
    /// Of those, how many the scan has not reached yet.
    pub pending: usize,
}

fn mark_subtree(folders: &[Folder], folder: usize, out: &mut [bool]) {
    out[folder] = true;
    for child in &folders[folder].children {
        mark_subtree(folders, *child, out);
    }
}

fn key_or_zero(asset: &Asset, f: impl Fn(&AssetStats) -> i64) -> i64 {
    asset.stats().map(f).unwrap_or(0)
}

fn modified_key(asset: &Asset) -> Duration {
    asset
        .stats()
        .and_then(|s| s.modified)
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .unwrap_or_default()
}

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The prefix an asset shares with its variants, if it has any.
///
/// Two shapes cover everything Veloren ships: a numeric tail after a
/// separator (`crate-0`, `bench_coastal-1`), and a descriptive tail after the
/// last separator (`bed_cliff_head`, `barrel_wood_coal`). Whether a prefix is
/// really a family is decided later, by counting how many files share it.
pub fn family_key(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i < bytes.len() && i > 1 && (bytes[i - 1] == b'-' || bytes[i - 1] == b'_') {
        return Some(name[..i - 1].to_string());
    }
    let cut = name.rfind(['_', '-'])?;
    if cut == 0 {
        None
    } else {
        Some(name[..cut].to_string())
    }
}

/// The part of `name` that distinguishes it inside its family.
pub fn family_tail<'a>(name: &'a str, key: &str) -> &'a str {
    name.strip_prefix(key)
        .map(|rest| rest.trim_start_matches(['_', '-']))
        .unwrap_or(name)
}

/// Compare names the way a person reads them, so `crate-10` sorts after
/// `crate-9` instead of between `crate-1` and `crate-2`.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.as_bytes(), b.as_bytes());
    loop {
        match (x.first(), y.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => {
                if p.is_ascii_digit() && q.is_ascii_digit() {
                    let (nx, rx) = take_number(x);
                    let (ny, ry) = take_number(y);
                    match nx.cmp(&ny) {
                        Ordering::Equal => {
                            x = rx;
                            y = ry;
                        }
                        other => return other,
                    }
                } else {
                    match p.to_ascii_lowercase().cmp(&q.to_ascii_lowercase()) {
                        Ordering::Equal => {
                            x = &x[1..];
                            y = &y[1..];
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

fn take_number(s: &[u8]) -> (u64, &[u8]) {
    let end = s
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(s.len());
    // A run longer than a u64 can hold is not a number anyone meant; clamp it
    // rather than wrap.
    let value = s[..end].iter().fold(0u64, |acc, c| {
        acc.saturating_mul(10).saturating_add((c - b'0') as u64)
    });
    (value, &s[end..])
}

/// `1234567` as `1,234,567`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(paths: &[&str]) -> Library {
        let root = PathBuf::from("root");
        Library::new(root.clone(), paths.iter().map(|p| root.join(p)).collect())
    }

    #[test]
    fn families_come_from_shared_prefixes() {
        assert_eq!(family_key("crate-0").as_deref(), Some("crate"));
        assert_eq!(
            family_key("bench_coastal-12").as_deref(),
            Some("bench_coastal")
        );
        assert_eq!(family_key("bed_cliff_head").as_deref(), Some("bed_cliff"));
        assert_eq!(
            family_key("barrel_wood_coal").as_deref(),
            Some("barrel_wood")
        );
        assert_eq!(family_key("barrel"), None);
        assert_eq!(family_key("_leading"), None);
    }

    #[test]
    fn a_prefix_only_one_file_uses_is_not_a_family() {
        let lib = lib(&["furniture/barrel.vox", "furniture/crate-0.vox"]);
        assert_eq!(lib.assets[0].family, None, "barrel has no siblings");
        assert_eq!(lib.assets[1].family, None, "one crate is not a family");
    }

    #[test]
    fn variants_stack_into_one_row_and_open_again() {
        let mut lib = lib(&[
            "furniture/barrel.vox",
            "furniture/crate-0.vox",
            "furniture/crate-1.vox",
            "furniture/crate-2.vox",
        ]);
        assert_eq!(
            lib.rows().len(),
            2,
            "barrel, plus one stack of three crates"
        );

        let stack = lib.rows()[1].clone();
        assert!(matches!(&stack, Row::Family { members, .. } if members.len() == 3));

        lib.toggle_family(&stack);
        assert_eq!(lib.rows().len(), 5, "the stack plus its three members");

        lib.stack_variants = false;
        lib.invalidate();
        assert_eq!(lib.rows().len(), 4);
    }

    #[test]
    fn identical_names_in_different_folders_are_different_families() {
        let lib = lib(&[
            "a/crate-0.vox",
            "a/crate-1.vox",
            "b/crate-0.vox",
            "b/crate-1.vox",
        ]);
        let mut lib = lib;
        assert_eq!(lib.rows().len(), 2, "one stack per folder, not one overall");
    }

    #[test]
    fn folder_totals_roll_up_and_scope_covers_the_subtree() {
        let mut lib = lib(&[
            "sprite/furniture/barrel.vox",
            "sprite/grass/blade.vox",
            "npc/wolf/head.vox",
        ]);
        assert_eq!(lib.folders[0].total, 3);
        assert_eq!(lib.folders[0].direct, 0);

        let sprite = lib
            .folders
            .iter()
            .position(|f| f.name == "sprite")
            .expect("sprite folder");
        assert_eq!(lib.folders[sprite].total, 2);

        lib.set_scope(Scope::Folder(sprite));
        assert_eq!(lib.rows().len(), 2);
    }

    #[test]
    fn a_pass_through_chain_becomes_one_tree_row() {
        let lib = lib(&["voxygen/voxel/sprite/barrel.vox"]);
        let rows = lib.tree_rows();
        assert_eq!(
            rows[0].label,
            "root \u{b7} voxygen \u{b7} voxel \u{b7} sprite"
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn find_matches_names_and_paths() {
        let mut lib = lib(&["a/barrel.vox", "b/basket.vox"]);
        lib.find = "bas".into();
        lib.invalidate();
        assert_eq!(lib.filtered().len(), 1);

        lib.find = "a/".into();
        lib.invalidate();
        assert_eq!(lib.filtered().len(), 1, "a slash makes it a path search");
    }

    #[test]
    fn shift_click_extends_from_the_anchor() {
        let mut lib = lib(&["a/one.vox", "a/two.vox", "a/three.vox", "a/four.vox"]);
        lib.stack_variants = false;
        let order = lib.filtered();
        lib.click(order[0], false, false);
        lib.click(order[2], false, true);
        assert_eq!(lib.selection.len(), 3);
        assert_eq!(
            lib.focused(),
            Some(order[2]),
            "the inspector follows the click"
        );
    }

    #[test]
    fn extent_facet_counts_ignore_their_own_group() {
        let mut lib = lib(&["a/small.vox", "a/big.vox"]);
        lib.assets[0].load = Load::Ready(stats([8, 8, 8]));
        lib.assets[1].load = Load::Ready(stats([40, 40, 40]));

        lib.facets.extent[0] = true;
        lib.invalidate();
        assert_eq!(lib.filtered().len(), 1);

        let counts = lib.facet_counts();
        assert_eq!(counts.extent[0].matching, 1);
        assert_eq!(
            counts.extent[2].matching, 1,
            "the > 32 count must not be filtered by the <= 16 box"
        );
        assert_eq!(counts.total, 2);
    }

    #[test]
    fn numbers_in_names_sort_the_way_people_read_them() {
        let mut names = ["crate-10", "crate-9", "crate-1"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["crate-1", "crate-9", "crate-10"]);
    }

    #[test]
    fn the_cursor_walks_the_filtered_order_and_stops_at_the_ends() {
        let mut lib = lib(&["a/barrel.vox", "a/basket.vox", "b/wolf.vox"]);
        lib.find = "ba".into();
        lib.invalidate();
        assert_eq!(lib.rows().len(), 2, "wolf is filtered out");

        assert_eq!(lib.move_cursor(0), Some(0), "the cursor starts at the top");
        assert_eq!(lib.move_cursor(1), Some(1));
        assert_eq!(lib.move_cursor(1), Some(1), "clamped, not wrapped");
        assert_eq!(lib.move_cursor(-50), Some(0), "clamped at the other end");
        assert_eq!(
            lib.selection.len(),
            1,
            "moving the cursor selects what it lands on"
        );
    }

    #[test]
    fn the_folder_hint_names_a_folder_only_when_it_is_not_the_one_in_view() {
        // Veloren's sprite variants are all called `0.vox`, so a cell caption
        // needs its folder unless the folder is already the subject.
        let mut lib = lib(&["sprite/carrot/0.vox", "sprite/radish/0.vox"]);
        assert_eq!(lib.folder_hint(0), Some("carrot"));

        let carrot = lib
            .folders
            .iter()
            .position(|f| f.name == "carrot")
            .expect("carrot folder");
        lib.set_scope(Scope::Folder(carrot));
        assert_eq!(lib.folder_hint(0), None, "already browsing carrot");
    }

    #[test]
    fn collections_cut_across_folders() {
        let mut lib = lib(&["a/one.vox", "b/two.vox", "c/three.vox"]);
        let set = lib.new_collection("Tavern".into());
        assert_eq!(lib.add_to_collection(set, &[0, 2]), 2);
        assert_eq!(lib.add_to_collection(set, &[0]), 0, "no duplicates");

        lib.set_scope(Scope::Collection(set));
        assert_eq!(lib.rows().len(), 2);
        assert_eq!(lib.collections_of(0), ["Tavern"]);
    }

    fn stats(dims: [i32; 3]) -> AssetStats {
        AssetStats {
            dims,
            voxels: 1,
            models: 1,
            instances: 1,
            palette_from_file: true,
            palette_head: Vec::new(),
            bytes: 0,
            modified: None,
        }
    }
}
