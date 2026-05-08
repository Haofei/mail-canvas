use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use fontdb::Database;

pub(crate) fn system_font_database() -> Database {
    let mut db = Database::new();
    db.load_system_fonts();
    #[cfg(target_os = "macos")]
    db.load_fonts_dir("/System/Library/Fonts/Supplemental");
    set_generic_font_families(&mut db);
    db
}

pub(crate) fn font_database_from_paths(paths: Vec<PathBuf>) -> Result<Database> {
    let mut db = Database::new();
    for path in paths {
        if !path.is_file() {
            bail!("font path is not a file: {}", path.display());
        }
        db.load_font_source(fontdb::Source::File(path));
    }
    if db.is_empty() {
        bail!("no valid font faces found in supplied font files");
    }
    set_generic_font_families(&mut db);
    Ok(db)
}

pub(crate) fn load_default_emoji_font_if_missing(db: &mut Database) {
    if emoji_font_available(db) {
        return;
    }
    let path = default_emoji_font_path();
    if path.is_file() {
        db.load_font_source(fontdb::Source::File(path));
    }
}

fn default_emoji_font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("fonts")
        .join("NotoColorEmoji.ttf")
}

fn emoji_font_available(db: &Database) -> bool {
    ["Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji"]
        .iter()
        .any(|family| font_family_available(db, family))
}

pub(crate) fn html_needs_emoji_font(html: &str) -> bool {
    html.chars().any(is_emoji_codepoint) || html_contains_numeric_emoji_entity(html.as_bytes())
}

fn html_contains_numeric_emoji_entity(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'&' && bytes[index + 1] == b'#' {
            if let Some((codepoint, end)) = parse_numeric_entity(&bytes[index + 2..]) {
                if is_emoji_scalar(codepoint) {
                    return true;
                }
                index += end + 2;
                continue;
            }
        }
        index += 1;
    }
    false
}

fn parse_numeric_entity(bytes: &[u8]) -> Option<(u32, usize)> {
    let (radix, mut index) = if matches!(bytes.first(), Some(b'x' | b'X')) {
        (16, 1)
    } else {
        (10, 0)
    };
    let start = index;
    let mut value = 0u32;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b';' {
            return (index > start).then_some((value, index + 1));
        }
        let digit = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'f' if radix == 16 => u32::from(byte - b'a' + 10),
            b'A'..=b'F' if radix == 16 => u32::from(byte - b'A' + 10),
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        value = value.checked_mul(radix)?.checked_add(digit)?;
        index += 1;
    }
    None
}

fn is_emoji_codepoint(ch: char) -> bool {
    is_emoji_scalar(u32::from(ch))
}

fn is_emoji_scalar(codepoint: u32) -> bool {
    matches!(
        codepoint,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF
    )
}

fn set_generic_font_families(db: &mut Database) {
    let fallback_family = db
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
        .next();
    let sans = first_available_family(
        db,
        &[
            "Arial",
            "Helvetica",
            "Helvetica Neue",
            "Avenir",
            "Segoe UI",
            "Tahoma",
            "Trebuchet MS",
            "Verdana",
            "Roboto",
            "Open Sans",
            "DejaVu Sans",
            "Arimo",
            "Noto Sans",
        ],
    )
    .or_else(|| fallback_family.clone());
    let serif = first_available_family(
        db,
        &[
            "Times",
            "Times New Roman",
            "Georgia",
            "Palatino",
            "Palatino Linotype",
            "Iowan Old Style",
            "DejaVu Serif",
            "Tinos",
            "Noto Serif",
        ],
    )
    .or_else(|| fallback_family.clone());
    let mono = first_available_family(
        db,
        &[
            "Courier New",
            "Menlo",
            "Monaco",
            "Consolas",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ],
    )
    .or(fallback_family);

    if let Some(sans) = sans {
        db.set_sans_serif_family(sans);
    }
    if let Some(serif) = serif {
        db.set_serif_family(serif);
    }
    if let Some(mono) = mono {
        db.set_monospace_family(mono);
    }
}

fn first_available_family(db: &Database, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| font_family_available(db, candidate))
        .map(|candidate| (*candidate).to_string())
}

pub(crate) fn font_family_available(db: &Database, candidate: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(candidate))
    })
}
