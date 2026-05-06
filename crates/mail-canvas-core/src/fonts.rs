use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::{Style as FontStyle, Weight as FontWeight};
use kuchiki::traits::TendrilSink as _;
use url::Url;

use crate::api::{AssetKind, AssetStatus, RenderDiagnostics};
use crate::css::{
    css_format_hint, css_function_value, first_css_url, first_quoted_css_string,
    font_face_declarations, next_css_segment_end, style_blocks, unquote_css_value,
};
use crate::resource::ResourceProvider;
use crate::{AssetReport, RenderWarning, RenderWarningCode, parse_font_style};

const MAX_WEB_FONT_IMPORTS: usize = 16;
const MAX_WEB_FONTS: usize = 32;

#[cfg(test)]
pub(crate) fn system_font_database() -> fontdb::Database {
    if let Ok(db) = fixture_font_database() {
        return db;
    }
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    #[cfg(target_os = "macos")]
    db.load_fonts_dir("/System/Library/Fonts/Supplemental");
    set_generic_font_families(&mut db);
    db
}

#[cfg(test)]
pub(crate) fn font_database_from_paths(paths: &[std::path::PathBuf]) -> Result<fontdb::Database> {
    let mut db = fontdb::Database::new();
    for path in paths {
        if !path.is_file() {
            bail!("font path is not a file: {}", path.display());
        }
        db.load_font_source(fontdb::Source::File(path.clone()));
    }
    if db.is_empty() {
        bail!("no valid font faces found in supplied font files");
    }
    set_generic_font_families(&mut db);
    Ok(db)
}

#[cfg(test)]
fn fixture_font_database() -> Result<fontdb::Database> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("fonts");
    let regular = root.join("NotoSans-Regular.ttf");
    let bold = root.join("NotoSans-Bold.ttf");
    if !regular.is_file() || !bold.is_file() {
        bail!("fixture fonts missing: {}", root.display());
    }
    font_database_from_paths(&[regular, bold])
}

#[cfg(test)]
fn set_generic_font_families(db: &mut fontdb::Database) {
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
            "Roboto",
            "Open Sans",
            "DejaVu Sans",
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

#[cfg(test)]
fn first_available_family(db: &fontdb::Database, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| font_family_available(db, candidate))
        .map(|candidate| (*candidate).to_string())
}

#[cfg(test)]
fn font_family_available(db: &fontdb::Database, candidate: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(candidate))
    })
}

pub(crate) fn font_database_families(db: &fontdb::Database) -> Vec<String> {
    let mut families = Vec::new();
    for family in db
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
    {
        push_unique_case_insensitive(&mut families, family);
    }
    families
}

fn push_unique_case_insensitive(values: &mut Vec<String>, value: String) {
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        values.push(value);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WebFontFace {
    pub(crate) css_family: String,
    pub(crate) actual_family: String,
    pub(crate) weight: FontWeight,
}

#[derive(Debug, Clone)]
struct LoadedFontSource {
    url: String,
    actual_family: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontCssSource {
    InlineOrImport,
    LinkedStylesheet,
}

#[derive(Debug, Clone)]
struct FontCssBlock {
    css: String,
    source: FontCssSource,
    base_url: Option<String>,
}

pub(crate) fn load_web_fonts_from_html(
    html: &str,
    document_base_url: Option<&Url>,
    policy: &impl ResourceProvider,
    db: &mut fontdb::Database,
    diagnostics: &mut RenderDiagnostics,
) -> Vec<WebFontFace> {
    let mut css_blocks: Vec<FontCssBlock> = style_blocks(html)
        .into_iter()
        .map(|css| FontCssBlock {
            css: css.to_string(),
            source: FontCssSource::InlineOrImport,
            base_url: document_base_url.map(Url::to_string),
        })
        .collect();
    let mut imported_urls = Vec::new();

    for stylesheet_url in stylesheet_link_urls(html) {
        let stylesheet_url =
            resolve_relative_stylesheet_url(&stylesheet_url, document_base_url.map(Url::as_str));
        if imported_urls.len() >= MAX_WEB_FONT_IMPORTS {
            break;
        }
        if imported_urls
            .iter()
            .any(|loaded: &String| loaded.eq_ignore_ascii_case(&stylesheet_url))
        {
            continue;
        }
        imported_urls.push(stylesheet_url.clone());
        match load_stylesheet(&stylesheet_url, policy) {
            Ok(css) => {
                if linked_stylesheet_fonts_are_supported(&css) {
                    css_blocks.push(FontCssBlock {
                        css,
                        source: FontCssSource::LinkedStylesheet,
                        base_url: Some(stylesheet_url.clone()),
                    });
                }
            }
            Err(error) => diagnostics.push_warning(
                RenderWarning::new(
                    RenderWarningCode::StylesheetLoadFailed,
                    format!("failed to load stylesheet {stylesheet_url}: {error}"),
                )
                .with_node("link")
                .with_url(stylesheet_url),
            ),
        }
    }

    let mut index = 0usize;
    while index < css_blocks.len() && imported_urls.len() < MAX_WEB_FONT_IMPORTS {
        let source = css_blocks[index].source;
        for import_url in css_import_urls(&css_blocks[index].css) {
            let import_url =
                resolve_relative_stylesheet_url(&import_url, css_blocks[index].base_url.as_deref());
            if imported_urls
                .iter()
                .any(|loaded: &String| loaded.eq_ignore_ascii_case(&import_url))
            {
                continue;
            }
            imported_urls.push(import_url.clone());
            match load_stylesheet(&import_url, policy) {
                Ok(css) => css_blocks.push(FontCssBlock {
                    css,
                    source,
                    base_url: Some(import_url.clone()),
                }),
                Err(error) => diagnostics.push_warning(
                    RenderWarning::new(
                        RenderWarningCode::StylesheetLoadFailed,
                        format!("failed to load stylesheet {import_url}: {error}"),
                    )
                    .with_node("@import")
                    .with_url(import_url),
                ),
            }
            if imported_urls.len() >= MAX_WEB_FONT_IMPORTS {
                break;
            }
        }
        index += 1;
    }

    let mut loaded_font_sources: Vec<LoadedFontSource> = Vec::new();
    let mut loaded_fonts = 0usize;
    let mut web_font_faces = Vec::new();
    for block in css_blocks {
        let preserve_descriptors = block.source == FontCssSource::LinkedStylesheet;
        for declarations in font_face_declarations(&block.css) {
            if loaded_fonts >= MAX_WEB_FONTS {
                diagnostics.push_warning(
                    RenderWarning::new(
                        RenderWarningCode::WebFontLimitReached,
                        "maximum web font count reached; skipped remaining @font-face rules",
                    )
                    .with_node("@font-face"),
                );
                return web_font_faces;
            }

            if !font_face_covers_basic_latin(&declarations) {
                continue;
            }
            if declaration_value(&declarations, "font-style")
                .map(parse_font_style)
                .unwrap_or(FontStyle::Normal)
                != FontStyle::Normal
            {
                continue;
            }
            let family = declaration_value(&declarations, "font-family")
                .map(unquote_css_value)
                .unwrap_or_else(|| "unknown".to_string());
            let Some(src) = declaration_value(&declarations, "src") else {
                continue;
            };
            let Some(candidate) = choose_font_source(src) else {
                continue;
            };
            let candidate_url =
                resolve_relative_stylesheet_url(&candidate.url, block.base_url.as_deref());
            let descriptor_weight = declaration_value(&declarations, "font-weight")
                .and_then(parse_font_face_weight)
                .unwrap_or(FontWeight::NORMAL);
            if let Some(source) = loaded_font_sources
                .iter()
                .find(|loaded| loaded.url.eq_ignore_ascii_case(&candidate_url))
            {
                if preserve_descriptors {
                    web_font_faces.push(WebFontFace {
                        css_family: family.clone(),
                        actual_family: source.actual_family.clone(),
                        weight: descriptor_weight,
                    });
                }
                continue;
            }

            match policy
                .load_bytes(&candidate_url, AssetKind::WebFont, "@font-face")
                .and_then(|bytes| {
                    decode_font_resource(&bytes, &candidate).inspect_err(|error| {
                        policy.record_asset_report(
                            AssetReport::new(
                                AssetKind::WebFont,
                                AssetStatus::Failed,
                                candidate_url.clone(),
                            )
                            .with_initiator("@font-face")
                            .with_detail(error.to_string()),
                        );
                    })
                }) {
                Ok(font_data) => {
                    let ids = db.load_font_source(fontdb::Source::Binary(Arc::new(font_data)));
                    if !ids.is_empty() {
                        for id in ids {
                            if let Some(face) = db.face(id) {
                                let actual_family = face
                                    .families
                                    .first()
                                    .map(|(family, _)| family.clone())
                                    .unwrap_or_else(|| family.clone());
                                web_font_faces.push(WebFontFace {
                                    css_family: family.clone(),
                                    actual_family: actual_family.clone(),
                                    weight: if preserve_descriptors {
                                        descriptor_weight
                                    } else {
                                        face.weight
                                    },
                                });
                                if !loaded_font_sources
                                    .iter()
                                    .any(|source| source.url.eq_ignore_ascii_case(&candidate_url))
                                {
                                    loaded_font_sources.push(LoadedFontSource {
                                        url: candidate_url.clone(),
                                        actual_family,
                                    });
                                }
                            }
                        }
                        loaded_fonts += 1;
                    } else {
                        policy.record_asset_report(
                            AssetReport::new(
                                AssetKind::WebFont,
                                AssetStatus::Failed,
                                candidate_url.clone(),
                            )
                            .with_initiator("@font-face")
                            .with_detail(format!(
                                "web font {family} did not contain a loadable face"
                            )),
                        );
                        diagnostics.push_warning(
                            RenderWarning::new(
                                RenderWarningCode::WebFontLoadFailed,
                                format!("web font {family} did not contain a loadable face"),
                            )
                            .with_node("@font-face")
                            .with_property("font-family", family.clone())
                            .with_url(candidate_url.clone()),
                        );
                    }
                }
                Err(error) => diagnostics.push_warning(
                    RenderWarning::new(
                        RenderWarningCode::WebFontLoadFailed,
                        format!(
                            "failed to load web font {family} from {}: {error}",
                            candidate_url
                        ),
                    )
                    .with_node("@font-face")
                    .with_property("font-family", family)
                    .with_url(candidate_url),
                ),
            }
        }
    }

    web_font_faces
}

fn load_stylesheet(url: &str, policy: &impl ResourceProvider) -> Result<String> {
    policy
        .load_bytes(url, AssetKind::Stylesheet, "stylesheet")
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .inspect_err(|error| {
                    policy.record_asset_report(
                        AssetReport::new(
                            AssetKind::Stylesheet,
                            AssetStatus::Failed,
                            url.to_string(),
                        )
                        .with_initiator("stylesheet")
                        .with_detail(format!("stylesheet is not UTF-8: {error}")),
                    );
                })
                .context("stylesheet is not UTF-8")
        })
}

fn resolve_relative_stylesheet_url(url: &str, base_url: Option<&str>) -> String {
    let Some(base_url) = base_url else {
        return url.to_string();
    };
    match Url::parse(base_url).and_then(|base| base.join(url)) {
        Ok(resolved) => resolved.to_string(),
        Err(_) => url.to_string(),
    }
}

pub(crate) fn stylesheet_link_urls(html: &str) -> Vec<String> {
    let document = kuchiki::parse_html().one(html.to_string());
    let Ok(links) = document.select("link") else {
        return Vec::new();
    };

    let mut urls = Vec::new();
    for link in links {
        let attrs = link.attributes.borrow();
        let rel = attrs.get("rel").unwrap_or_default();
        let is_stylesheet = rel
            .split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case("stylesheet"));
        let is_alternate = rel
            .split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case("alternate"));
        if !is_stylesheet || is_alternate {
            continue;
        }
        let Some(href) = attrs.get("href") else {
            continue;
        };
        urls.push(normalize_resource_url(href));
    }
    urls
}

pub(crate) fn linked_stylesheet_fonts_are_supported(css: &str) -> bool {
    font_face_declarations(css).into_iter().all(|declarations| {
        declaration_value(&declarations, "font-style")
            .map(parse_font_style)
            .unwrap_or(FontStyle::Normal)
            == FontStyle::Normal
    })
}

fn css_import_urls(css: &str) -> Vec<String> {
    let lower = css.to_ascii_lowercase();
    let mut urls = Vec::new();
    let mut offset = 0usize;

    while offset < lower.len() {
        let Some(import_rel) = lower[offset..].find("@import") else {
            break;
        };
        let import_start = offset + import_rel;
        let statement_start = import_start + "@import".len();
        let statement_end = css[statement_start..]
            .find(';')
            .map_or(css.len(), |rel| statement_start + rel);
        let statement = &css[statement_start..statement_end];
        if let Some(url) = first_css_url(statement).or_else(|| first_quoted_css_string(statement)) {
            urls.push(normalize_resource_url(&url));
        }
        offset = statement_end.saturating_add(1);
    }

    urls
}

fn declaration_value<'a>(declarations: &'a [(String, String)], name: &str) -> Option<&'a str> {
    declarations
        .iter()
        .find(|(declaration_name, _)| declaration_name == name)
        .map(|(_, value)| value.as_str())
}

fn parse_font_face_weight(value: &str) -> Option<FontWeight> {
    value
        .split_whitespace()
        .find_map(|token| match token.to_ascii_lowercase().as_str() {
            "normal" => Some(FontWeight::NORMAL),
            "bold" => Some(FontWeight::BOLD),
            raw => raw.parse::<u16>().ok().map(FontWeight),
        })
}

pub(crate) fn font_face_covers_basic_latin(declarations: &[(String, String)]) -> bool {
    let Some(range) = declaration_value(declarations, "unicode-range") else {
        return true;
    };
    unicode_range_contains(range, 0x41) || unicode_range_contains(range, 0x61)
}

fn unicode_range_contains(range_list: &str, codepoint: u32) -> bool {
    range_list
        .split(',')
        .any(|range| single_unicode_range_contains(range.trim(), codepoint))
}

fn single_unicode_range_contains(range: &str, codepoint: u32) -> bool {
    let Some(raw) = range
        .strip_prefix("U+")
        .or_else(|| range.strip_prefix("u+"))
    else {
        return false;
    };

    if raw.contains('?') {
        let start = u32::from_str_radix(&raw.replace('?', "0"), 16).ok();
        let end = u32::from_str_radix(&raw.replace('?', "F"), 16).ok();
        return matches!((start, end), (Some(start), Some(end)) if start <= codepoint && codepoint <= end);
    }

    if let Some((start, end)) = raw.split_once('-') {
        let start = u32::from_str_radix(start.trim(), 16).ok();
        let end = u32::from_str_radix(end.trim(), 16).ok();
        return matches!((start, end), (Some(start), Some(end)) if start <= codepoint && codepoint <= end);
    }

    u32::from_str_radix(raw.trim(), 16).is_ok_and(|value| value == codepoint)
}

#[derive(Debug, Clone)]
struct FontSourceCandidate {
    url: String,
    format: Option<String>,
}

fn choose_font_source(src: &str) -> Option<FontSourceCandidate> {
    font_source_candidates(src)
        .into_iter()
        .find(font_source_supported)
}

fn font_source_candidates(src: &str) -> Vec<FontSourceCandidate> {
    let lower = src.to_ascii_lowercase();
    let mut candidates = Vec::new();
    let mut offset = 0usize;

    while offset < lower.len() {
        let Some(url_rel) = lower[offset..].find("url(") else {
            break;
        };
        let url_start = offset + url_rel;
        let Some((url, end)) = css_function_value(src, url_start) else {
            offset = url_start + "url(".len();
            continue;
        };
        let segment_end = next_css_segment_end(src, end);
        let format = css_format_hint(&src[end..segment_end]);
        candidates.push(FontSourceCandidate { url, format });
        offset = segment_end.saturating_add(1);
    }

    candidates
}

fn font_source_supported(candidate: &FontSourceCandidate) -> bool {
    if let Some(format) = &candidate.format {
        let format = format.to_ascii_lowercase();
        if format.contains("woff2")
            || format.contains("woff")
            || format.contains("truetype")
            || format.contains("opentype")
        {
            return true;
        }
    }

    let path = candidate
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or(candidate.url.as_str())
        .to_ascii_lowercase();
    matches!(
        Path::new(&path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("woff2" | "woff" | "ttf" | "otf" | "ttc" | "otc")
    )
}

fn decode_font_resource(bytes: &[u8], candidate: &FontSourceCandidate) -> Result<Vec<u8>> {
    if bytes.starts_with(b"wOF2") {
        return wuff::decompress_woff2(bytes)
            .map_err(|error| anyhow!("failed to decode WOFF2 font: {error}"));
    }
    if bytes.starts_with(b"wOFF") {
        return wuff::decompress_woff1(bytes)
            .map_err(|error| anyhow!("failed to decode WOFF font: {error}"));
    }
    if font_bytes_look_raw(bytes) {
        return Ok(bytes.to_vec());
    }

    match candidate.format.as_deref().map(str::to_ascii_lowercase) {
        Some(format) if format.contains("woff2") => wuff::decompress_woff2(bytes)
            .map_err(|error| anyhow!("failed to decode WOFF2 font: {error}")),
        Some(format) if format.contains("woff") => wuff::decompress_woff1(bytes)
            .map_err(|error| anyhow!("failed to decode WOFF font: {error}")),
        _ => bail!("unsupported font data"),
    }
}

fn font_bytes_look_raw(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x00, 0x01, 0x00, 0x00])
        || bytes.starts_with(b"OTTO")
        || bytes.starts_with(b"ttcf")
        || bytes.starts_with(b"true")
}

fn normalize_resource_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}
