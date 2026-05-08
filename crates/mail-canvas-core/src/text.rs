//! Text metrics used by the email renderer.
//!
//! This module intentionally keeps Blink-alignment constants close to the
//! code that uses them. These values are not new feature behavior; they document
//! the current compatibility decisions so future rendering work can replace or
//! remove them deliberately.

use cosmic_text::{
    Attrs, CacheKeyFlags, Family as FontFamily, Style as FontStyle, Weight as FontWeight,
};
use kuchiki::NodeRef;

use crate::dom::{attr, element_tag, is_metadata_tag};
use crate::font_catalog::generic_font_family as generic_font_family_name;
use crate::style::{
    Display, Style, TextRunStyle, TextSpan, TextTransform, parse_css_length, style_for_node,
};
use crate::{HARD_BREAK, HARD_BREAK_STR};

const BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_FACTOR: f32 = 0.15;
const BLINK_WEB_STANDARD_ASCENT_ADJUSTMENT_BIAS: f32 = 0.5;

const RICH_TEXT_BASELINE_LEADING_FACTOR: f32 = 0.5;
const NORMAL_LINE_HEIGHT_FALLBACK_FACTOR: f32 = 1.4;
const DEFAULT_WRAP_WIDTH_SCALE: f32 = 1.0;
const WEB_STANDARD_SANS_WRAP_WIDTH_SCALE: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlinkGenericFamily {
    Serif,
    SansSerif,
    Monospace,
}

#[derive(Debug, Clone, Copy)]
struct TextCompatibilityProfile {
    generic_family: Option<BlinkGenericFamily>,
    apply_web_standard_ascent_adjustment: bool,
    wrap_width_scale: f32,
}

impl Default for TextCompatibilityProfile {
    fn default() -> Self {
        Self {
            generic_family: None,
            apply_web_standard_ascent_adjustment: false,
            wrap_width_scale: DEFAULT_WRAP_WIDTH_SCALE,
        }
    }
}

struct TextCompatibilityRule {
    families: &'static [&'static str],
    generic_family: Option<BlinkGenericFamily>,
    apply_web_standard_ascent_adjustment: bool,
    wrap_width_scale: f32,
}

const TEXT_COMPATIBILITY_RULES: &[TextCompatibilityRule] = &[
    TextCompatibilityRule {
        families: &["serif", "times"],
        generic_family: Some(BlinkGenericFamily::Serif),
        apply_web_standard_ascent_adjustment: true,
        wrap_width_scale: DEFAULT_WRAP_WIDTH_SCALE,
    },
    TextCompatibilityRule {
        families: &["sans-serif", "arial", "helvetica"],
        generic_family: Some(BlinkGenericFamily::SansSerif),
        apply_web_standard_ascent_adjustment: true,
        wrap_width_scale: WEB_STANDARD_SANS_WRAP_WIDTH_SCALE,
    },
    TextCompatibilityRule {
        families: &["monospace", "courier"],
        generic_family: Some(BlinkGenericFamily::Monospace),
        apply_web_standard_ascent_adjustment: true,
        wrap_width_scale: DEFAULT_WRAP_WIDTH_SCALE,
    },
];

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
    match text_compatibility_profile(font_family).generic_family {
        Some(BlinkGenericFamily::Serif) => FontFamily::Serif,
        Some(BlinkGenericFamily::Monospace) => FontFamily::Monospace,
        Some(BlinkGenericFamily::SansSerif) => FontFamily::SansSerif,
        None => font_family.map_or(FontFamily::SansSerif, FontFamily::Name),
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
    let apply_web_standard_ascent_adjustment =
        text_compatibility_profile(style.font_family.as_deref())
            .apply_web_standard_ascent_adjustment;
    db.with_face_data(id, |font_data, face_index| {
        blink_normal_line_height_from_face(
            font_data,
            face_index,
            style.font_size,
            apply_web_standard_ascent_adjustment,
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
    let apply_web_standard_ascent_adjustment =
        text_compatibility_profile(style.font_family.as_deref())
            .apply_web_standard_ascent_adjustment;
    db.with_face_data(id, |font_data, face_index| {
        blink_normal_line_height_from_face(
            font_data,
            face_index,
            style.font_size,
            apply_web_standard_ascent_adjustment,
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
    match text_compatibility_profile(font_family).generic_family {
        Some(BlinkGenericFamily::Serif) => fontdb::Family::Serif,
        Some(BlinkGenericFamily::Monospace) => fontdb::Family::Monospace,
        Some(BlinkGenericFamily::SansSerif) => fontdb::Family::SansSerif,
        None => font_family.map_or(fontdb::Family::Serif, fontdb::Family::Name),
    }
}

fn blink_normal_line_height_from_face(
    font_data: &[u8],
    face_index: u32,
    font_size: f32,
    apply_web_standard_ascent_adjustment: bool,
) -> Option<f32> {
    let face = ttf_parser::Face::parse(font_data, face_index).ok()?;
    let units_per_em = f32::from(face.units_per_em());
    if units_per_em <= 0.0 {
        return None;
    }

    let scale = font_size.max(1.0) / units_per_em;
    let mut ascent = (f32::from(face.ascender()) * scale).round();
    let descent = (-(f32::from(face.descender())) * scale).round();
    if apply_web_standard_ascent_adjustment {
        ascent += blink_web_standard_family_ascent_adjustment(ascent, descent);
    }
    let line_gap = (f32::from(face.line_gap()) * scale).round();
    let line_height = ascent + descent + line_gap;
    line_height.is_finite().then_some(line_height.max(1.0))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn blink_web_standard_ascent_adjustment_applies(font_family: Option<&str>) -> bool {
    text_compatibility_profile(font_family).apply_web_standard_ascent_adjustment
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

pub(super) fn wrap_width_adjustment(font_family: Option<&str>) -> f32 {
    text_compatibility_profile(font_family).wrap_width_scale
}

fn text_compatibility_profile(font_family: Option<&str>) -> TextCompatibilityProfile {
    let Some(family) = font_family.map(|family| family.trim().trim_matches(['"', '\''])) else {
        return TextCompatibilityProfile {
            generic_family: Some(BlinkGenericFamily::Serif),
            apply_web_standard_ascent_adjustment: true,
            ..TextCompatibilityProfile::default()
        };
    };

    let canonical_family = generic_font_family_name(family).unwrap_or(family);
    if let Some(rule) = TEXT_COMPATIBILITY_RULES.iter().find(|rule| {
        rule.families
            .iter()
            .any(|name| name.eq_ignore_ascii_case(canonical_family))
    }) {
        return TextCompatibilityProfile {
            generic_family: rule.generic_family,
            apply_web_standard_ascent_adjustment: rule.apply_web_standard_ascent_adjustment,
            wrap_width_scale: rule.wrap_width_scale,
        };
    }

    TextCompatibilityProfile::default()
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

pub(crate) fn text_content(node: &NodeRef) -> String {
    if let Some(text) = node.as_text() {
        return text.borrow().to_string();
    }

    let Some(tag) = element_tag(node) else {
        return String::new();
    };
    if is_metadata_tag(&tag) {
        return String::new();
    }
    if tag == "br" {
        return HARD_BREAK_STR.to_string();
    }
    if tag == "img" {
        return String::new();
    }

    let mut out = String::new();
    for child in node.children() {
        append_text(&mut out, &text_content(&child));
    }
    out
}
fn append_text(out: &mut String, text: &str) {
    out.push_str(text);
}
pub(crate) fn append_text_span(out: &mut Vec<TextSpan>, text: &str, style: &Style) {
    if !text.is_empty() {
        out.push(TextSpan::from_style(text.to_string(), style));
    }
}
pub(crate) fn text_spans_are_only_collapsible_whitespace(spans: &[TextSpan]) -> bool {
    spans.is_empty()
        || spans
            .iter()
            .all(|span| span.text.chars().all(is_collapsible_whitespace))
}
pub(crate) fn append_inline_spans(node: &NodeRef, style: &Style, out: &mut Vec<TextSpan>) {
    if let Some(text) = node.as_text() {
        append_text_span(out, &text.borrow(), style);
        return;
    }

    let Some(tag) = element_tag(node) else {
        return;
    };
    if is_metadata_tag(&tag) {
        return;
    }
    if tag == "br" {
        append_text_span(out, HARD_BREAK_STR, style);
        return;
    }
    if tag == "img" {
        append_text_span(out, &attr(node, "alt").unwrap_or_default(), style);
        return;
    }

    for child in node.children() {
        if child.as_text().is_some() {
            append_inline_spans(&child, style, out);
            continue;
        }
        let Some(child_tag) = element_tag(&child) else {
            continue;
        };
        if is_metadata_tag(&child_tag) {
            continue;
        }
        let child_style = style_for_node(&child, style);
        if child_style.display != Display::None {
            append_inline_spans(&child, &child_style, out);
        }
    }
}
#[cfg(test)]
pub(crate) fn normalize_text(text: &str) -> String {
    let style = Style::initial();
    spans_text(&normalize_text_spans(&[TextSpan::from_style(
        text.to_string(),
        &style,
    )]))
}
pub(crate) fn normalize_text_spans(spans: &[TextSpan]) -> Vec<TextSpan> {
    let mut out = Vec::new();
    let mut pending_space_style: Option<TextRunStyle> = None;

    for span in spans {
        let mut segment = String::new();
        for ch in span.text.chars() {
            if ch == HARD_BREAK {
                while segment.ends_with(' ') {
                    segment.pop();
                }
                push_text_span_segment(&mut out, segment, &span.style);
                trim_trailing_span_space(&mut out);
                out.push(TextSpan::with_run_style(
                    "\n".to_string(),
                    span.style.clone(),
                ));
                segment = String::new();
                pending_space_style = None;
            } else if is_collapsible_whitespace(ch) {
                pending_space_style.get_or_insert_with(|| span.style.clone());
            } else {
                let at_line_start_after_break =
                    segment.is_empty() && rich_text_ends_with_newline(&out);
                if let Some(space_style) = pending_space_style.take() {
                    if (!out.is_empty() || !segment.is_empty())
                        && !segment.ends_with('\n')
                        && !at_line_start_after_break
                    {
                        if space_style == span.style {
                            segment.push(' ');
                        } else {
                            push_text_span_segment(&mut out, segment, &span.style);
                            segment = String::new();
                            push_text_span_segment(&mut out, " ".to_string(), &space_style);
                        }
                    }
                }
                segment.push(ch);
            }
        }
        push_text_span_segment(&mut out, segment, &span.style);
    }

    trim_leading_span_space(&mut out);
    trim_trailing_span_space(&mut out);
    out
}
fn push_text_span_segment(out: &mut Vec<TextSpan>, text: String, style: &TextRunStyle) {
    if text.is_empty() {
        return;
    }
    let text = apply_text_transform(&text, style.text_transform);
    if !text.is_empty() {
        if let Some(last) = out.last_mut() {
            if last.style == *style {
                last.text.push_str(&text);
                return;
            }
        }
        out.push(TextSpan::with_run_style(text, style.clone()));
    }
}
fn trim_leading_span_space(spans: &mut Vec<TextSpan>) {
    while let Some(first) = spans.first_mut() {
        let trimmed = first
            .text
            .trim_start_matches(|ch: char| ch != '\n' && is_collapsible_whitespace(ch))
            .to_string();
        first.text = trimmed;
        if first.text.is_empty() {
            spans.remove(0);
        } else {
            break;
        }
    }
}
fn trim_trailing_span_space(spans: &mut Vec<TextSpan>) {
    while let Some(last) = spans.last_mut() {
        let trimmed = last
            .text
            .trim_end_matches(|ch: char| ch != '\n' && is_collapsible_whitespace(ch))
            .to_string();
        last.text = trimmed;
        if last.text.is_empty() {
            spans.pop();
        } else {
            break;
        }
    }
}
fn rich_text_ends_with_newline(spans: &[TextSpan]) -> bool {
    spans.last().is_some_and(|span| span.text.ends_with('\n'))
}
pub(crate) fn is_collapsible_whitespace(ch: char) -> bool {
    ch != '\u{00a0}' && ch.is_whitespace()
}
pub(crate) fn spans_text(spans: &[TextSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}
pub(crate) fn text_spans_match_style(spans: &[TextSpan], style: &Style) -> bool {
    let parent_style = TextRunStyle::from_style(style);
    spans.iter().all(|span| span.style == parent_style)
}
fn apply_text_transform(text: &str, transform: TextTransform) -> String {
    match transform {
        TextTransform::None => text.to_string(),
        TextTransform::Uppercase => text.to_uppercase(),
        TextTransform::Lowercase => text.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut at_word_start = true;
            for ch in text.chars() {
                if ch.is_alphanumeric() {
                    if at_word_start {
                        out.extend(ch.to_uppercase());
                    } else {
                        out.extend(ch.to_lowercase());
                    }
                    at_word_start = false;
                } else {
                    out.push(ch);
                    at_word_start = ch.is_whitespace();
                }
            }
            out
        }
    }
}

pub(crate) fn rich_text_style_spans<'a>(
    spans: &'a [TextSpan],
    db: &fontdb::Database,
    scale: f32,
    parent_style: &'a Style,
) -> Vec<(&'a str, Attrs<'a>)> {
    spans
        .iter()
        .map(|span| {
            (
                span.text.as_str(),
                span.style.text_attrs_for_span(db, scale, parent_style),
            )
        })
        .collect()
}
