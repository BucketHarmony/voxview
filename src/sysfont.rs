//! Borrowing fonts from the operating system, for text the bundled one
//! cannot draw.
//!
//! egui ships Ubuntu-Light, which covers Latin, Greek and Cyrillic and stops
//! there. A file called `橡木桶.vox` therefore renders as a row of hollow
//! boxes, with no error anywhere -- the worst kind of bug, because the program
//! is confident and wrong. Anyone browsing an asset library named in Chinese,
//! Japanese, Korean, Arabic, Hebrew, Thai or an Indic script sees that.
//!
//! Bundling a font that covers all of it would add tens of megabytes to the
//! binary for something most people never need, so instead this finds fonts
//! already on the machine. Every desktop that can show those file names in its
//! own file manager has them installed; the job is to locate them.
//!
//! Two things keep the cost down. Nothing is loaded until a name actually
//! needs it, so an ASCII-named library pays nothing; and what is loaded is
//! chosen by [`Script`], so a folder of Hebrew names does not drag in twenty
//! megabytes of Chinese.

use std::path::{Path, PathBuf};

/// A font file read off the system, ready to hand to egui.
pub struct SystemFont {
    /// The file it came from, for the line printed on stdout.
    pub name: String,
    pub bytes: Vec<u8>,
    /// Which face to use inside a `.ttc` collection; 0 for a plain `.ttf`.
    pub index: u32,
}

/// A group of scripts that tends to travel together in one font file.
///
/// Not a Unicode classification -- a shopping list. The question each variant
/// answers is "which file on this machine do I need to open", so Chinese,
/// Japanese and Korean are separate despite sharing Han characters (the
/// Japanese font is the one with kana in it), and every Indic script is one
/// entry because Windows and macOS each ship them in a single file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Script {
    /// Anything unrecognised: reach for the widest font available.
    Broad,
    /// Arabic, Hebrew, Syriac, Thaana.
    MiddleEast,
    /// Devanagari through Sinhala.
    Indic,
    /// Thai, Lao, Khmer, Myanmar.
    SouthEastAsian,
    /// Han: Chinese, and the kanji in the other two.
    Han,
    /// Hiragana and katakana.
    Kana,
    /// Hangul.
    Hangul,
}

impl Script {
    const ALL: [Script; 7] = [
        Script::Broad,
        Script::MiddleEast,
        Script::Indic,
        Script::SouthEastAsian,
        Script::Han,
        Script::Kana,
        Script::Hangul,
    ];

    fn bit(self) -> u8 {
        1 << Script::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Which of the [`Script`] groups some text turned out to need.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Scripts(u8);

impl Scripts {
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, script: Script) -> bool {
        self.0 & script.bit() != 0
    }

    pub fn insert(&mut self, script: Script) {
        self.0 |= script.bit();
    }

    /// Everything in either set.
    pub fn union(self, other: Scripts) -> Scripts {
        Scripts(self.0 | other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Script> {
        Script::ALL.into_iter().filter(move |s| self.contains(*s))
    }
}

/// The scripts in `text` that the bundled font cannot draw.
///
/// The ranges are coarse on purpose. This runs over every file name in a scan
/// and its answer only decides which font files to open, so naming a script
/// that was not really needed costs one wasted read, while missing one costs a
/// row of hollow boxes. The line is drawn generously.
pub fn scripts_beyond(text: &str) -> Scripts {
    let mut out = Scripts::default();
    for c in text.chars() {
        if let Some(script) = script_of(c) {
            out.insert(script);
        }
    }
    out
}

/// The script group `c` needs a fallback for, or `None` when Ubuntu-Light and
/// the bundled emoji font already have it.
pub fn script_of(c: char) -> Option<Script> {
    match c as u32 {
        // Latin, Latin Extended-A and -B, IPA, spacing modifiers.
        0x0000..=0x02FF
        // Greek and Cyrillic.
        | 0x0370..=0x04FF
        // Latin Extended Additional: the accented Vietnamese range.
        | 0x1E00..=0x1EFF
        // Punctuation, currency, letterlike symbols, arrows, maths.
        | 0x2000..=0x22FF
        // Emoji, which egui bundles Noto Emoji for.
        | 0x1F300..=0x1FAFF => None,

        // Hebrew, Arabic, Syriac, Thaana, and the Arabic supplements.
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF => Some(Script::MiddleEast),
        // Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu,
        // Kannada, Malayalam, Sinhala.
        0x0900..=0x0DFF => Some(Script::Indic),
        // Thai, Lao, Tibetan, Myanmar, Khmer.
        0x0E00..=0x0FFF | 0x1000..=0x109F | 0x1780..=0x17FF => Some(Script::SouthEastAsian),
        // Kana, and the halfwidth katakana at the top of the BMP.
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Some(Script::Kana),
        // Hangul jamo, compatibility jamo, and the syllable block.
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF => {
            Some(Script::Hangul)
        }
        // CJK radicals, symbols and punctuation, and the two big Han blocks,
        // plus the supplementary plane extensions.
        0x2E80..=0x303F | 0x3200..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF => {
            Some(Script::Han)
        }

        _ => Some(Script::Broad),
    }
}

/// At most this many font files, however many scripts turn up.
const MAX_FONTS: usize = 6;

/// Stop loading once the fallbacks have cost this much memory.
///
/// A full CJK font is 15-25 MB on its own, so this leaves room for one of
/// those plus everything else.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Find fonts on this machine covering the scripts in `wanted`.
///
/// An empty result means nothing suitable was found, which is a real
/// possibility on a stripped-down container and is not an error: the caller
/// carries on with the bundled font and says so.
pub fn fallbacks(wanted: Scripts) -> Vec<SystemFont> {
    let mut found: Vec<SystemFont> = Vec::new();
    let mut opened: Vec<PathBuf> = Vec::new();
    let mut spent = 0u64;

    for script in wanted.iter() {
        if found.len() >= MAX_FONTS {
            break;
        }
        for (path, index) in candidates(script) {
            // One file often answers for several scripts -- Segoe UI covers
            // Arabic, Hebrew and Thai between them -- so a script whose font
            // is already loaded needs nothing further.
            if opened.contains(&path) {
                break;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() == 0 || spent + meta.len() > MAX_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            spent += bytes.len() as u64;
            found.push(SystemFont {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
                bytes,
                index,
            });
            opened.push(path);
            break;
        }
    }
    found
}

/// Font files worth trying for `script`, best first, as `(path, face index)`.
///
/// These are literal paths rather than a fontconfig query or a platform font
/// API. `fc-match` would be more thorough on Linux and would also mean
/// spawning a process from inside a frame; the platform APIs would mean a
/// dependency and a code path per operating system. This covers the standard
/// install of every desktop that ships these fonts, and where it comes up
/// empty the result is "names appear as boxes, and voxview says so", not a
/// failure to start.
fn candidates(script: Script) -> Vec<(PathBuf, u32)> {
    if cfg!(target_os = "windows") {
        return windows_candidates(script);
    }
    if cfg!(target_os = "macos") {
        return macos_candidates(script);
    }
    unix_candidates(script)
}

fn windows_candidates(script: Script) -> Vec<(PathBuf, u32)> {
    let fonts = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    // Segoe UI is on every Windows since 7 and carries Arabic, Hebrew,
    // Armenian, Georgian and Thai in under a megabyte, so it is the first
    // answer wherever it is an answer at all. Arial Unicode MS, when Office
    // put it there, covers nearly everything and is the backstop.
    let names: &[&str] = match script {
        Script::Broad => &["arialuni.ttf", "segoeui.ttf"],
        Script::MiddleEast => &["segoeui.ttf", "arialuni.ttf"],
        // Nirmala UI is Windows' single file for every Indic script. It ships
        // as a collection on Windows 10 and later and as a plain TrueType
        // before that.
        Script::Indic => &["Nirmala.ttc", "Nirmala.ttf", "arialuni.ttf"],
        // Leelawadee UI is Thai and Lao; Segoe UI has Thai as well. The name
        // ending in "b" is the bold face, so it comes second.
        Script::SouthEastAsian => &["LeelawUI.ttf", "LeelaUIb.ttf", "segoeui.ttf"],
        Script::Han => &["msyh.ttc", "msjh.ttc", "simsun.ttc", "arialuni.ttf"],
        // Meiryo and MS Gothic have the kana that the Chinese fonts do not.
        Script::Kana => &["meiryo.ttc", "msgothic.ttc", "YuGothM.ttc", "arialuni.ttf"],
        Script::Hangul => &["malgun.ttf", "gulim.ttc", "batang.ttc", "arialuni.ttf"],
    };
    names.iter().map(|n| (fonts.join(n), 0)).collect()
}

/// Best effort, and the least verifiable of the three lists: Apple moves these
/// files between releases, and the Japanese faces live under names written in
/// Japanese. Arial Unicode is listed first everywhere it helps because it is
/// one file that covers almost all of this.
fn macos_candidates(script: Script) -> Vec<(PathBuf, u32)> {
    let unicode = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/Library/Fonts/Arial Unicode.ttf",
    ];
    let paths: Vec<&str> = match script {
        Script::Broad | Script::MiddleEast => unicode
            .iter()
            .copied()
            .chain(["/System/Library/Fonts/Supplemental/GeezaPro.ttc"])
            .collect(),
        Script::Indic => unicode
            .iter()
            .copied()
            .chain(["/System/Library/Fonts/Supplemental/DevanagariSangamMN.ttc"])
            .collect(),
        Script::SouthEastAsian => unicode
            .iter()
            .copied()
            .chain(["/System/Library/Fonts/Supplemental/Thonburi.ttc"])
            .collect(),
        Script::Han => ["/System/Library/Fonts/PingFang.ttc"]
            .iter()
            .copied()
            .chain(unicode)
            .collect(),
        Script::Kana => ["/System/Library/Fonts/Hiragino Sans GB.ttc"]
            .iter()
            .copied()
            .chain(unicode)
            .collect(),
        Script::Hangul => ["/System/Library/Fonts/AppleSDGothicNeo.ttc"]
            .iter()
            .copied()
            .chain(unicode)
            .collect(),
    };
    paths.into_iter().map(|p| (PathBuf::from(p), 0)).collect()
}

/// Linux, the BSDs, and anything else with a `/usr/share/fonts`.
///
/// The Noto families are named consistently enough to find by name, which is
/// what saves this from being a list of every distribution's idea of where
/// fonts live. DejaVu is the one file that is nearly always present.
fn unix_candidates(script: Script) -> Vec<(PathBuf, u32)> {
    let dejavu: Vec<(PathBuf, u32)> = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/local/share/fonts/dejavu/DejaVuSans.ttf",
    ]
    .iter()
    .map(|p| (PathBuf::from(p), 0))
    .collect();

    // DejaVu already has Hebrew, Armenian and Georgian, and enough Arabic to
    // beat a hollow box, so it leads for those two.
    let noto = match script {
        Script::Broad | Script::MiddleEast => {
            let mut out = dejavu.clone();
            out.extend(find_font("arabic"));
            return out;
        }
        Script::Indic => find_font("devanagari"),
        Script::SouthEastAsian => find_font("thai"),
        Script::Han | Script::Kana | Script::Hangul => {
            let mut out = find_font("cjk");
            out.extend(find_font("zenhei"));
            out
        }
    };
    let mut out = noto;
    out.extend(dejavu);
    out
}

/// Font files in the usual system directories whose names contain `needle`.
///
/// One level deep in a handful of directories, matched on the file name. A
/// full walk of the font tree would find more and cost more than it is worth
/// for something that runs once.
fn find_font(needle: &str) -> Vec<(PathBuf, u32)> {
    const DIRS: [&str; 10] = [
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/noto",
        "/usr/share/fonts/noto-cjk",
        "/usr/share/fonts/google-noto",
        "/usr/share/fonts/google-noto-cjk",
        "/usr/share/fonts/truetype/wqy",
        "/usr/share/fonts/wqy-zenhei",
        "/usr/share/fonts/truetype",
        "/usr/share/fonts",
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in DIRS {
        let Ok(entries) = std::fs::read_dir(Path::new(dir)) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let lower = name.to_ascii_lowercase();
            let shape =
                lower.ends_with(".ttc") || lower.ends_with(".otf") || lower.ends_with(".ttf");
            // "Regular" keeps this from picking the Bold or Black face that
            // sits beside it; the WenQuanYi fonts have no weight in the name.
            let regular = lower.contains("regular") || lower.contains("zenhei");
            if shape && regular && lower.contains(needle) {
                out.push(path);
            }
        }
    }
    // read_dir order is whatever the filesystem feels like, and this decides
    // which font people see, so pin it.
    out.sort();
    out.dedup();
    out.into_iter().map(|p| (p, 0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_european_names_need_no_fallback() {
        for name in [
            "barrel_wood_water",
            "chêne-ébène",
            "Ünterstützung",
            "здание",
            "Ελληνικά",
            "tiếng-việt",
            "half \u{2014} dash \u{2264} 3",
        ] {
            assert!(
                scripts_beyond(name).is_empty(),
                "{name} should not need a fallback"
            );
        }
    }

    #[test]
    fn each_script_asks_for_its_own_font() {
        let cases = [
            ("橡木桶", Script::Han),
            ("たる", Script::Kana),
            ("통나무", Script::Hangul),
            ("\u{05D7}\u{05D1}\u{05D9}\u{05EA}", Script::MiddleEast),
            (
                "\u{0628}\u{0631}\u{0645}\u{064A}\u{0644}",
                Script::MiddleEast,
            ),
            ("\u{0E16}\u{0E31}\u{0E07}", Script::SouthEastAsian),
            ("\u{092C}\u{0948}\u{0930}\u{0932}", Script::Indic),
        ];
        for (name, want) in cases {
            let scripts = scripts_beyond(name);
            assert!(
                scripts.contains(want),
                "{name} should have asked for {want:?}, got {scripts:?}"
            );
        }
    }

    #[test]
    fn a_japanese_name_asks_for_kana_as_well_as_han() {
        // The one case the Chinese fonts get wrong: they have the kanji and
        // not the kana, so both have to be requested.
        let scripts = scripts_beyond("樽たる_01.vox");
        assert!(scripts.contains(Script::Han));
        assert!(scripts.contains(Script::Kana));
    }

    #[test]
    fn a_mixed_name_asks_only_for_the_part_that_needs_it() {
        let scripts = scripts_beyond("barrel_橡木桶_01.vox");
        assert!(scripts.contains(Script::Han));
        assert!(!scripts.contains(Script::Indic));
        assert!(scripts_beyond("barrel_oak_01.vox").is_empty());
    }

    #[test]
    fn the_script_set_holds_every_script_at_once() {
        let mut set = Scripts::default();
        assert!(set.is_empty());
        for script in Script::ALL {
            set.insert(script);
        }
        assert_eq!(set.iter().count(), Script::ALL.len());
        assert_eq!(set.union(Scripts::default()), set);
    }

    #[test]
    fn every_script_has_somewhere_to_look_on_this_platform() {
        for script in Script::ALL {
            let list = candidates(script);
            assert!(!list.is_empty(), "no candidates for {script:?}");
            for (path, _) in &list {
                assert!(path.is_absolute(), "{} is not absolute", path.display());
            }
        }
    }

    #[test]
    fn loading_fallbacks_never_panics_and_respects_the_budget() {
        // On a desktop this finds fonts; in a bare container it finds none.
        // Both are correct; neither may panic or run away with memory.
        let mut wanted = Scripts::default();
        for script in Script::ALL {
            wanted.insert(script);
        }
        let fonts = fallbacks(wanted);
        assert!(fonts.len() <= MAX_FONTS);
        let total: u64 = fonts.iter().map(|f| f.bytes.len() as u64).sum();
        assert!(total <= MAX_BYTES, "fallback fonts cost {total} bytes");
        for font in &fonts {
            assert!(!font.bytes.is_empty());
        }
    }

    #[test]
    fn nothing_is_loaded_for_a_library_that_does_not_need_it() {
        assert!(fallbacks(Scripts::default()).is_empty());
    }
}
