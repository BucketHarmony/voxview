//! The file menu: a scrolling, filterable list of the `.vox` files on offer.
//!
//! `[` and `]` page blindly through a directory, which is fine for a handful
//! of files and useless for the 1,886 in a single Veloren npc folder. The menu
//! shows the listing, marks where you are in it, and lets you type to narrow
//! it down.
//!
//! State only: the drawing is [`crate::hud::layout`] on the lines this
//! produces. The one thing that has to be kept honest is that clicking a row
//! selects the row that was drawn, so the hit box is recorded during layout
//! from the same [`crate::hud::Metrics`] the renderer uses.

use crate::hud::{self, Anchor, HudLine};

/// Longest file name shown before it is elided. The panel is sized from this
/// rather than from whatever happens to be on screen, so it does not jitter
/// as the list scrolls.
const NAME_WIDTH: usize = 34;

/// Rows reserved for the title, the filter line, the footer and the two
/// "more above/below" markers. Fixed rather than computed, because the
/// visible-row count feeds back into whether the markers are needed at all.
const CHROME_ROWS: usize = 5;

/// What lies under a point on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A file row, carrying its index into the path list.
    Row(usize),
    /// Inside the panel but not on a row: the title, filter or footer.
    Panel,
}

/// The pixel rectangle the menu last occupied.
#[derive(Clone, Copy, Debug)]
struct HitBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    first_row_top: f32,
    line_h: f32,
    visible: usize,
}

#[derive(Default)]
pub struct Menu {
    open: bool,
    /// Index into `filtered`, not into the path list.
    cursor: usize,
    /// First row of `filtered` on screen.
    scroll: usize,
    filter: String,
    /// Indices into the path list that match `filter`, in listing order.
    filtered: Vec<usize>,
    hit: Option<HitBox>,
}

impl Menu {
    pub fn new() -> Menu {
        Menu::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open on `current`, or close if already open.
    pub fn toggle(&mut self, names: &[String], current: usize) {
        if self.open {
            self.close();
        } else {
            self.open = true;
            // Open on the whole directory. A filter left over from the last
            // time the menu was up would silently hide files, and the only
            // clue would be one line of chrome.
            self.filter.clear();
            self.refilter(names);
            self.follow(current);
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.hit = None;
    }

    /// Put the cursor on `current` if the filter admits it.
    ///
    /// Called whenever the file changes by some other route, so paging with
    /// `[` and `]` keeps the open menu in step.
    pub fn follow(&mut self, current: usize) {
        if let Some(at) = self.filtered.iter().position(|&i| i == current) {
            self.cursor = at;
        }
    }

    /// The path index under the cursor.
    pub fn selection(&self) -> Option<usize> {
        self.filtered.get(self.cursor).copied()
    }

    /// Move the cursor `step` rows and report the file to show.
    ///
    /// Returns `None` only when the filter matches nothing; otherwise the
    /// cursor always lands somewhere, so moving is always a live preview.
    pub fn step(&mut self, step: isize) -> Option<usize> {
        if self.filtered.is_empty() {
            return None;
        }
        let len = self.filtered.len() as isize;
        // Clamped rather than wrapped: a page down near the end should stop at
        // the end, not jump back to the top.
        self.cursor = (self.cursor as isize + step).clamp(0, len - 1) as usize;
        self.selection()
    }

    /// How many file rows a page-sized jump covers.
    pub fn page(&self) -> isize {
        self.hit.map_or(10, |h| h.visible.max(1) as isize)
    }

    /// Scroll the view without moving the cursor, for the mouse wheel.
    pub fn scroll_by(&mut self, rows: isize) {
        let max = self.max_scroll();
        self.scroll = (self.scroll as isize + rows).clamp(0, max as isize) as usize;
    }

    fn visible(&self) -> usize {
        self.hit.map_or(1, |h| h.visible)
    }

    fn max_scroll(&self) -> usize {
        self.filtered.len().saturating_sub(self.visible())
    }

    /// Add a character to the filter. Returns true if the list changed.
    pub fn filter_push(&mut self, c: char, names: &[String]) -> bool {
        self.filter.push(c.to_ascii_lowercase());
        self.refilter(names);
        true
    }

    /// Remove the last character. Returns true if there was one.
    pub fn filter_pop(&mut self, names: &[String]) -> bool {
        if self.filter.pop().is_none() {
            return false;
        }
        self.refilter(names);
        true
    }

    /// Drop the filter entirely. Returns true if there was one.
    pub fn filter_clear(&mut self, names: &[String]) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        self.filter.clear();
        self.refilter(names);
        true
    }

    /// Rebuild the match list, keeping the cursor on the same file if it
    /// survived and parking it on the nearest match if it did not.
    fn refilter(&mut self, names: &[String]) {
        let previous = self.selection();
        self.filtered = if self.filter.is_empty() {
            (0..names.len()).collect()
        } else {
            (0..names.len())
                .filter(|&i| names[i].to_ascii_lowercase().contains(&self.filter))
                .collect()
        };
        self.cursor = match previous {
            Some(p) => self
                .filtered
                .iter()
                .position(|&i| i == p)
                // Narrowing the filter usually drops the current file; landing
                // on the first match is more useful than landing on nothing.
                .unwrap_or(0),
            None => 0,
        };
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// What lies under a point in physical pixels, or `None` if the menu is
    /// closed or the point is outside it.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        let b = self.hit.filter(|_| self.open)?;
        if x < b.x0 || x > b.x1 || y < b.y0 || y > b.y1 {
            return None;
        }
        if y < b.first_row_top {
            return Some(Hit::Panel);
        }
        let row = ((y - b.first_row_top) / b.line_h).floor() as usize;
        if row >= b.visible {
            return Some(Hit::Panel);
        }
        match self.filtered.get(self.scroll + row) {
            Some(&index) => Some(Hit::Row(index)),
            // Past the end of a short list: inside the panel, on nothing.
            None => Some(Hit::Panel),
        }
    }

    /// Build the panel, and record where it landed so [`Menu::hit`] can map
    /// clicks back onto rows.
    ///
    /// `current` is the file actually on screen, which is not always the one
    /// under the cursor: typing a filter moves the cursor without loading.
    pub fn lines(
        &mut self,
        names: &[String],
        title: &str,
        current: usize,
        scale: f32,
        viewport: (f32, f32),
    ) -> Vec<HudLine> {
        let m = hud::metrics(scale);
        let visible = m
            .rows_that_fit(viewport.1)
            .saturating_sub(CHROME_ROWS)
            .max(1)
            .min(names.len().max(1));

        self.cursor = self.cursor.min(self.filtered.len().saturating_sub(1));
        // Keep the cursor on screen, moving the view as little as possible.
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + visible {
            self.scroll = self.cursor + 1 - visible;
        }
        self.scroll = self.scroll.min(self.filtered.len().saturating_sub(visible));

        let mut lines = Vec::with_capacity(visible + CHROME_ROWS);
        lines.push(HudLine::normal(pad(&elide(title), NAME_WIDTH + 2)));
        lines.push(HudLine::dim(pad(
            &format!("find: {}_", self.filter),
            NAME_WIDTH + 2,
        )));

        let above = self.scroll;
        let below = self.filtered.len().saturating_sub(self.scroll + visible);
        lines.push(HudLine::dim(pad(
            &if above > 0 {
                format!("  ^ {above} above")
            } else {
                String::new()
            },
            NAME_WIDTH + 2,
        )));

        let header_rows = lines.len();

        if self.filtered.is_empty() {
            lines.push(HudLine::dim(pad("  no match", NAME_WIDTH + 2)));
        }
        for row in 0..visible {
            let Some(&index) = self.filtered.get(self.scroll + row) else {
                break;
            };
            // `>` marks the file on screen; the highlight bar marks the cursor.
            let mark = if index == current { ">" } else { " " };
            let text = pad(&format!("{mark} {}", elide(&names[index])), NAME_WIDTH + 2);
            lines.push(if self.scroll + row == self.cursor {
                HudLine::selected(text)
            } else {
                HudLine::dim(text)
            });
        }

        lines.push(HudLine::dim(pad(
            &if below > 0 {
                format!("  v {below} below")
            } else {
                String::new()
            },
            NAME_WIDTH + 2,
        )));
        lines.push(HudLine::dim(pad(
            &format!(
                "{}/{}  type to find  [Esc] close",
                if self.filtered.is_empty() {
                    0
                } else {
                    self.cursor + 1
                },
                self.filtered.len()
            ),
            NAME_WIDTH + 2,
        )));

        let size = m.panel_size(&lines);
        let origin = m.origin(Anchor::TopRight, size, viewport);
        self.hit = Some(HitBox {
            x0: origin.0,
            y0: origin.1,
            x1: origin.0 + size.0,
            y1: origin.1 + size.1,
            first_row_top: m.row_top(origin, header_rows),
            line_h: m.line_h,
            visible,
        });
        lines
    }
}

/// Shorten a name that would stretch the panel, keeping the tail visible --
/// Veloren distinguishes `foot_br` from `foot_fr` at the end, not the start.
fn elide(name: &str) -> String {
    let count = name.chars().count();
    if count <= NAME_WIDTH {
        return name.to_string();
    }
    let tail: String = name.chars().skip(count - (NAME_WIDTH - 2)).collect();
    format!("..{tail}")
}

fn pad(text: &str, width: usize) -> String {
    let count = text.chars().count();
    let mut out = String::with_capacity(width);
    out.push_str(text);
    for _ in count..width {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (1280.0, 800.0);

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("file{i:03}.vox")).collect()
    }

    /// Open a menu and lay it out once, which is what gives it a hit box.
    fn opened(names: &[String], current: usize) -> Menu {
        let mut menu = Menu::new();
        menu.toggle(names, current);
        menu.lines(names, "dir", current, 2.0, VIEW);
        menu
    }

    #[test]
    fn opening_lands_on_the_current_file() {
        let names = names(50);
        let menu = opened(&names, 17);
        assert!(menu.is_open());
        assert_eq!(menu.selection(), Some(17));
    }

    #[test]
    fn toggling_closes_it_again() {
        let names = names(4);
        let mut menu = opened(&names, 0);
        menu.toggle(&names, 0);
        assert!(!menu.is_open());
        // A closed menu must not claim clicks.
        assert_eq!(menu.hit(VIEW.0 - 20.0, 100.0), None);
    }

    #[test]
    fn stepping_clamps_at_both_ends() {
        let names = names(5);
        let mut menu = opened(&names, 0);
        assert_eq!(menu.step(-1), Some(0));
        assert_eq!(menu.step(3), Some(3));
        assert_eq!(menu.step(99), Some(4));
        assert_eq!(menu.step(-99), Some(0));
    }

    #[test]
    fn the_cursor_stays_on_screen_when_it_moves() {
        let names = names(500);
        let mut menu = opened(&names, 0);
        menu.step(400);
        menu.lines(&names, "dir", 0, 2.0, VIEW);
        let visible = menu.visible();
        assert!(
            menu.scroll <= menu.cursor && menu.cursor < menu.scroll + visible,
            "cursor {} outside view {}..{}",
            menu.cursor,
            menu.scroll,
            menu.scroll + visible
        );
        // Stepping back to the top has to scroll back with it.
        menu.step(-400);
        menu.lines(&names, "dir", 0, 2.0, VIEW);
        assert_eq!(menu.scroll, 0);
    }

    #[test]
    fn filtering_narrows_the_list_case_insensitively() {
        let names: Vec<String> = ["Head.vox", "jaw.vox", "foot_br.vox", "foot_fr.vox"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut menu = opened(&names, 0);
        for c in "FOOT".chars() {
            menu.filter_push(c, &names);
        }
        assert_eq!(menu.filtered, vec![2, 3]);
        // The cursor was on a file the filter dropped, so it parks on the
        // first match rather than pointing at nothing.
        assert_eq!(menu.selection(), Some(2));

        menu.filter_pop(&names);
        assert_eq!(menu.filtered, vec![2, 3]);
        assert!(menu.filter_clear(&names));
        assert_eq!(menu.filtered.len(), 4);
        // Clearing keeps the file the cursor was on.
        assert_eq!(menu.selection(), Some(2));
        assert!(!menu.filter_clear(&names));
    }

    #[test]
    fn a_filter_that_matches_nothing_is_not_a_crash() {
        let names = names(20);
        let mut menu = opened(&names, 3);
        for c in "zzz".chars() {
            menu.filter_push(c, &names);
        }
        assert!(menu.filtered.is_empty());
        assert_eq!(menu.selection(), None);
        assert_eq!(menu.step(1), None);
        let lines = menu.lines(&names, "dir", 3, 2.0, VIEW);
        assert!(lines.iter().any(|l| l.text.contains("no match")));
        assert!(lines.iter().any(|l| l.text.contains("0/0")));
        assert!(lines.iter().any(|l| l.text.contains("[Esc] close")));
    }

    #[test]
    fn reopening_starts_from_the_whole_directory() {
        let names = names(30);
        let mut menu = opened(&names, 7);
        for c in "file01".chars() {
            menu.filter_push(c, &names);
        }
        assert!(menu.filtered.len() < names.len());
        menu.toggle(&names, 7); // close
        menu.toggle(&names, 7); // and open again
        assert_eq!(menu.filtered.len(), names.len());
        // And back on the file that is actually on screen.
        assert_eq!(menu.selection(), Some(7));
    }

    #[test]
    fn following_tracks_a_file_opened_some_other_way() {
        let names = names(30);
        let mut menu = opened(&names, 0);
        menu.follow(12);
        assert_eq!(menu.selection(), Some(12));
        // A file the filter excludes leaves the cursor alone rather than
        // moving it somewhere arbitrary.
        menu.filter_push('0', &names);
        menu.filter_push('0', &names);
        let before = menu.selection();
        menu.follow(29);
        assert_eq!(menu.selection(), before);
    }

    #[test]
    fn clicking_a_row_selects_the_row_that_was_drawn() {
        let names = names(200);
        let mut menu = opened(&names, 0);
        menu.scroll_by(5);
        let lines = menu.lines(&names, "dir", 0, 2.0, VIEW);
        let b = menu.hit.unwrap();

        // Every visible row, hit in its vertical middle.
        for row in 0..menu.visible() {
            let y = b.first_row_top + (row as f32 + 0.5) * b.line_h;
            let x = (b.x0 + b.x1) / 2.0;
            assert_eq!(menu.hit(x, y), Some(Hit::Row(menu.scroll + row)));
        }
        // The row text agrees with what hit-testing claims is there.
        let first = &lines[lines.len() - menu.visible() - 2];
        assert!(first.text.contains(&names[menu.scroll]));
    }

    #[test]
    fn clicks_outside_the_panel_are_not_the_menus() {
        let names = names(40);
        let menu = opened(&names, 0);
        let b = menu.hit.unwrap();
        assert_eq!(menu.hit(b.x0 - 1.0, b.y0 + 10.0), None);
        assert_eq!(menu.hit(b.x1 + 1.0, b.y0 + 10.0), None);
        assert_eq!(menu.hit(b.x0 + 10.0, b.y0 - 1.0), None);
        assert_eq!(menu.hit(b.x0 + 10.0, b.y1 + 1.0), None);
        // The title and footer are inside the panel but are not files.
        assert_eq!(menu.hit(b.x0 + 10.0, b.y0 + 2.0), Some(Hit::Panel));
        assert_eq!(menu.hit(b.x0 + 10.0, b.y1 - 2.0), Some(Hit::Panel));
    }

    #[test]
    fn scrolling_stops_at_both_ends() {
        let names = names(100);
        let mut menu = opened(&names, 0);
        menu.scroll_by(-10);
        assert_eq!(menu.scroll, 0);
        menu.scroll_by(1000);
        assert_eq!(menu.scroll, 100 - menu.visible());
    }

    #[test]
    fn the_panel_is_a_stable_width_however_long_the_names_are() {
        let long: Vec<String> = vec![
            "a.vox".into(),
            "a_very_long_asset_name_that_would_stretch_the_panel_0123456789.vox".into(),
        ];
        let mut menu = opened(&long, 0);
        let lines = menu.lines(&long, "dir", 0, 2.0, VIEW);
        let widths: Vec<usize> = lines.iter().map(|l| l.text.chars().count()).collect();
        assert!(
            widths.iter().all(|&w| w == NAME_WIDTH + 2),
            "ragged rows: {widths:?}"
        );
        // The long name is elided from the front, keeping its tail.
        assert!(lines.iter().any(|l| l.text.contains("0123456789.vox")));
    }

    #[test]
    fn a_short_window_still_shows_a_row() {
        let names = names(50);
        let mut menu = Menu::new();
        menu.toggle(&names, 0);
        let lines = menu.lines(&names, "dir", 0, 2.0, (1280.0, 40.0));
        assert!(!lines.is_empty());
        assert_eq!(menu.visible(), 1);
    }

    #[test]
    fn an_empty_listing_lays_out_without_panicking() {
        let mut menu = Menu::new();
        menu.toggle(&[], 0);
        let lines = menu.lines(&[], "dir", 0, 2.0, VIEW);
        assert!(lines.iter().any(|l| l.text.contains("no match")));
        assert_eq!(menu.selection(), None);
    }
}
