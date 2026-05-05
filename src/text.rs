//! Text metrics used by the email renderer.
//!
//! This module intentionally keeps Blink-alignment constants close to the
//! code that uses them. These values are not new feature behavior; they document
//! the current compatibility decisions so future rendering work can replace or
//! remove them deliberately.

use cosmic_text::{
    Attrs, CacheKeyFlags, Family as FontFamily, Style as FontStyle, Weight as FontWeight,
};

use super::{Style, TextRunStyle, TextSpan, parse_css_length};

const BLINK_SERIF_FAMILY: &str = "Times";
const BLINK_SANS_SERIF_FAMILY: &str = "Helvetica";
const BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_FACTOR: f32 = 0.15;
const BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_BIAS: f32 = 0.5;

const RICH_TEXT_BASELINE_LEADING_FACTOR: f32 = 0.5;
const NORMAL_LINE_HEIGHT_FALLBACK_FACTOR: f32 = 1.4;

#[derive(Debug, Clone, Copy)]
pub(super) struct LineHeightDeclaration {
    pub(super) height: f32,
    pub(super) factor: Option<f32>,
    pub(super) normal: bool,
}

pub(super) fn text_style_attrs(
    font_family: Option<&str>,
    font_weight: FontWeight,
    font_face_weight: Option<FontWeight>,
    font_style: FontStyle,
    font_hinting_disabled: bool,
    letter_spacing: f32,
    font_size: f32,
) -> Attrs<'_> {
    let mut attrs = Attrs::new()
        .family(cosmic_font_family(font_family))
        .weight(font_face_weight.unwrap_or(font_weight))
        .style(font_style);
    if font_hinting_disabled {
        attrs = attrs.cache_key_flags(CacheKeyFlags::DISABLE_HINTING);
    }
    if letter_spacing != 0.0 {
        attrs = attrs.letter_spacing(letter_spacing / font_size.max(1.0));
    }
    attrs
}

fn cosmic_font_family(font_family: Option<&str>) -> FontFamily<'_> {
    match font_family.map(str::to_ascii_lowercase) {
        Some(family) if family == "serif" => FontFamily::Name(BLINK_SERIF_FAMILY),
        Some(family) if family == "monospace" => FontFamily::Monospace,
        Some(family) if family == "sans-serif" => FontFamily::Name(BLINK_SANS_SERIF_FAMILY),
        Some(_) => font_family.map_or(FontFamily::SansSerif, FontFamily::Name),
        None => FontFamily::Name(BLINK_SERIF_FAMILY),
    }
}

pub(super) fn resolved_line_height_from_db(db: &fontdb::Database, style: &Style) -> f32 {
    if !style.line_height_normal {
        return style.line_height.max(1.0);
    }

    blink_normal_line_height_from_db(db, style).unwrap_or_else(|| style.line_height.max(1.0))
}

pub(super) fn resolved_line_height_from_run_db(db: &fontdb::Database, style: &TextRunStyle) -> f32 {
    if !style.line_height_normal {
        return style.line_height.max(1.0);
    }

    blink_normal_line_height_from_run_db(db, style).unwrap_or_else(|| style.line_height.max(1.0))
}

fn blink_normal_line_height_from_db(db: &fontdb::Database, style: &Style) -> Option<f32> {
    let family = fontdb_family_for_style(style);
    let families = [family];
    let query = fontdb::Query {
        families: &families,
        weight: style.font_face_weight.unwrap_or(style.font_weight),
        stretch: fontdb::Stretch::Normal,
        style: style.font_style,
    };
    let id = db.query(&query)?;
    let apply_mac_ascent_hack = blink_mac_ascent_hack_applies(style.font_family.as_deref());
    db.with_face_data(id, |font_data, face_index| {
        blink_normal_line_height_from_face(
            font_data,
            face_index,
            style.font_size,
            apply_mac_ascent_hack,
        )
    })
    .flatten()
}

pub(super) fn blink_font_descent_from_db(db: &fontdb::Database, style: &Style) -> Option<f32> {
    let family = fontdb_family_for_style(style);
    let families = [family];
    let query = fontdb::Query {
        families: &families,
        weight: style.font_face_weight.unwrap_or(style.font_weight),
        stretch: fontdb::Stretch::Normal,
        style: style.font_style,
    };
    let id = db.query(&query)?;
    db.with_face_data(id, |font_data, face_index| {
        blink_font_descent_from_face(font_data, face_index, style.font_size)
    })
    .flatten()
}

fn blink_normal_line_height_from_run_db(
    db: &fontdb::Database,
    style: &TextRunStyle,
) -> Option<f32> {
    let family = fontdb_family_for_run_style(style);
    let families = [family];
    let query = fontdb::Query {
        families: &families,
        weight: style.font_face_weight.unwrap_or(style.font_weight),
        stretch: fontdb::Stretch::Normal,
        style: style.font_style,
    };
    let id = db.query(&query)?;
    let apply_mac_ascent_hack = blink_mac_ascent_hack_applies(style.font_family.as_deref());
    db.with_face_data(id, |font_data, face_index| {
        blink_normal_line_height_from_face(
            font_data,
            face_index,
            style.font_size,
            apply_mac_ascent_hack,
        )
    })
    .flatten()
}

fn fontdb_family_for_style(style: &Style) -> fontdb::Family<'_> {
    fontdb_family(style.font_family.as_deref())
}

fn fontdb_family_for_run_style(style: &TextRunStyle) -> fontdb::Family<'_> {
    fontdb_family(style.font_family.as_deref())
}

pub(super) fn fontdb_family(font_family: Option<&str>) -> fontdb::Family<'_> {
    match font_family {
        Some(family) if family.eq_ignore_ascii_case("serif") => {
            fontdb::Family::Name(BLINK_SERIF_FAMILY)
        }
        Some(family) if family.eq_ignore_ascii_case("monospace") => fontdb::Family::Monospace,
        Some(family) if family.eq_ignore_ascii_case("sans-serif") => {
            fontdb::Family::Name(BLINK_SANS_SERIF_FAMILY)
        }
        Some(family) => fontdb::Family::Name(family),
        None => fontdb::Family::Name(BLINK_SERIF_FAMILY),
    }
}

fn blink_normal_line_height_from_face(
    font_data: &[u8],
    face_index: u32,
    font_size: f32,
    apply_mac_ascent_hack: bool,
) -> Option<f32> {
    let face = ttf_parser::Face::parse(font_data, face_index).ok()?;
    let units_per_em = f32::from(face.units_per_em());
    if units_per_em <= 0.0 {
        return None;
    }

    let scale = font_size.max(1.0) / units_per_em;
    let mut ascent = (f32::from(face.ascender()) * scale).round();
    let descent = (-(f32::from(face.descender())) * scale).round();
    if apply_mac_ascent_hack {
        ascent += blink_web_standard_family_ascent_adjustment(ascent, descent);
    }
    let line_gap = (f32::from(face.line_gap()) * scale).round();
    let line_height = ascent + descent + line_gap;
    line_height.is_finite().then_some(line_height.max(1.0))
}

pub(super) fn blink_mac_ascent_hack_applies(font_family: Option<&str>) -> bool {
    let Some(family) = font_family else {
        return true;
    };
    let family = family.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    matches!(
        family.as_str(),
        "serif" | "times" | "sans-serif" | "helvetica" | "monospace" | "courier"
    )
}

pub(super) fn blink_web_standard_family_ascent_adjustment(ascent: f32, descent: f32) -> f32 {
    ((ascent + descent) * BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_FACTOR
        + BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_BIAS)
        .floor()
}

fn blink_font_descent_from_face(font_data: &[u8], face_index: u32, font_size: f32) -> Option<f32> {
    let face = ttf_parser::Face::parse(font_data, face_index).ok()?;
    let units_per_em = f32::from(face.units_per_em());
    if units_per_em <= 0.0 {
        return None;
    }

    let scale = font_size.max(1.0) / units_per_em;
    let descent = (-(f32::from(face.descender())) * scale).round();
    descent.is_finite().then_some(descent.max(0.0))
}

pub(super) fn normal_line_height_fallback(font_size: f32) -> f32 {
    font_size.max(1.0) * NORMAL_LINE_HEIGHT_FALLBACK_FACTOR
}

pub(super) fn parse_line_height_declaration(
    value: &str,
    font_size: f32,
) -> Option<LineHeightDeclaration> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return Some(LineHeightDeclaration {
            height: normal_line_height_fallback(font_size),
            factor: None,
            normal: true,
        });
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| LineHeightDeclaration {
                height: font_size * value / 100.0,
                factor: None,
                normal: false,
            });
    }
    if let Some(length) = parse_css_length(value, font_size, false) {
        return Some(LineHeightDeclaration {
            height: length,
            factor: None,
            normal: false,
        });
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|scale| LineHeightDeclaration {
            height: font_size * scale,
            factor: Some(scale),
            normal: false,
        })
}

pub(super) fn rich_text_baseline_leading_offset(spans: &[TextSpan], style: &Style) -> f32 {
    let max_span_size = spans
        .iter()
        .map(|span| span.style.font_size)
        .fold(0.0, f32::max);
    if max_span_size >= style.font_size - 0.5 {
        return 0.0;
    }
    ((style.line_height - style.font_size).max(0.0) * RICH_TEXT_BASELINE_LEADING_FACTOR).round()
}
