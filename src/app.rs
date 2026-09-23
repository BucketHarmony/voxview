//! The window, the input handling and the frame loop.

use crate::camera::{Axis, OrbitCamera};
use crate::gfx::{Background, FrameParams, Renderer};
use crate::hud::HudLine;
use crate::library::{Library, Scope, View};
use crate::loader::{self, VoxScene};
use crate::menu::{Hit, Menu};
use crate::mesh;
use crate::scan::{self, Scan};
use crate::settings::Settings;
use crate::ui::{Action, Chrome, ScanStatus, Toggle, Ui, ViewerChrome};
use crate::watch::FileWatcher;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Open voxview on `paths[index]`, browsing `root`.
///
/// A directory argument opens the library; a file argument opens that file
/// and leaves the library a keystroke away.
pub fn run(
    root: PathBuf,
    paths: Vec<PathBuf>,
    index: usize,
    browse: bool,
    settings: Settings,
) -> Result<()> {
    let event_loop = EventLoop::new().context("could not create an event loop")?;
    // Redraw continuously: the swapchain is in Fifo mode, so this paces
    // itself at the display's refresh rate rather than spinning.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(root, paths, index, browse, settings);
    event_loop.run_app(&mut app).context("the viewer stopped")?;
    match app.fatal.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Which face of the program is up.
///
/// They share one window, one device and one set of thumbnails; only the
/// input routing and what gets drawn differ.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Library,
    Viewer,
}

/// What to do with the camera when new geometry arrives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// Re-frame from the default angles: a different file is being shown.
    Reset,
    /// Leave it exactly where it is: the same file was edited.
    Keep,
}

/// The parts of a loaded scene the HUD reports.
struct SceneInfo {
    name: String,
    models: usize,
    instances: usize,
    dimensions: glam::IVec3,
    voxels: usize,
    default_palette: bool,
    load_ms: f32,
    mesh_ms: f32,
}

impl SceneInfo {
    fn new(path: &Path, scene: &VoxScene, load_ms: f32, mesh_ms: f32) -> SceneInfo {
        SceneInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            models: scene.models.len(),
            instances: scene.instances.len(),
            dimensions: scene.dimensions(),
            voxels: scene.voxel_count,
            default_palette: !scene.palette.from_file,
            load_ms,
            mesh_ms,
        }
    }
}

/// A rolling frame-rate estimate.
struct Fps {
    frames: u32,
    since: Instant,
    value: f32,
}

impl Fps {
    fn new() -> Fps {
        Fps {
            frames: 0,
            since: Instant::now(),
            value: 0.0,
        }
    }

    fn tick(&mut self) {
        self.frames += 1;
        let elapsed = self.since.elapsed().as_secs_f32();
        if elapsed >= 0.5 {
            self.value = self.frames as f32 / elapsed;
            self.frames = 0;
            self.since = Instant::now();
        }
    }
}

/// Which mouse buttons are held, and where the cursor was last seen.
#[derive(Default)]
struct Mouse {
    position: PhysicalPosition<f64>,
    last: Option<PhysicalPosition<f64>>,
    left: bool,
    pan: bool,
}

struct App {
    mode: Mode,
    /// Where the library browses from. Not necessarily the file's own folder:
    /// a directory argument browses its whole subtree.
    root: PathBuf,
    lib: Library,
    scan: Option<Scan>,
    /// Names found, and files parsed, for the status bar.
    found: usize,
    parsed: usize,
    ui: Option<Ui>,
    /// The library index of whatever the viewer is showing, so the filmstrip
    /// knows where it is.
    current: Option<usize>,

    paths: Vec<PathBuf>,
    index: usize,
    /// File names for the menu, built once: the listing does not change while
    /// the viewer is open.
    names: Vec<String>,
    /// The directory the listing came from, shown as the menu's heading.
    title: String,
    menu: Menu,

    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    watcher: Option<FileWatcher>,

    camera: OrbitCamera,
    info: Option<SceneInfo>,
    /// Set when the current file would not parse. The last good render stays
    /// on screen underneath it.
    error: Option<String>,

    show_grid: bool,
    show_bbox: bool,
    show_axes: bool,
    ambient_occlusion: bool,
    background: Background,

    mouse: Mouse,
    /// Modifier state, which only arrives in its own event on winit.
    shift: bool,
    ctrl: bool,
    fps: Fps,
    status: Option<(String, Instant)>,
    /// An error that should make the process exit non-zero.
    fatal: Option<anyhow::Error>,

    /// What was on disk at startup, and what gets written back on the way out.
    ///
    /// Only the parts that cannot be read back off the live state are kept
    /// here -- window geometry is taken from the window itself at exit, and
    /// the toggles from the fields above.
    settings: Settings,
    /// Collections from the settings file, waiting for a scan to resolve their
    /// paths into asset indices. Taken once, on the first completed walk.
    pending_collections: Vec<(String, Vec<PathBuf>)>,
}

/// How long a one-off message (a screenshot path, say) stays in the HUD.
const STATUS_SECONDS: f32 = 4.0;
/// File rows one notch of the wheel scrolls the menu by.
const WHEEL_ROWS: usize = 3;
/// Edge of a PNG written from the library, where there is no viewport to
/// borrow a size from.
const SHOT_SIZE: u32 = 512;

impl App {
    fn new(
        root: PathBuf,
        paths: Vec<PathBuf>,
        index: usize,
        browse: bool,
        settings: Settings,
    ) -> App {
        let names = paths.iter().map(|p| file_name(p)).collect();
        let title = paths
            .first()
            .and_then(|p| p.parent())
            .filter(|d| !d.as_os_str().is_empty())
            .map(file_name)
            .unwrap_or_else(|| ".".into());
        let mut lib = Library::new(root.clone(), Vec::new());
        lib.view = settings.view;
        lib.sort = settings.sort;
        lib.descending = settings.descending;
        lib.stack_variants = settings.stack_variants;
        lib.cell = settings.cell;

        App {
            mode: if browse { Mode::Library } else { Mode::Viewer },
            lib,
            root,
            scan: None,
            found: 0,
            parsed: 0,
            ui: None,
            current: None,
            paths,
            index,
            names,
            title,
            menu: Menu::new(),
            window: None,
            renderer: None,
            watcher: None,
            camera: OrbitCamera::with_projection(settings.orthographic),
            info: None,
            error: None,
            show_grid: settings.grid,
            show_bbox: settings.bbox,
            show_axes: settings.axes,
            ambient_occlusion: settings.occlusion,
            background: settings.background,
            mouse: Mouse::default(),
            shift: false,
            ctrl: false,
            fps: Fps::new(),
            status: None,
            fatal: None,
            pending_collections: settings.collections.clone(),
            settings,
        }
    }

    /// Fold the live state back into the settings and write them out.
    ///
    /// Called once, on the way out. A failure gets one line on stderr: there
    /// is nothing left to do about it, and silence would leave "it keeps
    /// forgetting my window size" with no explanation anywhere.
    fn save_settings(&mut self) {
        if let Some(window) = &self.window {
            let scale = window.scale_factor();
            let size = window.inner_size().to_logical::<f64>(scale);
            self.settings.window.width = size.width.round().max(1.0) as u32;
            self.settings.window.height = size.height.round().max(1.0) as u32;
            self.settings.window.maximized = window.is_maximized();
            // A maximized window's position is the compositor's business, not
            // something to restore, so it is only recorded when it is real.
            let placed = (!window.is_maximized())
                .then(|| window.outer_position().ok())
                .flatten();
            if let Some(position) = placed {
                let position = position.to_logical::<f64>(scale);
                self.settings.window.x = Some(position.x.round() as i32);
                self.settings.window.y = Some(position.y.round() as i32);
            }
        }

        self.settings.background = self.background;
        self.settings.grid = self.show_grid;
        self.settings.bbox = self.show_bbox;
        self.settings.axes = self.show_axes;
        self.settings.occlusion = self.ambient_occlusion;
        self.settings.orthographic = self.camera.orthographic;
        self.settings.view = self.lib.view;
        self.settings.sort = self.lib.sort;
        self.settings.descending = self.lib.descending;
        self.settings.stack_variants = self.lib.stack_variants;
        self.settings.cell = self.lib.cell;
        self.settings.root = Some(self.root.clone());

        // Collections are stored by path, because an asset index only means
        // something relative to the scan that produced it. If the scan never
        // finished, the ones loaded at startup are written back untouched
        // rather than thrown away.
        self.settings.collections = if self.lib.assets.is_empty() {
            std::mem::take(&mut self.pending_collections)
        } else {
            self.lib
                .collections
                .iter()
                .map(|c| {
                    let members = c
                        .members
                        .iter()
                        .filter_map(|i| self.lib.assets.get(*i))
                        .map(|a| a.path.clone())
                        .collect();
                    (c.name.clone(), members)
                })
                .collect()
        };

        if let Err(e) = self.settings.save() {
            eprintln!("voxview: could not save settings: {e}");
        }
    }

    /// Turn the saved collection paths into memberships in the library that
    /// has just been scanned.
    ///
    /// Paths that are no longer on disk simply drop out; a collection that
    /// loses every member is still kept, because an empty collection you made
    /// on purpose is not the same thing as one that never existed.
    fn restore_collections(&mut self) {
        if self.pending_collections.is_empty() {
            return;
        }
        let index: std::collections::HashMap<&Path, usize> = self
            .lib
            .assets
            .iter()
            .enumerate()
            .map(|(i, a)| (a.path.as_path(), i))
            .collect();
        let saved: Vec<(String, Vec<usize>)> = self
            .pending_collections
            .iter()
            .map(|(name, paths)| {
                let members = paths
                    .iter()
                    .filter_map(|p| index.get(p.as_path()).copied())
                    .collect();
                (name.clone(), members)
            })
            .collect();
        self.pending_collections.clear();
        for (name, members) in saved {
            let set = self.lib.new_collection(name);
            self.lib.add_to_collection(set, &members);
        }
    }

    fn path(&self) -> &Path {
        &self.paths[self.index]
    }

    /// Take in whatever the background scan has produced since the last frame.
    ///
    /// The walk arrives first, in one message, and rebuilds the tree; the
    /// per-file stats trickle in after it and fill the cells in place.
    fn drain_scan(&mut self) {
        let Some(scan) = &mut self.scan else { return };
        let messages = scan.drain();
        if messages.is_empty() {
            return;
        }
        let mut touched = false;
        for message in messages {
            match message {
                scan::Msg::Found {
                    files,
                    skipped,
                    truncated,
                } => {
                    self.found = files.len();
                    // Rebuilding is the honest way to fold a whole walk into
                    // the tree, so the few settings you could have reached in
                    // that first moment are carried across by hand.
                    let mut lib = Library::new(self.root.clone(), files);
                    lib.view = self.lib.view;
                    lib.sort = self.lib.sort;
                    lib.descending = self.lib.descending;
                    lib.stack_variants = self.lib.stack_variants;
                    lib.cell = self.lib.cell;
                    lib.skipped = skipped;
                    lib.truncated = truncated;
                    self.lib = lib;
                    self.restore_collections();
                    self.sync_current();
                    // A library named in Chinese or Hebrew needs a font egui
                    // does not ship with. Ask now, while the names are in
                    // hand, rather than when the first one is drawn.
                    if let Some(ui) = &mut self.ui {
                        for asset in &self.lib.assets {
                            ui.may_show(&asset.name);
                        }
                        for folder in &self.lib.folders {
                            ui.may_show(&folder.name);
                        }
                    }
                    touched = true;
                }
                scan::Msg::Stats { path, load } => {
                    self.parsed += 1;
                    self.lib.apply(&path, load);
                    touched = true;
                }
                scan::Msg::Done => {}
            }
        }
        if touched {
            // Extent and palette facets depend on the stats, so the row list
            // and the counts are stale as soon as any of them lands.
            self.lib.invalidate();
        }
    }

    /// Say in the title bar which face is up and what it is looking at.
    fn retitle(&self) {
        let Some(window) = &self.window else { return };
        window.set_title(&match self.mode {
            Mode::Library => format!("voxview - {}", self.root.display()),
            Mode::Viewer => format!("voxview - {}", self.path().display()),
        });
    }

    /// Find the library entry for the file the viewer is showing.
    fn sync_current(&mut self) {
        let path = self.path().to_path_buf();
        self.current = self.lib.assets.iter().position(|a| a.path == path);
    }

    /// Show a library asset in the viewer, with `[` and `]` walking whatever
    /// the library was filtered down to.
    fn open(&mut self, asset: usize) {
        let order = self.lib.filtered();
        let Some(position) = order.iter().position(|i| *i == asset) else {
            return;
        };
        self.paths = order
            .iter()
            .map(|i| self.lib.assets[*i].path.clone())
            .collect();
        self.names = self.paths.iter().map(|p| file_name(p)).collect();
        self.title = match self.lib.scope {
            Scope::Folder(f) => self.lib.folders[f].name.clone(),
            Scope::Collection(c) => self
                .lib
                .collections
                .get(c)
                .map(|c| c.name.clone())
                .unwrap_or_default(),
        };
        self.index = position;
        self.current = Some(asset);
        self.menu.close();
        self.mode = Mode::Viewer;
        self.load(Framing::Reset);
        self.rewatch();
        self.retitle();
    }

    /// Write a PNG next to each of a batch of assets, at thumbnail quality
    /// but full size -- the library's "screenshot all".
    fn screenshot_all(&mut self, assets: &[usize]) {
        let paths: Vec<PathBuf> = assets
            .iter()
            .filter_map(|i| self.lib.assets.get(*i).map(|a| a.path.clone()))
            .collect();
        let mut written = 0;
        for path in &paths {
            match self.write_png(path) {
                Ok(()) => written += 1,
                Err(e) => eprintln!("voxview: {e:#}"),
            }
        }
        self.status = Some((
            format!("saved {written} of {} screenshots", paths.len()),
            Instant::now(),
        ));
    }

    /// One off-screen render, saved beside its model.
    fn write_png(&mut self, path: &Path) -> Result<()> {
        let scene = loader::load_file(path)?;
        let meshes = mesh::mesh_models(&scene.models, &scene.materials);
        let renderer = self
            .renderer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no renderer yet"))?;
        let pixels = renderer.thumbnail(&scene, &meshes, SHOT_SIZE)?;
        let image = image::RgbaImage::from_raw(SHOT_SIZE, SHOT_SIZE, pixels)
            .ok_or_else(|| anyhow::anyhow!("the render came back the wrong size"))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "voxview".into());
        let out = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .join(format!("{stem}_{}.png", timestamp()));
        image
            .save(&out)
            .with_context(|| format!("could not write {}", out.display()))?;
        println!("wrote {}", out.display());
        Ok(())
    }

    /// Apply one thing the browser asked for.
    fn act(&mut self, action: Action) {
        match action {
            Action::Open(asset) => self.open(asset),
            Action::Screenshot(assets) => self.screenshot_all(&assets),
            Action::ToLibrary => {
                self.mode = Mode::Library;
                self.retitle();
            }
            Action::Reload => self.load(Framing::Keep),
            Action::Toggle(toggle) => match toggle {
                Toggle::Grid => self.show_grid = !self.show_grid,
                Toggle::Bbox => self.show_bbox = !self.show_bbox,
                Toggle::Axes => self.show_axes = !self.show_axes,
                Toggle::Occlusion => self.ambient_occlusion = !self.ambient_occlusion,
                Toggle::Orthographic => self.camera.orthographic = !self.camera.orthographic,
                Toggle::Msaa => self.cycle_msaa(),
                Toggle::Background => self.background = self.background.toggled(),
            },
        }
    }

    /// Read, mesh and upload the current file.
    ///
    /// A failure here is never fatal: the message goes to stderr and into the
    /// HUD, and whatever was last rendered successfully stays on screen.
    fn load(&mut self, framing: Framing) {
        let path = self.path().to_path_buf();

        let started = Instant::now();
        let scene = match loader::load_file(&path) {
            Ok(scene) => scene,
            Err(e) => {
                eprintln!("voxview: {e:#}");
                self.error = Some(first_line(&format!("{e:#}")));
                return;
            }
        };
        let load_ms = started.elapsed().as_secs_f32() * 1000.0;

        let started = Instant::now();
        let meshes = mesh::mesh_models(&scene.models, &scene.materials);
        let mesh_ms = started.elapsed().as_secs_f32() * 1000.0;

        if let Some(renderer) = &mut self.renderer {
            renderer.set_scene(&scene, &meshes);
        }
        if framing == Framing::Reset {
            self.camera.reset(&scene.bounds);
        }
        self.info = Some(SceneInfo::new(&path, &scene, load_ms, mesh_ms));
        self.error = None;
    }

    /// Point the watcher at the current file.
    fn rewatch(&mut self) {
        match FileWatcher::new(self.path()) {
            Ok(w) => self.watcher = Some(w),
            Err(e) => {
                eprintln!("voxview: hot reload unavailable: {e:#}");
                self.watcher = None;
            }
        }
    }

    /// Switch to `index` in the listing.
    fn show(&mut self, index: usize) {
        if index >= self.paths.len() || index == self.index {
            return;
        }
        self.index = index;
        self.load(Framing::Reset);
        self.rewatch();
        self.menu.follow(index);
        if let Some(window) = &self.window {
            window.set_title(&format!("voxview - {}", self.path().display()));
        }
    }

    /// Move `step` files through the directory listing, wrapping around.
    fn cycle(&mut self, step: isize) {
        if self.paths.len() < 2 {
            self.status = Some((
                "only one .vox file in this directory".into(),
                Instant::now(),
            ));
            return;
        }
        let len = self.paths.len() as isize;
        let next = (self.index as isize + step).rem_euclid(len) as usize;
        self.show(next);
    }

    /// Move the menu cursor and show whatever it lands on, so arrowing
    /// through the list previews each file as you pass it.
    fn step_menu(&mut self, step: isize) {
        if let Some(index) = self.menu.step(step) {
            self.show(index);
        }
    }

    fn menu_under_cursor(&self) -> Option<Hit> {
        menu_under_cursor_impl(&self.menu, self.mouse.position)
    }

    /// Keys the menu claims while it is open. Returns whether it took one.
    fn menu_key(&mut self, code: KeyCode) -> bool {
        match code {
            // Escape undoes the last thing you did: it drops the filter if
            // there is one, and only then closes the menu.
            KeyCode::Escape => {
                if !self.menu.filter_clear(&self.names) {
                    self.menu.close();
                }
            }
            KeyCode::Tab => self.menu.close(),
            KeyCode::ArrowUp => self.step_menu(-1),
            KeyCode::ArrowDown => self.step_menu(1),
            KeyCode::PageUp => self.step_menu(-self.menu.page()),
            KeyCode::PageDown => self.step_menu(self.menu.page()),
            KeyCode::Enter | KeyCode::NumpadEnter => {
                if let Some(index) = self.menu.selection() {
                    self.show(index);
                }
                self.menu.close();
            }
            KeyCode::Backspace => {
                self.menu.filter_pop(&self.names);
            }
            _ => return false,
        }
        true
    }

    fn screenshot(&mut self) {
        if self.renderer.is_none() {
            return;
        }
        let stem = self
            .path()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "voxview".into());
        let out = self
            .path()
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .join(format!("{stem}_{}.png", timestamp()));

        let params = FrameParams {
            camera: &self.camera,
            show_grid: self.show_grid,
            show_bbox: self.show_bbox,
            show_axes: self.show_axes,
            ambient_occlusion: self.ambient_occlusion,
            background: self.background,
            hud: &[],
            menu: &[],
            hud_scale: 1.0,
            draw_scene: true,
        };
        let result = self
            .renderer
            .as_mut()
            .expect("checked above")
            .screenshot(&params, &out);
        match result {
            Ok(()) => {
                let name = out
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                println!("wrote {}", out.display());
                self.status = Some((format!("saved {name}"), Instant::now()));
            }
            Err(e) => {
                eprintln!("voxview: {e:#}");
                self.status = Some((first_line(&format!("{e:#}")), Instant::now()));
            }
        }
    }

    /// Assemble the text panel for this frame.
    fn hud(&self) -> Vec<HudLine> {
        let mut lines = Vec::with_capacity(6);
        let triangles = self.renderer.as_ref().map_or(0, Renderer::triangle_count);
        match &self.info {
            Some(info) => {
                lines.push(HudLine::normal(&info.name));
                let plural = if info.models == 1 { "" } else { "s" };
                lines.push(HudLine::dim(format!(
                    "{} model{plural}, {} instance{}",
                    info.models,
                    info.instances,
                    if info.instances == 1 { "" } else { "s" },
                )));
                lines.push(HudLine::dim(format!(
                    "{} x {} x {}",
                    info.dimensions.x, info.dimensions.y, info.dimensions.z
                )));
                lines.push(HudLine::dim(format!(
                    "{} voxels, {} tris",
                    thousands(info.voxels),
                    thousands(triangles)
                )));
                lines.push(HudLine::dim(format!(
                    "{:.0} fps, load {:.0} ms, mesh {:.0} ms",
                    self.fps.value, info.load_ms, info.mesh_ms
                )));
                if self.camera.orthographic {
                    // Only when it is on: perspective is the default, and a
                    // HUD line that is always there teaches nobody anything.
                    lines.push(HudLine::dim("orthographic"));
                }
                if info.default_palette {
                    lines.push(HudLine::dim("default palette (none in file)"));
                }
            }
            None => lines.push(HudLine::normal("no model loaded")),
        }

        // The overlay switches and the file counter live in the egui chrome
        // now, so the text HUD stops repeating them.
        if self.paths.len() < 2 {
            lines.push(HudLine::dim("[M]enu  [Esc] library"));
        }
        if let Some(error) = &self.error {
            lines.push(HudLine::error(format!("error: {error}")));
        }
        if let Some((text, at)) = &self.status
            && at.elapsed().as_secs_f32() < STATUS_SECONDS
        {
            lines.push(HudLine::normal(text));
        }
        lines
    }

    fn redraw(&mut self) {
        self.drain_scan();
        if self.watcher.as_mut().is_some_and(FileWatcher::poll) {
            self.load(Framing::Keep);
            // The file on disk changed, so its cached picture is wrong.
            if let (Some(ui), Some(asset)) = (&mut self.ui, self.current) {
                let path = self.lib.assets[asset].path.clone();
                ui.thumbs.forget(&path);
            }
        }
        self.fps.tick();

        let viewing = self.mode == Mode::Viewer;
        let hud = if viewing { self.hud() } else { Vec::new() };
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor().round().max(1.0) as f32);
        let menu = if viewing && self.menu.is_open() {
            let (w, h) = self.renderer.as_ref().map_or((1280, 800), Renderer::size);
            self.menu.lines(
                &self.names,
                &self.title,
                self.index,
                scale,
                (w as f32, h as f32),
            )
        } else {
            Vec::new()
        };
        // egui draws before the scene is submitted, because building a
        // thumbnail needs the device and must not land inside a render pass.
        let ui_frame = match (self.ui.take(), self.window.clone(), self.renderer.as_mut()) {
            (Some(mut ui), Some(window), Some(renderer)) => {
                let chrome = if viewing {
                    Chrome::Viewer(ViewerChrome {
                        current: self.current,
                        name: self.names.get(self.index).map_or("", String::as_str),
                        show_grid: self.show_grid,
                        show_bbox: self.show_bbox,
                        show_axes: self.show_axes,
                        occlusion: self.ambient_occlusion,
                        orthographic: self.camera.orthographic,
                        samples: renderer.samples(),
                        error: self.error.as_deref(),
                    })
                } else {
                    Chrome::Library(ScanStatus {
                        found: self.found,
                        parsed: self.parsed,
                        scanning: self.scan.as_ref().is_some_and(|s| !s.finished()),
                    })
                };
                let (frame, actions) = ui.frame(&window, renderer, &mut self.lib, chrome);
                self.ui = Some(ui);
                for action in actions {
                    self.act(action);
                }
                Some(frame)
            }
            (ui, _, _) => {
                self.ui = ui;
                None
            }
        };

        let params = FrameParams {
            camera: &self.camera,
            show_grid: self.show_grid,
            show_bbox: self.show_bbox,
            show_axes: self.show_axes,
            ambient_occlusion: self.ambient_occlusion,
            background: self.background,
            hud: &hud,
            menu: &menu,
            hud_scale: scale,
            draw_scene: viewing,
        };
        if let Some(renderer) = &mut self.renderer {
            renderer.render(&params, ui_frame);
        }
    }

    /// Keys while the grid is up. The viewer's own bindings are left alone.
    fn library_key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        let columns = self.ui.as_ref().map_or(1, Ui::columns) as isize;
        let step = match code {
            KeyCode::ArrowLeft => Some(-1),
            KeyCode::ArrowRight => Some(1),
            KeyCode::ArrowUp => Some(-columns),
            KeyCode::ArrowDown => Some(columns),
            KeyCode::Home => Some(isize::MIN / 2),
            KeyCode::End => Some(isize::MAX / 2),
            _ => None,
        };
        if let Some(step) = step {
            if let Some(row) = self.lib.move_cursor(step)
                && let Some(ui) = &mut self.ui
            {
                ui.scroll_to_row(row);
            }
            return;
        }

        match code {
            KeyCode::Escape => {
                // Escape undoes the last thing you did: an overlay, then the
                // filters, then the program.
                let overlay = self.ui.as_mut().is_some_and(Ui::close_overlay);
                if overlay {
                } else if self.lib.facets.any() || !self.lib.find.is_empty() {
                    self.lib.clear_filters();
                } else {
                    event_loop.exit();
                }
            }
            KeyCode::KeyQ => event_loop.exit(),
            KeyCode::Slash => {
                if let Some(ui) = &mut self.ui {
                    ui.focus_find();
                }
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                if let Some(asset) = self.lib.focused() {
                    self.open(asset);
                }
            }
            KeyCode::Space => {
                if let Some(ui) = &mut self.ui {
                    ui.toggle_peek();
                }
            }
            KeyCode::KeyC => {
                if let Some(ui) = &mut self.ui {
                    ui.prompt_collection();
                }
            }
            KeyCode::KeyV => {
                self.lib.view = match self.lib.view {
                    View::Grid => View::List,
                    View::List => View::Grid,
                };
            }
            KeyCode::KeyS => {
                self.lib.stack_variants = !self.lib.stack_variants;
                self.lib.invalidate();
            }
            KeyCode::KeyA if self.ctrl => self.lib.select_all(),
            KeyCode::KeyP => {
                let assets = if self.lib.selection.is_empty() {
                    self.lib.focused().into_iter().collect()
                } else {
                    self.lib.selection.clone()
                };
                self.screenshot_all(&assets);
            }
            KeyCode::BracketLeft | KeyCode::BracketRight => {
                let delta = if code == KeyCode::BracketLeft { -1 } else { 1 };
                if let Some(row) = self.lib.move_cursor(delta)
                    && let Some(ui) = &mut self.ui
                {
                    ui.scroll_to_row(row);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            // Escape steps back to the grid; only Q leaves outright.
            KeyCode::Escape => {
                self.sync_current();
                self.mode = Mode::Library;
                self.retitle();
            }
            KeyCode::KeyQ => event_loop.exit(),
            KeyCode::KeyF => {
                if let Some(bounds) = self.renderer.as_ref().and_then(Renderer::bounds) {
                    self.camera.frame(&bounds);
                }
            }
            KeyCode::Home => {
                if let Some(bounds) = self.renderer.as_ref().and_then(Renderer::bounds) {
                    self.camera.reset(&bounds);
                }
            }
            KeyCode::KeyG => self.show_grid = !self.show_grid,
            KeyCode::KeyB => self.show_bbox = !self.show_bbox,
            KeyCode::KeyA => self.show_axes = !self.show_axes,
            KeyCode::KeyO => self.ambient_occlusion = !self.ambient_occlusion,
            KeyCode::KeyS => self.cycle_msaa(),
            KeyCode::KeyT => self.background = self.background.toggled(),
            KeyCode::KeyP => self.screenshot(),
            KeyCode::KeyM | KeyCode::Tab => self.menu.toggle(&self.names, self.index),
            KeyCode::BracketLeft => self.cycle(-1),
            KeyCode::BracketRight => self.cycle(1),
            KeyCode::KeyR => {
                self.load(Framing::Keep);
            }
            // Blender's numpad views, on the numpad and on the digit row for
            // the laptops that have no numpad. Ctrl gives the opposite side,
            // which is the convention people already have in their fingers.
            KeyCode::Digit1 | KeyCode::Numpad1 => self.look_along(Axis::Front, Axis::Back),
            KeyCode::Digit3 | KeyCode::Numpad3 => self.look_along(Axis::Right, Axis::Left),
            KeyCode::Digit7 | KeyCode::Numpad7 => self.look_along(Axis::Top, Axis::Bottom),
            KeyCode::Digit5 | KeyCode::Numpad5 => {
                self.camera.orthographic = !self.camera.orthographic;
                self.note(if self.camera.orthographic {
                    "orthographic"
                } else {
                    "perspective"
                });
            }
            _ => {}
        }
    }

    /// Swing to `axis`, or to `opposite` when Ctrl is down.
    fn look_along(&mut self, axis: Axis, opposite: Axis) {
        let axis = if self.ctrl { opposite } else { axis };
        self.camera.look_along(axis);
        self.note(format!("{axis:?} view").to_lowercase());
    }

    /// Put a line in the HUD for a few seconds.
    /// Step to the next multisample count this adapter will give us.
    ///
    /// A cycle rather than an on/off switch: the useful question on a laptop
    /// is not whether to antialias but how much to pay for it, and the answer
    /// is visible in the frame rate in the corner while you press the key.
    fn cycle_msaa(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let counts = renderer.supported_samples().to_vec();
        let next = counts
            .iter()
            .position(|&n| n == renderer.samples())
            .map_or(0, |i| (i + 1) % counts.len());
        let samples = renderer.set_samples(counts[next]);
        self.settings.msaa = samples;
        self.note(match samples {
            1 => "no antialiasing".to_owned(),
            n => format!("{n}x antialiasing"),
        });
    }

    fn note(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On mobile backends this fires again after a suspend; there is
        // nothing to rebuild on desktop.
        if self.window.is_some() {
            return;
        }
        let saved = self.settings.window.sane();
        let mut attributes = Window::default_attributes()
            .with_title(format!("voxview - {}", self.path().display()))
            .with_inner_size(LogicalSize::new(saved.width as f64, saved.height as f64))
            .with_maximized(saved.maximized);
        if let (Some(x), Some(y)) = (saved.x, saved.y) {
            attributes = attributes.with_position(LogicalPosition::new(x as f64, y as f64));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fatal = Some(anyhow::Error::new(e).context("could not open a window"));
                event_loop.exit();
                return;
            }
        };
        match Renderer::new(window.clone(), self.settings.msaa) {
            Ok(renderer) => {
                println!("voxview: rendering on {}", renderer.adapter_name);
                // The adapter may not do the count the settings asked for, so
                // the settings take what it gave rather than asking again
                // every launch.
                self.settings.msaa = renderer.samples();
                self.renderer = Some(renderer);
            }
            Err(e) => {
                self.fatal = Some(e);
                event_loop.exit();
                return;
            }
        }
        let mut ui = Ui::new(&window);
        // The viewer's own file listing comes from the command line, not from
        // the scan, so it gets asked separately.
        for name in &self.names {
            ui.may_show(name);
        }
        self.ui = Some(ui);
        self.window = Some(window);
        self.scan = Some(Scan::start(self.root.clone()));
        self.load(Framing::Reset);
        self.rewatch();
        self.retitle();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // egui sees every event first. In the library it owns the window
        // outright; in the viewer it claims only what the chrome is under,
        // so an orbit started on bare background still works.
        let consumed = match (self.ui.take(), self.window.clone()) {
            (Some(mut ui), Some(window)) => {
                let consumed = ui.on_event(&window, &event);
                self.ui = Some(ui);
                consumed
            }
            (ui, _) => {
                self.ui = ui;
                false
            }
        };
        let browsing = self.mode == Mode::Library;
        // A redraw and a resize are the window's business whoever consumed
        // them, and a close request always closes.
        let plumbing = matches!(
            event,
            WindowEvent::CloseRequested
                | WindowEvent::Resized(_)
                | WindowEvent::RedrawRequested
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::ScaleFactorChanged { .. }
        );
        if consumed && !plumbing {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || event.repeat {
                    return;
                }
                // While a text field has the keyboard, single letters are
                // letters, not shortcuts.
                if self.ui.as_ref().is_some_and(Ui::typing) {
                    return;
                }
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                if self.menu.is_open() {
                    if code.is_some_and(|c| self.menu_key(c)) {
                        return;
                    }
                    // Anything else printable spells a filter. The overlay
                    // shortcuts are single letters, so they have to give way
                    // while the menu has the keyboard.
                    if let Some(c) = event.text.as_deref().and_then(printable) {
                        self.menu.filter_push(c, &self.names);
                        return;
                    }
                }
                if let Some(code) = code {
                    if browsing {
                        self.library_key(code, event_loop);
                    } else {
                        self.key(code, event_loop);
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.shift = state.shift_key();
                self.ctrl = state.control_key() || state.super_key();
            }
            WindowEvent::MouseInput { state, button, .. } if !browsing => {
                let down = state == ElementState::Pressed;
                // A press inside the menu picks a file; it must not also start
                // an orbit, so it is swallowed here and never reaches `Mouse`.
                if down && button == MouseButton::Left {
                    match self.menu_under_cursor() {
                        Some(Hit::Row(index)) => {
                            self.show(index);
                            return;
                        }
                        Some(Hit::Panel) => return,
                        None => {}
                    }
                }
                match button {
                    MouseButton::Left => self.mouse.left = down,
                    MouseButton::Right | MouseButton::Middle => self.mouse.pan = down,
                    _ => {}
                }
                if !down && !self.mouse.left && !self.mouse.pan {
                    self.mouse.last = None;
                }
            }
            WindowEvent::CursorMoved { position, .. } if !browsing => {
                self.mouse.position = position;
                let last = self.mouse.last.replace(position);
                if !self.mouse.left && !self.mouse.pan {
                    return;
                }
                let Some(last) = last else { return };
                let (dx, dy) = ((position.x - last.x) as f32, (position.y - last.y) as f32);
                if self.mouse.left {
                    self.camera.orbit(dx, dy);
                } else {
                    let height = self.renderer.as_ref().map_or(1.0, |r| r.size().1 as f32);
                    self.camera.pan(dx, dy, height);
                }
            }
            WindowEvent::CursorLeft { .. } => self.mouse.last = None,
            WindowEvent::MouseWheel { delta, .. } if !browsing => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    // Touchpads report pixels; 50 of them feels like one notch.
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                if self.menu_under_cursor().is_some() {
                    // Scrolling the list moves the view without moving the
                    // cursor, so it does not load a file per notch.
                    self.menu.scroll_by(-(notches * WHEEL_ROWS as f32) as isize);
                } else {
                    self.camera.zoom(notches);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// The last thing winit calls, on every way out of the loop.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.save_settings();
    }
}

/// What the menu has under the cursor, if anything.
fn menu_under_cursor_impl(menu: &Menu, at: PhysicalPosition<f64>) -> Option<Hit> {
    menu.hit(at.x as f32, at.y as f32)
}

/// The single character a key press contributes to the filter.
///
/// Dead keys and IME sequences deliver more than one character at a time;
/// a file filter is plain text, so anything exotic is dropped rather than
/// half-applied.
fn printable(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() {
        return None;
    }
    Some(c)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}

/// Group digits so voxel and triangle counts stay readable.
fn thousands(n: usize) -> String {
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

/// `YYYYMMDD-HHMMSS` in UTC, for screenshot filenames.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since the Unix epoch to a calendar date (Howard Hinnant's algorithm).
/// Cheaper than taking on a date-and-time crate for one filename.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the year.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe as i64 + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_grouping() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn calendar_conversion_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        // 2000-02-29 -- a leap day in a century year that is a leap year.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn timestamps_are_fixed_width_and_sortable() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 15);
        assert_eq!(stamp.as_bytes()[8], b'-');
        assert!(stamp.chars().filter(char::is_ascii_digit).count() == 14);
    }

    #[test]
    fn only_the_first_line_of_an_error_reaches_the_hud() {
        assert_eq!(first_line("bad file\ncaused by: nonsense"), "bad file");
        assert_eq!(first_line("bad file"), "bad file");
    }
}
