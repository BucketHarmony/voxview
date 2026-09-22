//! The window, the input handling and the frame loop.

use crate::camera::OrbitCamera;
use crate::gfx::{Background, FrameParams, Renderer};
use crate::hud::HudLine;
use crate::loader::{self, VoxScene};
use crate::menu::{Hit, Menu};
use crate::mesh;
use crate::watch::FileWatcher;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// Open the viewer on `paths[index]`.
pub fn run(paths: Vec<PathBuf>, index: usize) -> Result<()> {
    let event_loop = EventLoop::new().context("could not create an event loop")?;
    // Redraw continuously: the swapchain is in Fifo mode, so this paces
    // itself at the display's refresh rate rather than spinning.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new(paths, index);
    event_loop.run_app(&mut app).context("the viewer stopped")?;
    match app.fatal.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
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
    fps: Fps,
    status: Option<(String, Instant)>,
    /// An error that should make the process exit non-zero.
    fatal: Option<anyhow::Error>,
}

/// How long a one-off message (a screenshot path, say) stays in the HUD.
const STATUS_SECONDS: f32 = 4.0;
/// File rows one notch of the wheel scrolls the menu by.
const WHEEL_ROWS: usize = 3;

impl App {
    fn new(paths: Vec<PathBuf>, index: usize) -> App {
        let names = paths.iter().map(|p| file_name(p)).collect();
        let title = paths
            .first()
            .and_then(|p| p.parent())
            .filter(|d| !d.as_os_str().is_empty())
            .map(file_name)
            .unwrap_or_else(|| ".".into());
        App {
            paths,
            index,
            names,
            title,
            menu: Menu::new(),
            window: None,
            renderer: None,
            watcher: None,
            camera: OrbitCamera::default(),
            info: None,
            error: None,
            show_grid: true,
            show_bbox: false,
            show_axes: true,
            ambient_occlusion: true,
            background: Background::Dark,
            mouse: Mouse::default(),
            fps: Fps::new(),
            status: None,
            fatal: None,
        }
    }

    fn path(&self) -> &Path {
        &self.paths[self.index]
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
        let meshes = mesh::mesh_models(&scene.models);
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
                if info.default_palette {
                    lines.push(HudLine::dim("default palette (none in file)"));
                }
            }
            None => lines.push(HudLine::normal("no model loaded")),
        }

        lines.push(HudLine::dim(format!(
            "[G]rid {}  [B]ox {}  [A]xes {}  [O]cclusion {}",
            on_off(self.show_grid),
            on_off(self.show_bbox),
            on_off(self.show_axes),
            on_off(self.ambient_occlusion),
        )));
        if self.paths.len() > 1 {
            lines.push(HudLine::dim(format!(
                "file {} of {}  [ ]  [M]enu",
                self.index + 1,
                self.paths.len()
            )));
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
        if self.watcher.as_mut().is_some_and(FileWatcher::poll) {
            self.load(Framing::Keep);
        }
        self.fps.tick();

        let hud = self.hud();
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |w| w.scale_factor().round().max(1.0) as f32);
        let menu = if self.menu.is_open() {
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
            draw_scene: true,
        };
        if let Some(renderer) = &mut self.renderer {
            renderer.render(&params, None);
        }
    }

    fn key(&mut self, code: KeyCode, event_loop: &ActiveEventLoop) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
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
            KeyCode::KeyT => self.background = self.background.toggled(),
            KeyCode::KeyP => self.screenshot(),
            KeyCode::KeyM | KeyCode::Tab => self.menu.toggle(&self.names, self.index),
            KeyCode::BracketLeft => self.cycle(-1),
            KeyCode::BracketRight => self.cycle(1),
            KeyCode::KeyR => {
                self.load(Framing::Keep);
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On mobile backends this fires again after a suspend; there is
        // nothing to rebuild on desktop.
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(format!("voxview - {}", self.path().display()))
            .with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                self.fatal = Some(anyhow::Error::new(e).context("could not open a window"));
                event_loop.exit();
                return;
            }
        };
        match Renderer::new(window.clone()) {
            Ok(renderer) => {
                println!("voxview: rendering on {}", renderer.adapter_name);
                self.renderer = Some(renderer);
            }
            Err(e) => {
                self.fatal = Some(e);
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        self.load(Framing::Reset);
        self.rewatch();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
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
                    self.key(code, event_loop);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
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
            WindowEvent::CursorMoved { position, .. } => {
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
            WindowEvent::MouseWheel { delta, .. } => {
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

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
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
