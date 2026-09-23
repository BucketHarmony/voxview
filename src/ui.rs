//! The library browser and the viewer's chrome, drawn with egui.
//!
//! Everything here is presentation. What to show is decided in
//! [`crate::library`]; what happens when you click is handed back to
//! [`crate::app`] as an [`Action`] rather than done on the spot, so the
//! browser never has to know about windows, cameras or file watchers.
//!
//! The grid and the list are both virtualised: a folder of four thousand
//! files lays out the dozen rows on screen and nothing else, and a cell only
//! asks for its thumbnail while it is being drawn.

use crate::gfx::{Renderer, UiFrame};
use crate::library::{self, Library, Load, Row, Scope, Sort, View, thousands};
use crate::sysfont;
use crate::thumb::Thumbs;
use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use std::path::PathBuf;
use winit::window::Window;

// ---- palette ------------------------------------------------------------

const BG: Color32 = Color32::from_rgb(0x14, 0x16, 0x1A);
const BAR: Color32 = Color32::from_rgb(0x10, 0x12, 0x16);
const PANEL: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x24);
const CENTRE: Color32 = Color32::from_rgb(0x16, 0x19, 0x1E);
const LINE: Color32 = Color32::from_rgb(0x22, 0x26, 0x2E);
const EDGE: Color32 = Color32::from_rgb(0x2C, 0x31, 0x3A);
const CELL: Color32 = Color32::from_rgb(0x1D, 0x20, 0x27);
const CELL_HOVER: Color32 = Color32::from_rgb(0x22, 0x26, 0x2E);
const TEXT: Color32 = Color32::from_rgb(0xE3, 0xE6, 0xEA);
const DIM: Color32 = Color32::from_rgb(0x8B, 0x93, 0xA0);
const FAINT: Color32 = Color32::from_rgb(0x75, 0x7C, 0x87);
const ACCENT: Color32 = Color32::from_rgb(0x6E, 0x97, 0xDA);
const ACCENT_FILL: Color32 = Color32::from_rgb(0x2E, 0x4A, 0x7D);
const GOOD: Color32 = Color32::from_rgb(0x5F, 0xBF, 0xA8);
const WARN: Color32 = Color32::from_rgb(0xE0, 0xB9, 0x6A);
const BAD: Color32 = Color32::from_rgb(0xE4, 0x69, 0x5C);

const RAIL_WIDTH: f32 = 268.0;
const INSPECTOR_WIDTH: f32 = 320.0;
const GAP: f32 = 14.0;
/// Room under a cell's thumbnail for the name and the extent.
const CAPTION: f32 = 38.0;
const LIST_ROW: f32 = 26.0;
const FILMSTRIP: f32 = 118.0;

/// What the browser wants the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Open this asset in the viewer.
    Open(usize),
    /// Write a PNG next to each of these assets.
    Screenshot(Vec<usize>),
    /// Leave the viewer and go back to the grid.
    ToLibrary,
    Toggle(Toggle),
    Reload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Grid,
    Bbox,
    Axes,
    Occlusion,
    Orthographic,
    Msaa,
    Background,
}

/// How far the background scan has got, for the status bar.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanStatus {
    pub found: usize,
    pub parsed: usize,
    pub scanning: bool,
}

/// The viewer's state, for the overlay drawn on top of the model.
pub struct ViewerChrome<'a> {
    pub current: Option<usize>,
    /// The file on screen. Drawn here as well as in the HUD, because the HUD's
    /// bitmap font is ASCII and a name in another script reaches it as `?`.
    pub name: &'a str,
    pub show_grid: bool,
    pub show_bbox: bool,
    pub show_axes: bool,
    pub occlusion: bool,
    pub orthographic: bool,
    /// Multisample count in force, for the pill that cycles it.
    pub samples: u32,
    pub error: Option<&'a str>,
}

/// Which face the browser is showing.
pub enum Chrome<'a> {
    Library(ScanStatus),
    Viewer(ViewerChrome<'a>),
}

pub struct Ui {
    ctx: egui::Context,
    state: egui_winit::State,
    pub thumbs: Thumbs,
    /// Set by `/` so the find field takes focus on the next frame.
    focus_find: bool,
    new_collection: String,
    /// Columns the grid laid out last frame, so the arrow keys know what a
    /// row is worth.
    columns: usize,
    /// A row the keyboard moved to that the scroll area has yet to reveal.
    pending_scroll: Option<usize>,
    /// `Space`: the focused asset, large, without leaving the grid.
    peek: bool,
    /// `C`: choose a collection for the selection.
    prompt: bool,
    /// Collected during a frame, returned from [`Ui::frame`].
    actions: Vec<Action>,
    /// Scripts seen in names that the bundled font cannot draw, and not yet
    /// answered with a system font.
    wanted_scripts: sysfont::Scripts,
    /// Scripts a font has already been looked for, so each is looked for once.
    loaded_scripts: sysfont::Scripts,
    /// The fallback fonts in use, kept because egui's font definitions are
    /// replaced wholesale and every one has to be listed each time.
    loaded_fonts: Vec<(String, std::sync::Arc<egui::FontData>)>,
}

impl Ui {
    pub fn new(window: &Window) -> Ui {
        let ctx = egui::Context::default();
        ctx.set_visuals(visuals());
        ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = vec2(8.0, 6.0);
            style.spacing.button_padding = vec2(8.0, 4.0);
        });
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        Ui {
            ctx,
            state,
            thumbs: Thumbs::new(),
            focus_find: false,
            new_collection: String::new(),
            columns: 1,
            pending_scroll: None,
            peek: false,
            prompt: false,
            actions: Vec::new(),
            wanted_scripts: sysfont::Scripts::default(),
            loaded_scripts: sysfont::Scripts::default(),
            loaded_fonts: Vec::new(),
        }
    }

    /// Note that `text` is about to be shown, so fonts that can draw it are
    /// found before it is.
    ///
    /// Cheap enough to call on every file name in a scan: for an ASCII name it
    /// is one range check per character with no allocation, and the answer is
    /// folded into a bitset.
    pub fn may_show(&mut self, text: &str) {
        let scripts = sysfont::scripts_beyond(text);
        if scripts.is_empty() {
            return;
        }
        for script in scripts.iter() {
            if !self.loaded_scripts.contains(script) {
                self.wanted_scripts.insert(script);
            }
        }
    }

    /// Fold any system fallback fonts into egui's font list.
    ///
    /// Called at the top of a frame rather than from [`Ui::may_show`], because
    /// reading twenty megabytes of font is not something to do in the middle
    /// of a scan drain, and because a whole scan's worth of names may ask for
    /// fonts before the first frame that needs one.
    fn load_fallback(&mut self) {
        let wanted = std::mem::take(&mut self.wanted_scripts);
        if wanted.is_empty() {
            return;
        }
        // Marked as done whether or not a font turned up: a script with no
        // font on this machine has no font on the next frame either, and
        // re-reading the directories every frame would be worse than boxes.
        self.loaded_scripts = self.loaded_scripts.union(wanted);

        let fonts = sysfont::fallbacks(wanted);
        if fonts.is_empty() {
            let names: Vec<String> = wanted.iter().map(|s| format!("{s:?}")).collect();
            eprintln!(
                "voxview: some names need {}, which the built-in font cannot draw, \
                 and no system font was found to fall back to; they will appear as boxes",
                names.join(", ")
            );
            return;
        }
        for font in fonts {
            println!(
                "voxview: using {} for text the built-in font cannot draw",
                font.name
            );
            self.loaded_fonts.push((
                font.name,
                std::sync::Arc::new(egui::FontData {
                    font: font.bytes.into(),
                    index: font.index,
                    tweak: egui::FontTweak::default(),
                }),
            ));
        }

        // Rebuilt from the defaults and re-applied whole: egui's font
        // definitions are a complete state, not something to append to, so
        // fonts loaded on an earlier frame are listed again. They are shared
        // by `Arc`, so this re-lists them without re-reading them.
        let mut definitions = egui::FontDefinitions::default();
        for (name, data) in &self.loaded_fonts {
            definitions.font_data.insert(name.clone(), data.clone());
            // Appended, not prepended: the bundled font stays in charge of
            // Latin, so the interface does not change shape on a machine that
            // happens to have a different font installed. egui only consults
            // the next entry for a glyph the previous one lacks.
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                definitions
                    .families
                    .entry(family)
                    .or_default()
                    .push(name.clone());
            }
        }
        self.ctx.set_fonts(definitions);
    }

    /// Feed a window event to egui. Returns true when egui used it, so the
    /// viewer does not also orbit the camera on a click in the filmstrip.
    pub fn on_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    /// True while a text field has the keyboard, so single-letter shortcuts
    /// do not fire while you are typing a filter.
    pub fn typing(&self) -> bool {
        self.ctx.egui_wants_keyboard_input()
    }

    /// True when the pointer is over a panel rather than the model.
    pub fn over_ui(&self) -> bool {
        self.ctx.is_pointer_over_egui()
    }

    pub fn focus_find(&mut self) {
        self.focus_find = true;
    }

    /// How many cells the grid fitted across last frame. One in list view, so
    /// up and down move a single row either way.
    pub fn columns(&self) -> usize {
        self.columns.max(1)
    }

    /// Bring a row into view on the next frame.
    pub fn scroll_to_row(&mut self, row: usize) {
        self.pending_scroll = Some(row);
    }

    pub fn toggle_peek(&mut self) {
        self.peek = !self.peek;
    }

    /// Dismiss whatever overlay is up. Returns true if there was one, so
    /// `Esc` closes it before it means anything else.
    pub fn close_overlay(&mut self) -> bool {
        let was = self.peek || self.prompt;
        self.peek = false;
        self.prompt = false;
        was
    }

    pub fn prompt_collection(&mut self) {
        self.prompt = true;
    }

    /// Build one frame of UI and hand back what to draw and what to do.
    pub fn frame(
        &mut self,
        window: &Window,
        renderer: &mut Renderer,
        lib: &mut Library,
        chrome: Chrome,
    ) -> (UiFrame, Vec<Action>) {
        self.load_fallback();
        self.thumbs.pump(&self.ctx, renderer);
        self.actions.clear();

        let input = self.state.take_egui_input(window);
        let ctx = self.ctx.clone();
        let output = ctx.run_ui(input, |ui| match &chrome {
            Chrome::Library(status) => self.library(ui, lib, *status),
            Chrome::Viewer(viewer) => self.viewer(ui, lib, viewer),
        });

        self.state
            .handle_platform_output(window, output.platform_output);
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        (
            UiFrame {
                jobs,
                textures: output.textures_delta,
                pixels_per_point: output.pixels_per_point,
            },
            std::mem::take(&mut self.actions),
        )
    }

    // ---- library --------------------------------------------------------

    fn library(&mut self, ui: &mut egui::Ui, lib: &mut Library, status: ScanStatus) {
        self.title_bar(ui, lib, status);
        self.toolbar(ui, lib);
        self.status_bar(ui, lib, status);
        self.rail(ui, lib);
        self.inspector(ui, lib);

        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(CENTRE))
            .show(ui, |ui| {
                self.centre_header(ui, lib);
                match lib.view {
                    View::Grid => self.grid(ui, lib),
                    View::List => self.list(ui, lib),
                }
            });

        if self.peek {
            self.peek_overlay(ui, lib);
        }
        if self.prompt {
            self.collection_prompt(ui, lib);
        }
    }

    /// `Space`: the focused asset at a size you can actually judge, over the
    /// grid rather than instead of it, so it costs nothing to dismiss.
    fn peek_overlay(&mut self, ui: &mut egui::Ui, lib: &Library) {
        let Some(asset) = lib.focused() else {
            self.peek = false;
            return;
        };
        let path = lib.assets[asset].path.clone();
        let name = lib.assets[asset].name.clone();
        let folder = lib.relative(asset);
        let detail = match &lib.assets[asset].load {
            Load::Ready(stats) => format!(
                "{} \u{d7} {} \u{d7} {}   {} voxels",
                stats.dims[0],
                stats.dims[1],
                stats.dims[2],
                thousands(stats.voxels)
            ),
            Load::Pending => "reading\u{2026}".into(),
            Load::Failed(why) => why.clone(),
        };

        egui::Area::new(egui::Id::new("peek"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(&ui.ctx().clone(), |ui| {
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, EDGE))
                    .corner_radius(12.0)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        let side = 420.0;
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
                        ui.painter().rect_filled(rect, 8.0, BAR);
                        match self.thumbs.get(&path) {
                            Some(texture) => {
                                ui.painter().image(
                                    texture.id(),
                                    rect.shrink(12.0),
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                            }
                            None => placeholder(ui.painter(), rect.center(), 22.0, EDGE),
                        }
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new(name).size(15.0).strong().color(TEXT));
                        ui.label(
                            egui::RichText::new(folder)
                                .monospace()
                                .size(10.5)
                                .color(FAINT),
                        );
                        ui.label(
                            egui::RichText::new(detail)
                                .monospace()
                                .size(11.5)
                                .color(DIM),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Space to close  \u{b7}  Enter to open")
                                .size(10.5)
                                .color(FAINT),
                        );
                    });
            });
    }

    /// `C`: put the selection in a collection, existing or new.
    fn collection_prompt(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        let targets: Vec<usize> = if lib.selection.is_empty() {
            lib.focused().into_iter().collect()
        } else {
            lib.selection.clone()
        };
        if targets.is_empty() {
            self.prompt = false;
            return;
        }

        let mut close = false;
        egui::Area::new(egui::Id::new("collection-prompt"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(&ui.ctx().clone(), |ui| {
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, EDGE))
                    .corner_radius(12.0)
                    .inner_margin(16)
                    .show(ui, |ui| {
                        ui.set_width(300.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Add {} asset{} to",
                                targets.len(),
                                if targets.len() == 1 { "" } else { "s" }
                            ))
                            .size(13.0)
                            .color(TEXT),
                        );
                        ui.add_space(8.0);
                        let mut chosen = None;
                        for (i, collection) in lib.collections.iter().enumerate() {
                            let label =
                                format!("{}   {}", collection.name, collection.members.len());
                            if ui.button(label).clicked() {
                                chosen = Some(i);
                            }
                        }
                        if !lib.collections.is_empty() {
                            ui.add_space(8.0);
                        }
                        ui.horizontal(|ui| {
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut self.new_collection)
                                    .desired_width(180.0)
                                    .hint_text("new collection"),
                            );
                            field.request_focus();
                            let entered =
                                field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (entered || ui.button("Create").clicked())
                                && !self.new_collection.trim().is_empty()
                            {
                                let name = std::mem::take(&mut self.new_collection);
                                chosen = Some(lib.new_collection(name));
                            }
                        });
                        if let Some(i) = chosen {
                            lib.add_to_collection(i, &targets);
                            close = true;
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Esc to cancel").size(10.5).color(FAINT));
                    });
            });
        if close {
            self.prompt = false;
        }
    }

    fn title_bar(&mut self, ui: &mut egui::Ui, lib: &Library, status: ScanStatus) {
        bar("title", 36.0, BAR).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    egui::RichText::new("VOXVIEW")
                        .color(ACCENT)
                        .size(12.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(lib.root.display().to_string())
                        .monospace()
                        .size(11.0)
                        .color(DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, colour) = if status.scanning {
                        (
                            format!(
                                "reading {} of {}",
                                thousands(status.parsed),
                                thousands(status.found)
                            ),
                            WARN,
                        )
                    } else {
                        (format!("watching {} files", thousands(status.found)), GOOD)
                    };
                    ui.label(egui::RichText::new(text).size(11.0).color(colour));
                });
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        bar("toolbar", 52.0, PANEL).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                // Breadcrumb. Every ancestor is a click back up the tree.
                let crumbs = lib.breadcrumb();
                let mut jump = None;
                for (i, folder) in crumbs.iter().enumerate() {
                    if i > 0 {
                        ui.label(egui::RichText::new("/").monospace().size(12.0).color(EDGE));
                    }
                    let last = i + 1 == crumbs.len();
                    let text = egui::RichText::new(&lib.folders[*folder].name)
                        .monospace()
                        .size(12.0)
                        .color(if last { TEXT } else { DIM });
                    if ui
                        .add(egui::Label::new(text).sense(Sense::click()))
                        .clicked()
                        && !last
                    {
                        jump = Some(*folder);
                    }
                }
                if let Scope::Collection(c) = lib.scope
                    && let Some(collection) = lib.collections.get(c)
                {
                    ui.label(
                        egui::RichText::new(collection.name.clone())
                            .size(12.0)
                            .color(TEXT),
                    );
                }
                if let Some(folder) = jump {
                    lib.set_scope(Scope::Folder(folder));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Cell size, in the same 3..6 steps the design's slider has.
                    ui.spacing_mut().slider_width = 110.0;
                    ui.add(egui::Slider::new(&mut lib.cell, 96.0..=232.0).show_value(false));
                    ui.label(egui::RichText::new("Size").size(11.0).color(DIM));

                    let list = lib.view == View::List;
                    if ui.selectable_label(list, "List").clicked() {
                        lib.view = View::List;
                    }
                    if ui.selectable_label(!list, "Grid").clicked() {
                        lib.view = View::Grid;
                    }

                    ui.add_space(6.0);
                    let find = ui.add(
                        egui::TextEdit::singleline(&mut lib.find)
                            .desired_width(240.0)
                            .hint_text("find in this folder")
                            .font(egui::TextStyle::Monospace),
                    );
                    if self.focus_find {
                        find.request_focus();
                        self.focus_find = false;
                    }
                    if find.changed() {
                        lib.invalidate();
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, lib: &Library, status: ScanStatus) {
        egui::Panel::bottom("status")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(30.0)
            .frame(bar_frame(BAR))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let scope = match lib.scope {
                        Scope::Folder(f) => lib.folders[f].name.clone(),
                        Scope::Collection(c) => lib
                            .collections
                            .get(c)
                            .map(|c| c.name.clone())
                            .unwrap_or_default(),
                    };
                    let failures = lib.failures();
                    mono(
                        ui,
                        &format!("{} in {scope}", thousands(status.found)),
                        FAINT,
                    );
                    mono(ui, "\u{b7}", EDGE);
                    mono(ui, &format!("{} selected", lib.selection.len()), FAINT);
                    mono(ui, "\u{b7}", EDGE);
                    mono(
                        ui,
                        &format!("{failures} failed to parse"),
                        if failures > 0 { BAD } else { FAINT },
                    );
                    if lib.truncated {
                        mono(ui, "\u{b7}", EDGE);
                        mono(ui, "listing truncated", WARN);
                    }
                    if !lib.skipped.is_empty() {
                        mono(ui, "\u{b7}", EDGE);
                        let text = format!("{} folders unreadable", lib.skipped.len());
                        mono(ui, &text, WARN);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if status.scanning {
                            mono(
                                ui,
                                &format!(
                                    "read {} / {}",
                                    thousands(status.parsed),
                                    thousands(status.found)
                                ),
                                WARN,
                            );
                        } else {
                            mono(ui, "scan complete", GOOD);
                        }
                    });
                });
            });
    }

    fn rail(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        egui::Panel::left("rail")
            .exact_size(RAIL_WIDTH)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(12.0);
                    section(ui, "LIBRARY");
                    let mut toggle = None;
                    let mut scope = None;
                    for row in lib.tree_rows() {
                        let current = lib.scope == Scope::Folder(row.folder);
                        let (rect, response) = ui.allocate_exact_size(
                            vec2(ui.available_width(), 22.0),
                            Sense::click(),
                        );
                        let painter = ui.painter();
                        if current {
                            painter.rect_filled(rect, 4.0, ACCENT_FILL);
                        } else if response.hovered() {
                            painter.rect_filled(rect, 4.0, CELL_HOVER);
                        }
                        let x = rect.left() + 8.0 + row.depth as f32 * 13.0;
                        if row.has_children {
                            caret(painter, pos2(x + 4.0, rect.center().y), row.expanded, FAINT);
                        }
                        painter.text(
                            pos2(x + 14.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &row.label,
                            FontId::proportional(13.0),
                            if current { TEXT } else { DIM },
                        );
                        painter.text(
                            pos2(rect.right() - 10.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            thousands(row.total),
                            FontId::monospace(11.0),
                            FAINT,
                        );
                        // The caret opens the branch; the row itself moves you
                        // into it -- two different intentions, two targets.
                        if response.clicked() {
                            let on_caret = response
                                .interact_pointer_pos()
                                .is_some_and(|p| p.x < x + 12.0);
                            if on_caret && row.has_children {
                                toggle = Some(row.folder);
                            } else {
                                scope = Some(row.folder);
                            }
                        }
                    }
                    if let Some(folder) = toggle {
                        lib.toggle_folder(folder);
                    }
                    if let Some(folder) = scope {
                        if !lib.folders[folder].children.is_empty() {
                            lib.folders[folder].expanded = true;
                        }
                        lib.set_scope(Scope::Folder(folder));
                    }

                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        section(ui, "COLLECTIONS");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("+").on_hover_text("New collection").clicked() {
                                let n = lib.collections.len() + 1;
                                lib.new_collection(format!("Collection {n}"));
                            }
                        });
                    });
                    if lib.collections.is_empty() {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(
                                "  A collection cuts across folders.\n  Select assets, then press C.",
                            )
                            .size(11.0)
                            .color(FAINT),
                        );
                    }
                    let mut pick = None;
                    for (i, collection) in lib.collections.iter().enumerate() {
                        let current = lib.scope == Scope::Collection(i);
                        let label =
                            format!("{}   {}", collection.name, collection.members.len());
                        if ui.selectable_label(current, label).clicked() {
                            pick = Some(i);
                        }
                    }
                    if let Some(i) = pick {
                        lib.set_scope(Scope::Collection(i));
                    }

                    ui.add_space(16.0);
                    section(ui, "FILTERS");
                    self.facet_rail(ui, lib);
                    ui.add_space(18.0);
                });
            });
    }

    fn facet_rail(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        let counts = lib.facet_counts();
        let mut changed = false;

        ui.add_space(4.0);
        group_label(ui, "Extent");
        for (i, label) in ["\u{2264} 16", "\u{2264} 32", "> 32"].iter().enumerate() {
            changed |= facet_row(
                ui,
                label,
                counts.extent[i].matching,
                &mut lib.facets.extent[i],
            );
        }

        ui.add_space(8.0);
        group_label(ui, "Palette");
        changed |= facet_row(
            ui,
            "from file",
            counts.palette_from_file.matching,
            &mut lib.facets.palette_from_file,
        );
        changed |= facet_row(
            ui,
            "MagicaVoxel default",
            counts.palette_default.matching,
            &mut lib.facets.palette_default,
        );

        ui.add_space(8.0);
        group_label(ui, "State");
        changed |= facet_row(
            ui,
            "changed in 24h",
            counts.recent.matching,
            &mut lib.facets.recent,
        );
        changed |= facet_row(
            ui,
            "failed to parse",
            counts.failed.matching,
            &mut lib.facets.failed,
        );

        if counts.pending > 0 {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("  {} still being read", thousands(counts.pending)))
                    .size(10.5)
                    .color(FAINT),
            );
        }
        if lib.facets.any() || !lib.find.is_empty() {
            ui.add_space(6.0);
            if ui.small_button("Clear filters").clicked() {
                lib.clear_filters();
            }
        }
        if changed {
            lib.invalidate();
        }
    }

    fn centre_header(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        let shown = lib.rows().len();
        let filtered = lib.filtered().len();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(format!("{} assets", thousands(filtered)))
                    .size(12.0)
                    .color(TEXT),
            );
            ui.label(egui::RichText::new("\u{b7}").color(EDGE));

            egui::ComboBox::from_id_salt("sort")
                .selected_text(egui::RichText::new(lib.sort.label()).size(12.0).color(DIM))
                .width(96.0)
                .show_ui(ui, |ui| {
                    for option in Sort::ALL {
                        if ui
                            .selectable_label(lib.sort == option, option.label())
                            .clicked()
                        {
                            lib.sort = option;
                            lib.invalidate();
                        }
                    }
                });
            let arrow = if lib.descending { "desc" } else { "asc" };
            if ui.small_button(arrow).clicked() {
                lib.descending = !lib.descending;
                lib.invalidate();
            }

            ui.label(egui::RichText::new("\u{b7}").color(EDGE));
            if ui
                .selectable_label(lib.stack_variants, "Stack variants")
                .on_hover_text("Collapse crate-0 .. crate-6 into one cell")
                .clicked()
            {
                lib.stack_variants = !lib.stack_variants;
                lib.invalidate();
            }
            if lib.stack_variants && shown != filtered {
                ui.label(
                    egui::RichText::new(format!("{} rows", thousands(shown)))
                        .size(11.0)
                        .color(FAINT),
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("{} selected", lib.selection.len()))
                        .size(12.0)
                        .color(FAINT),
                );
            });
        });
        ui.add_space(6.0);
        let y = ui.min_rect().bottom();
        ui.painter()
            .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, LINE));
        ui.add_space(8.0);
    }

    // ---- the grid -------------------------------------------------------

    fn grid(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        let rows = lib.rows().to_vec();
        if rows.is_empty() {
            self.empty(ui, lib);
            return;
        }

        let cell = lib.cell;
        let width = (ui.available_width() - 2.0 * GAP).max(cell);
        let columns = ((width + GAP) / (cell + GAP)).floor().max(1.0) as usize;
        let lines = rows.len().div_ceil(columns);
        let line_height = cell + CAPTION + GAP;
        self.columns = columns;

        let mut area = egui::ScrollArea::vertical().auto_shrink([false; 2]);
        if let Some(row) = self.pending_scroll.take() {
            area = area.vertical_scroll_offset(offset_for(
                row / columns,
                line_height,
                lines,
                ui.available_height(),
            ));
        }
        area.show_rows(ui, line_height, lines, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for line in range {
                let (strip, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), line_height), Sense::hover());
                for column in 0..columns {
                    let Some(row) = rows.get(line * columns + column) else {
                        break;
                    };
                    let origin = strip.min + vec2(GAP + column as f32 * (cell + GAP), 0.0);
                    let rect = Rect::from_min_size(origin, vec2(cell, cell + CAPTION));
                    self.cell(ui, lib, row, rect);
                }
            }
        });
    }

    fn cell(&mut self, ui: &mut egui::Ui, lib: &mut Library, row: &Row, rect: Rect) {
        let asset = row.lead();
        let id = ui.id().with(("cell", asset));
        let response = ui.interact(rect, id, Sense::click());
        let selected = lib.is_selected(asset);
        let stacked = matches!(row, Row::Family { .. });
        let open = lib.family_expanded(row);

        let painter = ui.painter();
        let thumb = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));

        // A stack gets a second card peeking out behind the first.
        if stacked && !open {
            painter.rect_filled(thumb.translate(vec2(3.0, -3.0)), 8.0, CELL);
            painter.rect_stroke(
                thumb.translate(vec2(3.0, -3.0)),
                8.0,
                Stroke::new(1.0, EDGE),
                StrokeKind::Inside,
            );
        }
        let fill = if response.hovered() { CELL_HOVER } else { CELL };
        painter.rect_filled(thumb, 8.0, fill);
        let border = if selected {
            Stroke::new(1.5, ACCENT)
        } else {
            Stroke::new(1.0, LINE)
        };
        painter.rect_stroke(thumb, 8.0, border, StrokeKind::Inside);

        let path = lib.assets[asset].path.clone();
        let inner = thumb.shrink(10.0);
        match self.thumbs.get(&path) {
            Some(texture) => {
                painter.image(
                    texture.id(),
                    inner,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                if lib.assets[asset].failed() || self.thumbs.is_failed(&path) {
                    warning(painter, thumb.center(), 22.0, BAD);
                } else {
                    placeholder(painter, thumb.center(), 13.0, EDGE);
                }
            }
        }

        if let Row::Family { members, .. } = row {
            badge(
                painter,
                pos2(thumb.right() - 7.0, thumb.bottom() - 7.0),
                Align2::RIGHT_BOTTOM,
                &format!("\u{d7}{}", members.len()),
                Color32::from_rgba_unmultiplied(0x10, 0x12, 0x15, 0xD9),
                TEXT,
            );
        }
        let sets = lib.collections_of(asset);
        if let Some(first) = sets.first() {
            badge(
                painter,
                pos2(thumb.left() + 7.0, thumb.top() + 7.0),
                Align2::LEFT_TOP,
                first,
                Color32::from_rgb(0x22, 0x30, 0x4B),
                Color32::from_rgb(0xBB, 0xD0, 0xF0),
            );
        }

        // Caption. The tail of a name is what distinguishes siblings, so it is
        // the part that has to survive: wrap rather than elide.
        let label = match row {
            Row::Family { key, members } => format!("{key}-*  ({})", members.len()),
            Row::One(_) => lib.assets[asset].name.clone(),
        };
        let label = match lib.folder_hint(asset) {
            Some(folder) => format!("{folder}/{label}"),
            None => label,
        };
        let galley = wrapped(painter, &label, 12.0, TEXT, rect.width() - 4.0, 2);
        painter.galley(pos2(rect.left() + 2.0, thumb.bottom() + 6.0), galley, TEXT);

        let detail = match &lib.assets[asset].load {
            Load::Ready(stats) => extent_of(lib, row).unwrap_or_else(|| {
                format!(
                    "{} \u{d7} {} \u{d7} {}",
                    stats.dims[0], stats.dims[1], stats.dims[2]
                )
            }),
            Load::Pending => "reading\u{2026}".into(),
            Load::Failed(_) => "would not parse".into(),
        };
        painter.text(
            pos2(rect.left() + 2.0, rect.bottom() - 9.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::monospace(10.0),
            if lib.assets[asset].failed() {
                BAD
            } else {
                FAINT
            },
        );

        self.cell_input(ui, lib, row, &response);
    }

    /// Clicks shared by the grid and the list.
    fn cell_input(
        &mut self,
        ui: &egui::Ui,
        lib: &mut Library,
        row: &Row,
        response: &egui::Response,
    ) {
        let asset = row.lead();
        if response.double_clicked() {
            lib.select_only(asset);
            self.actions.push(Action::Open(asset));
            return;
        }
        if response.clicked() {
            let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
            // Clicking a stack opens it; the modifier clicks still select, so
            // you can put a whole family in a collection without opening it.
            if matches!(row, Row::Family { .. }) && !ctrl && !shift {
                lib.toggle_family(row);
            } else {
                lib.click(asset, ctrl, shift);
            }
            lib.focus_row(asset);
        }
    }

    // ---- the list -------------------------------------------------------

    fn list(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        let rows = lib.rows().to_vec();
        if rows.is_empty() {
            self.empty(ui, lib);
            return;
        }
        self.columns = 1;
        let columns = [0.0f32, 300.0, 400.0, 480.0, 560.0];
        ui.horizontal(|ui| {
            ui.add_space(GAP);
            let base = ui.cursor().left();
            let painter = ui.painter();
            for (x, title) in columns
                .iter()
                .zip(["Name", "Extent", "Voxels", "Models", "Folder"])
            {
                painter.text(
                    pos2(base + x, ui.cursor().top() + 8.0),
                    Align2::LEFT_CENTER,
                    title,
                    FontId::proportional(10.5),
                    FAINT,
                );
            }
            ui.add_space(16.0);
        });

        let mut area = egui::ScrollArea::vertical().auto_shrink([false; 2]);
        if let Some(row) = self.pending_scroll.take() {
            area = area.vertical_scroll_offset(offset_for(
                row,
                LIST_ROW,
                rows.len(),
                ui.available_height(),
            ));
        }
        area.show_rows(ui, LIST_ROW, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for index in range {
                let row = &rows[index];
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), LIST_ROW), Sense::click());
                let id = ui.id().with(("list", index));
                let response = response.union(ui.interact(rect, id, Sense::click()));
                self.list_row(ui, lib, row, rect, &response, &columns);
                self.cell_input(ui, lib, row, &response);
            }
        });
    }

    fn list_row(
        &mut self,
        ui: &egui::Ui,
        lib: &Library,
        row: &Row,
        rect: Rect,
        response: &egui::Response,
        columns: &[f32; 5],
    ) {
        let asset = row.lead();
        let selected = lib.is_selected(asset);
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 0.0, ACCENT_FILL);
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, CELL_HOVER);
        }

        let left = rect.left() + GAP;
        let y = rect.center().y;
        let indent = match row {
            Row::One(_) if lib.assets[asset].family.is_some() && lib.stack_variants => 20.0,
            _ => 0.0,
        };

        // The shared prefix is dimmed and the tail is bright, because the tail
        // is the only part that tells two siblings apart.
        let mut x = left + indent;
        match row {
            Row::Family { key, members } => {
                caret(painter, pos2(x + 4.0, y), lib.family_expanded(row), DIM);
                x += 14.0;
                let galley =
                    painter.layout_no_wrap(format!("{key}-*"), FontId::proportional(12.5), TEXT);
                painter.galley(pos2(x, y - galley.size().y * 0.5), galley, TEXT);
                painter.text(
                    pos2(x + 200.0, y),
                    Align2::LEFT_CENTER,
                    format!("{} files", members.len()),
                    FontId::monospace(11.0),
                    FAINT,
                );
            }
            Row::One(_) => {
                let name = &lib.assets[asset].name;
                let family = lib.assets[asset].family.clone();
                let (head, tail) = match &family {
                    Some(key) if lib.stack_variants => {
                        let tail = library::family_tail(name, key);
                        (
                            name[..name.len() - tail.len()].to_string(),
                            tail.to_string(),
                        )
                    }
                    _ => (String::new(), name.clone()),
                };
                if !head.is_empty() {
                    let galley =
                        painter.layout_no_wrap(head.clone(), FontId::proportional(12.5), FAINT);
                    let width = galley.size().x;
                    painter.galley(pos2(x, y - galley.size().y * 0.5), galley, FAINT);
                    x += width;
                }
                let galley = painter.layout_no_wrap(tail, FontId::proportional(12.5), TEXT);
                painter.galley(pos2(x, y - galley.size().y * 0.5), galley, TEXT);
            }
        }

        let (extent, colour) = match extent_of(lib, row) {
            Some(text) if text == "varies" => (text, WARN),
            Some(text) => (text, DIM),
            None => match &lib.assets[asset].load {
                Load::Failed(_) => ("--".into(), BAD),
                _ => ("\u{2026}".into(), FAINT),
            },
        };
        painter.text(
            pos2(left + columns[1], y),
            Align2::LEFT_CENTER,
            extent,
            FontId::monospace(11.0),
            colour,
        );

        let stats = lib.assets[asset].stats();
        painter.text(
            pos2(left + columns[2] + 60.0, y),
            Align2::RIGHT_CENTER,
            stats.map(|s| thousands(s.voxels)).unwrap_or_default(),
            FontId::monospace(11.0),
            DIM,
        );
        painter.text(
            pos2(left + columns[3] + 40.0, y),
            Align2::RIGHT_CENTER,
            stats.map(|s| s.models.to_string()).unwrap_or_default(),
            FontId::monospace(11.0),
            DIM,
        );
        painter.text(
            pos2(left + columns[4], y),
            Align2::LEFT_CENTER,
            lib.relative(asset),
            FontId::monospace(11.0),
            FAINT,
        );
    }

    fn empty(&mut self, ui: &mut egui::Ui, lib: &Library) {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            let message = if lib.assets.is_empty() {
                "Nothing found under this root."
            } else if lib.facets.any() || !lib.find.is_empty() {
                "No asset matches these filters."
            } else {
                "This folder holds no .vox files. Open one of its children."
            };
            ui.label(egui::RichText::new(message).size(13.0).color(DIM));
        });
    }

    // ---- inspector ------------------------------------------------------

    fn inspector(&mut self, ui: &mut egui::Ui, lib: &mut Library) {
        egui::Panel::right("inspector")
            .exact_size(INSPECTOR_WIDTH)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(16))
            .show(ui, |ui| {
                let Some(asset) = lib.focused() else {
                    ui.add_space(40.0);
                    ui.label(
                        egui::RichText::new("Select an asset to inspect it.")
                            .size(12.5)
                            .color(FAINT),
                    );
                    return;
                };

                let path = lib.assets[asset].path.clone();
                let preview = ui.available_width();
                let (rect, _) =
                    ui.allocate_exact_size(vec2(preview, preview * 0.62), Sense::hover());
                ui.painter().rect_filled(rect, 9.0, BAR);
                ui.painter()
                    .rect_stroke(rect, 9.0, Stroke::new(1.0, LINE), StrokeKind::Inside);
                match self.thumbs.get(&path) {
                    Some(texture) => {
                        let side = rect.height() - 16.0;
                        let box_ = Rect::from_center_size(rect.center(), Vec2::splat(side));
                        ui.painter().image(
                            texture.id(),
                            box_,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => placeholder(ui.painter(), rect.center(), 18.0, EDGE),
                }

                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(&lib.assets[asset].name)
                        .size(15.0)
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    egui::RichText::new(lib.relative(asset))
                        .monospace()
                        .size(10.0)
                        .color(FAINT),
                );

                ui.add_space(12.0);
                match &lib.assets[asset].load {
                    Load::Ready(stats) => {
                        stat(
                            ui,
                            "Extent",
                            &format!(
                                "{} \u{d7} {} \u{d7} {}",
                                stats.dims[0], stats.dims[1], stats.dims[2]
                            ),
                        );
                        stat(ui, "Voxels", &thousands(stats.voxels));
                        stat(ui, "Models", &stats.models.to_string());
                        stat(ui, "Instances", &stats.instances.to_string());
                        stat(
                            ui,
                            "Palette",
                            if stats.palette_from_file {
                                "from file"
                            } else {
                                "MagicaVoxel default"
                            },
                        );
                        stat(ui, "On disk", &bytes(stats.bytes));

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            section(ui, "PALETTE");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new("first 32 of 256")
                                            .monospace()
                                            .size(10.0)
                                            .color(FAINT),
                                    );
                                },
                            );
                        });
                        ui.add_space(4.0);
                        swatches(ui, &stats.palette_head);
                    }
                    Load::Pending => {
                        ui.label(
                            egui::RichText::new("reading\u{2026}")
                                .size(12.0)
                                .color(FAINT),
                        );
                    }
                    Load::Failed(why) => {
                        ui.label(egui::RichText::new("Would not parse").size(13.0).color(BAD));
                        ui.label(egui::RichText::new(why).size(11.5).color(DIM));
                    }
                }

                let family = lib.family_of(asset);
                if family.len() > 1 {
                    ui.add_space(14.0);
                    section(ui, "SAME FAMILY");
                    ui.add_space(4.0);
                    let key = lib.assets[asset].family.clone().unwrap_or_default();
                    let mut jump = None;
                    ui.horizontal_wrapped(|ui| {
                        for sibling in &family {
                            let tail = library::family_tail(&lib.assets[*sibling].name, &key);
                            let here = *sibling == asset;
                            if ui.selectable_label(here, tail).clicked() {
                                jump = Some(*sibling);
                            }
                        }
                    });
                    if let Some(sibling) = jump {
                        lib.select_only(sibling);
                    }
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let targets: Vec<usize> = if lib.selection.is_empty() {
                            vec![asset]
                        } else {
                            lib.selection.clone()
                        };
                        ui.menu_button(format!("+ Collection ({})", targets.len()), |ui| {
                            let mut chosen = None;
                            for (i, collection) in lib.collections.iter().enumerate() {
                                if ui.button(&collection.name).clicked() {
                                    chosen = Some(i);
                                }
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.new_collection)
                                        .desired_width(120.0)
                                        .hint_text("new collection"),
                                );
                                if ui.button("Create").clicked() && !self.new_collection.is_empty()
                                {
                                    let name = std::mem::take(&mut self.new_collection);
                                    chosen = Some(lib.new_collection(name));
                                }
                            });
                            if let Some(i) = chosen {
                                lib.add_to_collection(i, &targets);
                                ui.close();
                            }
                        });
                        if ui
                            .button("Save PNG")
                            .on_hover_text("Save a PNG next to each selected model")
                            .clicked()
                        {
                            self.actions.push(Action::Screenshot(targets));
                        }
                    });
                    ui.add_space(6.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::new("Open in viewer"),
                        )
                        .clicked()
                    {
                        self.actions.push(Action::Open(asset));
                    }
                });
            });
    }

    // ---- viewer chrome --------------------------------------------------

    fn viewer(&mut self, ui: &mut egui::Ui, lib: &mut Library, chrome: &ViewerChrome) {
        let ctx = &ui.ctx().clone();
        // Top right: where you are, and the way back.
        egui::Area::new(egui::Id::new("viewer-top"))
            .anchor(egui::Align2::RIGHT_TOP, vec2(-12.0, 12.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // `relative` is the folder, which is empty for a file
                    // sitting directly in the browsed root, so the name does
                    // the work and the folder is a prefix when there is one.
                    let folder = chrome.current.map(|a| lib.relative(a)).unwrap_or_default();
                    let where_it_is = if folder.is_empty() {
                        chrome.name.to_owned()
                    } else {
                        format!("{folder}/{}", chrome.name)
                    };
                    ui.label(
                        egui::RichText::new(where_it_is)
                            .monospace()
                            .size(11.0)
                            .color(DIM),
                    );
                    if ui.button("Library  Esc").clicked() {
                        self.actions.push(Action::ToLibrary);
                    }
                });
            });

        // Overlay switches, in the order the keys are laid out.
        egui::Area::new(egui::Id::new("viewer-pills"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-12.0, -(FILMSTRIP + 12.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let pills = [
                        ("Grid  G", chrome.show_grid, Toggle::Grid),
                        ("Box  B", chrome.show_bbox, Toggle::Bbox),
                        ("Axes  A", chrome.show_axes, Toggle::Axes),
                        ("AO  O", chrome.occlusion, Toggle::Occlusion),
                        ("Ortho  5", chrome.orthographic, Toggle::Orthographic),
                    ];
                    for (label, on, toggle) in pills {
                        if ui.selectable_label(on, label).clicked() {
                            self.actions.push(Action::Toggle(toggle));
                        }
                    }
                    let msaa = match chrome.samples {
                        1 => "MSAA off  S".to_owned(),
                        n => format!("MSAA {n}x  S"),
                    };
                    if ui.selectable_label(chrome.samples > 1, msaa).clicked() {
                        self.actions.push(Action::Toggle(Toggle::Msaa));
                    }
                    if ui.button("Background  T").clicked() {
                        self.actions.push(Action::Toggle(Toggle::Background));
                    }
                    if ui.button("Reload  R").clicked() {
                        self.actions.push(Action::Reload);
                    }
                });
            });

        if let Some(error) = chrome.error {
            egui::Area::new(egui::Id::new("viewer-error"))
                .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 16.0))
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(0x23, 0x1A, 0x1A))
                        .stroke(Stroke::new(1.0, Color32::from_rgb(0x5A, 0x2F, 0x2B)))
                        .corner_radius(7.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(error).size(12.0).color(BAD));
                        });
                });
        }

        self.filmstrip(ui, lib, chrome);
    }

    /// The current set, along the bottom, so `[` and `]` have somewhere to
    /// point. It is the same list the library is showing, which is what makes
    /// a collection worth making.
    fn filmstrip(&mut self, ui: &mut egui::Ui, lib: &mut Library, chrome: &ViewerChrome) {
        let order = lib.filtered();
        if order.len() < 2 {
            return;
        }
        let position = chrome
            .current
            .and_then(|a| order.iter().position(|i| *i == a));

        egui::Panel::bottom("filmstrip")
            .resizable(false)
            .show_separator_line(false)
            .exact_size(FILMSTRIP)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(0x10, 0x12, 0x16, 0xE0))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let scope = match lib.scope {
                        Scope::Folder(f) => lib.folders[f].name.clone(),
                        Scope::Collection(c) => lib
                            .collections
                            .get(c)
                            .map(|c| c.name.clone())
                            .unwrap_or_default(),
                    };
                    ui.label(egui::RichText::new(scope).size(12.0).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("[  ]")
                                .monospace()
                                .size(11.0)
                                .color(FAINT),
                        );
                        let counter = match position {
                            Some(i) => format!("{} / {}", i + 1, order.len()),
                            None => format!("\u{2013} / {}", order.len()),
                        };
                        ui.label(
                            egui::RichText::new(counter)
                                .monospace()
                                .size(11.0)
                                .color(DIM),
                        );
                    });
                });

                ui.add_space(4.0);
                let side = 62.0f32;
                egui::ScrollArea::horizontal()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for asset in &order {
                                let (rect, response) =
                                    ui.allocate_exact_size(vec2(side, side), Sense::click());
                                let here = Some(*asset) == chrome.current;
                                let painter = ui.painter();
                                painter.rect_filled(
                                    rect,
                                    6.0,
                                    if response.hovered() { CELL_HOVER } else { CELL },
                                );
                                painter.rect_stroke(
                                    rect,
                                    6.0,
                                    if here {
                                        Stroke::new(1.5, ACCENT)
                                    } else {
                                        Stroke::new(1.0, LINE)
                                    },
                                    StrokeKind::Inside,
                                );
                                let path = lib.assets[*asset].path.clone();
                                if let Some(texture) = self.thumbs.get(&path) {
                                    painter.image(
                                        texture.id(),
                                        rect.shrink(5.0),
                                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                        Color32::WHITE,
                                    );
                                }
                                if response.clicked() {
                                    self.actions.push(Action::Open(*asset));
                                }
                                if here {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                    });
            });
    }
}

// ---- small pieces -------------------------------------------------------

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = BAR;
    v.faint_bg_color = CELL;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT_FILL;
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x23, 0x27, 0x2F);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x23, 0x27, 0x2F);
    v.widgets.hovered.bg_fill = CELL_HOVER;
    v.widgets.hovered.weak_bg_fill = CELL_HOVER;
    v.widgets.active.bg_fill = ACCENT_FILL;
    v.widgets.active.weak_bg_fill = ACCENT_FILL;
    v
}

/// A fixed strip across the top. It draws its own seam, so egui's separator
/// is switched off and the borders all match.
fn bar(id: &'static str, height: f32, fill: Color32) -> egui::Panel {
    egui::Panel::top(id)
        .resizable(false)
        .show_separator_line(false)
        .exact_size(height)
        .frame(bar_frame(fill))
}

fn bar_frame(fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(14, 0))
        .stroke(Stroke::new(1.0, LINE))
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(10.0)
            .strong()
            .color(Color32::from_rgb(0x6B, 0x72, 0x7D)),
    );
}

fn group_label(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).size(11.0).color(DIM));
}

fn mono(ui: &mut egui::Ui, text: &str, colour: Color32) {
    ui.label(
        egui::RichText::new(text)
            .monospace()
            .size(11.0)
            .color(colour),
    );
}

fn stat(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui(vec2(104.0, 16.0), |ui| {
            ui.label(egui::RichText::new(key).monospace().size(11.5).color(FAINT));
        });
        ui.label(
            egui::RichText::new(value)
                .monospace()
                .size(11.5)
                .color(Color32::from_rgb(0xDD, 0xE2, 0xE8)),
        );
    });
}

/// One filter checkbox with its live count. Returns true if it was clicked.
fn facet_row(ui: &mut egui::Ui, label: &str, count: usize, on: &mut bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.checkbox(on, "").changed() {
            changed = true;
        }
        if ui
            .add(
                egui::Label::new(egui::RichText::new(label).size(12.0).color(TEXT))
                    .sense(Sense::click()),
            )
            .clicked()
        {
            *on = !*on;
            changed = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(thousands(count))
                    .monospace()
                    .size(11.0)
                    .color(FAINT),
            );
        });
    });
    changed
}

/// A disclosure triangle, pointing right when closed and down when open.
///
/// Painted rather than typed. egui's default font carries no geometric
/// shapes, so `\u{25b8}` and its neighbours come out as hollow boxes.
fn caret(painter: &egui::Painter, centre: egui::Pos2, open: bool, colour: Color32) {
    let r = 3.6;
    let points = if open {
        vec![
            pos2(centre.x - r, centre.y - r * 0.7),
            pos2(centre.x + r, centre.y - r * 0.7),
            pos2(centre.x, centre.y + r * 0.8),
        ]
    } else {
        vec![
            pos2(centre.x - r * 0.7, centre.y - r),
            pos2(centre.x + r * 0.8, centre.y),
            pos2(centre.x - r * 0.7, centre.y + r),
        ]
    };
    painter.add(egui::Shape::convex_polygon(points, colour, Stroke::NONE));
}

/// The ring that stands in for a thumbnail that has not been drawn yet.
fn placeholder(painter: &egui::Painter, centre: egui::Pos2, radius: f32, colour: Color32) {
    painter.circle_stroke(centre, radius, Stroke::new(1.5, colour));
}

/// The marker for a file that would not parse.
fn warning(painter: &egui::Painter, centre: egui::Pos2, size: f32, colour: Color32) {
    let h = size * 0.5;
    painter.add(egui::Shape::closed_line(
        vec![
            pos2(centre.x, centre.y - h),
            pos2(centre.x + h, centre.y + h * 0.75),
            pos2(centre.x - h, centre.y + h * 0.75),
        ],
        Stroke::new(1.5, colour),
    ));
    painter.text(
        centre + vec2(0.0, 2.0),
        Align2::CENTER_CENTER,
        "!",
        FontId::proportional(size * 0.55),
        colour,
    );
}

fn swatches(ui: &mut egui::Ui, colours: &[[u8; 3]]) {
    let width = ui.available_width();
    let per_row = 16;
    let side = ((width - 15.0 * 2.0) / per_row as f32).floor().max(4.0);
    let rows = colours.len().div_ceil(per_row);
    let (rect, _) = ui.allocate_exact_size(vec2(width, rows as f32 * (side + 2.0)), Sense::hover());
    for (i, colour) in colours.iter().enumerate() {
        let x = rect.left() + (i % per_row) as f32 * (side + 2.0);
        let y = rect.top() + (i / per_row) as f32 * (side + 2.0);
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(x, y), Vec2::splat(side)),
            2.0,
            Color32::from_rgb(colour[0], colour[1], colour[2]),
        );
    }
}

fn badge(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    align: Align2,
    text: &str,
    fill: Color32,
    colour: Color32,
) {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::monospace(10.0), colour);
    let size = galley.size() + vec2(12.0, 5.0);
    let rect = align.anchor_size(anchor, size);
    painter.rect_filled(rect, 9.0, fill);
    painter.rect_stroke(rect, 9.0, Stroke::new(1.0, EDGE), StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() * 0.5, galley, colour);
}

fn wrapped(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    colour: Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple(text.to_owned(), FontId::proportional(size), colour, width);
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(job)
}

/// The extent column. A stack whose members disagree says so rather than
/// picking one of them: `crate-0` is 11 x 11 x 10 and `crate-2` is 6 x 6 x 5,
/// and hiding that behind one number would be a lie.
fn extent_of(lib: &Library, row: &Row) -> Option<String> {
    let members: Vec<usize> = match row {
        Row::One(i) => vec![*i],
        Row::Family { members, .. } => members.clone(),
    };
    let first = lib.assets[*members.first()?].stats()?.dims;
    for member in &members[1..] {
        match lib.assets[*member].stats() {
            Some(stats) if stats.dims == first => {}
            Some(_) => return Some("varies".into()),
            None => return None,
        }
    }
    Some(format!(
        "{} \u{d7} {} \u{d7} {}",
        first[0], first[1], first[2]
    ))
}

/// Where to scroll so `line` sits a third of the way down, which reads better
/// than dead centre when you are arrowing forward through a folder.
fn offset_for(line: usize, line_height: f32, lines: usize, viewport: f32) -> f32 {
    let total = lines as f32 * line_height;
    let wanted = line as f32 * line_height - viewport / 3.0;
    wanted.clamp(0.0, (total - viewport).max(0.0))
}

fn bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Paths of the assets a batch action applies to.
pub fn paths_of(lib: &Library, assets: &[usize]) -> Vec<PathBuf> {
    assets
        .iter()
        .filter_map(|i| lib.assets.get(*i).map(|a| a.path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stack_whose_members_disagree_reports_that_rather_than_a_number() {
        let root = PathBuf::from("root");
        let mut lib = Library::new(
            root.clone(),
            vec![root.join("a/crate-0.vox"), root.join("a/crate-1.vox")],
        );
        lib.assets[0].load = ready([11, 11, 10]);
        lib.assets[1].load = ready([6, 6, 5]);

        let rows = lib.rows().to_vec();
        assert_eq!(extent_of(&lib, &rows[0]).as_deref(), Some("varies"));

        lib.assets[1].load = ready([11, 11, 10]);
        assert_eq!(
            extent_of(&lib, &rows[0]).as_deref(),
            Some("11 \u{d7} 11 \u{d7} 10")
        );
    }

    #[test]
    fn an_unread_member_leaves_the_extent_blank_instead_of_guessing() {
        let root = PathBuf::from("root");
        let mut lib = Library::new(
            root.clone(),
            vec![root.join("a/crate-0.vox"), root.join("a/crate-1.vox")],
        );
        lib.assets[0].load = ready([11, 11, 10]);
        let rows = lib.rows().to_vec();
        assert_eq!(extent_of(&lib, &rows[0]), None);
    }

    #[test]
    fn scrolling_to_a_row_stays_inside_the_scroll_area() {
        // Near the top there is nothing above to show, and at the end the
        // last screenful must not scroll past its own bottom.
        assert_eq!(offset_for(0, 100.0, 50, 600.0), 0.0);
        assert_eq!(offset_for(1, 100.0, 50, 600.0), 0.0);
        assert_eq!(offset_for(49, 100.0, 50, 600.0), 4400.0);
        assert_eq!(offset_for(5, 100.0, 3, 600.0), 0.0);
    }

    #[test]
    fn byte_counts_read_the_way_a_file_manager_shows_them() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KB");
        assert_eq!(bytes(3 * 1024 * 1024), "3.0 MB");
    }

    fn ready(dims: [i32; 3]) -> Load {
        Load::Ready(library::AssetStats {
            dims,
            voxels: 1,
            models: 1,
            instances: 1,
            palette_from_file: true,
            palette_head: Vec::new(),
            bytes: 0,
            modified: None,
        })
    }
}
