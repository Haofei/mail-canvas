#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::{
    Align as TextAlignMode, Attrs, Buffer, Color as TextColor, FontSystem, Metrics, Shaping,
    Style as FontStyle, SwashCache, Weight as FontWeight, Wrap,
};
use kuchiki::{NodeRef, traits::TendrilSink as _};
use taffy::geometry::{Rect as TaffyRect, Size as TaffySize};
use taffy::prelude::{
    AlignItems as TaffyAlignItems, AvailableSpace, Dimension as TaffyDimension,
    Display as TaffyDisplay, FlexDirection as TaffyFlexDirection, FlexWrap as TaffyFlexWrap,
    JustifyContent as TaffyJustifyContent, NodeId as TaffyNodeId, Style as TaffyStyle, TaffyTree,
};
use taffy::style_helpers::{auto as taffy_auto, length as taffy_length, percent as taffy_percent};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect as SkiaRect, Transform};
use url::Url;

mod api;
mod css;
mod document;
mod pdf;
mod resource;
mod text;

#[cfg(test)]
use api::DEFAULT_MAX_IMAGE_BYTES;
pub use api::{
    ConsoleMessage, EmailRenderer, RenderRequest, RenderWarning, RenderWarningCode, RenderedImage,
    RenderedPdf,
};
use api::{DEFAULT_MAX_DECODED_PIXELS, RenderDiagnostics};
use css::{
    css_declarations, css_format_hint, css_function_value, first_css_url, first_quoted_css_string,
    font_face_declarations, inline_css, next_css_segment_end, strip_hidden_conditional_comments,
    style_blocks, unquote_css_value,
};
pub use document::{PreparedDocument, build_document, build_document_from_files};
use pdf::raster_pdf_from_png;
use resource::{ImageData, ResourcePolicy, load_image, load_resource_bytes};
use text::{
    blink_font_descent_from_db, normal_line_height_fallback, parse_line_height_declaration,
    resolved_line_height_from_db, resolved_line_height_from_run_db,
    rich_text_baseline_leading_offset, text_style_attrs,
};
#[cfg(test)]
use text::{
    blink_mac_ascent_hack_applies, blink_web_standard_family_ascent_adjustment, fontdb_family,
};

const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;
const MAX_LAYOUT_DEPTH: usize = 64;
const MAX_WEB_FONT_IMPORTS: usize = 16;
const MAX_WEB_FONTS: usize = 32;
const HARD_BREAK: char = '\u{000B}';

pub struct MailCanvasRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl MailCanvasRenderer {
    pub fn new(width: u32, viewport_height: u32, scale: f32) -> Result<Self> {
        Self::with_fonts(width, viewport_height, scale, [])
    }

    pub fn with_fonts(
        width: u32,
        viewport_height: u32,
        scale: f32,
        font_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        validate_scale(scale)?;
        let _ = scaled_dimension(width, scale, "width")?;
        let _ = scaled_dimension(viewport_height.max(1), scale, "viewport-height")?;
        let font_paths: Vec<PathBuf> = font_paths.into_iter().collect();
        let font_db = if font_paths.is_empty() {
            system_font_database()
        } else {
            font_database_from_paths(&font_paths)?
        };
        let font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            font_db,
            cosmic_text::PlatformFallback,
        );

        Ok(Self {
            font_system,
            swash_cache: SwashCache::new(),
        })
    }
}

fn system_font_database() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    #[cfg(target_os = "macos")]
    db.load_fonts_dir("/System/Library/Fonts/Supplemental");
    set_generic_font_families(&mut db);
    db
}

fn font_database_from_paths(paths: &[PathBuf]) -> Result<fontdb::Database> {
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

fn first_available_family(db: &fontdb::Database, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| font_family_available(db, candidate))
        .map(|candidate| (*candidate).to_string())
}

fn font_family_available(db: &fontdb::Database, candidate: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(candidate))
    })
}

fn font_database_families(db: &fontdb::Database) -> Vec<String> {
    let mut families = Vec::new();
    for family in db
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
    {
        push_unique_case_insensitive(&mut families, family);
    }
    families
}

#[derive(Debug, Clone)]
struct WebFontFace {
    css_family: String,
    actual_family: String,
    weight: FontWeight,
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
}

pub type RustEmailRenderer = MailCanvasRenderer;
pub type ServoEmailRenderer = MailCanvasRenderer;

impl EmailRenderer for MailCanvasRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        validate_request(&request)?;

        let render_html = strip_hidden_conditional_comments(&request.html);
        let source_document = kuchiki::parse_html().one(render_html.clone());
        let resources = ResourcePolicy::from_request(&request, document_base_url(&source_document));
        let mut diagnostics = RenderDiagnostics::default();
        let web_font_faces = load_web_fonts_from_html(
            &render_html,
            &resources,
            self.font_system.db_mut(),
            &mut diagnostics,
        );

        let available_font_families = font_database_families(self.font_system.db());
        let html = inline_css(&render_html, request.width)?;
        let document = kuchiki::parse_html().one(html);
        let mut engine = LayoutEngine::new(
            &mut self.font_system,
            resources,
            available_font_families,
            web_font_faces,
        );
        let mut layout = engine.layout_document(&document, request.width)?;
        for warning in std::mem::take(&mut engine.warnings) {
            diagnostics.push_warning(warning);
        }
        drop(engine);

        let css_height = clamp_css_height(
            ceil_to_u32(layout.rect.height)?,
            request.min_height,
            request.max_height,
            request.scale,
        )?;
        layout.rect.height = css_height as f32;

        let pixel_width = scaled_dimension(request.width, request.scale, "width")?;
        let pixel_height = scaled_dimension(css_height, request.scale, "height")?;
        let mut pixmap = Pixmap::new(pixel_width, pixel_height)
            .ok_or_else(|| anyhow!("failed to allocate {pixel_width}x{pixel_height} pixmap"))?;

        fill_rect(
            &mut pixmap,
            request.scale,
            Rect::new(0.0, 0.0, request.width as f32, css_height as f32),
            Rgba::WHITE,
        );

        let mut painter = LayoutPainter {
            pixmap: &mut pixmap,
            font_system: &mut self.font_system,
            swash_cache: &mut self.swash_cache,
            scale: request.scale,
        };
        painter.paint(&layout);

        let png = pixmap.encode_png().context("failed to encode PNG")?;

        Ok(RenderedImage {
            png,
            css_width: request.width,
            css_height,
            pixel_width,
            pixel_height,
            scale: request.scale,
            content_css_width: ceil_to_u32(layout.rect.width)?,
            console_messages: diagnostics.console_messages,
            warnings: diagnostics.warnings,
        })
    }

    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf> {
        let rendered = self.render_png(request)?;
        let pdf = raster_pdf_from_png(&rendered)?;
        Ok(RenderedPdf {
            pdf,
            css_width: rendered.css_width,
            css_height: rendered.css_height,
            pixel_width: rendered.pixel_width,
            pixel_height: rendered.pixel_height,
            scale: rendered.scale,
            console_messages: rendered.console_messages,
            warnings: rendered.warnings,
        })
    }
}

fn push_unique_case_insensitive(values: &mut Vec<String>, value: String) {
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        values.push(value);
    }
}

fn load_web_fonts_from_html(
    html: &str,
    policy: &ResourcePolicy,
    db: &mut fontdb::Database,
    diagnostics: &mut RenderDiagnostics,
) -> Vec<WebFontFace> {
    let mut css_blocks: Vec<FontCssBlock> = style_blocks(html)
        .into_iter()
        .map(|css| FontCssBlock {
            css: css.to_string(),
            source: FontCssSource::InlineOrImport,
        })
        .collect();
    let mut imported_urls = Vec::new();

    for stylesheet_url in stylesheet_link_urls(html) {
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
            if imported_urls
                .iter()
                .any(|loaded: &String| loaded.eq_ignore_ascii_case(&import_url))
            {
                continue;
            }
            imported_urls.push(import_url.clone());
            match load_stylesheet(&import_url, policy) {
                Ok(css) => css_blocks.push(FontCssBlock { css, source }),
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
            let descriptor_weight = declaration_value(&declarations, "font-weight")
                .and_then(parse_font_face_weight)
                .unwrap_or(FontWeight::NORMAL);
            if let Some(source) = loaded_font_sources
                .iter()
                .find(|loaded| loaded.url.eq_ignore_ascii_case(&candidate.url))
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

            match load_resource_bytes(&candidate.url, policy)
                .and_then(|bytes| decode_font_resource(&bytes, &candidate))
            {
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
                                    .any(|source| source.url.eq_ignore_ascii_case(&candidate.url))
                                {
                                    loaded_font_sources.push(LoadedFontSource {
                                        url: candidate.url.clone(),
                                        actual_family,
                                    });
                                }
                            }
                        }
                        loaded_fonts += 1;
                    } else {
                        diagnostics.push_warning(
                            RenderWarning::new(
                                RenderWarningCode::WebFontLoadFailed,
                                format!("web font {family} did not contain a loadable face"),
                            )
                            .with_node("@font-face")
                            .with_property("font-family", family.clone())
                            .with_url(candidate.url.clone()),
                        );
                    }
                }
                Err(error) => diagnostics.push_warning(
                    RenderWarning::new(
                        RenderWarningCode::WebFontLoadFailed,
                        format!(
                            "failed to load web font {family} from {}: {error}",
                            candidate.url
                        ),
                    )
                    .with_node("@font-face")
                    .with_property("font-family", family)
                    .with_url(candidate.url),
                ),
            }
        }
    }

    web_font_faces
}

fn load_stylesheet(url: &str, policy: &ResourcePolicy) -> Result<String> {
    load_resource_bytes(url, policy)
        .and_then(|bytes| String::from_utf8(bytes).context("stylesheet is not UTF-8"))
}

fn stylesheet_link_urls(html: &str) -> Vec<String> {
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

fn linked_stylesheet_fonts_are_supported(css: &str) -> bool {
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

fn font_face_covers_basic_latin(declarations: &[(String, String)]) -> bool {
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

fn document_base_url(document: &NodeRef) -> Option<Url> {
    let base = find_first_tag(document, "base")?;
    let href = attr(&base, "href")?;
    Url::parse(&href).ok()
}

fn normalize_resource_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    }
}

struct LayoutEngine<'a> {
    font_system: &'a mut FontSystem,
    resources: ResourcePolicy,
    available_font_families: Vec<String>,
    web_font_faces: Vec<WebFontFace>,
    warnings: Vec<RenderWarning>,
}

impl<'a> LayoutEngine<'a> {
    fn new(
        font_system: &'a mut FontSystem,
        resources: ResourcePolicy,
        available_font_families: Vec<String>,
        web_font_faces: Vec<WebFontFace>,
    ) -> Self {
        Self {
            font_system,
            resources,
            available_font_families,
            web_font_faces,
            warnings: Vec::new(),
        }
    }

    fn style_for_node(&self, node: &NodeRef, parent: &Style) -> Style {
        let mut style = style_for_node_with_fonts(
            node,
            parent,
            &self.available_font_families,
            &self.web_font_faces,
        );
        self.load_style_background(&mut style);
        style
    }

    fn load_style_background(&self, style: &mut Style) {
        let Some(src) = style.background_image_src.as_deref() else {
            return;
        };
        if src.is_empty() {
            return;
        }
        if let Ok(image) = load_image(src, &self.resources) {
            style.background_image = Some(image);
        }
    }

    fn layout_document(&mut self, document: &NodeRef, width: u32) -> Result<LayoutBox> {
        let root_node = find_first_tag(document, "body").unwrap_or_else(|| document.clone());
        let initial = Style::initial();
        let parent_style = if element_tag(&root_node).as_deref() == Some("body") {
            find_first_tag(document, "html")
                .map(|html| self.style_for_node(&html, &initial))
                .unwrap_or_else(|| initial.clone())
        } else {
            initial.clone()
        };
        let root_style = if root_node.as_element().is_some() {
            self.style_for_node(&root_node, &parent_style)
        } else {
            initial
        };

        let viewport_width = width as f32;
        let layout_width = root_style
            .resolve_width(viewport_width)
            .unwrap_or(viewport_width)
            .max(1.0);
        let content = self.layout_children(&root_node, &root_style, 0.0, 0.0, layout_width, 0)?;

        Ok(LayoutBox {
            kind: LayoutKind::Block,
            rect: Rect::new(0.0, 0.0, layout_width, content.advance.max(1.0)),
            style: root_style,
            children: content.children,
        })
    }

    fn layout_children(
        &mut self,
        node: &NodeRef,
        style: &Style,
        x: f32,
        y: f32,
        width: f32,
        depth: usize,
    ) -> Result<LayoutChildren> {
        if depth > MAX_LAYOUT_DEPTH {
            self.push_warning(RenderWarning::new(
                RenderWarningCode::LayoutLimitReached,
                "maximum layout depth reached; truncated nested content",
            ));
            return Ok(LayoutChildren::default());
        }

        let mut children = Vec::new();
        let mut cursor_y = y;
        let mut text = Vec::new();
        let parent_tag = element_tag(node);
        let mut ordered_list_index = 1usize;
        let mut previous_margin_bottom = None;
        let mut inline_row = Vec::new();
        let mut inline_row_width = 0.0;
        let mut inline_row_height = 0.0;
        let parent_line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let mut floats = Vec::new();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                let text_value = text_node.borrow();
                if !inline_row.is_empty() && text_value.chars().all(is_collapsible_whitespace) {
                    continue;
                }
                if flush_inline_row(
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    style,
                    width,
                    &mut cursor_y,
                    &mut children,
                ) {
                    previous_margin_bottom = None;
                }
                append_text_span(&mut text, &text_value, style);
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };

            if is_metadata_tag(&tag) {
                continue;
            }
            if tag == "br" {
                if flush_inline_row(
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    style,
                    width,
                    &mut cursor_y,
                    &mut children,
                ) {
                    previous_margin_bottom = None;
                }
                append_text_span(&mut text, &HARD_BREAK.to_string(), style);
                continue;
            }

            let mut child_style = self.style_for_node(&child, style);
            if parent_tag.as_deref() == Some("li") && tag == "p" {
                child_style.margin.top = 0.0;
            }
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }

            if child_style.display == Display::Inline
                && tag != "img"
                && !inline_style_has_own_box(&child_style)
                && inline_can_flatten(&child, &child_style)
            {
                append_inline_spans(&child, &child_style, &mut text);
                continue;
            }

            let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
            if self.flush_text(
                &mut text,
                style,
                text_x,
                &mut cursor_y,
                text_width,
                &mut children,
            )? {
                previous_margin_bottom = None;
            }
            let child_display = child_style.display;
            let child_float_side = child_style.float_side;
            let child_clear = child_style.clear;
            if child_clear != Clear::None {
                cursor_y = cursor_y.max(clear_float_y(&floats, child_clear));
                previous_margin_bottom = None;
            }
            let child_is_inline_flow = is_inline_flow(&tag, &child_style);
            if !child_is_inline_flow
                && flush_inline_row(
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    style,
                    width,
                    &mut cursor_y,
                    &mut children,
                )
            {
                previous_margin_bottom = None;
            }
            let list_marker = if tag == "li" {
                match child_style.list_style_type {
                    ListStyleType::None => None,
                    ListStyleType::Decimal => {
                        let marker = format!("{ordered_list_index}.");
                        ordered_list_index += 1;
                        Some(marker)
                    }
                    ListStyleType::Disc => match parent_tag.as_deref() {
                        Some("ol") => {
                            let marker = format!("{ordered_list_index}.");
                            ordered_list_index += 1;
                            Some(marker)
                        }
                        Some("ul") => Some("\u{2022}".to_string()),
                        _ => None,
                    },
                }
            } else {
                None
            };
            let flow = if let Some(marker) = list_marker {
                self.layout_list_item(
                    &child,
                    child_style,
                    marker,
                    Rect::new(x, cursor_y, width, 0.0),
                    depth + 1,
                )?
            } else {
                self.layout_element_with_style(&child, child_style, x, cursor_y, width, depth + 1)?
            };
            if let Some(flow) = flow {
                let mut flow = flow;
                let collapsible_margin_bottom = flow.collapsible_margin_bottom;
                if child_float_side != FloatSide::None {
                    let occupied_width =
                        (flow.node.rect.width + flow.node.style.margin.horizontal()).max(1.0);
                    let occupied_height = flow.advance.max(flow.node.rect.height).max(1.0);
                    let float_y = float_placement_y(&floats, x, width, cursor_y, occupied_width);
                    let (left_offset, right_offset) =
                        float_offsets_at_y(x, width, float_y, &floats);
                    let occupied_x = match child_float_side {
                        FloatSide::Left => x + left_offset,
                        FloatSide::Right => x + width - right_offset - occupied_width,
                        FloatSide::None => x,
                    };
                    let target_x = occupied_x + flow.node.style.margin.left;
                    let target_y = float_y + flow.node.style.margin.top;
                    let dx = target_x - flow.node.rect.x;
                    let dy = target_y - flow.node.rect.y;
                    translate_layout(&mut flow.node, dx, dy);
                    floats.push(PlacedFloat {
                        side: child_float_side,
                        rect: Rect::new(occupied_x, float_y, occupied_width, occupied_height),
                    });
                    previous_margin_bottom = None;
                    children.push(flow.node);
                    continue;
                }
                if child_is_inline_flow {
                    if inline_row_width > 0.0 {
                        translate_layout(&mut flow.node, inline_row_width, 0.0);
                    }
                    inline_row_width += flow.node.rect.width;
                    let baseline_descent = if flow.node.style.vertical_align
                        == VerticalAlign::Baseline
                        && inline_flow_uses_bottom_edge_baseline(&flow.node)
                    {
                        blink_font_descent_from_db(self.font_system.db(), style)
                            .unwrap_or(style.font_size * 0.25)
                    } else {
                        0.0
                    };
                    let line_advance = inline_flow_line_advance(
                        &flow.node,
                        flow.advance,
                        parent_line_height,
                        self.font_system.db(),
                    );
                    inline_row_height = inline_row_height.max(line_advance + baseline_descent);
                    inline_row.push(flow.node);
                    previous_margin_bottom = None;
                    continue;
                }
                align_table_child_to_parent_text(&mut flow.node, style, x, width);
                align_image_child_to_legacy_align(&mut flow.node, style, x, width);
                let margin_overlap = if can_collapse_sibling_margin(child_display) {
                    previous_margin_bottom
                        .map(|previous: f32| previous.min(flow.node.style.margin.top))
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                if margin_overlap > 0.0 {
                    translate_layout(&mut flow.node, 0.0, -margin_overlap);
                }
                cursor_y += flow.advance - margin_overlap;
                previous_margin_bottom =
                    can_collapse_sibling_margin(child_display).then_some(collapsible_margin_bottom);
                children.push(flow.node);
            }
        }

        let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
        if self.flush_text(
            &mut text,
            style,
            text_x,
            &mut cursor_y,
            text_width,
            &mut children,
        )? {
            previous_margin_bottom = None;
        }
        if flush_inline_row(
            &mut inline_row,
            &mut inline_row_width,
            &mut inline_row_height,
            style,
            width,
            &mut cursor_y,
            &mut children,
        ) {
            previous_margin_bottom = None;
        }
        let float_bottom = floats
            .iter()
            .map(|float| float.rect.y + float.rect.height)
            .fold(cursor_y, f32::max);
        Ok(LayoutChildren {
            children,
            advance: float_bottom - y,
            trailing_collapsible_margin: previous_margin_bottom.unwrap_or(0.0),
        })
    }

    fn flush_text(
        &mut self,
        text: &mut Vec<TextSpan>,
        style: &Style,
        x: f32,
        cursor_y: &mut f32,
        width: f32,
        children: &mut Vec<LayoutBox>,
    ) -> Result<bool> {
        let normalized = normalize_text_spans(text);
        text.clear();

        if normalized.is_empty() {
            return Ok(false);
        }

        let plain_text = spans_text(&normalized);
        let matches_parent_style = text_spans_match_style(&normalized, style);
        let height = if matches_parent_style {
            self.measure_text_height(&plain_text, width, style)?
        } else {
            self.measure_rich_text_height(&normalized, width, style)?
        };
        let kind = if matches_parent_style {
            LayoutKind::Text(plain_text)
        } else {
            LayoutKind::RichText(normalized)
        };
        children.push(LayoutBox {
            kind,
            rect: Rect::new(x, *cursor_y, width, height),
            style: style.clone(),
            children: Vec::new(),
        });
        *cursor_y += height;
        Ok(true)
    }

    fn layout_element_with_style(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let Some(tag) = element_tag(node) else {
            return Ok(None);
        };

        if tag == "img" {
            return Ok(Some(self.layout_image(node, style, x, y, containing_width)));
        }
        if tag == "hr" {
            return Ok(Some(self.layout_hr(style, x, y, containing_width)));
        }

        match style.display {
            Display::None => Ok(None),
            Display::Flex => self.layout_flex(node, style, x, y, containing_width, depth),
            Display::Table => self.layout_table(node, style, x, y, containing_width, depth),
            Display::Inline => {
                if inline_style_has_own_box(&style) {
                    self.layout_inline_block(node, style, x, y, containing_width, depth)
                } else {
                    let mut inline_style = style;
                    inline_style.width = None;
                    inline_style.min_width = None;
                    inline_style.max_width = None;
                    self.layout_block(node, inline_style, x, y, containing_width, depth)
                }
            }
            Display::InlineBlock => {
                self.layout_inline_block(node, style, x, y, containing_width, depth)
            }
            _ => self.layout_block(node, style, x, y, containing_width, depth),
        }
    }

    fn layout_inline_block(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let explicit_width = style.resolve_width(containing_width);
        let max_outer_width = explicit_width
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let max_inner_width = style.inner_width_for_outer(max_outer_width);
        let preferred_inner_width = if explicit_width.is_some() {
            match style.box_sizing {
                BoxSizing::BorderBox => max_inner_width,
                BoxSizing::ContentBox => explicit_width.unwrap_or(max_inner_width).max(1.0),
            }
        } else {
            self.preferred_content_width(node, &style, max_inner_width)?
                .min(max_inner_width)
                .max(1.0)
        };

        let rect_x = x + style.horizontal_offset(containing_width, max_outer_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let mut content =
            self.layout_children(node, &style, inner_x, inner_y, preferred_inner_width, depth)?;
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let rect_width = if explicit_width.is_some() {
            max_outer_width
        } else {
            (preferred_inner_width + style.padding.horizontal() + style.border.horizontal())
                .max(1.0)
        };
        let rect_height = (content.advance + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, rect_width, rect_height),
            &mut content.children,
            depth,
        )?;

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, rect_width, rect_height),
                style,
                children: content.children,
            },
        }))
    }

    fn layout_block(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let outer_width = style
            .resolve_width(containing_width)
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, outer_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let inner_width = style.inner_width_for_outer(outer_width);

        let mut content =
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth)?;
        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let collapsed_trailing_margin = if block_allows_trailing_margin_collapse(&style) {
            content.trailing_collapsible_margin.min(content.advance)
        } else {
            0.0
        };
        let content_box_height = (content.advance - collapsed_trailing_margin).max(0.0);
        let rect_height = (content_box_height + style.padding.vertical() + style.border.vertical())
            .max(min_height)
            .max(0.0);
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, outer_width, rect_height),
            &mut content.children,
            depth,
        )?;

        let collapsed_bottom_margin = style.margin.bottom.max(collapsed_trailing_margin);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsed_bottom_margin,
            collapsible_margin_bottom: collapsed_bottom_margin,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                children: content.children,
            },
        }))
    }

    fn layout_flex(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let outer_width = style
            .resolve_width(containing_width)
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, outer_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let inner_width = style.inner_width_for_outer(outer_width);
        let explicit_height = style.resolve_height(0.0);
        let explicit_inner_height = explicit_height
            .map(|height| (height - style.padding.vertical() - style.border.vertical()).max(0.0));

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        taffy.disable_rounding();
        let mut child_nodes: Vec<TaffyNodeId> = Vec::new();
        let mut flex_items: Vec<(TaffyNodeId, LayoutBox)> = Vec::new();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                let text_value = text_node.borrow();
                if text_value.chars().all(is_collapsible_whitespace) {
                    continue;
                }
                let normalized =
                    normalize_text_spans(&[TextSpan::from_style(text_value.to_string(), &style)]);
                let plain_text = spans_text(&normalized);
                let matches_parent_style = text_spans_match_style(&normalized, &style);
                let item_width = if matches_parent_style {
                    self.measure_text_width(&plain_text, &style)
                } else {
                    self.measure_rich_text_width(&normalized, &style)
                }
                .max(1.0);
                let item_height = if matches_parent_style {
                    self.measure_text_height(&plain_text, item_width, &style)?
                } else {
                    self.measure_rich_text_height(&normalized, item_width, &style)?
                };
                let kind = if matches_parent_style {
                    LayoutKind::Text(plain_text)
                } else {
                    LayoutKind::RichText(normalized)
                };
                let item = LayoutBox {
                    kind,
                    rect: Rect::new(0.0, 0.0, item_width, item_height),
                    style: style.clone(),
                    children: Vec::new(),
                };
                let node_id =
                    taffy.new_leaf(taffy_leaf_style(&item.style, item_width, item_height))?;
                child_nodes.push(node_id);
                flex_items.push((node_id, item));
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }

            let child_style = self.style_for_node(&child, &style);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }
            let Some(flow) = self.layout_element_with_style(
                &child,
                child_style,
                0.0,
                0.0,
                inner_width,
                depth + 1,
            )?
            else {
                continue;
            };

            let mut item = flow.node;
            let item_width = item.rect.width.max(1.0);
            let item_height = item.rect.height.max(1.0);
            let item_x = item.rect.x;
            let item_y = item.rect.y;
            translate_layout(&mut item, -item_x, -item_y);
            let node_id = taffy.new_leaf(taffy_leaf_style(&item.style, item_width, item_height))?;
            child_nodes.push(node_id);
            flex_items.push((node_id, item));
        }

        let root = taffy.new_with_children(
            taffy_flex_container_style(&style, inner_width, explicit_inner_height),
            &child_nodes,
        )?;
        taffy.compute_layout(
            root,
            TaffySize {
                width: AvailableSpace::Definite(inner_width),
                height: AvailableSpace::MaxContent,
            },
        )?;
        let root_layout = *taffy.layout(root)?;
        let mut children = Vec::with_capacity(flex_items.len());
        for (node_id, mut item) in flex_items {
            let layout = *taffy.layout(node_id)?;
            translate_layout(
                &mut item,
                inner_x + layout.location.x,
                inner_y + layout.location.y,
            );
            children.push(item);
        }

        let min_height = explicit_height.unwrap_or(0.0);
        let rect_height =
            (root_layout.size.height + style.padding.vertical() + style.border.vertical())
                .max(min_height)
                .max(0.0);
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, outer_width, rect_height),
            &mut children,
            depth,
        )?;

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                children,
            },
        }))
    }

    fn layout_table(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let grid = build_table_grid(node);
        if grid.rows.is_empty() {
            return self.layout_block(node, style, x, y, containing_width, depth);
        }

        let max_table_width = (containing_width - style.margin.horizontal()).max(1.0);
        let spacing = if style.border_collapse == BorderCollapse::Collapse {
            0.0
        } else {
            style.cell_spacing.max(0.0)
        };
        let table_width = if let Some(width) = style.resolve_width(containing_width) {
            style.outer_width_for_declared(width)
        } else {
            self.preferred_table_outer_width(&grid, &style, max_table_width, spacing)?
                .min(max_table_width)
        }
        .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, table_width);
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border.left + style.padding.left;
        let content_y = rect_y + style.border.top + style.padding.top;
        let content_width = style.inner_width_for_outer(table_width);

        let mut row_boxes = Vec::new();
        let mut row_y = content_y;
        let column_widths =
            self.resolve_table_column_widths(&grid, &style, content_width, spacing)?;

        for row in grid.rows {
            let row_style = self.style_for_node(&row.node, &style);
            if row_style.display == Display::None {
                continue;
            }
            if row.cells.is_empty() {
                continue;
            }

            let mut cell_boxes = Vec::with_capacity(row.cells.len());
            let mut row_height: f32 = 0.0;

            for cell in row.cells {
                let mut cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
                cell_style.apply_table_cell_padding(style.cell_padding);

                let cell_x = content_x + column_offset(&column_widths, cell.col, spacing);
                let cell_width = spanned_width(&column_widths, cell.col, cell.colspan, spacing);
                let cell_inner_x = cell_x + cell_style.border.left + cell_style.padding.left;
                let cell_inner_y = row_y + cell_style.border.top + cell_style.padding.top;
                let cell_inner_width =
                    (cell_width - cell_style.padding.horizontal() - cell_style.border.horizontal())
                        .max(1.0);
                let content = self.layout_children(
                    &cell.node,
                    &cell_style,
                    cell_inner_x,
                    cell_inner_y,
                    cell_inner_width,
                    depth + 1,
                )?;
                let explicit_height = cell_style.resolve_height(0.0).unwrap_or(0.0);
                let natural_cell_height = (content.advance
                    + cell_style.padding.vertical()
                    + cell_style.border.vertical())
                .max(1.0);
                let cell_height = natural_cell_height.max(explicit_height).max(1.0);
                row_height = row_height.max(cell_height);
                cell_boxes.push((
                    cell.node.clone(),
                    LayoutBox {
                        kind: LayoutKind::Cell,
                        rect: Rect::new(cell_x, row_y, cell_width, cell_height),
                        style: cell_style,
                        children: content.children,
                    },
                    natural_cell_height,
                ));
            }

            if cell_boxes.is_empty() {
                continue;
            }

            for (cell_node, cell, natural_cell_height) in &mut cell_boxes {
                let delta = (row_height - *natural_cell_height).max(0.0);
                let offset_y = match cell.style.vertical_align {
                    VerticalAlign::Baseline | VerticalAlign::Top => 0.0,
                    VerticalAlign::Middle => delta / 2.0,
                    VerticalAlign::Bottom => delta,
                };
                if offset_y > 0.0 {
                    translate_layout_children(cell, 0.0, offset_y);
                }
                cell.rect.height = row_height;
                self.append_absolute_children(
                    cell_node,
                    &cell.style,
                    cell.rect,
                    &mut cell.children,
                    depth + 1,
                )?;
            }

            row_boxes.push(LayoutBox {
                kind: LayoutKind::Row,
                rect: Rect::new(content_x, row_y, content_width, row_height),
                style: row_style,
                children: cell_boxes.into_iter().map(|(_, cell, _)| cell).collect(),
            });
            row_y += row_height + spacing;
        }

        let content_height = (row_y - content_y - spacing).max(0.0);
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let table_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + table_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                children: row_boxes,
            },
        }))
    }

    fn append_absolute_children(
        &mut self,
        node: &NodeRef,
        parent_style: &Style,
        containing_rect: Rect,
        children: &mut Vec<LayoutBox>,
        depth: usize,
    ) -> Result<()> {
        let mut absolute_children = Vec::new();
        for child in node.children() {
            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            let mut child_style = self.style_for_node(&child, parent_style);
            if child_style.display == Display::None
                || !matches!(child_style.position, Position::Absolute | Position::Fixed)
            {
                continue;
            }
            let Some(flow) =
                self.layout_absolute_child(&child, &mut child_style, containing_rect, depth + 1)?
            else {
                continue;
            };
            absolute_children.push(flow.node);
        }
        if !absolute_children.is_empty() {
            children.splice(0..0, absolute_children);
        }
        Ok(())
    }

    fn layout_absolute_child(
        &mut self,
        child: &NodeRef,
        child_style: &mut Style,
        containing_rect: Rect,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let left = child_style
            .inset_left
            .and_then(|length| length.resolve(containing_rect.width));
        let right = child_style
            .inset_right
            .and_then(|length| length.resolve(containing_rect.width));
        let top = child_style
            .inset_top
            .and_then(|length| length.resolve(containing_rect.height));
        let bottom = child_style
            .inset_bottom
            .and_then(|length| length.resolve(containing_rect.height));

        if child_style.width.is_none() {
            if let (Some(left), Some(right)) = (left, right) {
                child_style.width =
                    Some(Length::Px((containing_rect.width - left - right).max(0.0)));
            }
        }
        if child_style.height.is_none() {
            if let (Some(top), Some(bottom)) = (top, bottom) {
                child_style.height =
                    Some(Length::Px((containing_rect.height - top - bottom).max(0.0)));
            }
        }

        let resolved_width = child_style
            .resolve_width(containing_rect.width)
            .map(|width| child_style.outer_width_for_declared(width))
            .unwrap_or(containing_rect.width)
            .max(1.0);
        let x = if let Some(left) = left {
            containing_rect.x + left
        } else if let Some(right) = right {
            containing_rect.x + containing_rect.width - right - resolved_width
        } else {
            containing_rect.x
        };

        let resolved_height = child_style.resolve_height(containing_rect.height);
        let y = if let Some(top) = top {
            containing_rect.y + top
        } else if let (Some(bottom), Some(height)) = (bottom, resolved_height) {
            containing_rect.y + containing_rect.height - bottom - height
        } else {
            containing_rect.y
        };

        self.layout_element_with_style(child, child_style.clone(), x, y, resolved_width, depth)
    }

    fn preferred_table_outer_width(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        max_outer_width: f32,
        spacing: f32,
    ) -> Result<f32> {
        let count = grid.column_count.max(1);
        let max_content_width = table_style.inner_width_for_outer(max_outer_width);
        let available =
            (max_content_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = vec![0.0_f32; count];

        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width.and_then(|width| width.resolve(available)) {
                widths[col] = widths[col].max(width.max(1.0));
            }
        }

        for row in &grid.rows {
            let row_style = self.style_for_node(&row.node, table_style);
            if row_style.display == Display::None {
                continue;
            }
            for cell in &row.cells {
                let mut cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
                cell_style.apply_table_cell_padding(table_style.cell_padding);

                let preferred = if let Some(width) =
                    cell_style.width.and_then(|width| width.resolve(available))
                {
                    cell_style.outer_width_for_declared(width)
                } else {
                    self.preferred_content_width(&cell.node, &cell_style, available)?
                        + cell_style.padding.horizontal()
                        + cell_style.border.horizontal()
                }
                .max(0.0);
                let per_col = ((preferred - spacing * cell.colspan.saturating_sub(1) as f32)
                    / cell.colspan as f32)
                    .max(0.0);
                for col in cell.col..cell.col + cell.colspan {
                    if col < widths.len() {
                        widths[col] = widths[col].max(per_col);
                    }
                }
            }
        }

        let content_width = widths.iter().sum::<f32>() + spacing * count.saturating_sub(1) as f32;
        Ok(
            (content_width + table_style.padding.horizontal() + table_style.border.horizontal())
                .max(1.0),
        )
    }

    fn resolve_table_column_widths(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        table_width: f32,
        spacing: f32,
    ) -> Result<Vec<f32>> {
        let count = grid.column_count.max(1);
        let available = (table_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = vec![None; count];
        let mut minimums = vec![0.0_f32; count];
        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width.and_then(|width| width.resolve(available)) {
                let width = width.max(1.0);
                widths[col] = Some(width);
            }
        }

        if table_style.table_layout_fixed {
            for row in &grid.rows {
                let row_style = self.style_for_node(&row.node, table_style);
                if row_style.display == Display::None {
                    continue;
                }
                for cell in &row.cells {
                    let mut style = self.style_for_node(&cell.node, &row_style);
                    if style.display == Display::None {
                        continue;
                    }
                    style.apply_table_cell_padding(table_style.cell_padding);
                    if let Some(width) = style.width.and_then(|width| width.resolve(available)) {
                        let outer_width = style.outer_width_for_declared(width);
                        let per_col = ((outer_width
                            - spacing * cell.colspan.saturating_sub(1) as f32)
                            / cell.colspan as f32)
                            .max(1.0);
                        for col in cell.col..cell.col + cell.colspan {
                            if col < widths.len() {
                                widths[col] = Some(widths[col].unwrap_or(0.0).max(per_col));
                            }
                        }
                    }
                }
                break;
            }

            return Ok(distribute_fixed_table_column_widths(widths, available));
        }

        for row in &grid.rows {
            let row_style = self.style_for_node(&row.node, table_style);
            if row_style.display == Display::None {
                continue;
            }
            for cell in &row.cells {
                let mut style = self.style_for_node(&cell.node, &row_style);
                if style.display == Display::None {
                    continue;
                }
                style.apply_table_cell_padding(table_style.cell_padding);
                if let Some(width) = style.width.and_then(|width| width.resolve(available)) {
                    let outer_width = style.outer_width_for_declared(width);
                    let per_col = ((outer_width - spacing * cell.colspan.saturating_sub(1) as f32)
                        / cell.colspan as f32)
                        .max(1.0);
                    for col in cell.col..cell.col + cell.colspan {
                        if col < widths.len() {
                            widths[col] = Some(widths[col].unwrap_or(0.0).max(per_col));
                        }
                    }
                } else if table_cell_is_spacer(&cell.node) {
                    let preferred = (self
                        .preferred_content_width(&cell.node, &style, available)?
                        + style.padding.horizontal()
                        + style.border.horizontal())
                    .max(0.0);
                    let per_col = ((preferred - spacing * cell.colspan.saturating_sub(1) as f32)
                        / cell.colspan as f32)
                        .max(0.0);
                    for col in cell.col..cell.col + cell.colspan {
                        if col < minimums.len() {
                            minimums[col] = minimums[col].max(per_col);
                        }
                    }
                }
            }
        }

        let mut fixed_total: f32 = widths.iter().flatten().sum();
        let flexible_minimum: f32 = widths
            .iter()
            .zip(&minimums)
            .filter_map(|(width, minimum)| width.is_none().then_some(*minimum))
            .sum();

        if fixed_total + flexible_minimum > available && fixed_total > 0.0 {
            let target_fixed = (available - flexible_minimum).max(0.0);
            let scale = target_fixed / fixed_total;
            for width in widths.iter_mut().flatten() {
                *width = (*width * scale).max(1.0);
            }
        }

        fixed_total = widths.iter().flatten().sum();
        let flexible = widths.iter().filter(|width| width.is_none()).count();
        let flexible_width = if flexible > 0 {
            ((available - fixed_total).max(flexible as f32)) / flexible as f32
        } else {
            0.0
        };

        Ok(widths
            .into_iter()
            .zip(minimums)
            .map(|(width, minimum)| {
                width
                    .unwrap_or_else(|| flexible_width.max(minimum))
                    .max(1.0)
            })
            .collect())
    }

    fn layout_image(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
    ) -> FlowBox {
        let image = attr(node, "src").and_then(|src| match load_image(&src, &self.resources) {
            Ok(image) => Some(image),
            Err(error) => {
                self.push_warning(
                    RenderWarning::new(
                        RenderWarningCode::ImageLoadFailed,
                        format!("failed to load image {src}: {error}; left image box empty"),
                    )
                    .with_node("img")
                    .with_url(src),
                );
                None
            }
        });
        let natural_width = image.as_ref().map_or(0.0, |image| image.width as f32);
        let natural_height = image.as_ref().map_or(0.0, |image| image.height as f32);
        let min_size = if image.is_some() { 1.0 } else { 0.0 };
        let declared_width = style.resolve_width(containing_width).or_else(|| {
            if style.width_auto {
                None
            } else {
                attr(node, "width").and_then(|value| {
                    parse_length(&value).and_then(|length| length.resolve(containing_width))
                })
            }
        });
        let declared_height = style
            .resolve_height(declared_width.unwrap_or(containing_width))
            .or_else(|| {
                if style.height_auto {
                    None
                } else {
                    attr(node, "height").and_then(|value| {
                        parse_length(&value).and_then(|length| {
                            length.resolve(declared_width.unwrap_or(containing_width))
                        })
                    })
                }
            });
        let mut width = declared_width
            .or_else(|| {
                declared_height.and_then(|height| {
                    (natural_height > 0.0).then_some((height / natural_height) * natural_width)
                })
            })
            .unwrap_or(natural_width.min(containing_width))
            .max(min_size);
        width = style.constrain_width(width, containing_width).max(min_size);
        let height = declared_height
            .or_else(|| {
                if natural_width > 0.0 {
                    Some((width / natural_width) * natural_height)
                } else {
                    None
                }
            })
            .unwrap_or(natural_height)
            .max(min_size);

        let collapsible_margin_bottom = style.margin.bottom;
        FlowBox {
            advance: style.margin.top + height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            node: LayoutBox {
                kind: LayoutKind::Image(image),
                rect: Rect::new(
                    x + style.horizontal_offset(containing_width, width),
                    y + style.margin.top,
                    width,
                    height,
                ),
                style,
                children: Vec::new(),
            },
        }
    }

    fn layout_list_item(
        &mut self,
        node: &NodeRef,
        style: Style,
        marker: String,
        flow_rect: Rect,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let outer_width = style
            .resolve_width(flow_rect.width)
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(flow_rect.width - style.margin.horizontal())
            .max(1.0);
        let rect_x = flow_rect.x + style.horizontal_offset(flow_rect.width, outer_width);
        let rect_y = flow_rect.y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let inner_width = style.inner_width_for_outer(outer_width);
        let marker_width = (style.font_size * 1.5).max(18.0).min(inner_width);
        let content_x = inner_x;
        let content_width = inner_width;

        let content =
            self.layout_children(node, &style, content_x, inner_y, content_width, depth)?;
        let mut marker_style = style.clone();
        marker_style.text_align = TextAlign::Right;
        let mut children = content.children;
        children.insert(
            0,
            LayoutBox {
                kind: LayoutKind::Text(marker),
                rect: Rect::new(
                    inner_x - marker_width,
                    inner_y,
                    (marker_width - 6.0).max(1.0),
                    resolved_line_height_from_db(self.font_system.db(), &style),
                ),
                style: marker_style,
                children: Vec::new(),
            },
        );

        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let line_height = resolved_line_height_from_db(self.font_system.db(), &style);
        let collapsed_trailing_margin = if block_allows_trailing_margin_collapse(&style) {
            content.trailing_collapsible_margin.min(content.advance)
        } else {
            0.0
        };
        let content_box_height = (content.advance - collapsed_trailing_margin).max(0.0);
        let rect_height = (content_box_height.max(line_height)
            + style.padding.vertical()
            + style.border.vertical())
        .max(min_height)
        .max(0.0);
        let collapsed_bottom_margin = style.margin.bottom.max(collapsed_trailing_margin);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsed_bottom_margin,
            collapsible_margin_bottom: collapsed_bottom_margin,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                children,
            },
        }))
    }

    fn layout_hr(&mut self, style: Style, x: f32, y: f32, containing_width: f32) -> FlowBox {
        let width = style
            .resolve_width(containing_width)
            .unwrap_or(containing_width);
        let content_height = style.resolve_height(0.0).unwrap_or(0.0).max(0.0);
        let height = (content_height + style.border.vertical()).max(1.0);

        let collapsible_margin_bottom = style.margin.bottom;
        FlowBox {
            advance: style.margin.top + height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
                style,
                children: Vec::new(),
            },
        }
    }

    fn measure_text_height(&mut self, text: &str, width: f32, style: &Style) -> Result<f32> {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(self.font_system, Some(width.max(1.0)), None);
        buffer.set_text(
            self.font_system,
            text,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(style.text_align.to_cosmic()),
        );

        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            height = height.max(run.line_top + run.line_height);
        }
        Ok(height.max(line_height))
    }

    fn measure_rich_text_height(
        &mut self,
        spans: &[TextSpan],
        width: f32,
        style: &Style,
    ) -> Result<f32> {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let rich_spans = rich_text_style_spans(spans, self.font_system.db(), 1.0, style);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(self.font_system, Some(width.max(1.0)), None);
        buffer.set_rich_text(
            self.font_system,
            rich_spans,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(style.text_align.to_cosmic()),
        );

        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            height = height.max(run.line_top + run.line_height);
        }
        Ok(height.max(line_height))
    }

    fn measure_text_width(&mut self, text: &str, style: &Style) -> f32 {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, Wrap::None);
        buffer.set_size(self.font_system, None, None);
        buffer.set_text(
            self.font_system,
            text,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(TextAlignMode::Left),
        );

        let mut width: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
        }
        width.ceil()
    }

    fn measure_rich_text_width(&mut self, spans: &[TextSpan], style: &Style) -> f32 {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let rich_spans = rich_text_style_spans(spans, self.font_system.db(), 1.0, style);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, Wrap::None);
        buffer.set_size(self.font_system, None, None);
        buffer.set_rich_text(
            self.font_system,
            rich_spans,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(TextAlignMode::Left),
        );

        let mut width: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
        }
        width.ceil()
    }

    fn preferred_content_width(
        &mut self,
        node: &NodeRef,
        style: &Style,
        containing_width: f32,
    ) -> Result<f32> {
        let mut max_width: f32 = 0.0;
        let mut inline_text = String::new();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                append_text(&mut inline_text, &text_node.borrow());
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            if tag == "br" {
                let text = normalize_text(&inline_text);
                inline_text.clear();
                if !text.is_empty() {
                    max_width = max_width.max(self.measure_text_width(
                        &apply_text_transform(&text, style.text_transform),
                        style,
                    ));
                }
                continue;
            }

            let child_style = self.style_for_node(&child, style);
            if child_style.display == Display::None {
                continue;
            }

            if child_style.display == Display::Inline
                && tag != "img"
                && inline_can_flatten(&child, &child_style)
            {
                append_text(&mut inline_text, &text_content(&child));
                continue;
            }

            let text = normalize_text(&inline_text);
            inline_text.clear();
            if !text.is_empty() {
                max_width =
                    max_width.max(self.measure_text_width(
                        &apply_text_transform(&text, style.text_transform),
                        style,
                    ));
            }

            let child_width = if tag == "img" {
                self.preferred_image_width(&child, &child_style, containing_width)
            } else if child_style.display == Display::InlineBlock {
                self.preferred_content_width(&child, &child_style, containing_width)?
                    + child_style.padding.horizontal()
                    + child_style.border.horizontal()
            } else {
                child_style
                    .resolve_width(containing_width)
                    .map(|width| child_style.outer_width_for_declared(width))
                    .unwrap_or(
                        self.preferred_content_width(&child, &child_style, containing_width)?
                            + child_style.padding.horizontal()
                            + child_style.border.horizontal(),
                    )
            };
            max_width = max_width.max(child_width);
        }

        let text = normalize_text(&inline_text);
        if !text.is_empty() {
            max_width = max_width.max(
                self.measure_text_width(&apply_text_transform(&text, style.text_transform), style),
            );
        }
        Ok(max_width.max(1.0))
    }

    fn preferred_image_width(&self, node: &NodeRef, style: &Style, containing_width: f32) -> f32 {
        style
            .resolve_width(containing_width)
            .or_else(|| {
                attr(node, "width").and_then(|value| {
                    parse_length(&value).and_then(|length| length.resolve(containing_width))
                })
            })
            .unwrap_or_else(|| {
                attr(node, "src")
                    .and_then(|src| load_image(&src, &self.resources).ok())
                    .map_or(0.0, |image| image.width as f32)
                    .min(containing_width)
            })
            .max(0.0)
    }

    fn push_warning(&mut self, warning: RenderWarning) {
        if self.warnings.len() < api::MAX_RENDER_WARNINGS {
            self.warnings.push(warning);
        }
    }
}

fn taffy_flex_container_style(
    style: &Style,
    inner_width: f32,
    inner_height: Option<f32>,
) -> TaffyStyle {
    TaffyStyle {
        display: TaffyDisplay::Flex,
        size: TaffySize {
            width: taffy_length(inner_width),
            height: inner_height.map_or_else(taffy_auto, taffy_length),
        },
        flex_direction: taffy_flex_direction(style.flex_direction),
        flex_wrap: taffy_flex_wrap(style.flex_wrap),
        justify_content: Some(taffy_justify_content(style.justify_content)),
        align_items: Some(taffy_align_items(style.align_items)),
        gap: TaffySize {
            width: taffy_length(style.column_gap),
            height: taffy_length(style.row_gap),
        },
        ..Default::default()
    }
}

fn float_adjusted_line(x: f32, width: f32, y: f32, floats: &[PlacedFloat]) -> (f32, f32) {
    let (left_offset, right_offset) = float_offsets_at_y(x, width, y, floats);
    let line_x = x + left_offset;
    let line_width = (width - left_offset - right_offset).max(1.0);
    (line_x, line_width)
}

fn float_offsets_at_y(x: f32, width: f32, y: f32, floats: &[PlacedFloat]) -> (f32, f32) {
    let mut left_offset: f32 = 0.0;
    let mut right_offset: f32 = 0.0;
    for float in floats.iter().filter(|float| float_intersects_y(float, y)) {
        match float.side {
            FloatSide::Left => left_offset = left_offset.max(float.rect.x + float.rect.width - x),
            FloatSide::Right => right_offset = right_offset.max(x + width - float.rect.x),
            FloatSide::None => {}
        }
    }
    (left_offset.min(width), right_offset.min(width))
}

fn float_placement_y(floats: &[PlacedFloat], x: f32, width: f32, y: f32, needed_width: f32) -> f32 {
    let mut candidate_y = y;
    loop {
        let (left_offset, right_offset) = float_offsets_at_y(x, width, candidate_y, floats);
        if width - left_offset - right_offset >= needed_width {
            return candidate_y;
        }
        let Some(next_y) = floats
            .iter()
            .filter(|float| float_intersects_y(float, candidate_y))
            .map(|float| float.rect.y + float.rect.height)
            .min_by(|a, b| a.total_cmp(b))
        else {
            return candidate_y;
        };
        if next_y <= candidate_y {
            return candidate_y;
        }
        candidate_y = next_y;
    }
}

fn clear_float_y(floats: &[PlacedFloat], clear: Clear) -> f32 {
    floats
        .iter()
        .filter(|float| match clear {
            Clear::None => false,
            Clear::Left => float.side == FloatSide::Left,
            Clear::Right => float.side == FloatSide::Right,
            Clear::Both => matches!(float.side, FloatSide::Left | FloatSide::Right),
        })
        .map(|float| float.rect.y + float.rect.height)
        .fold(0.0, f32::max)
}

fn float_intersects_y(float: &PlacedFloat, y: f32) -> bool {
    y >= float.rect.y && y < float.rect.y + float.rect.height
}

fn taffy_leaf_style(style: &Style, measured_width: f32, measured_height: f32) -> TaffyStyle {
    TaffyStyle {
        size: TaffySize {
            width: taffy_length(measured_width),
            height: taffy_length(measured_height),
        },
        min_size: TaffySize {
            width: taffy_dimension(style.min_width),
            height: taffy_dimension(style.min_height),
        },
        max_size: TaffySize {
            width: taffy_dimension(style.max_width),
            height: taffy_dimension(style.max_height),
        },
        margin: TaffyRect {
            left: if style.margin_left_auto {
                taffy_auto()
            } else {
                taffy_length(style.margin.left)
            },
            right: if style.margin_right_auto {
                taffy_auto()
            } else {
                taffy_length(style.margin.right)
            },
            top: taffy_length(style.margin.top),
            bottom: taffy_length(style.margin.bottom),
        },
        align_self: style.align_self.map(taffy_align_items),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: taffy_dimension(style.flex_basis),
        ..Default::default()
    }
}

fn taffy_dimension(length: Option<Length>) -> TaffyDimension {
    match length {
        Some(Length::Px(value)) => taffy_length(value),
        Some(Length::Percent(value)) => taffy_percent(value),
        None => taffy_auto(),
    }
}

fn taffy_flex_direction(direction: FlexDirection) -> TaffyFlexDirection {
    match direction {
        FlexDirection::Row => TaffyFlexDirection::Row,
        FlexDirection::RowReverse => TaffyFlexDirection::RowReverse,
        FlexDirection::Column => TaffyFlexDirection::Column,
        FlexDirection::ColumnReverse => TaffyFlexDirection::ColumnReverse,
    }
}

fn taffy_flex_wrap(wrap: FlexWrap) -> TaffyFlexWrap {
    match wrap {
        FlexWrap::NoWrap => TaffyFlexWrap::NoWrap,
        FlexWrap::Wrap => TaffyFlexWrap::Wrap,
        FlexWrap::WrapReverse => TaffyFlexWrap::WrapReverse,
    }
}

fn taffy_justify_content(justify: JustifyContent) -> TaffyJustifyContent {
    match justify {
        JustifyContent::FlexStart => TaffyJustifyContent::FlexStart,
        JustifyContent::FlexEnd => TaffyJustifyContent::FlexEnd,
        JustifyContent::Center => TaffyJustifyContent::Center,
        JustifyContent::SpaceBetween => TaffyJustifyContent::SpaceBetween,
        JustifyContent::SpaceAround => TaffyJustifyContent::SpaceAround,
        JustifyContent::SpaceEvenly => TaffyJustifyContent::SpaceEvenly,
    }
}

fn taffy_align_items(align: AlignItems) -> TaffyAlignItems {
    match align {
        AlignItems::FlexStart => TaffyAlignItems::FlexStart,
        AlignItems::FlexEnd => TaffyAlignItems::FlexEnd,
        AlignItems::Center => TaffyAlignItems::Center,
        AlignItems::Baseline => TaffyAlignItems::Baseline,
        AlignItems::Stretch => TaffyAlignItems::Stretch,
    }
}

struct LayoutPainter<'a> {
    pixmap: &'a mut Pixmap,
    font_system: &'a mut FontSystem,
    swash_cache: &'a mut SwashCache,
    scale: f32,
}

impl LayoutPainter<'_> {
    fn paint(&mut self, layout: &LayoutBox) {
        self.paint_with_opacity(layout, 1.0);
    }

    fn paint_with_opacity(&mut self, layout: &LayoutBox, parent_opacity: f32) {
        let opacity = (parent_opacity * layout.style.opacity).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        for shadow in layout.style.box_shadows.iter().rev() {
            if !shadow.inset {
                paint_box_shadow(
                    self.pixmap,
                    self.scale,
                    layout.rect,
                    layout.style.border_radius,
                    with_opacity(shadow.color, opacity),
                    shadow,
                );
            }
        }
        if let Some(background) = layout.style.background {
            fill_style_rect(
                self.pixmap,
                self.scale,
                layout.rect,
                with_opacity(background, opacity),
                layout.style.border_radius,
            );
        }
        if let Some(background_image) = &layout.style.background_image {
            self.paint_background_image(layout.rect, &layout.style, background_image, opacity);
        }
        if layout.style.border.max_width() > 0.0 {
            stroke_style_border(
                self.pixmap,
                self.scale,
                layout.rect,
                layout.style.border,
                with_opacity(layout.style.border_color, opacity),
                layout.style.border_style,
                layout.style.border_radius,
            );
        }

        match &layout.kind {
            LayoutKind::Text(text) => self.paint_text(layout.rect, &layout.style, text, opacity),
            LayoutKind::RichText(spans) => {
                self.paint_rich_text(layout.rect, &layout.style, spans, opacity)
            }
            LayoutKind::Image(Some(image)) => {
                self.paint_image(layout.rect, &layout.style, image, opacity)
            }
            LayoutKind::Image(None) => {}
            LayoutKind::Block | LayoutKind::Table | LayoutKind::Row | LayoutKind::Cell => {}
        }

        for child in &layout.children {
            self.paint_with_opacity(child, opacity);
        }
    }

    fn paint_text(&mut self, rect: Rect, style: &Style, text: &str, opacity: f32) {
        self.paint_text_buffer(rect, style, opacity, 0.0, |buffer, font_system| {
            buffer.set_text(
                font_system,
                text,
                &style.text_attrs(),
                Shaping::Advanced,
                Some(style.text_align.to_cosmic()),
            );
        });
    }

    fn paint_rich_text(&mut self, rect: Rect, style: &Style, spans: &[TextSpan], opacity: f32) {
        let scale = self.scale;
        let baseline_offset = rich_text_baseline_leading_offset(spans, style);
        self.paint_text_buffer(
            rect,
            style,
            opacity,
            baseline_offset,
            |buffer, font_system| {
                let rich_spans = rich_text_style_spans(spans, font_system.db(), scale, style);
                buffer.set_rich_text(
                    font_system,
                    rich_spans,
                    &style.text_attrs(),
                    Shaping::Advanced,
                    Some(style.text_align.to_cosmic()),
                );
            },
        );
    }

    fn paint_text_buffer(
        &mut self,
        rect: Rect,
        style: &Style,
        opacity: f32,
        origin_y_extra: f32,
        set_text: impl FnOnce(&mut Buffer, &mut FontSystem),
    ) {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(
            (style.font_size * self.scale).max(1.0),
            (line_height * self.scale).max(1.0),
        );
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(
            self.font_system,
            Some((rect.width * self.scale).max(1.0)),
            Some((rect.height * self.scale).max(1.0)),
        );
        set_text(&mut buffer, self.font_system);

        let origin_x = rect.x * self.scale;
        let origin_y = rect.y * self.scale + origin_y_extra * self.scale;
        let color = TextColor::rgba(style.color.r, style.color.g, style.color.b, style.color.a);
        let synthetic_bold = needs_synthetic_bold_paint(style);
        for shadow in style.text_shadows.iter().rev() {
            if shadow.blur_radius > 0.0 {
                continue;
            }
            let shadow_color = TextColor::rgba(
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a,
            );
            self.paint_text_runs(
                &buffer,
                origin_x + shadow.offset_x * self.scale,
                origin_y + shadow.offset_y * self.scale,
                PaintTextRunOptions {
                    color: shadow_color,
                    opacity,
                    synthetic_bold,
                    use_glyph_color: false,
                },
            );
        }
        self.paint_text_runs(
            &buffer,
            origin_x,
            origin_y,
            PaintTextRunOptions {
                color,
                opacity,
                synthetic_bold,
                use_glyph_color: true,
            },
        );
    }

    fn paint_text_runs(
        &mut self,
        buffer: &Buffer,
        origin_x: f32,
        origin_y: f32,
        options: PaintTextRunOptions,
    ) {
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let physical_glyph = glyph.physical((origin_x, origin_y + run.line_y), 1.0);
                let glyph_color = if options.use_glyph_color {
                    glyph.color_opt.map_or(options.color, |some| some)
                } else {
                    options.color
                };
                self.swash_cache.with_pixels(
                    self.font_system,
                    physical_glyph.cache_key,
                    glyph_color,
                    |x, y, color| {
                        let color = apply_text_base_alpha(color, glyph_color);
                        let color = apply_text_opacity(color, options.opacity);
                        blend_text_rect(
                            self.pixmap,
                            physical_glyph.x + x,
                            physical_glyph.y + y,
                            1,
                            1,
                            color,
                        );
                        if options.synthetic_bold {
                            blend_text_rect(
                                self.pixmap,
                                physical_glyph.x + x + 1,
                                physical_glyph.y + y,
                                1,
                                1,
                                color,
                            );
                        }
                    },
                );
            }
        }
    }

    fn paint_image(&mut self, rect: Rect, style: &Style, image: &ImageData, opacity: f32) {
        draw_image_with_fit(
            self.pixmap,
            self.scale,
            rect,
            image,
            ImageFitPaint {
                fit: style.object_fit,
                position: style.object_position,
                radius: style.border_radius,
                opacity,
            },
        );
    }

    fn paint_background_image(
        &mut self,
        rect: Rect,
        style: &Style,
        image: &ImageData,
        opacity: f32,
    ) {
        draw_background_image(
            self.pixmap,
            self.scale,
            rect,
            image,
            BackgroundImagePaint {
                repeat: style.background_repeat,
                size: style.background_size,
                position: style.background_position,
                radius: style.border_radius,
                opacity,
            },
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct PaintTextRunOptions {
    color: TextColor,
    opacity: f32,
    synthetic_bold: bool,
    use_glyph_color: bool,
}

fn apply_text_opacity(color: TextColor, opacity: f32) -> TextColor {
    if opacity >= 1.0 {
        return color;
    }
    let (r, g, b, a) = color.as_rgba_tuple();
    let a = (a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    TextColor::rgba(r, g, b, a)
}

fn apply_text_base_alpha(mask_color: TextColor, base_color: TextColor) -> TextColor {
    let (r, g, b, a) = mask_color.as_rgba_tuple();
    let (_, _, _, base_a) = base_color.as_rgba_tuple();
    if base_a == 255 {
        return mask_color;
    }
    let a = ((u16::from(a) * u16::from(base_a) + 127) / 255) as u8;
    TextColor::rgba(r, g, b, a)
}

#[derive(Debug, Clone)]
struct LayoutBox {
    kind: LayoutKind,
    rect: Rect,
    style: Style,
    children: Vec<LayoutBox>,
}

#[derive(Debug, Clone)]
enum LayoutKind {
    Block,
    Table,
    Row,
    Cell,
    Text(String),
    RichText(Vec<TextSpan>),
    Image(Option<ImageData>),
}

#[derive(Debug, Clone)]
struct TextSpan {
    text: String,
    style: TextRunStyle,
}

impl TextSpan {
    fn from_style(text: String, style: &Style) -> Self {
        Self {
            text,
            style: TextRunStyle::from_style(style),
        }
    }

    fn with_run_style(text: String, style: TextRunStyle) -> Self {
        Self { text, style }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TextRunStyle {
    color: Rgba,
    font_family: Option<String>,
    font_weight: FontWeight,
    font_face_weight: Option<FontWeight>,
    font_style: FontStyle,
    font_size: f32,
    line_height: f32,
    line_height_factor: Option<f32>,
    line_height_normal: bool,
    font_hinting_disabled: bool,
    letter_spacing: f32,
    text_transform: TextTransform,
}

impl TextRunStyle {
    fn from_style(style: &Style) -> Self {
        Self {
            color: style.color,
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            font_face_weight: style.font_face_weight,
            font_style: style.font_style,
            font_size: style.font_size,
            line_height: style.line_height,
            line_height_factor: style.line_height_factor,
            line_height_normal: style.line_height_normal,
            font_hinting_disabled: style.font_hinting_disabled,
            letter_spacing: style.letter_spacing,
            text_transform: style.text_transform,
        }
    }

    fn text_attrs(&self) -> Attrs<'_> {
        text_style_attrs(
            self.font_family.as_deref(),
            self.font_weight,
            self.font_face_weight,
            self.font_style,
            self.font_hinting_disabled,
            self.letter_spacing,
            self.font_size,
        )
    }

    fn text_attrs_for_span(
        &self,
        db: &fontdb::Database,
        scale: f32,
        parent_style: &Style,
    ) -> Attrs<'_> {
        let attrs = self.text_attrs().color(TextColor::rgba(
            self.color.r,
            self.color.g,
            self.color.b,
            self.color.a,
        ));
        if !self.needs_own_metrics(db, parent_style) {
            return attrs;
        }

        let font_size = (self.font_size * scale).max(1.0);
        let line_height = (resolved_line_height_from_run_db(db, self) * scale).max(1.0);
        attrs.metrics(Metrics::new(font_size, line_height))
    }

    fn needs_own_metrics(&self, db: &fontdb::Database, parent_style: &Style) -> bool {
        if (self.font_size - parent_style.font_size).abs() > 0.01 {
            return true;
        }
        let run_line_height = resolved_line_height_from_run_db(db, self);
        let parent_line_height = resolved_line_height_from_db(db, parent_style);
        (run_line_height - parent_line_height).abs() > 0.01
    }
}

#[derive(Debug)]
struct FlowBox {
    node: LayoutBox,
    advance: f32,
    collapsible_margin_bottom: f32,
}

#[derive(Debug, Default)]
struct LayoutChildren {
    children: Vec<LayoutBox>,
    advance: f32,
    trailing_collapsible_margin: f32,
}

#[derive(Debug, Clone)]
struct Style {
    display: Display,
    width: Option<Length>,
    width_auto: bool,
    min_width: Option<Length>,
    max_width: Option<Length>,
    height: Option<Length>,
    height_auto: bool,
    min_height: Option<Length>,
    max_height: Option<Length>,
    margin: Edges,
    margin_left_auto: bool,
    margin_right_auto: bool,
    margin_top_em: Option<f32>,
    margin_bottom_em: Option<f32>,
    padding: Edges,
    padding_explicit: EdgeFlags,
    background: Option<Rgba>,
    background_image: Option<ImageData>,
    background_image_src: Option<String>,
    background_repeat: BackgroundRepeat,
    background_size: BackgroundSize,
    background_position: BackgroundPosition,
    object_fit: ObjectFit,
    object_position: ObjectPosition,
    opacity: f32,
    color: Rgba,
    box_shadows: Vec<BoxShadow>,
    text_shadows: Vec<BoxShadow>,
    font_family: Option<String>,
    font_weight: FontWeight,
    font_face_weight: Option<FontWeight>,
    font_style: FontStyle,
    font_size: f32,
    line_height: f32,
    line_height_factor: Option<f32>,
    line_height_normal: bool,
    font_hinting_disabled: bool,
    letter_spacing: f32,
    text_align: TextAlign,
    align_from_attribute: bool,
    text_transform: TextTransform,
    vertical_align: VerticalAlign,
    wrap: TextWrap,
    list_style_type: ListStyleType,
    box_sizing: BoxSizing,
    position: Position,
    inset_top: Option<Length>,
    inset_right: Option<Length>,
    inset_bottom: Option<Length>,
    inset_left: Option<Length>,
    flex_direction: FlexDirection,
    flex_wrap: FlexWrap,
    justify_content: JustifyContent,
    align_items: AlignItems,
    align_self: Option<AlignItems>,
    row_gap: f32,
    column_gap: f32,
    flex_grow: f32,
    flex_shrink: f32,
    flex_basis: Option<Length>,
    float_side: FloatSide,
    clear: Clear,
    border: Edges,
    border_radius: f32,
    border_color: Rgba,
    border_style: BorderLineStyle,
    border_collapse: BorderCollapse,
    table_layout_fixed: bool,
    cell_padding: Edges,
    cell_spacing: f32,
}

impl Style {
    fn initial() -> Self {
        Self {
            display: Display::Block,
            width: None,
            width_auto: false,
            min_width: None,
            max_width: None,
            height: None,
            height_auto: false,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            margin_top_em: None,
            margin_bottom_em: None,
            padding: Edges::ZERO,
            padding_explicit: EdgeFlags::NONE,
            background: None,
            background_image: None,
            background_image_src: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_size: BackgroundSize::Auto,
            background_position: BackgroundPosition::default(),
            object_fit: ObjectFit::Fill,
            object_position: ObjectPosition::default(),
            opacity: 1.0,
            color: Rgba::BLACK,
            box_shadows: Vec::new(),
            text_shadows: Vec::new(),
            font_family: None,
            font_weight: FontWeight::NORMAL,
            font_face_weight: None,
            font_style: FontStyle::Normal,
            font_size: 16.0,
            line_height: normal_line_height_fallback(16.0),
            line_height_factor: None,
            line_height_normal: true,
            font_hinting_disabled: false,
            letter_spacing: 0.0,
            text_align: TextAlign::Left,
            align_from_attribute: false,
            text_transform: TextTransform::None,
            vertical_align: VerticalAlign::Baseline,
            wrap: TextWrap::WordOrGlyph,
            list_style_type: ListStyleType::Disc,
            box_sizing: BoxSizing::ContentBox,
            position: Position::Static,
            inset_top: None,
            inset_right: None,
            inset_bottom: None,
            inset_left: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: None,
            row_gap: 0.0,
            column_gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            float_side: FloatSide::None,
            clear: Clear::None,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: Rgba::BLACK,
            border_style: BorderLineStyle::Solid,
            border_collapse: BorderCollapse::Separate,
            table_layout_fixed: false,
            cell_padding: Edges::ZERO,
            cell_spacing: 0.0,
        }
    }

    fn from_parent_for_tag(parent: &Self, tag: &str) -> Self {
        let mut style = Self {
            display: default_display(tag),
            width: None,
            width_auto: false,
            min_width: None,
            max_width: None,
            height: None,
            height_auto: false,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            margin_top_em: None,
            margin_bottom_em: None,
            padding: Edges::ZERO,
            padding_explicit: EdgeFlags::NONE,
            background: None,
            background_image: None,
            background_image_src: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_size: BackgroundSize::Auto,
            background_position: BackgroundPosition::default(),
            object_fit: ObjectFit::Fill,
            object_position: ObjectPosition::default(),
            opacity: 1.0,
            color: parent.color,
            box_shadows: Vec::new(),
            text_shadows: parent.text_shadows.clone(),
            font_family: parent.font_family.clone(),
            font_weight: parent.font_weight,
            font_face_weight: parent.font_face_weight,
            font_style: parent.font_style,
            font_size: parent.font_size,
            line_height: parent.line_height,
            line_height_factor: parent.line_height_factor,
            line_height_normal: parent.line_height_normal,
            font_hinting_disabled: parent.font_hinting_disabled,
            letter_spacing: parent.letter_spacing,
            text_align: if tag == "table" {
                TextAlign::Left
            } else {
                parent.text_align
            },
            align_from_attribute: if tag == "table" {
                false
            } else {
                parent.align_from_attribute
            },
            text_transform: parent.text_transform,
            vertical_align: default_vertical_align(tag, parent.vertical_align),
            wrap: parent.wrap,
            list_style_type: parent.list_style_type,
            box_sizing: parent.box_sizing,
            position: Position::Static,
            inset_top: None,
            inset_right: None,
            inset_bottom: None,
            inset_left: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: None,
            row_gap: 0.0,
            column_gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            float_side: FloatSide::None,
            clear: Clear::None,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: parent.border_color,
            border_style: BorderLineStyle::Solid,
            border_collapse: BorderCollapse::Separate,
            table_layout_fixed: false,
            cell_padding: Edges::ZERO,
            cell_spacing: 0.0,
        };

        match tag {
            "h1" => {
                style.set_font_size(parent.font_size * 2.0);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(0.67, 0.67);
            }
            "h2" => {
                style.set_font_size(parent.font_size * 1.5);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(0.83, 0.83);
            }
            "h3" => {
                style.set_font_size(parent.font_size * 1.17);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.0, 1.0);
            }
            "h4" => {
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.33, 1.33);
            }
            "h5" => {
                style.set_font_size(parent.font_size * 0.83);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.67, 1.67);
            }
            "h6" => {
                style.set_font_size(parent.font_size * 0.67);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(2.33, 2.33);
            }
            "small" => style.set_font_size(parent.font_size * 0.85),
            "p" => {
                style.set_default_em_margins(1.0, 1.0);
            }
            "ul" => {
                style.set_default_em_margins(1.0, 1.0);
                style.padding.left = 40.0;
                style.list_style_type = ListStyleType::Disc;
            }
            "ol" => {
                style.set_default_em_margins(1.0, 1.0);
                style.padding.left = 40.0;
                style.list_style_type = ListStyleType::Decimal;
            }
            "hr" => {
                style.margin.top = 8.0;
                style.margin.bottom = 8.0;
                style.border = Edges::all(1.0);
                style.border_color = Rgba::rgb(0x80, 0x80, 0x80);
                style.border_style = BorderLineStyle::Inset;
            }
            "strong" | "b" => style.font_weight = FontWeight::BOLD,
            "em" | "i" => style.font_style = FontStyle::Italic,
            "th" => {
                style.text_align = TextAlign::Center;
                style.font_weight = FontWeight::BOLD;
            }
            _ => {}
        }

        style
    }

    fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size.max(1.0);
        if let Some(factor) = self.line_height_factor {
            self.line_height = self.font_size * factor;
        } else if self.line_height_normal {
            self.line_height = normal_line_height_fallback(self.font_size);
        }
        if let Some(factor) = self.margin_top_em {
            self.margin.top = self.font_size * factor;
        }
        if let Some(factor) = self.margin_bottom_em {
            self.margin.bottom = self.font_size * factor;
        }
    }

    fn set_default_em_margins(&mut self, top: f32, bottom: f32) {
        self.margin_top_em = Some(top);
        self.margin_bottom_em = Some(bottom);
        self.margin.top = self.font_size * top;
        self.margin.bottom = self.font_size * bottom;
    }

    fn apply_declaration(&mut self, name: &str, value: &str) {
        let value = strip_important(value);
        match name {
            "display" => {
                if let Some(display) = parse_display(value) {
                    self.display = display;
                }
            }
            "width" => {
                self.width_auto = value.trim().eq_ignore_ascii_case("auto");
                self.width = parse_length(value);
            }
            "min-width" => self.min_width = parse_length(value),
            "max-width" => self.max_width = parse_length(value),
            "height" => {
                self.height_auto = value.trim().eq_ignore_ascii_case("auto");
                self.height = parse_length(value);
            }
            "min-height" => self.min_height = parse_length(value),
            "max-height" => self.max_height = parse_length(value),
            "margin" => {
                if let Some((edges, left_auto, right_auto)) =
                    parse_margin_edges(value, self.font_size)
                {
                    self.margin_top_em = None;
                    self.margin_bottom_em = None;
                    self.margin = edges;
                    self.margin_left_auto = left_auto;
                    self.margin_right_auto = right_auto;
                }
            }
            "margin-top" => {
                self.margin_top_em = None;
                self.margin.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-right" => {
                self.margin_right_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-bottom" => {
                self.margin_bottom_em = None;
                self.margin.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-left" => {
                self.margin_left_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding" => {
                if let Some(edges) = parse_edges_with_font(value, self.font_size) {
                    self.padding = edges;
                    self.padding_explicit = EdgeFlags::ALL;
                }
            }
            "padding-top" => {
                self.padding.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
                self.padding_explicit.top = true;
            }
            "padding-right" => {
                self.padding.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
                self.padding_explicit.right = true;
            }
            "padding-bottom" => {
                self.padding.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
                self.padding_explicit.bottom = true;
            }
            "padding-left" => {
                self.padding.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
                self.padding_explicit.left = true;
            }
            "background" => {
                if let Some(color) = parse_color(value) {
                    self.background = Some(color);
                }
                if let Some(src) = parse_background_image(value) {
                    self.background_image_src = Some(src);
                    self.background_image = None;
                } else if background_shorthand_removes_image(value) {
                    self.background_image_src = None;
                    self.background_image = None;
                }
                self.background_repeat = parse_background_repeat(value).unwrap_or_default();
                self.background_size = parse_background_size_from_shorthand(value);
                self.background_position = parse_background_position_from_shorthand(value);
            }
            "background-repeat" => {
                if let Some(repeat) = parse_background_repeat(value) {
                    self.background_repeat = repeat;
                }
            }
            "background-size" => {
                if let Some(size) = parse_background_size(value) {
                    self.background_size = size;
                }
            }
            "background-position" => {
                if let Some(position) = parse_background_position(value) {
                    self.background_position = position;
                }
            }
            "background-color" => {
                if let Some(color) = parse_color(value) {
                    self.background = Some(color);
                }
            }
            "background-image" => {
                self.background_image_src = parse_background_image(value);
                self.background_image = None;
            }
            "object-fit" => {
                if let Some(object_fit) = parse_object_fit(value) {
                    self.object_fit = object_fit;
                }
            }
            "object-position" => {
                if let Some(object_position) = parse_object_position(value) {
                    self.object_position = object_position;
                }
            }
            "opacity" => {
                if let Ok(opacity) = value.trim().parse::<f32>() {
                    if opacity.is_finite() {
                        self.opacity = opacity.clamp(0.0, 1.0);
                    }
                }
            }
            "color" => {
                if let Some(color) = parse_color(value) {
                    self.color = color;
                }
            }
            "box-shadow" => {
                if let Some(shadows) = parse_box_shadow(value, self.font_size, self.color) {
                    self.box_shadows = shadows;
                }
            }
            "text-shadow" => {
                if let Some(shadows) = parse_text_shadow(value, self.font_size, self.color) {
                    self.text_shadows = shadows;
                }
            }
            "font-size" => {
                if let Some(font_size) = parse_font_size(value, self.font_size) {
                    self.set_font_size(font_size);
                }
            }
            "font-family" => {
                if let Some(font_family) = parse_font_family(value) {
                    self.font_family = Some(font_family);
                    self.font_face_weight = None;
                }
            }
            "font-weight" => {
                if !is_inherit_keyword(value) {
                    self.font_weight = parse_font_weight(value);
                }
            }
            "font-style" => {
                if !is_inherit_keyword(value) {
                    self.font_style = parse_font_style(value);
                }
            }
            "line-height" => {
                if let Some(line_height) = parse_line_height_declaration(value, self.font_size) {
                    self.line_height = line_height.height.max(1.0);
                    self.line_height_factor = line_height.factor;
                    self.line_height_normal = line_height.normal;
                }
            }
            "letter-spacing" => {
                self.letter_spacing = if value.trim().eq_ignore_ascii_case("normal") {
                    0.0
                } else {
                    parse_css_length(value, self.font_size, true).unwrap_or(0.0)
                };
            }
            "-webkit-font-smoothing" => {
                self.font_hinting_disabled = value.trim().eq_ignore_ascii_case("antialiased");
            }
            "text-rendering" => {
                if value.trim().eq_ignore_ascii_case("geometricprecision") {
                    self.font_hinting_disabled = true;
                }
            }
            "text-align" | "align" => {
                if let Some(align) = parse_text_align(value) {
                    self.text_align = align;
                    self.align_from_attribute = false;
                }
            }
            "text-transform" => {
                if let Some(transform) = parse_text_transform(value) {
                    self.text_transform = transform;
                }
            }
            "vertical-align" => {
                if let Some(align) = parse_vertical_align(value) {
                    self.vertical_align = align;
                }
            }
            "white-space" => {
                if value.trim().eq_ignore_ascii_case("nowrap") {
                    self.wrap = TextWrap::None;
                }
            }
            "list-style" | "list-style-type" => {
                if let Some(list_style_type) = parse_list_style_type(value) {
                    self.list_style_type = list_style_type;
                }
            }
            "word-break" => {
                if value.trim().eq_ignore_ascii_case("break-all") {
                    self.wrap = TextWrap::Glyph;
                }
            }
            "overflow-wrap" | "word-wrap" => {
                if matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "break-word" | "anywhere"
                ) {
                    self.wrap = TextWrap::WordOrGlyph;
                }
            }
            "box-sizing" => {
                if let Some(box_sizing) = parse_box_sizing(value) {
                    self.box_sizing = box_sizing;
                }
            }
            "position" => {
                if let Some(position) = parse_position(value) {
                    self.position = position;
                }
            }
            "top" => self.inset_top = parse_length(value),
            "right" => self.inset_right = parse_length(value),
            "bottom" => self.inset_bottom = parse_length(value),
            "left" => self.inset_left = parse_length(value),
            "flex-direction" => {
                if let Some(direction) = parse_flex_direction(value) {
                    self.flex_direction = direction;
                }
            }
            "flex-wrap" => {
                if let Some(wrap) = parse_flex_wrap(value) {
                    self.flex_wrap = wrap;
                }
            }
            "flex-flow" => apply_flex_flow(self, value),
            "justify-content" => {
                if let Some(justify) = parse_justify_content(value) {
                    self.justify_content = justify;
                }
            }
            "align-items" => {
                if let Some(align) = parse_align_items(value) {
                    self.align_items = align;
                }
            }
            "align-self" => {
                self.align_self = parse_align_items(value);
            }
            "gap" => {
                if let Some((row_gap, column_gap)) = parse_gap(value, self.font_size) {
                    self.row_gap = row_gap;
                    self.column_gap = column_gap;
                }
            }
            "row-gap" => {
                self.row_gap = parse_css_length(value, self.font_size, false).unwrap_or(0.0);
            }
            "column-gap" => {
                self.column_gap = parse_css_length(value, self.font_size, false).unwrap_or(0.0);
            }
            "flex" => apply_flex(self, value),
            "flex-grow" => self.flex_grow = parse_flex_factor(value).unwrap_or(self.flex_grow),
            "flex-shrink" => {
                self.flex_shrink = parse_flex_factor(value).unwrap_or(self.flex_shrink)
            }
            "flex-basis" => self.flex_basis = parse_length(value),
            "float" => {
                if let Some(float_side) = parse_float_side(value) {
                    self.float_side = float_side;
                }
            }
            "clear" => {
                if let Some(clear) = parse_clear(value) {
                    self.clear = clear;
                }
            }
            "border" => apply_border(self, value),
            "border-radius" => self.border_radius = parse_radius(value).unwrap_or(0.0).max(0.0),
            "border-style" => {
                if let Some(border_style) = parse_border_line_style(value) {
                    self.border_style = border_style;
                }
            }
            "border-width" => {
                self.border = parse_edges(value).unwrap_or(Edges::ZERO);
            }
            "border-top-width" => {
                self.border.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-right-width" => {
                self.border.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-bottom-width" => {
                self.border.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-left-width" => {
                self.border.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-color" => {
                if let Some(color) = parse_color(value) {
                    self.border_color = color;
                }
            }
            "border-top-style"
            | "border-right-style"
            | "border-bottom-style"
            | "border-left-style" => {
                if let Some(border_style) = parse_border_line_style(value) {
                    self.border_style = border_style;
                }
            }
            "border-top" => apply_border_side(self, BorderSide::Top, value),
            "border-right" => apply_border_side(self, BorderSide::Right, value),
            "border-bottom" => apply_border_side(self, BorderSide::Bottom, value),
            "border-left" => apply_border_side(self, BorderSide::Left, value),
            "border-collapse" => {
                if value.trim().eq_ignore_ascii_case("collapse") {
                    self.border_collapse = BorderCollapse::Collapse;
                }
            }
            "table-layout" => {
                self.table_layout_fixed = value.trim().eq_ignore_ascii_case("fixed");
            }
            "border-spacing" => self.cell_spacing = parse_px(value).unwrap_or(0.0),
            _ => {}
        }
    }

    fn resolve_width(&self, containing_width: f32) -> Option<f32> {
        let mut width = self.width.and_then(|width| width.resolve(containing_width));
        width = width.map(|width| self.constrain_width(width, containing_width));
        width
    }

    fn constrain_width(&self, width: f32, containing_width: f32) -> f32 {
        let mut width = width;
        if let Some(min_width) = self
            .min_width
            .and_then(|width| width.resolve(containing_width))
        {
            width = width.max(min_width);
        }
        if let Some(max_width) = self
            .max_width
            .and_then(|width| width.resolve(containing_width))
        {
            width = width.min(max_width);
        }
        width
    }

    fn resolve_height(&self, basis: f32) -> Option<f32> {
        let mut height = self.height.and_then(|height| height.resolve(basis));
        if let Some(min_height) = self.min_height.and_then(|height| height.resolve(basis)) {
            height = Some(height.unwrap_or(min_height).max(min_height));
        }
        if let Some(max_height) = self.max_height.and_then(|height| height.resolve(basis)) {
            height = Some(height.unwrap_or(max_height).min(max_height));
        }
        height
    }

    fn outer_width_for_declared(&self, width: f32) -> f32 {
        match self.box_sizing {
            BoxSizing::BorderBox => width,
            BoxSizing::ContentBox => width + self.padding.horizontal() + self.border.horizontal(),
        }
    }

    fn inner_width_for_outer(&self, width: f32) -> f32 {
        (width - self.padding.horizontal() - self.border.horizontal()).max(1.0)
    }

    fn apply_table_cell_padding(&mut self, padding: Edges) {
        if !self.padding_explicit.top && padding.top > 0.0 {
            self.padding.top = padding.top;
        }
        if !self.padding_explicit.right && padding.right > 0.0 {
            self.padding.right = padding.right;
        }
        if !self.padding_explicit.bottom && padding.bottom > 0.0 {
            self.padding.bottom = padding.bottom;
        }
        if !self.padding_explicit.left && padding.left > 0.0 {
            self.padding.left = padding.left;
        }
    }

    fn horizontal_offset(&self, containing_width: f32, outer_width: f32) -> f32 {
        let fixed_left = if self.margin_left_auto {
            0.0
        } else {
            self.margin.left
        };
        let fixed_right = if self.margin_right_auto {
            0.0
        } else {
            self.margin.right
        };
        let free = (containing_width - outer_width - fixed_left - fixed_right).max(0.0);
        if self.margin_left_auto && self.margin_right_auto {
            fixed_left + free / 2.0
        } else if self.margin_left_auto {
            fixed_left + free
        } else {
            fixed_left
        }
    }

    fn text_attrs(&self) -> Attrs<'_> {
        text_style_attrs(
            self.font_family.as_deref(),
            self.font_weight,
            self.font_face_weight,
            self.font_style,
            self.font_hinting_disabled,
            self.letter_spacing,
            self.font_size,
        )
    }
}

fn strip_important(value: &str) -> &str {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix("!important") {
        return value[..stripped.len()].trim();
    }
    value
}

fn is_inherit_keyword(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "inherit" | "unset"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Display {
    Block,
    Inline,
    InlineBlock,
    Flex,
    Table,
    TableRow,
    TableCell,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignItems {
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloatSide {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clear {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListStyleType {
    Disc,
    Decimal,
    None,
}

#[derive(Debug, Clone, Copy)]
struct PlacedFloat {
    side: FloatSide,
    rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Length {
    Px(f32),
    Percent(f32),
}

impl Length {
    fn resolve(self, basis: f32) -> Option<f32> {
        match self {
            Self::Px(value) => Some(value),
            Self::Percent(value) if basis.is_finite() && basis > 0.0 => Some(basis * value),
            Self::Percent(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Edges {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

impl Edges {
    const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    fn horizontal(self) -> f32 {
        self.left + self.right
    }

    fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    fn max_width(self) -> f32 {
        self.top.max(self.right).max(self.bottom).max(self.left)
    }

    fn is_zero(self) -> bool {
        self.top == 0.0 && self.right == 0.0 && self.bottom == 0.0 && self.left == 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EdgeFlags {
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
}

impl EdgeFlags {
    const NONE: Self = Self {
        top: false,
        right: false,
        bottom: false,
        left: false,
    };

    const ALL: Self = Self {
        top: true,
        right: true,
        bottom: true,
        left: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Rgba {
    const BLACK: Self = Self::rgb(0, 0, 0);
    const WHITE: Self = Self::rgb(255, 255, 255);

    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    const fn with_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BoxShadow {
    offset_x: f32,
    offset_y: f32,
    blur_radius: f32,
    spread: f32,
    color: Rgba,
    inset: bool,
}

fn with_opacity(color: Rgba, opacity: f32) -> Rgba {
    if opacity >= 1.0 {
        return color;
    }
    Rgba {
        a: ((f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8),
        ..color
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextAlign {
    Left,
    Center,
    Right,
}

impl TextAlign {
    fn to_cosmic(self) -> TextAlignMode {
        match self {
            Self::Left => TextAlignMode::Left,
            Self::Center => TextAlignMode::Center,
            Self::Right => TextAlignMode::Right,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerticalAlign {
    Baseline,
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextWrap {
    None,
    WordOrGlyph,
    Glyph,
}

impl TextWrap {
    fn to_cosmic(self) -> Wrap {
        match self {
            Self::None => Wrap::None,
            Self::WordOrGlyph => Wrap::WordOrGlyph,
            Self::Glyph => Wrap::Glyph,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderCollapse {
    Separate,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderLineStyle {
    Solid,
    Dashed,
    Inset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoxSizing {
    BorderBox,
    ContentBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundRepeat {
    Repeat,
    NoRepeat,
}

impl Default for BackgroundRepeat {
    fn default() -> Self {
        Self::Repeat
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundSize {
    Auto,
    Cover,
    Contain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ObjectPosition {
    x: PositionAxis,
    y: PositionAxis,
}

impl Default for ObjectPosition {
    fn default() -> Self {
        Self {
            x: PositionAxis::Center,
            y: PositionAxis::Center,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BackgroundPosition {
    x: PositionAxis,
    y: PositionAxis,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BackgroundImagePaint {
    repeat: BackgroundRepeat,
    size: BackgroundSize,
    position: BackgroundPosition,
    radius: f32,
    opacity: f32,
}

impl Default for BackgroundPosition {
    fn default() -> Self {
        Self {
            x: PositionAxis::Start,
            y: PositionAxis::Start,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PositionAxis {
    Start,
    Center,
    End,
}

impl PositionAxis {
    fn factor(self) -> f32 {
        match self {
            Self::Start => 0.0,
            Self::Center => 0.5,
            Self::End => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl Rect {
    const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

fn style_for_node(node: &NodeRef, parent: &Style) -> Style {
    style_for_node_with_fonts(node, parent, &[], &[])
}

fn style_for_node_with_fonts(
    node: &NodeRef,
    parent: &Style,
    available_font_families: &[String],
    web_font_faces: &[WebFontFace],
) -> Style {
    let Some(element) = node.as_element() else {
        return parent.clone();
    };
    let tag = element.name.local.to_string();
    let mut style = Style::from_parent_for_tag(parent, &tag);
    let attrs = element.attributes.borrow();

    if let Some(width) = attrs.get("width").and_then(parse_length) {
        style.width = Some(width);
    }
    if let Some(height) = attrs.get("height").and_then(parse_length) {
        style.height = Some(height);
    }
    if let Some(background) = attrs.get("bgcolor").and_then(parse_color) {
        style.background = Some(background);
    }
    if let Some(background_image) = attrs.get("background") {
        style.background_image_src = Some(background_image.trim().to_string());
        style.background_image = None;
    }
    if let Some(raw_align) = attrs.get("align") {
        if tag == "table" {
            match parse_text_align(raw_align) {
                Some(TextAlign::Center) => {
                    style.margin_left_auto = true;
                    style.margin_right_auto = true;
                }
                Some(TextAlign::Right) => {
                    style.margin_left_auto = true;
                    style.margin_right_auto = false;
                }
                _ => {}
            }
        } else if let Some(align) = parse_text_align(raw_align) {
            style.text_align = align;
            style.align_from_attribute = true;
        }
    }
    if let Some(vertical_align) = attrs
        .get("valign")
        .or_else(|| attrs.get("vertical-align"))
        .and_then(parse_vertical_align)
    {
        style.vertical_align = vertical_align;
    }
    if tag == "table" {
        style.cell_padding = attrs
            .get("cellpadding")
            .and_then(parse_px)
            .map(Edges::all)
            .unwrap_or(Edges::all(1.0));
        if let Some(cell_spacing) = attrs.get("cellspacing").and_then(parse_px) {
            style.cell_spacing = cell_spacing;
        }
        if let Some(border) = attrs.get("border").and_then(parse_px) {
            if border > 0.0 {
                style.border = Edges::all(border);
            }
        }
    }
    if let Some(style_attr) = attrs.get("style") {
        for (name, value) in css_declarations(style_attr) {
            match name.as_str() {
                "font-family" => {
                    if let Some(selection) =
                        parse_font_family_selection(&value, available_font_families, web_font_faces)
                    {
                        style.font_family = Some(selection.family);
                        style.font_face_weight = selection.forced_weight;
                    }
                }
                "font-weight" if is_inherit_keyword(&value) => {
                    style.font_weight = parent.font_weight;
                }
                "font-style" if is_inherit_keyword(&value) => {
                    style.font_style = parent.font_style;
                }
                _ => style.apply_declaration(&name, &value),
            }
        }
    }
    style
}

fn default_display(tag: &str) -> Display {
    match tag {
        "html" | "body" | "div" | "p" | "section" | "article" | "header" | "footer" | "main"
        | "center" | "blockquote" | "ul" | "ol" | "li" | "h1" | "h2" | "h3" | "h4" | "h5"
        | "h6" | "hr" => Display::Block,
        "table" => Display::Table,
        "thead" | "tbody" | "tfoot" => Display::Block,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        "script" | "style" | "head" | "meta" | "link" | "title" | "base" => Display::None,
        _ => Display::Inline,
    }
}

fn default_vertical_align(tag: &str, parent: VerticalAlign) -> VerticalAlign {
    match tag {
        "thead" | "tbody" | "tfoot" | "tr" => VerticalAlign::Middle,
        "td" | "th" => parent,
        _ => VerticalAlign::Baseline,
    }
}

fn parse_display(value: &str) -> Option<Display> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Some(Display::Block),
        "inline" => Some(Display::Inline),
        "inline-block" => Some(Display::InlineBlock),
        "flex" | "inline-flex" => Some(Display::Flex),
        "table" => Some(Display::Table),
        "table-row" => Some(Display::TableRow),
        "table-cell" => Some(Display::TableCell),
        "none" => Some(Display::None),
        _ => None,
    }
}

fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "row" => Some(FlexDirection::Row),
        "row-reverse" => Some(FlexDirection::RowReverse),
        "column" => Some(FlexDirection::Column),
        "column-reverse" => Some(FlexDirection::ColumnReverse),
        _ => None,
    }
}

fn parse_flex_wrap(value: &str) -> Option<FlexWrap> {
    match value.trim().to_ascii_lowercase().as_str() {
        "nowrap" => Some(FlexWrap::NoWrap),
        "wrap" => Some(FlexWrap::Wrap),
        "wrap-reverse" => Some(FlexWrap::WrapReverse),
        _ => None,
    }
}

fn parse_justify_content(value: &str) -> Option<JustifyContent> {
    match value.trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" | "left" => Some(JustifyContent::FlexStart),
        "end" | "flex-end" | "right" => Some(JustifyContent::FlexEnd),
        "center" => Some(JustifyContent::Center),
        "space-between" => Some(JustifyContent::SpaceBetween),
        "space-around" => Some(JustifyContent::SpaceAround),
        "space-evenly" => Some(JustifyContent::SpaceEvenly),
        _ => None,
    }
}

fn parse_align_items(value: &str) -> Option<AlignItems> {
    match value.trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" => Some(AlignItems::FlexStart),
        "end" | "flex-end" => Some(AlignItems::FlexEnd),
        "center" => Some(AlignItems::Center),
        "baseline" => Some(AlignItems::Baseline),
        "stretch" | "normal" => Some(AlignItems::Stretch),
        _ => None,
    }
}

fn parse_gap(value: &str, font_size: f32) -> Option<(f32, f32)> {
    if value.trim().eq_ignore_ascii_case("normal") {
        return Some((0.0, 0.0));
    }
    let mut parts = value.split_whitespace();
    let row_gap = parse_css_length(parts.next()?, font_size, false)?;
    let column_gap = parts
        .next()
        .and_then(|value| parse_css_length(value, font_size, false))
        .unwrap_or(row_gap);
    Some((row_gap.max(0.0), column_gap.max(0.0)))
}

fn apply_flex_flow(style: &mut Style, value: &str) {
    for token in value.split_whitespace() {
        if let Some(direction) = parse_flex_direction(token) {
            style.flex_direction = direction;
        } else if let Some(wrap) = parse_flex_wrap(token) {
            style.flex_wrap = wrap;
        }
    }
}

fn apply_flex(style: &mut Style, value: &str) {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
        style.flex_basis = None;
        return;
    }
    if value.eq_ignore_ascii_case("auto") {
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        style.flex_basis = None;
        return;
    }
    if value.eq_ignore_ascii_case("initial") {
        style.flex_grow = 0.0;
        style.flex_shrink = 1.0;
        style.flex_basis = None;
        return;
    }

    let mut numbers = Vec::new();
    let mut basis = None;
    for token in value.split_whitespace() {
        if let Some(factor) = parse_flex_factor(token) {
            numbers.push(factor);
        } else if token.eq_ignore_ascii_case("auto") {
            basis = None;
        } else if let Some(length) = parse_length(token) {
            basis = Some(length);
        }
    }

    if let Some(grow) = numbers.first().copied() {
        style.flex_grow = grow;
        style.flex_shrink = numbers.get(1).copied().unwrap_or(1.0);
        style.flex_basis = basis.or(Some(Length::Percent(0.0)));
    } else if basis.is_some() {
        style.flex_basis = basis;
    }
}

fn parse_flex_factor(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_position(value: &str) -> Option<Position> {
    match value.trim().to_ascii_lowercase().as_str() {
        "static" => Some(Position::Static),
        "relative" => Some(Position::Relative),
        "absolute" => Some(Position::Absolute),
        "fixed" => Some(Position::Fixed),
        _ => None,
    }
}

fn parse_list_style_type(value: &str) -> Option<ListStyleType> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.split_whitespace().any(|token| token == "none") {
        return Some(ListStyleType::None);
    }
    if lower
        .split_whitespace()
        .any(|token| matches!(token, "decimal" | "decimal-leading-zero"))
    {
        return Some(ListStyleType::Decimal);
    }
    if lower
        .split_whitespace()
        .any(|token| matches!(token, "disc" | "circle" | "square"))
    {
        return Some(ListStyleType::Disc);
    }
    None
}

fn parse_float_side(value: &str) -> Option<FloatSide> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(FloatSide::None),
        "left" => Some(FloatSide::Left),
        "right" => Some(FloatSide::Right),
        _ => None,
    }
}

fn parse_clear(value: &str) -> Option<Clear> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(Clear::None),
        "left" => Some(Clear::Left),
        "right" => Some(Clear::Right),
        "both" => Some(Clear::Both),
        _ => None,
    }
}

fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| Length::Percent(value / 100.0));
    }
    parse_px(value).map(Length::Px)
}

fn parse_px(value: &str) -> Option<f32> {
    parse_css_length(value, 16.0, true)
}

fn parse_css_length(value: &str, font_size: f32, allow_unitless: bool) -> Option<f32> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = lower.strip_suffix("rem") {
        (number, 16.0)
    } else if let Some(number) = lower.strip_suffix("em") {
        (number, font_size.max(1.0))
    } else if let Some(number) = lower.strip_suffix("px") {
        (number, 1.0)
    } else if let Some(number) = lower.strip_suffix("pt") {
        (number, 96.0 / 72.0)
    } else if allow_unitless {
        (lower.as_str(), 1.0)
    } else {
        return None;
    };

    number
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value * multiplier)
}

fn parse_font_size(value: &str, parent_font_size: f32) -> Option<f32> {
    parse_css_length(value, parent_font_size, false).or_else(|| {
        value
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && *value == 0.0)
    })
}

fn parse_edges(value: &str) -> Option<Edges> {
    parse_edges_with_font(value, 16.0)
}

fn parse_margin_edges(value: &str, font_size: f32) -> Option<(Edges, bool, bool)> {
    let values: Vec<(&str, f32, bool)> = value
        .split_whitespace()
        .filter_map(|token| {
            let is_auto = token.eq_ignore_ascii_case("auto");
            parse_css_length(token, font_size, true)
                .or(Some(0.0).filter(|_| is_auto))
                .map(|length| (token, length, is_auto))
        })
        .collect();

    let expanded = match values.as_slice() {
        [all] => [all, all, all, all],
        [vertical, horizontal] => [vertical, horizontal, vertical, horizontal],
        [top, horizontal, bottom] => [top, horizontal, bottom, horizontal],
        [top, right, bottom, left, ..] => [top, right, bottom, left],
        _ => return None,
    };

    Some((
        Edges {
            top: expanded[0].1,
            right: expanded[1].1,
            bottom: expanded[2].1,
            left: expanded[3].1,
        },
        expanded[3].2,
        expanded[1].2,
    ))
}

fn parse_edges_with_font(value: &str, font_size: f32) -> Option<Edges> {
    let values: Vec<f32> = value
        .split_whitespace()
        .filter_map(|token| {
            parse_css_length(token, font_size, true).or(Some(0.0).filter(|_| token == "auto"))
        })
        .collect();

    match values.as_slice() {
        [all] => Some(Edges::all(*all)),
        [vertical, horizontal] => Some(Edges {
            top: *vertical,
            right: *horizontal,
            bottom: *vertical,
            left: *horizontal,
        }),
        [top, horizontal, bottom] => Some(Edges {
            top: *top,
            right: *horizontal,
            bottom: *bottom,
            left: *horizontal,
        }),
        [top, right, bottom, left, ..] => Some(Edges {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        }),
        _ => None,
    }
}

fn parse_radius(value: &str) -> Option<f32> {
    let token = value.split_whitespace().next()?.trim();
    if let Some(percent) = token.strip_suffix('%') {
        let percent = percent.trim().parse::<f32>().ok()?;
        return (percent > 0.0).then_some(if percent >= 50.0 {
            1_000_000.0
        } else {
            percent
        });
    }
    parse_px(token)
}

fn parse_color(value: &str) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        if let Some(color) = parse_hex_color(hex) {
            return Some(color);
        }
    }
    if value.starts_with("rgb(") || value.starts_with("rgba(") {
        return parse_rgb_function(&value);
    }
    for token in value.split_whitespace() {
        if let Some(color) = parse_color_token(token) {
            return Some(color);
        }
    }
    parse_color_token(&value)
}

fn parse_box_shadow(value: &str, font_size: f32, default_color: Rgba) -> Option<Vec<BoxShadow>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }

    let mut shadows = Vec::new();
    for shadow in split_css_top_level_list(value, ',') {
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;
        for token in css_top_level_whitespace_tokens(shadow) {
            if token.eq_ignore_ascii_case("inset") {
                inset = true;
                continue;
            }
            if let Some(parsed_color) = parse_color(&token) {
                color = Some(parsed_color);
                continue;
            }
            if let Some(length) = parse_css_length(&token, font_size, true) {
                lengths.push(length);
            }
        }
        if lengths.len() < 2 {
            continue;
        }
        shadows.push(BoxShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lengths.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(default_color),
            inset,
        });
    }

    Some(shadows)
}

fn parse_text_shadow(value: &str, font_size: f32, default_color: Rgba) -> Option<Vec<BoxShadow>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }

    let mut shadows = Vec::new();
    for shadow in split_css_top_level_list(value, ',') {
        let mut lengths = Vec::new();
        let mut color = None;
        for token in css_top_level_whitespace_tokens(shadow) {
            if let Some(parsed_color) = parse_color(&token) {
                color = Some(parsed_color);
                continue;
            }
            if let Some(length) = parse_css_length(&token, font_size, true) {
                lengths.push(length);
            }
        }
        if lengths.len() < 2 {
            continue;
        }
        shadows.push(BoxShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: 0.0,
            color: color.unwrap_or(default_color),
            inset: false,
        });
    }

    Some(shadows)
}

fn split_css_top_level_list(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut paren_depth = 0usize;

    for (index, ch) in value.char_indices() {
        if let Some(current_quote) = quote {
            if ch == current_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ if ch == separator && paren_depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(value[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn css_top_level_whitespace_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut paren_depth = 0usize;

    for (index, ch) in value.char_indices() {
        if start.is_none() && !ch.is_whitespace() {
            start = Some(index);
        }

        if let Some(current_quote) = quote {
            if ch == current_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ if ch.is_whitespace() && paren_depth == 0 => {
                if let Some(token_start) = start.take() {
                    tokens.push(value[token_start..index].trim().to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(token_start) = start {
        tokens.push(value[token_start..].trim().to_string());
    }
    tokens
        .into_iter()
        .filter(|token| !token.is_empty())
        .collect()
}

fn parse_background_image(value: &str) -> Option<String> {
    let value = strip_important(value).trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    first_css_url(value).map(|url| unquote_css_value(&url))
}

fn background_shorthand_removes_image(value: &str) -> bool {
    let value = strip_important(value).trim();
    value.eq_ignore_ascii_case("none") || !value.to_ascii_lowercase().contains("url(")
}

fn parse_background_repeat(value: &str) -> Option<BackgroundRepeat> {
    let lower = strip_important(value).to_ascii_lowercase();
    if lower.contains("no-repeat") {
        Some(BackgroundRepeat::NoRepeat)
    } else if lower.contains("repeat") {
        Some(BackgroundRepeat::Repeat)
    } else {
        None
    }
}

fn parse_background_size(value: &str) -> Option<BackgroundSize> {
    let value = strip_important(value).trim().to_ascii_lowercase();
    match value.split_whitespace().next()? {
        "auto" => Some(BackgroundSize::Auto),
        "cover" => Some(BackgroundSize::Cover),
        "contain" => Some(BackgroundSize::Contain),
        _ => None,
    }
}

fn parse_background_size_from_shorthand(value: &str) -> BackgroundSize {
    strip_important(value)
        .split_once('/')
        .and_then(|(_, size)| parse_background_size(size))
        .unwrap_or(BackgroundSize::Auto)
}

fn parse_object_fit(value: &str) -> Option<ObjectFit> {
    match strip_important(value).trim().to_ascii_lowercase().as_str() {
        "fill" => Some(ObjectFit::Fill),
        "contain" => Some(ObjectFit::Contain),
        "cover" => Some(ObjectFit::Cover),
        "none" => Some(ObjectFit::None),
        "scale-down" => Some(ObjectFit::ScaleDown),
        _ => None,
    }
}

fn parse_object_position(value: &str) -> Option<ObjectPosition> {
    let position = parse_position_keywords(value)?;
    Some(ObjectPosition {
        x: position.x,
        y: position.y,
    })
}

fn parse_background_position_from_shorthand(value: &str) -> BackgroundPosition {
    let position = strip_important(value)
        .split_once('/')
        .map_or(value, |(position, _)| position);
    parse_background_position(position).unwrap_or_default()
}

fn parse_background_position(value: &str) -> Option<BackgroundPosition> {
    parse_position_keywords(value)
}

fn parse_position_keywords(value: &str) -> Option<BackgroundPosition> {
    let mut x = None;
    let mut y = None;
    let mut saw_keyword = false;

    for keyword in background_position_keywords(value) {
        saw_keyword = true;
        match keyword {
            PositionKeyword::Left => x = Some(PositionAxis::Start),
            PositionKeyword::Right => x = Some(PositionAxis::End),
            PositionKeyword::Top => y = Some(PositionAxis::Start),
            PositionKeyword::Bottom => y = Some(PositionAxis::End),
            PositionKeyword::Center => {
                if x.is_none() {
                    x = Some(PositionAxis::Center);
                } else if y.is_none() {
                    y = Some(PositionAxis::Center);
                }
            }
        }
    }

    saw_keyword.then_some(BackgroundPosition {
        x: x.unwrap_or(PositionAxis::Center),
        y: y.unwrap_or(PositionAxis::Center),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionKeyword {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

fn background_position_keywords(value: &str) -> Vec<PositionKeyword> {
    css_ident_tokens_without_functions(value)
        .into_iter()
        .filter_map(|token| match token.as_str() {
            "left" => Some(PositionKeyword::Left),
            "right" => Some(PositionKeyword::Right),
            "top" => Some(PositionKeyword::Top),
            "bottom" => Some(PositionKeyword::Bottom),
            "center" => Some(PositionKeyword::Center),
            _ => None,
        })
        .collect()
}

fn css_ident_tokens_without_functions(value: &str) -> Vec<String> {
    let mut scrubbed = String::with_capacity(value.len());
    let mut paren_depth = 0usize;
    for ch in strip_important(value).chars() {
        match ch {
            '(' => {
                paren_depth += 1;
                scrubbed.push(' ');
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                scrubbed.push(' ');
            }
            _ if paren_depth > 0 => scrubbed.push(' '),
            ',' | '/' => scrubbed.push(' '),
            _ => scrubbed.push(ch),
        }
    }

    scrubbed
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn parse_color_token(value: &str) -> Option<Rgba> {
    let token = value.trim_matches(',');
    if let Some(hex) = token.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    match token {
        "black" => Some(Rgba::BLACK),
        "white" => Some(Rgba::WHITE),
        "red" => Some(Rgba::rgb(255, 0, 0)),
        "green" => Some(Rgba::rgb(0, 128, 0)),
        "blue" => Some(Rgba::rgb(0, 0, 255)),
        "gray" | "grey" => Some(Rgba::rgb(128, 128, 128)),
        "transparent" => Some(Rgba::with_alpha(0, 0, 0, 0)),
        _ => None,
    }
}

fn parse_hex_color(hex: &str) -> Option<Rgba> {
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Rgba::rgb(r, g, b))
        }
        4 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            let a = u8::from_str_radix(&hex[3..4].repeat(2), 16).ok()?;
            Some(Rgba::with_alpha(r, g, b, a))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgba::rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some(Rgba::with_alpha(r, g, b, a))
        }
        _ => None,
    }
}

fn parse_rgb_function(value: &str) -> Option<Rgba> {
    let start = value.find('(')?;
    let end = value.rfind(')')?;
    let body = value[start + 1..end].trim();
    if body.contains(',') {
        let channels: Vec<&str> = body.split(',').collect();
        if channels.len() < 3 {
            return None;
        }
        let r = parse_rgb_channel(channels[0])?;
        let g = parse_rgb_channel(channels[1])?;
        let b = parse_rgb_channel(channels[2])?;
        let a = channels
            .get(3)
            .and_then(|alpha| parse_alpha_channel(alpha))
            .unwrap_or(255);
        return Some(Rgba::with_alpha(r, g, b, a));
    }

    let (channels, alpha) = body
        .split_once('/')
        .map_or((body, None), |(channels, alpha)| (channels, Some(alpha)));
    let mut channels = channels.split_whitespace();
    let r = parse_rgb_channel(channels.next()?)?;
    let g = parse_rgb_channel(channels.next()?)?;
    let b = parse_rgb_channel(channels.next()?)?;
    let a = alpha.and_then(parse_alpha_channel).unwrap_or(255);
    Some(Rgba::with_alpha(r, g, b, a))
}

fn parse_rgb_channel(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (value.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(0.0, 255.0) as u8)
}

fn parse_alpha_channel(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (value.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn parse_text_align(value: &str) -> Option<TextAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Some(TextAlign::Left),
        "center" | "middle" => Some(TextAlign::Center),
        "right" | "end" => Some(TextAlign::Right),
        _ => None,
    }
}

fn parse_text_transform(value: &str) -> Option<TextTransform> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(TextTransform::None),
        "uppercase" => Some(TextTransform::Uppercase),
        "lowercase" => Some(TextTransform::Lowercase),
        "capitalize" => Some(TextTransform::Capitalize),
        _ => None,
    }
}

fn parse_vertical_align(value: &str) -> Option<VerticalAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "baseline" => Some(VerticalAlign::Baseline),
        "top" | "text-top" => Some(VerticalAlign::Top),
        "middle" => Some(VerticalAlign::Middle),
        "bottom" | "text-bottom" => Some(VerticalAlign::Bottom),
        _ => None,
    }
}

fn parse_box_sizing(value: &str) -> Option<BoxSizing> {
    match value.trim().to_ascii_lowercase().as_str() {
        "border-box" => Some(BoxSizing::BorderBox),
        "content-box" => Some(BoxSizing::ContentBox),
        _ => None,
    }
}

fn parse_font_family(value: &str) -> Option<String> {
    parse_font_family_with_available(value, &[])
}

fn parse_font_family_with_available(
    value: &str,
    available_font_families: &[String],
) -> Option<String> {
    parse_font_family_selection(value, available_font_families, &[])
        .map(|selection| selection.family)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FontFamilySelection {
    family: String,
    forced_weight: Option<FontWeight>,
}

fn parse_font_family_selection(
    value: &str,
    available_font_families: &[String],
    web_font_faces: &[WebFontFace],
) -> Option<FontFamilySelection> {
    if font_family_value_has_invalid_unquoted_colon(value) {
        return None;
    }

    let candidates = parse_font_family_candidates(value);

    if let Some(first) = candidates.first() {
        if let Some(generic) = generic_font_family(first) {
            return Some(FontFamilySelection {
                family: generic.to_string(),
                forced_weight: None,
            });
        }
    }

    for family in &candidates {
        if let Some(selection) = web_font_selection_for_family(family, web_font_faces) {
            return Some(selection);
        }
        if available_font_families
            .iter()
            .any(|available| available.eq_ignore_ascii_case(family))
        {
            return Some(FontFamilySelection {
                family: family.clone(),
                forced_weight: None,
            });
        }
    }

    for family in &candidates {
        if is_safe_system_font(family) {
            return Some(FontFamilySelection {
                family: family.clone(),
                forced_weight: None,
            });
        }
    }
    for family in &candidates {
        if let Some(generic) = generic_font_family(family) {
            return Some(FontFamilySelection {
                family: generic.to_string(),
                forced_weight: None,
            });
        }
    }
    candidates
        .into_iter()
        .next()
        .map(|family| FontFamilySelection {
            family,
            forced_weight: None,
        })
}

fn font_family_value_has_invalid_unquoted_colon(value: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ':' => return true,
            None => {}
        }
    }
    false
}

fn web_font_selection_for_family(
    family: &str,
    web_font_faces: &[WebFontFace],
) -> Option<FontFamilySelection> {
    let mut matched = web_font_faces
        .iter()
        .filter(|face| face.css_family.eq_ignore_ascii_case(family));
    let first = matched.next()?;
    let mut weights = vec![first.weight];
    for face in matched {
        if !weights.iter().any(|weight| weight.0 == face.weight.0) {
            weights.push(face.weight);
        }
    }

    Some(FontFamilySelection {
        family: first.actual_family.clone(),
        forced_weight: (weights.len() == 1).then_some(weights[0]),
    })
}

fn parse_font_family_candidates(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|candidate| {
            let family = candidate.trim().trim_matches('"').trim_matches('\'').trim();
            if family.is_empty()
                || matches!(
                    family.to_ascii_lowercase().as_str(),
                    "inherit" | "initial" | "unset"
                )
            {
                None
            } else {
                Some(family.to_string())
            }
        })
        .collect()
}

fn generic_font_family(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "sans-serif" | "ui-sans-serif" | "system-ui" | "-apple-system" => Some("sans-serif"),
        "serif" | "ui-serif" => Some("serif"),
        "monospace" | "ui-monospace" => Some("monospace"),
        _ => None,
    }
}

fn is_safe_system_font(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "arial"
            | "arial nova"
            | "avenir"
            | "avenir next"
            | "avenir next lt pro"
            | "helvetica"
            | "helvetica neue"
            | "nimbus sans"
            | "segoe ui"
            | "corbel"
            | "georgia"
            | "times"
            | "times new roman"
            | "cambria"
            | "courier"
            | "courier new"
    )
}

fn parse_font_weight(value: &str) -> FontWeight {
    match value.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => FontWeight::BOLD,
        "normal" | "lighter" => FontWeight::NORMAL,
        raw => raw
            .parse::<u16>()
            .ok()
            .map(FontWeight)
            .unwrap_or(FontWeight::NORMAL),
    }
}

fn parse_font_style(value: &str) -> FontStyle {
    match value.trim().to_ascii_lowercase().as_str() {
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => FontStyle::Normal,
    }
}

fn apply_border(style: &mut Style, value: &str) {
    if value.contains("none") {
        style.border = Edges::ZERO;
        return;
    }
    if let Some(border_style) = parse_border_line_style(value) {
        style.border_style = border_style;
    }

    let mut saw_width = false;
    for token in value.split_whitespace() {
        if let Some(width) = parse_px(token) {
            style.border = Edges::all(width);
            saw_width = true;
        }
        if let Some(color) = parse_color(token) {
            style.border_color = color;
        }
    }

    if !saw_width && !value.trim().is_empty() {
        style.border = Edges::all(1.0);
    }
}

#[derive(Debug, Clone, Copy)]
enum BorderSide {
    Top,
    Right,
    Bottom,
    Left,
}

fn apply_border_side(style: &mut Style, side: BorderSide, value: &str) {
    if value.contains("none") {
        set_border_side(&mut style.border, side, 0.0);
        return;
    }
    if let Some(border_style) =
        parse_border_line_style(value).filter(|style| !matches!(style, BorderLineStyle::Solid))
    {
        style.border_style = border_style;
    }

    let mut saw_width = false;
    for token in value.split_whitespace() {
        if let Some(width) = parse_px(token) {
            set_border_side(&mut style.border, side, width);
            saw_width = true;
        }
        if let Some(color) = parse_color(token) {
            style.border_color = color;
        }
    }

    if !saw_width && !value.trim().is_empty() {
        set_border_side(&mut style.border, side, 1.0);
    }
}

fn parse_border_line_style(value: &str) -> Option<BorderLineStyle> {
    for token in value.split_whitespace() {
        match token.to_ascii_lowercase().as_str() {
            "dashed" | "dotted" => return Some(BorderLineStyle::Dashed),
            "inset" | "groove" => return Some(BorderLineStyle::Inset),
            "solid" => return Some(BorderLineStyle::Solid),
            _ => {}
        }
    }
    None
}

fn set_border_side(border: &mut Edges, side: BorderSide, width: f32) {
    match side {
        BorderSide::Top => border.top = width,
        BorderSide::Right => border.right = width,
        BorderSide::Bottom => border.bottom = width,
        BorderSide::Left => border.left = width,
    }
}

fn element_tag(node: &NodeRef) -> Option<String> {
    node.as_element()
        .map(|element| element.name.local.to_string())
}

fn is_metadata_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "script" | "style" | "meta" | "link" | "title" | "base"
    )
}

fn find_first_tag(node: &NodeRef, tag: &str) -> Option<NodeRef> {
    if element_tag(node).as_deref() == Some(tag) {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = find_first_tag(&child, tag) {
            return Some(found);
        }
    }
    None
}

fn collect_rows(node: &NodeRef) -> Vec<NodeRef> {
    let mut rows = Vec::new();
    collect_rows_inner(node, &mut rows);
    rows
}

fn collect_rows_inner(node: &NodeRef, rows: &mut Vec<NodeRef>) {
    for child in node.children() {
        match element_tag(&child).as_deref() {
            Some("tr") => rows.push(child),
            Some("thead" | "tbody" | "tfoot") => collect_rows_inner(&child, rows),
            _ => {}
        }
    }
}

fn collect_cells(row: &NodeRef) -> Vec<NodeRef> {
    row.children()
        .filter(|child| matches!(element_tag(child).as_deref(), Some("td" | "th")))
        .collect()
}

#[derive(Debug)]
struct TableGrid {
    rows: Vec<TableRow>,
    column_count: usize,
    col_widths: Vec<Option<Length>>,
}

#[derive(Debug)]
struct TableRow {
    node: NodeRef,
    cells: Vec<TableCell>,
}

#[derive(Debug)]
struct TableCell {
    node: NodeRef,
    col: usize,
    colspan: usize,
}

fn build_table_grid(table: &NodeRef) -> TableGrid {
    let rows = collect_rows(table);
    let mut active_rowspans: Vec<usize> = Vec::new();
    let mut grid_rows = Vec::with_capacity(rows.len());
    let mut column_count = 0usize;

    for row in rows {
        let mut col = 0usize;
        let mut cells = Vec::new();

        for cell in collect_cells(&row) {
            while active_rowspans.get(col).copied().unwrap_or(0) > 0 {
                col += 1;
            }

            let colspan = parse_span_attr(&cell, "colspan");
            let rowspan = parse_span_attr(&cell, "rowspan");
            if active_rowspans.len() < col + colspan {
                active_rowspans.resize(col + colspan, 0);
            }

            cells.push(TableCell {
                node: cell,
                col,
                colspan,
            });

            for occupied in &mut active_rowspans[col..col + colspan] {
                *occupied = (*occupied).max(rowspan);
            }
            col += colspan;
        }

        for occupied in &mut active_rowspans {
            *occupied = occupied.saturating_sub(1);
        }

        column_count = column_count.max(col).max(active_rowspans.len());
        grid_rows.push(TableRow { node: row, cells });
    }

    let mut col_widths = collect_col_widths(table);
    if col_widths.len() < column_count {
        col_widths.resize(column_count, None);
    }

    TableGrid {
        rows: grid_rows,
        column_count,
        col_widths,
    }
}

fn collect_col_widths(table: &NodeRef) -> Vec<Option<Length>> {
    let mut widths = Vec::new();
    collect_col_widths_inner(table, &mut widths);
    widths
}

fn collect_col_widths_inner(node: &NodeRef, widths: &mut Vec<Option<Length>>) {
    for child in node.children() {
        match element_tag(&child).as_deref() {
            Some("col") => {
                widths.push(attr(&child, "width").and_then(|value| parse_length(&value)))
            }
            Some("colgroup") => collect_col_widths_inner(&child, widths),
            _ => {}
        }
    }
}

fn parse_span_attr(node: &NodeRef, attr_name: &str) -> usize {
    attr(node, attr_name)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
        .min(32)
}

fn column_offset(widths: &[f32], col: usize, spacing: f32) -> f32 {
    widths.iter().take(col).copied().sum::<f32>() + spacing * col as f32
}

fn spanned_width(widths: &[f32], col: usize, colspan: usize, spacing: f32) -> f32 {
    let end = (col + colspan).min(widths.len());
    let span = end.saturating_sub(col).max(1);
    widths[col.min(widths.len().saturating_sub(1))..end]
        .iter()
        .copied()
        .sum::<f32>()
        + spacing * span.saturating_sub(1) as f32
}

fn distribute_fixed_table_column_widths(widths: Vec<Option<f32>>, available: f32) -> Vec<f32> {
    let count = widths.len().max(1);
    let fixed_total: f32 = widths.iter().flatten().sum();
    let auto_count = widths.iter().filter(|width| width.is_none()).count();

    if auto_count > 0 {
        let auto_width = ((available - fixed_total).max(auto_count as f32)) / auto_count as f32;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(auto_width).max(1.0))
            .collect();
    }

    if fixed_total > 0.0 {
        let scale = available / fixed_total;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(0.0) * scale)
            .map(|width| width.max(1.0))
            .collect();
    }

    vec![(available / count as f32).max(1.0); count]
}

fn translate_layout_children(layout: &mut LayoutBox, dx: f32, dy: f32) {
    for child in &mut layout.children {
        translate_layout(child, dx, dy);
    }
}

fn translate_layout(layout: &mut LayoutBox, dx: f32, dy: f32) {
    layout.rect.x += dx;
    layout.rect.y += dy;
    for child in &mut layout.children {
        translate_layout(child, dx, dy);
    }
}

fn is_inline_flow(tag: &str, style: &Style) -> bool {
    matches!(style.display, Display::InlineBlock)
        || (style.display == Display::Inline && (tag == "img" || inline_style_has_own_box(style)))
}

fn inline_flow_uses_bottom_edge_baseline(layout: &LayoutBox) -> bool {
    matches!(layout.kind, LayoutKind::Image(_)) || !layout_contains_line_box(layout)
}

fn needs_synthetic_bold_paint(style: &Style) -> bool {
    style.font_weight.0 >= FontWeight::SEMIBOLD.0
        && style
            .font_face_weight
            .is_some_and(|face_weight| face_weight.0 < FontWeight::SEMIBOLD.0)
}

fn inline_flow_line_advance(
    layout: &LayoutBox,
    advance: f32,
    parent_line_height: f32,
    db: &fontdb::Database,
) -> f32 {
    match layout.style.display {
        Display::Inline if layout_contains_line_box(layout) => {
            resolved_line_height_from_db(db, &layout.style)
        }
        Display::InlineBlock if !matches!(layout.kind, LayoutKind::Image(_)) => {
            layout.style.margin.vertical() + layout.rect.height.max(parent_line_height)
        }
        _ => advance,
    }
}

fn layout_contains_line_box(layout: &LayoutBox) -> bool {
    matches!(layout.kind, LayoutKind::Text(_) | LayoutKind::RichText(_))
        || layout.children.iter().any(layout_contains_line_box)
}

fn inline_style_has_own_box(style: &Style) -> bool {
    style.background.is_some()
        || style.background_image.is_some()
        || !style.padding.is_zero()
        || style.border.max_width() > 0.0
        || style.border_radius > 0.0
}

fn flush_inline_row(
    row: &mut Vec<LayoutBox>,
    row_width: &mut f32,
    row_height: &mut f32,
    style: &Style,
    containing_width: f32,
    cursor_y: &mut f32,
    children: &mut Vec<LayoutBox>,
) -> bool {
    if row.is_empty() {
        return false;
    }

    let free = (containing_width - *row_width).max(0.0);
    let dx = match style.text_align {
        TextAlign::Left => 0.0,
        TextAlign::Center => free / 2.0,
        TextAlign::Right => free,
    };
    for mut child in row.drain(..) {
        if dx > 0.0 {
            translate_layout(&mut child, dx, 0.0);
        }
        children.push(child);
    }
    *cursor_y += *row_height;
    *row_width = 0.0;
    *row_height = 0.0;
    true
}

fn align_table_child_to_parent_text(
    child: &mut LayoutBox,
    parent_style: &Style,
    container_x: f32,
    container_width: f32,
) {
    if !matches!(child.kind, LayoutKind::Table)
        || child.style.margin_left_auto
        || child.style.margin_right_auto
    {
        return;
    }

    let free = (container_width - child.rect.width).max(0.0);
    let target_x = match parent_style.text_align {
        TextAlign::Center => container_x + free / 2.0,
        TextAlign::Right => container_x + free,
        TextAlign::Left => return,
    };
    let dx = target_x - child.rect.x;
    if dx.abs() > f32::EPSILON {
        translate_layout(child, dx, 0.0);
    }
}

fn align_image_child_to_legacy_align(
    child: &mut LayoutBox,
    parent_style: &Style,
    container_x: f32,
    container_width: f32,
) {
    if !parent_style.align_from_attribute || !matches!(child.kind, LayoutKind::Image(_)) {
        return;
    }

    let free = (container_width - child.rect.width).max(0.0);
    let target_x = match parent_style.text_align {
        TextAlign::Center => container_x + free / 2.0,
        TextAlign::Right => container_x + free,
        TextAlign::Left => return,
    };
    let dx = target_x - child.rect.x;
    if dx.abs() > f32::EPSILON {
        translate_layout(child, dx, 0.0);
    }
}

fn can_collapse_sibling_margin(display: Display) -> bool {
    matches!(display, Display::Block | Display::Table)
}

fn block_allows_trailing_margin_collapse(style: &Style) -> bool {
    style.height.is_none()
        && style.min_height.is_none()
        && style.border.top <= 0.0
        && style.border.bottom <= 0.0
        && style.padding.top <= 0.0
        && style.padding.bottom <= 0.0
}

fn attr(node: &NodeRef, name: &str) -> Option<String> {
    node.as_element().and_then(|element| {
        element
            .attributes
            .borrow()
            .get(name)
            .map(std::borrow::ToOwned::to_owned)
    })
}

fn text_content(node: &NodeRef) -> String {
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
        return HARD_BREAK.to_string();
    }
    if tag == "img" {
        return attr(node, "alt").unwrap_or_default();
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

fn table_cell_is_spacer(node: &NodeRef) -> bool {
    let text = text_content(node);
    text.chars().any(|ch| ch == '\u{00a0}')
        && text
            .chars()
            .all(|ch| ch == '\u{00a0}' || is_collapsible_whitespace(ch))
}

fn inline_can_flatten(node: &NodeRef, style: &Style) -> bool {
    for child in node.children() {
        if child.as_text().is_some() {
            continue;
        }

        let Some(tag) = element_tag(&child) else {
            continue;
        };
        if is_metadata_tag(&tag) || tag == "br" {
            continue;
        }
        if tag == "img" {
            return false;
        }

        let child_style = style_for_node(&child, style);
        match child_style.display {
            Display::None => {}
            Display::Inline => {
                if !inline_can_flatten(&child, &child_style) {
                    return false;
                }
            }
            Display::Block
            | Display::InlineBlock
            | Display::Flex
            | Display::Table
            | Display::TableRow
            | Display::TableCell => return false,
        }
    }
    true
}

fn append_text_span(out: &mut Vec<TextSpan>, text: &str, style: &Style) {
    if !text.is_empty() {
        out.push(TextSpan::from_style(text.to_string(), style));
    }
}

fn append_inline_spans(node: &NodeRef, style: &Style, out: &mut Vec<TextSpan>) {
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
        append_text_span(out, &HARD_BREAK.to_string(), style);
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

fn normalize_text(text: &str) -> String {
    let style = Style::initial();
    spans_text(&normalize_text_spans(&[TextSpan::from_style(
        text.to_string(),
        &style,
    )]))
}

fn normalize_text_spans(spans: &[TextSpan]) -> Vec<TextSpan> {
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
                if !rich_text_ends_with_newline(&out) {
                    out.push(TextSpan::with_run_style(
                        "\n".to_string(),
                        span.style.clone(),
                    ));
                }
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

fn is_collapsible_whitespace(ch: char) -> bool {
    ch != '\u{00a0}' && ch.is_whitespace()
}

fn spans_text(spans: &[TextSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

fn text_spans_match_style(spans: &[TextSpan], style: &Style) -> bool {
    let parent_style = TextRunStyle::from_style(style);
    spans.iter().all(|span| span.style == parent_style)
}

fn rich_text_style_spans<'a>(
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

fn fill_style_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba, radius: f32) {
    if radius <= 0.0 {
        fill_rect(pixmap, scale, rect, color);
        return;
    }
    fill_rounded_rect(pixmap, scale, rect, color, radius);
}

fn paint_box_shadow(
    pixmap: &mut Pixmap,
    scale: f32,
    rect: Rect,
    radius: f32,
    color: Rgba,
    shadow: &BoxShadow,
) {
    if color.a == 0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || (shadow.offset_x == 0.0
            && shadow.offset_y == 0.0
            && shadow.blur_radius == 0.0
            && shadow.spread == 0.0)
    {
        return;
    }

    let spread = shadow.spread;
    let shadow_width = rect.width + spread * 2.0;
    let shadow_height = rect.height + spread * 2.0;
    if shadow_width <= 0.0 || shadow_height <= 0.0 {
        return;
    }

    // Blink passes sigma = blur-radius / 2 to Skia. Use a 3-sigma pad so the
    // local mask has room for the visible falloff.
    let sigma = (shadow.blur_radius * scale * 0.5).max(0.0);
    let pad_px = (sigma * 3.0).ceil().max(0.0);
    let x0 = ((rect.x + shadow.offset_x - spread) * scale - pad_px)
        .floor()
        .max(0.0);
    let y0 = ((rect.y + shadow.offset_y - spread) * scale - pad_px)
        .floor()
        .max(0.0);
    let x1 = ((rect.x + shadow.offset_x + rect.width + spread) * scale + pad_px)
        .ceil()
        .min(pixmap.width() as f32);
    let y1 = ((rect.y + shadow.offset_y + rect.height + spread) * scale + pad_px)
        .ceil()
        .min(pixmap.height() as f32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    let width = (x1 - x0) as u32;
    let height = (y1 - y0) as u32;
    if u64::from(width) * u64::from(height) > DEFAULT_MAX_DECODED_PIXELS {
        return;
    }
    let Some(mut mask) = Pixmap::new(width, height) else {
        return;
    };

    let mask_rect = Rect::new(
        (rect.x + shadow.offset_x - spread) * scale - x0,
        (rect.y + shadow.offset_y - spread) * scale - y0,
        shadow_width * scale,
        shadow_height * scale,
    );
    let mask_radius = ((radius + spread).max(0.0) * scale)
        .min(mask_rect.width / 2.0)
        .min(mask_rect.height / 2.0);
    fill_style_rect(&mut mask, 1.0, mask_rect, Rgba::BLACK, mask_radius);

    let alpha = blurred_mask_alpha(&mask, sigma);
    let x0 = x0 as i32;
    let y0 = y0 as i32;
    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;
    let data = pixmap.data_mut();
    for y in 0..height as i32 {
        let py = y0 + y;
        if py < 0 || py >= pixmap_height {
            continue;
        }
        for x in 0..width as i32 {
            let px = x0 + x;
            if px < 0 || px >= pixmap_width {
                continue;
            }
            let src_alpha = alpha[(y as u32 * width + x as u32) as usize];
            if src_alpha == 0 {
                continue;
            }
            let a = ((u16::from(src_alpha) * u16::from(color.a) + 127) / 255) as u8;
            if a == 0 {
                continue;
            }
            let index = ((py as u32 * pixmap_width as u32 + px as u32) * 4) as usize;
            composite_pixel(&mut data[index..index + 4], color.r, color.g, color.b, a);
        }
    }
}

fn blurred_mask_alpha(mask: &Pixmap, sigma: f32) -> Vec<u8> {
    let width = mask.width() as usize;
    let height = mask.height() as usize;
    let mut alpha = vec![0u8; width * height];
    for (index, pixel) in mask.data().chunks_exact(4).enumerate() {
        alpha[index] = pixel[3];
    }
    if sigma <= 0.0 {
        return alpha;
    }

    let kernel = gaussian_kernel(sigma);
    let radius = (kernel.len() / 2) as isize;
    let mut horizontal = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (kernel_index, weight) in kernel.iter().enumerate() {
                let dx = kernel_index as isize - radius;
                let sx = (x as isize + dx).clamp(0, width as isize - 1) as usize;
                sum += f32::from(alpha[y * width + sx]) * weight;
            }
            horizontal[y * width + x] = sum;
        }
    }

    let mut blurred = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (kernel_index, weight) in kernel.iter().enumerate() {
                let dy = kernel_index as isize - radius;
                let sy = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                sum += horizontal[sy * width + x] * weight;
            }
            blurred[y * width + x] = sum.round().clamp(0.0, 255.0) as u8;
        }
    }
    blurred
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let denominator = 2.0 * sigma * sigma;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut total = 0.0;
    for offset in -radius..=radius {
        let value = (-(offset * offset) as f32 / denominator).exp();
        kernel.push(value);
        total += value;
    }
    for value in &mut kernel {
        *value /= total;
    }
    kernel
}

fn fill_rounded_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba, radius: f32) {
    if color.a == 0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let x = rect.x * scale;
    let y = rect.y * scale;
    let width = rect.width * scale;
    let height = rect.height * scale;
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return;
    }

    let radius = (radius * scale).min(width / 2.0).min(height / 2.0).max(0.0);
    if radius <= 0.0 {
        fill_rect(pixmap, scale, rect, color);
        return;
    }

    let x0 = x;
    let y0 = y;
    let x1 = x + width;
    let y1 = y + height;
    let mut path = PathBuilder::new();
    path.move_to(x0 + radius, y0);
    path.line_to(x1 - radius, y0);
    path.quad_to(x1, y0, x1, y0 + radius);
    path.line_to(x1, y1 - radius);
    path.quad_to(x1, y1, x1 - radius, y1);
    path.line_to(x0 + radius, y1);
    path.quad_to(x0, y1, x0, y1 - radius);
    path.line_to(x0, y0 + radius);
    path.quad_to(x0, y0, x0 + radius, y0);
    path.close();
    let Some(path) = path.finish() else {
        return;
    };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
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

fn fill_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba) {
    if color.a == 0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let x = rect.x * scale;
    let y = rect.y * scale;
    let width = rect.width * scale;
    let height = rect.height * scale;
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return;
    }
    let x0 = x.max(0.0).floor();
    let y0 = y.max(0.0).floor();
    let x1 = (x + width).min(pixmap.width() as f32).ceil();
    let y1 = (y + height).min(pixmap.height() as f32).ceil();
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let Some(rect) = SkiaRect::from_xywh(x0, y0, x1 - x0, y1 - y0) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
}

fn stroke_style_border(
    pixmap: &mut Pixmap,
    scale: f32,
    rect: Rect,
    border: Edges,
    color: Rgba,
    style: BorderLineStyle,
    _radius: f32,
) {
    if style == BorderLineStyle::Dashed {
        stroke_dashed_border(pixmap, scale, rect, border, color);
        return;
    }
    if style == BorderLineStyle::Inset {
        stroke_inset_border(pixmap, scale, rect, border, color);
        return;
    }

    if border.top == border.right
        && border.top == border.bottom
        && border.top == border.left
        && border.top > 0.0
    {
        stroke_rect(pixmap, scale, rect, border.top, color);
        return;
    }

    if border.top > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, rect.width, border.top),
            color,
        );
    }
    if border.bottom > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(
                rect.x,
                rect.y + rect.height - border.bottom,
                rect.width,
                border.bottom,
            ),
            color,
        );
    }
    if border.left > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, border.left, rect.height),
            color,
        );
    }
    if border.right > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(
                rect.x + rect.width - border.right,
                rect.y,
                border.right,
                rect.height,
            ),
            color,
        );
    }
}

fn stroke_inset_border(pixmap: &mut Pixmap, scale: f32, rect: Rect, border: Edges, color: Rgba) {
    let dark = inset_border_edge_color(color, true);
    let light = inset_border_edge_color(color, false);
    if border.top > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, rect.width, border.top),
            dark,
        );
    }
    if border.left > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, border.left, rect.height),
            dark,
        );
    }
    if border.bottom > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(
                rect.x,
                rect.y + rect.height - border.bottom,
                rect.width,
                border.bottom,
            ),
            light,
        );
    }
    if border.right > 0.0 {
        fill_rect(
            pixmap,
            scale,
            Rect::new(
                rect.x + rect.width - border.right,
                rect.y,
                border.right,
                rect.height,
            ),
            light,
        );
    }
}

fn inset_border_edge_color(color: Rgba, dark_edge: bool) -> Rgba {
    let mix = if dark_edge { 0.2 } else { 0.86 };
    Rgba::with_alpha(
        mix_channel(color.r, 255, mix),
        mix_channel(color.g, 255, mix),
        mix_channel(color.b, 255, mix),
        color.a,
    )
}

fn mix_channel(from: u8, to: u8, amount: f32) -> u8 {
    (f32::from(from) + (f32::from(to) - f32::from(from)) * amount)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn stroke_dashed_border(pixmap: &mut Pixmap, scale: f32, rect: Rect, border: Edges, color: Rgba) {
    if border.top > 0.0 {
        fill_dashed_line(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, rect.width, border.top),
            color,
            true,
        );
    }
    if border.bottom > 0.0 {
        fill_dashed_line(
            pixmap,
            scale,
            Rect::new(
                rect.x,
                rect.y + rect.height - border.bottom,
                rect.width,
                border.bottom,
            ),
            color,
            true,
        );
    }
    if border.left > 0.0 {
        fill_dashed_line(
            pixmap,
            scale,
            Rect::new(rect.x, rect.y, border.left, rect.height),
            color,
            false,
        );
    }
    if border.right > 0.0 {
        fill_dashed_line(
            pixmap,
            scale,
            Rect::new(
                rect.x + rect.width - border.right,
                rect.y,
                border.right,
                rect.height,
            ),
            color,
            false,
        );
    }
}

fn fill_dashed_line(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba, horizontal: bool) {
    let thickness = if horizontal { rect.height } else { rect.width }.max(1.0);
    let dash = (thickness * 3.0).max(6.0);
    let gap = (thickness * 2.0).max(4.0);
    let end = if horizontal {
        rect.x + rect.width
    } else {
        rect.y + rect.height
    };
    let mut cursor = if horizontal { rect.x } else { rect.y };
    while cursor < end {
        let length = dash.min(end - cursor);
        let dash_rect = if horizontal {
            Rect::new(cursor, rect.y, length, rect.height)
        } else {
            Rect::new(rect.x, cursor, rect.width, length)
        };
        fill_rect(pixmap, scale, dash_rect, color);
        cursor += dash + gap;
    }
}

fn stroke_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, width: f32, color: Rgba) {
    let width = width.max(1.0);
    fill_rect(
        pixmap,
        scale,
        Rect::new(rect.x, rect.y, rect.width, width),
        color,
    );
    fill_rect(
        pixmap,
        scale,
        Rect::new(rect.x, rect.y + rect.height - width, rect.width, width),
        color,
    );
    fill_rect(
        pixmap,
        scale,
        Rect::new(rect.x, rect.y, width, rect.height),
        color,
    );
    fill_rect(
        pixmap,
        scale,
        Rect::new(rect.x + rect.width - width, rect.y, width, rect.height),
        color,
    );
}

fn blend_text_rect(pixmap: &mut Pixmap, x: i32, y: i32, width: u32, height: u32, color: TextColor) {
    let (r, g, b, a) = color.as_rgba_tuple();
    if a == 0 {
        return;
    }

    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;
    let data = pixmap.data_mut();

    for dy in 0..height as i32 {
        let py = y + dy;
        if py < 0 || py >= pixmap_height {
            continue;
        }
        for dx in 0..width as i32 {
            let px = x + dx;
            if px < 0 || px >= pixmap_width {
                continue;
            }
            let index = ((py as u32 * pixmap_width as u32 + px as u32) * 4) as usize;
            composite_pixel(&mut data[index..index + 4], r, g, b, a);
        }
    }
}

fn draw_image_with_fit(
    pixmap: &mut Pixmap,
    scale: f32,
    rect: Rect,
    image: &ImageData,
    paint: ImageFitPaint,
) {
    let object_rect = object_fit_rect(rect, image, paint.fit, paint.position);
    let snapped_rect = pixel_snapped_rect(object_rect, scale);
    let snapped_clip = pixel_snapped_rect(rect, scale);
    draw_image_clipped(
        pixmap,
        scale,
        snapped_rect,
        image,
        ImageClipPaint {
            source: ImageSourceRect::full(image),
            clip: Some(snapped_clip),
            radius: paint.radius,
            opacity: paint.opacity,
        },
    );
}

fn draw_background_image(
    pixmap: &mut Pixmap,
    scale: f32,
    rect: Rect,
    image: &ImageData,
    paint: BackgroundImagePaint,
) {
    if image.width == 0 || image.height == 0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let (tile_width, tile_height) = background_tile_size(rect, image, paint.size);
    let tile_x = positioned_offset(rect.x, rect.width, tile_width, paint.position.x);
    let tile_y = positioned_offset(rect.y, rect.height, tile_height, paint.position.y);

    if paint.repeat == BackgroundRepeat::NoRepeat || paint.size != BackgroundSize::Auto {
        draw_image_clipped(
            pixmap,
            scale,
            Rect::new(tile_x, tile_y, tile_width, tile_height),
            image,
            ImageClipPaint {
                source: ImageSourceRect::full(image),
                clip: Some(rect),
                radius: paint.radius,
                opacity: paint.opacity,
            },
        );
        return;
    }

    let end_x = rect.x + rect.width;
    let end_y = rect.y + rect.height;
    let mut tile_y = first_repeated_tile_position(tile_y, rect.y, tile_height);
    while tile_y < end_y {
        let mut tile_x = first_repeated_tile_position(tile_x, rect.x, tile_width);
        while tile_x < end_x {
            draw_image_clipped(
                pixmap,
                scale,
                Rect::new(tile_x, tile_y, tile_width, tile_height),
                image,
                ImageClipPaint {
                    source: ImageSourceRect::full(image),
                    clip: Some(rect),
                    radius: paint.radius,
                    opacity: paint.opacity,
                },
            );
            tile_x += tile_width.max(1.0);
        }
        tile_y += tile_height.max(1.0);
    }
}

fn background_tile_size(rect: Rect, image: &ImageData, size: BackgroundSize) -> (f32, f32) {
    let natural_width = image.width as f32;
    let natural_height = image.height as f32;
    match size {
        BackgroundSize::Auto => (natural_width, natural_height),
        BackgroundSize::Cover => {
            let ratio = (rect.width / natural_width).max(rect.height / natural_height);
            (natural_width * ratio, natural_height * ratio)
        }
        BackgroundSize::Contain => {
            let ratio = (rect.width / natural_width).min(rect.height / natural_height);
            (natural_width * ratio, natural_height * ratio)
        }
    }
}

fn positioned_offset(origin: f32, available: f32, size: f32, axis: PositionAxis) -> f32 {
    origin + (available - size) * axis.factor()
}

fn first_repeated_tile_position(positioned: f32, clip_start: f32, tile_size: f32) -> f32 {
    let tile_size = tile_size.max(1.0);
    let mut position = positioned;
    if position > clip_start {
        let steps = ((position - clip_start) / tile_size).ceil();
        position -= steps * tile_size;
    }
    while position + tile_size <= clip_start {
        position += tile_size;
    }
    position
}

fn pixel_snapped_rect(rect: Rect, scale: f32) -> Rect {
    if scale <= 0.0 {
        return rect;
    }
    let x = (rect.x * scale).round() / scale;
    let y = (rect.y * scale).round() / scale;
    let right = ((rect.x + rect.width) * scale).round() / scale;
    let bottom = ((rect.y + rect.height) * scale).round() / scale;
    Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ImageFitPaint {
    fit: ObjectFit,
    position: ObjectPosition,
    radius: f32,
    opacity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ImageSourceRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl ImageSourceRect {
    fn full(image: &ImageData) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: image.width as f32,
            height: image.height as f32,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ImageClipPaint {
    source: ImageSourceRect,
    clip: Option<Rect>,
    radius: f32,
    opacity: f32,
}

fn object_fit_rect(
    rect: Rect,
    image: &ImageData,
    fit: ObjectFit,
    position: ObjectPosition,
) -> Rect {
    if rect.width <= 0.0 || rect.height <= 0.0 || image.width == 0 || image.height == 0 {
        return rect;
    }

    let natural_width = image.width as f32;
    let natural_height = image.height as f32;
    let (object_width, object_height) = match fit {
        ObjectFit::Fill => (rect.width, rect.height),
        ObjectFit::Contain => fit_size_to_aspect(
            rect.width,
            rect.height,
            natural_width,
            natural_height,
            false,
        ),
        ObjectFit::Cover => {
            fit_size_to_aspect(rect.width, rect.height, natural_width, natural_height, true)
        }
        ObjectFit::None => (natural_width, natural_height),
        ObjectFit::ScaleDown => {
            let contained = fit_size_to_aspect(
                rect.width,
                rect.height,
                natural_width,
                natural_height,
                false,
            );
            if contained.0 <= natural_width && contained.1 <= natural_height {
                contained
            } else {
                (natural_width, natural_height)
            }
        }
    };

    Rect::new(
        positioned_offset(rect.x, rect.width, object_width, position.x),
        positioned_offset(rect.y, rect.height, object_height, position.y),
        object_width.max(0.0),
        object_height.max(0.0),
    )
}

fn fit_size_to_aspect(
    available_width: f32,
    available_height: f32,
    natural_width: f32,
    natural_height: f32,
    cover: bool,
) -> (f32, f32) {
    if natural_width <= 0.0 || natural_height <= 0.0 {
        return (available_width.max(0.0), available_height.max(0.0));
    }
    let width_ratio = available_width / natural_width;
    let height_ratio = available_height / natural_height;
    let ratio = if cover {
        width_ratio.max(height_ratio)
    } else {
        width_ratio.min(height_ratio)
    };
    (
        (natural_width * ratio).max(1.0),
        (natural_height * ratio).max(1.0),
    )
}

fn draw_image_clipped(
    pixmap: &mut Pixmap,
    scale: f32,
    rect: Rect,
    image: &ImageData,
    paint: ImageClipPaint,
) {
    let source = paint.source;
    let clip = paint.clip;
    if rect.width <= 0.0
        || rect.height <= 0.0
        || scale <= 0.0
        || image.width == 0
        || image.height == 0
        || source.width <= 0.0
        || source.height <= 0.0
        || image.rgba.is_empty()
    {
        return;
    }

    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;
    let image_right = rect.x + rect.width;
    let image_bottom = rect.y + rect.height;
    let (mut start_x, mut end_x) = pixel_bounds(rect.x, image_right, scale, pixmap_width);
    let (mut start_y, mut end_y) = pixel_bounds(rect.y, image_bottom, scale, pixmap_height);

    if let Some(clip) = clip {
        let (clip_start_x, clip_end_x) =
            pixel_bounds(clip.x, clip.x + clip.width, scale, pixmap_width);
        let (clip_start_y, clip_end_y) =
            pixel_bounds(clip.y, clip.y + clip.height, scale, pixmap_height);
        start_x = start_x.max(clip_start_x);
        start_y = start_y.max(clip_start_y);
        end_x = end_x.min(clip_end_x);
        end_y = end_y.min(clip_end_y);
    }
    if start_x >= end_x || start_y >= end_y {
        return;
    }

    let data = pixmap.data_mut();
    let source_pixel_width = source.width / (rect.width * scale);
    let source_pixel_height = source.height / (rect.height * scale);
    let downscaling = source_pixel_width > 1.2 || source_pixel_height > 1.2;
    let pixel_area = 1.0 / (scale * scale);
    let radius_rect = clip.unwrap_or(rect);

    for py in start_y..end_y {
        let pixel_top = py as f32 / scale;
        let pixel_bottom = (py as f32 + 1.0) / scale;
        let paint_top = pixel_top
            .max(rect.y)
            .max(clip.map_or(f32::NEG_INFINITY, |clip| clip.y));
        let paint_bottom = pixel_bottom
            .min(image_bottom)
            .min(clip.map_or(f32::INFINITY, |clip| clip.y + clip.height));
        if paint_top >= paint_bottom {
            continue;
        }
        let src_y0 = source.y + (paint_top - rect.y) * source.height / rect.height;
        let src_y1 = source.y + (paint_bottom - rect.y) * source.height / rect.height;
        let src_y = (src_y0 + src_y1) / 2.0 - 0.5;
        for px in start_x..end_x {
            let pixel_left = px as f32 / scale;
            let pixel_right = (px as f32 + 1.0) / scale;
            let paint_left = pixel_left
                .max(rect.x)
                .max(clip.map_or(f32::NEG_INFINITY, |clip| clip.x));
            let paint_right = pixel_right
                .min(image_right)
                .min(clip.map_or(f32::INFINITY, |clip| clip.x + clip.width));
            if paint_left >= paint_right {
                continue;
            }
            let mut coverage = ((paint_right - paint_left) * (paint_bottom - paint_top)
                / pixel_area)
                .clamp(0.0, 1.0);
            coverage *= rounded_rect_coverage(
                radius_rect,
                paint.radius,
                paint_left,
                paint_top,
                paint_right,
                paint_bottom,
            );
            if coverage <= 0.0 {
                continue;
            }

            let src_x0 = source.x + (paint_left - rect.x) * source.width / rect.width;
            let src_x1 = source.x + (paint_right - rect.x) * source.width / rect.width;
            let src_x = (src_x0 + src_x1) / 2.0 - 0.5;
            let [r, g, b, a] = if downscaling {
                sample_image_area(
                    image,
                    src_x0 + IMAGE_AREA_SAMPLE_PHASE,
                    src_y0 + IMAGE_AREA_SAMPLE_PHASE,
                    src_x1 + IMAGE_AREA_SAMPLE_PHASE,
                    src_y1 + IMAGE_AREA_SAMPLE_PHASE,
                )
            } else {
                sample_image_bilinear(image, src_x, src_y)
            };
            let a = (a as f32 * coverage * paint.opacity)
                .round()
                .clamp(0.0, 255.0) as u8;
            if a == 0 {
                continue;
            }
            let dst_index = ((py as u32 * pixmap_width as u32 + px as u32) * 4) as usize;
            composite_pixel(&mut data[dst_index..dst_index + 4], r, g, b, a);
        }
    }
}

// Skia's downscale sampling lands slightly later than a pure source-edge box
// average for the regression image set. Keep this limited to area sampling so
// 1:1 and upscaled images still use the normal bilinear center convention.
const IMAGE_AREA_SAMPLE_PHASE: f32 = 0.25;

fn pixel_bounds(start: f32, end: f32, scale: f32, limit: i32) -> (i32, i32) {
    let start = (start * scale).floor() as i32;
    let end = (end * scale).ceil() as i32;
    (start.max(0), end.min(limit))
}

fn rounded_rect_coverage(
    rect: Rect,
    radius: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> f32 {
    if radius <= 0.0 {
        return 1.0;
    }

    let sample_points = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    let mut inside = 0;
    for (x_factor, y_factor) in sample_points {
        let x = left + (right - left) * x_factor;
        let y = top + (bottom - top) * y_factor;
        if point_in_rounded_rect(x, y, rect, radius) {
            inside += 1;
        }
    }

    inside as f32 / sample_points.len() as f32
}

fn point_in_rounded_rect(x: f32, y: f32, rect: Rect, radius: f32) -> bool {
    if radius <= 0.0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return true;
    }
    let radius = radius.min(rect.width / 2.0).min(rect.height / 2.0).max(0.0);
    if radius <= 0.0 {
        return true;
    }

    let left = rect.x;
    let top = rect.y;
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    if x < left || x > right || y < top || y > bottom {
        return false;
    }

    let corner_x = if x < left + radius {
        left + radius
    } else if x > right - radius {
        right - radius
    } else {
        return true;
    };
    let corner_y = if y < top + radius {
        top + radius
    } else if y > bottom - radius {
        bottom - radius
    } else {
        return true;
    };

    let dx = x - corner_x;
    let dy = y - corner_y;
    dx * dx + dy * dy <= radius * radius
}

fn sample_image_bilinear(image: &ImageData, x: f32, y: f32) -> [u8; 4] {
    let max_x = image.width.saturating_sub(1);
    let max_y = image.height.saturating_sub(1);
    let x = x.clamp(0.0, max_x as f32);
    let y = y.clamp(0.0, max_y as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = x0.saturating_add(1).min(max_x);
    let y1 = y0.saturating_add(1).min(max_y);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;

    let p00 = premultiply_pixel(image_pixel(image, x0, y0));
    let p10 = premultiply_pixel(image_pixel(image, x1, y0));
    let p01 = premultiply_pixel(image_pixel(image, x0, y1));
    let p11 = premultiply_pixel(image_pixel(image, x1, y1));
    let mut sampled = [0.0; 4];

    for channel in 0..4 {
        let top = lerp(p00[channel], p10[channel], tx);
        let bottom = lerp(p01[channel], p11[channel], tx);
        sampled[channel] = lerp(top, bottom, ty);
    }

    unpremultiply_sample(sampled)
}

fn sample_image_area(image: &ImageData, x0: f32, y0: f32, x1: f32, y1: f32) -> [u8; 4] {
    let max_x = image.width as f32;
    let max_y = image.height as f32;
    let x0 = x0.clamp(0.0, max_x);
    let y0 = y0.clamp(0.0, max_y);
    let x1 = x1.clamp(0.0, max_x);
    let y1 = y1.clamp(0.0, max_y);
    if x1 <= x0 || y1 <= y0 {
        return sample_image_bilinear(image, (x0 + x1) / 2.0 - 0.5, (y0 + y1) / 2.0 - 0.5);
    }

    let sx0 = x0.floor().max(0.0) as u32;
    let sy0 = y0.floor().max(0.0) as u32;
    let sx1 = x1.ceil().min(max_x) as u32;
    let sy1 = y1.ceil().min(max_y) as u32;
    let mut sums = [0.0_f32; 4];
    let mut total = 0.0_f32;

    for sy in sy0..sy1 {
        let py0 = sy as f32;
        let py1 = py0 + 1.0;
        let wy = (py1.min(y1) - py0.max(y0)).max(0.0);
        if wy <= 0.0 {
            continue;
        }
        for sx in sx0..sx1 {
            let px0 = sx as f32;
            let px1 = px0 + 1.0;
            let wx = (px1.min(x1) - px0.max(x0)).max(0.0);
            let weight = wx * wy;
            if weight <= 0.0 {
                continue;
            }
            let pixel = premultiply_pixel(image_pixel(image, sx, sy));
            for channel in 0..4 {
                sums[channel] += pixel[channel] * weight;
            }
            total += weight;
        }
    }

    if total <= 0.0 {
        return sample_image_bilinear(image, (x0 + x1) / 2.0 - 0.5, (y0 + y1) / 2.0 - 0.5);
    }

    for channel in &mut sums {
        *channel /= total;
    }
    unpremultiply_sample(sums)
}

fn premultiply_pixel(pixel: [u8; 4]) -> [f32; 4] {
    let alpha = pixel[3] as f32;
    let alpha_scale = alpha / 255.0;
    [
        pixel[0] as f32 * alpha_scale,
        pixel[1] as f32 * alpha_scale,
        pixel[2] as f32 * alpha_scale,
        alpha,
    ]
}

fn unpremultiply_sample(sample: [f32; 4]) -> [u8; 4] {
    let alpha = sample[3].round().clamp(0.0, 255.0);
    if alpha <= 0.0 {
        return [0, 0, 0, 0];
    }

    let unpremultiply = |channel: f32| (channel * 255.0 / alpha).round().clamp(0.0, 255.0) as u8;
    [
        unpremultiply(sample[0]),
        unpremultiply(sample[1]),
        unpremultiply(sample[2]),
        alpha as u8,
    ]
}

fn image_pixel(image: &ImageData, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[index],
        image.rgba[index + 1],
        image.rgba[index + 2],
        image.rgba[index + 3],
    ]
}

fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    start + (end - start) * amount
}

fn composite_pixel(dst: &mut [u8], r: u8, g: u8, b: u8, a: u8) {
    let inv_a = 255u16.saturating_sub(a as u16);
    let src_r = premultiply(r, a);
    let src_g = premultiply(g, a);
    let src_b = premultiply(b, a);

    dst[0] = src_r.saturating_add(((dst[0] as u16 * inv_a + 127) / 255) as u8);
    dst[1] = src_g.saturating_add(((dst[1] as u16 * inv_a + 127) / 255) as u8);
    dst[2] = src_b.saturating_add(((dst[2] as u16 * inv_a + 127) / 255) as u8);
    dst[3] = a.saturating_add(((dst[3] as u16 * inv_a + 127) / 255) as u8);
}

fn premultiply(channel: u8, alpha: u8) -> u8 {
    ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8
}

fn validate_request(request: &RenderRequest) -> Result<()> {
    if request.width == 0 {
        bail!("width must be greater than zero");
    }
    if request.viewport_height == 0 {
        bail!("viewport-height must be greater than zero");
    }
    validate_scale(request.scale)?;
    let _ = scaled_dimension(request.width, request.scale, "width")?;
    let _ = scaled_dimension(request.viewport_height, request.scale, "viewport-height")?;
    Ok(())
}

fn validate_scale(scale: f32) -> Result<()> {
    if !scale.is_finite() || scale <= 0.0 {
        bail!("scale must be a finite positive number");
    }
    Ok(())
}

fn scaled_dimension(value: u32, scale: f32, label: &str) -> Result<u32> {
    if value == 0 {
        bail!("{label} must be greater than zero");
    }
    let scaled = f64::from(value) * f64::from(scale);
    if scaled > f64::from(MAX_RENDER_PIXELS_PER_AXIS) {
        bail!(
            "{label} at requested scale is too large: {scaled:.0}px > {MAX_RENDER_PIXELS_PER_AXIS}px"
        );
    }
    Ok(scaled.ceil().max(1.0) as u32)
}

fn clamp_css_height(
    measured_height: u32,
    min_height: u32,
    max_height: Option<u32>,
    scale: f32,
) -> Result<u32> {
    let requested = measured_height.max(min_height).max(1);
    if let Some(max_height) = max_height {
        if requested > max_height {
            bail!(
                "rendered content is too tall: {requested} CSS px > max-height {max_height} CSS px"
            );
        }
    }
    let max_css_height = (f64::from(MAX_RENDER_PIXELS_PER_AXIS) / f64::from(scale)).floor();
    if f64::from(requested) > max_css_height {
        bail!(
            "rendered content is too tall: {requested} CSS px at {scale}x exceeds {MAX_RENDER_PIXELS_PER_AXIS}px"
        );
    }
    Ok(requested)
}

fn ceil_to_u32(value: f32) -> Result<u32> {
    if !value.is_finite() || value < 0.0 {
        bail!("invalid layout size: {value}");
    }
    let value = value.ceil();
    if value > u32::MAX as f32 {
        bail!("layout size is too large: {value}");
    }
    Ok(value.max(1.0) as u32)
}

#[cfg(test)]
fn resource_policy_for_test() -> ResourcePolicy {
    ResourcePolicy {
        base_url: None,
        allow_remote: false,
        https_only: true,
        timeout: Duration::from_secs(30),
        max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
    }
}

#[cfg(test)]
fn layout_for_test(html: &str, width: u32) -> LayoutBox {
    let html = inline_css(&build_document(html, None, None, width), width).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new();
    let mut engine = LayoutEngine::new(
        &mut font_system,
        resource_policy_for_test(),
        Vec::new(),
        Vec::new(),
    );
    engine.layout_document(&document, width).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_layout_with_display(layout: &LayoutBox, display: Display) -> Option<&LayoutBox> {
        if layout.style.display == display {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_layout_with_display(child, display))
    }

    fn find_text_layout(layout: &LayoutBox) -> Option<&LayoutBox> {
        if matches!(layout.kind, LayoutKind::Text(_) | LayoutKind::RichText(_)) {
            return Some(layout);
        }
        layout.children.iter().find_map(find_text_layout)
    }

    fn find_layout_with_clear(layout: &LayoutBox, clear: Clear) -> Option<&LayoutBox> {
        if layout.style.clear == clear {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_layout_with_clear(child, clear))
    }

    fn find_layout_with_float(layout: &LayoutBox, float_side: FloatSide) -> Option<&LayoutBox> {
        if layout.style.float_side == float_side {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_layout_with_float(child, float_side))
    }

    #[test]
    fn wraps_html_fragments() {
        let html = build_document("<p>Hello</p>", Some("p { color: red; }"), None, 600);
        assert!(html.contains("<div id=\"email-render-root\"><p>Hello</p></div>"));
        assert!(html.contains("width: 600px"));
        assert!(html.contains("p { color: red; }"));
    }

    #[test]
    fn injects_existing_head() {
        let html = build_document(
            "<html><head><title>x</title></head><body>Hi</body></html>",
            None,
            None,
            640,
        );
        assert!(html.contains("<title>x</title>"));
        assert!(html.contains("email-render-defaults"));
        assert!(html.contains("width: 640px"));
    }

    #[test]
    fn inlines_css_before_rendering() {
        let html = build_document(
            "<p class=\"x\">Hello</p>",
            Some(".x { color: #f00; }"),
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("style=\"color: #f00;\""));
        assert!(!inlined.contains("email-render-css"));
    }

    #[test]
    fn inlines_text_shadow_for_rendering() {
        let html = build_document(
            "<a class=\"x\">Hello</a>",
            Some(".x { text-shadow: 0 1px 0 white; }"),
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("text-shadow: 0 1px 0 white"));
    }

    #[test]
    fn inliner_ignores_hidden_mso_conditional_styles() {
        let html = build_document(
            r#"<style>.x { color: red; }</style><!--[if mso]><style>.x { color: blue; }</style><![endif]--><p class="x">Hello</p>"#,
            None,
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("color: red"));
        assert!(!inlined.contains("color: blue"));
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content() {
        let html = "<!--[if !mso]><!--><style>.x { color: red; }</style><!--<![endif]-->";
        assert!(strip_hidden_conditional_comments(html).contains(".x { color: red; }"));
    }

    #[test]
    fn applies_active_max_width_media_before_inlining() {
        let html = build_document(
            r#"<div class="x" style="padding: 24px">Hello</div>"#,
            Some("@media only screen and (max-width: 640px) { .x { padding: 8px !important; } }"),
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("padding: 8px"));
    }

    #[test]
    fn ignores_inactive_max_width_media_rules() {
        let html = build_document(
            r#"<div class="x" style="padding: 24px">Hello</div>"#,
            Some("@media only screen and (max-width: 480px) { .x { padding: 8px !important; } }"),
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("padding: 24px"));
        assert!(!inlined.contains("padding: 8px"));
    }

    #[test]
    fn parses_unitless_line_height_as_font_multiplier() {
        let unitless = parse_line_height_declaration("1.625", 16.0).unwrap();
        assert!((unitless.height - 26.0).abs() < 0.1);
        assert_eq!(unitless.factor, Some(1.625));
        assert!(!unitless.normal);

        let percent = parse_line_height_declaration("150%", 16.0).unwrap();
        assert!((percent.height - 24.0).abs() < 0.1);
        assert_eq!(percent.factor, None);
        assert!(!percent.normal);
    }

    #[test]
    fn line_height_normal_keeps_normal_state() {
        let mut style = Style::initial();
        assert!(style.line_height_normal);

        style.apply_declaration("line-height", "normal");
        assert!(style.line_height_normal);
        assert_eq!(style.line_height_factor, None);

        style.apply_declaration("font-size", "20px");
        assert!((style.line_height - normal_line_height_fallback(20.0)).abs() < 0.1);

        style.apply_declaration("line-height", "24px");
        assert!(!style.line_height_normal);
        assert_eq!(style.line_height_factor, None);
    }

    #[test]
    fn blink_mac_ascent_hack_matches_web_standard_families() {
        assert!(blink_mac_ascent_hack_applies(Some("Helvetica")));
        assert!(blink_mac_ascent_hack_applies(Some("serif")));
        assert!(blink_mac_ascent_hack_applies(None));
        assert!(!blink_mac_ascent_hack_applies(Some("Helvetica Neue")));
        assert_eq!(blink_web_standard_family_ascent_adjustment(12.0, 4.0), 2.0);
    }

    #[test]
    fn text_mask_alpha_is_multiplied_by_css_color_alpha() {
        let color = apply_text_base_alpha(
            TextColor::rgba(10, 20, 30, 200),
            TextColor::rgba(1, 2, 3, 128),
        );
        assert_eq!(color.as_rgba_tuple(), (10, 20, 30, 100));
    }

    #[test]
    fn text_opacity_multiplies_mask_alpha() {
        let color = apply_text_opacity(TextColor::rgba(10, 20, 30, 200), 0.5);
        assert_eq!(color.as_rgba_tuple(), (10, 20, 30, 100));
    }

    #[test]
    fn rich_text_smaller_inline_uses_parent_leading_for_baseline() {
        let mut parent = Style::initial();
        parent.set_font_size(30.0);
        parent.apply_declaration("line-height", "1.8");

        let mut child = parent.clone();
        child.set_font_size(20.0);
        let spans = vec![TextSpan::from_style("RestoBar".to_string(), &child)];

        assert!((rich_text_baseline_leading_offset(&spans, &parent) - 12.0).abs() < 0.01);

        let same_size = vec![TextSpan::from_style("RestoBar".to_string(), &parent)];
        assert_eq!(rich_text_baseline_leading_offset(&same_size, &parent), 0.0);
    }

    #[test]
    fn unitless_line_height_scales_when_font_size_changes() {
        let mut style = Style::initial();
        style.apply_declaration("line-height", "1.5");
        style.apply_declaration("font-size", "24px");
        assert!((style.line_height - 36.0).abs() < 0.1);

        style.apply_declaration("line-height", "20px");
        style.apply_declaration("font-size", "10px");
        assert!((style.line_height - 20.0).abs() < 0.1);
    }

    #[test]
    fn parses_letter_spacing_against_current_font_size() {
        let mut style = Style::initial();
        style.apply_declaration("letter-spacing", "0.00938em");
        assert!((style.letter_spacing - 0.15008).abs() < 0.001);
        style.apply_declaration("letter-spacing", "normal");
        assert_eq!(style.letter_spacing, 0.0);
    }

    #[test]
    fn font_smoothing_antialiased_disables_hinting() {
        let mut style = Style::initial();
        style.apply_declaration("-webkit-font-smoothing", "antialiased");

        assert!(style.font_hinting_disabled);
        assert!(Style::from_parent_for_tag(&style, "p").font_hinting_disabled);

        style.apply_declaration("-webkit-font-smoothing", "subpixel-antialiased");
        assert!(!style.font_hinting_disabled);

        style.apply_declaration("text-rendering", "geometricPrecision");
        assert!(style.font_hinting_disabled);
    }

    #[test]
    fn parses_em_spacing_against_current_font_size() {
        let edges = parse_edges_with_font(".4em 0 1.1875em", 16.0).unwrap();
        assert!((edges.top - 6.4).abs() < 0.1);
        assert!((edges.bottom - 19.0).abs() < 0.1);
    }

    #[test]
    fn headings_keep_browser_like_default_font_defaults() {
        let parent = Style::initial();
        let h1 = Style::from_parent_for_tag(&parent, "h1");
        assert_eq!(h1.font_weight, FontWeight::BOLD);
        assert!((h1.font_size - 32.0).abs() < 0.1);
        assert!((h1.margin.bottom - 21.44).abs() < 0.1);

        let h3 = Style::from_parent_for_tag(&parent, "h3");
        assert_eq!(h3.font_weight, FontWeight::BOLD);
        assert!((h3.font_size - 18.72).abs() < 0.1);
        assert!((h3.margin.top - 18.72).abs() < 0.1);

        let mut h2 = Style::from_parent_for_tag(&parent, "h2");
        h2.apply_declaration("font-size", "28px");
        assert!((h2.margin.bottom - 23.24).abs() < 0.1);
        h2.apply_declaration("margin-bottom", "0");
        h2.apply_declaration("font-size", "32px");
        assert!((h2.margin.bottom - 0.0).abs() < 0.1);
    }

    #[test]
    fn inherited_font_weight_keeps_parent_weight() {
        let mut parent = Style::initial();
        parent.font_weight = FontWeight::BOLD;
        let mut child = Style::from_parent_for_tag(&parent, "h2");
        child.apply_declaration("font-weight", "inherit");
        assert_eq!(child.font_weight, FontWeight::BOLD);

        let layout = layout_for_test(
            r#"<div style="font-weight: normal"><h1 style="font-weight: inherit; margin: 0">Title</h1></div>"#,
            300,
        );
        let title = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "Title"),
        )
        .expect("title text");
        assert_eq!(title.style.font_weight, FontWeight::NORMAL);
    }

    #[test]
    fn selects_safe_fallback_font_from_web_font_stack() {
        let family = parse_font_family(r#""Nunito Sans", Helvetica, Arial, sans-serif"#).unwrap();
        assert_eq!(family, "Helvetica");
        let family = parse_font_family("ui-serif, Georgia, serif").unwrap();
        assert_eq!(family, "serif");
        let family = parse_font_family("Avenir, Montserrat, Corbel, sans-serif").unwrap();
        assert_eq!(family, "Avenir");
    }

    #[test]
    fn selects_loaded_web_font_before_safe_fallback() {
        let available = vec!["Nunito Sans".to_string()];
        let family = parse_font_family_with_available(
            r#""Nunito Sans", Helvetica, Arial, sans-serif"#,
            &available,
        )
        .unwrap();
        assert_eq!(family, "Nunito Sans");
    }

    #[test]
    fn invalid_font_family_declaration_is_ignored() {
        assert!(
            parse_font_family(r#"" undefined: IowanOldStyle" undefined: , P052, serif"#).is_none()
        );
        assert_eq!(
            parse_font_family(r#""Iowan Old Style", "Times New Roman", serif"#).as_deref(),
            Some("Times New Roman")
        );
    }

    #[test]
    fn web_font_alias_preserves_actual_face_weight() {
        let faces = vec![WebFontFace {
            css_family: "Merriweather".to_string(),
            actual_family: "Merriweather".to_string(),
            weight: FontWeight(250),
        }];
        let selection =
            parse_font_family_selection(r#""Merriweather", Georgia, serif"#, &[], &faces)
                .expect("font family");
        assert_eq!(selection.family, "Merriweather");
        assert_eq!(selection.forced_weight, Some(FontWeight(250)));
    }

    #[test]
    fn repeated_web_font_descriptors_keep_family_weight_matching_open() {
        let faces = vec![
            WebFontFace {
                css_family: "Work Sans".to_string(),
                actual_family: "Work Sans".to_string(),
                weight: FontWeight(200),
            },
            WebFontFace {
                css_family: "Work Sans".to_string(),
                actual_family: "Work Sans".to_string(),
                weight: FontWeight(700),
            },
        ];
        let selection =
            parse_font_family_selection(r#""Work Sans", Arial, sans-serif"#, &[], &faces)
                .expect("font family");
        assert_eq!(selection.family, "Work Sans");
        assert_eq!(selection.forced_weight, None);
    }

    #[test]
    fn parses_stylesheet_link_urls() {
        let urls = stylesheet_link_urls(
            r#"<html><head>
                <link rel="preload" href="ignore.css">
                <link rel="stylesheet" href="fonts.css">
                <link rel="alternate stylesheet" href="theme.css">
              </head></html>"#,
        );

        assert_eq!(urls, vec!["fonts.css"]);
    }

    #[test]
    fn linked_stylesheet_font_filter_requires_normal_style_faces() {
        assert!(linked_stylesheet_fonts_are_supported(
            r#"@font-face { font-family: Work; font-style: normal; font-weight: 400; src: url(work.woff2); }"#
        ));
        assert!(!linked_stylesheet_fonts_are_supported(
            r#"@font-face { font-family: Playfair; font-style: italic; font-weight: 400; src: url(playfair.woff2); }"#
        ));
    }

    #[test]
    fn skips_non_latin_web_font_unicode_ranges() {
        let cyrillic = vec![(
            "unicode-range".to_string(),
            "U+0460-052F, U+1C80-1C8A".to_string(),
        )];
        let latin = vec![("unicode-range".to_string(), "U+0000-00FF".to_string())];
        assert!(!font_face_covers_basic_latin(&cyrillic));
        assert!(font_face_covers_basic_latin(&latin));
    }

    #[test]
    fn generic_first_font_family_stays_generic_with_available_fallbacks() {
        let available = vec!["Georgia".to_string()];
        let family = parse_font_family_with_available("ui-serif, Georgia, serif", &available)
            .expect("font family");
        assert_eq!(family, "serif");
    }

    #[test]
    fn generic_font_families_follow_blink_mac_defaults() {
        assert_eq!(fontdb_family(None), fontdb::Family::Name("Times"));
        assert_eq!(
            fontdb_family(Some("sans-serif")),
            fontdb::Family::Name("Helvetica")
        );
        assert_eq!(fontdb_family(Some("serif")), fontdb::Family::Name("Times"));
    }

    #[test]
    fn important_longhand_declarations_override_later_shorthand() {
        let layout = layout_for_test(
            r#"<div style="padding-left: 24px !important; padding: 48px; background: #000">Hello</div>"#,
            200,
        );
        let block = find_layout(&layout, |child| child.style.background == Some(Rgba::BLACK))
            .expect("block");
        assert!((block.style.padding.left - 24.0).abs() < 0.1);
        assert!((block.style.padding.top - 48.0).abs() < 0.1);
    }

    #[test]
    fn zero_border_shorthand_does_not_create_default_border() {
        let mut style = Style::initial();
        style.apply_declaration("border", "0");
        assert_eq!(style.border, Edges::ZERO);
    }

    #[test]
    fn parses_asymmetric_border_widths() {
        let mut style = Style::initial();
        style.apply_declaration("border-width", "10px 20px");
        assert_eq!(
            style.border,
            Edges {
                top: 10.0,
                right: 20.0,
                bottom: 10.0,
                left: 20.0,
            }
        );
        style.apply_declaration("border-left-width", "0");
        assert_eq!(style.border.left, 0.0);
    }

    #[test]
    fn parses_border_side_shorthand() {
        let mut style = Style::initial();
        style.apply_declaration("border-top", "10px dashed #22BC66");
        style.apply_declaration("border-right", "18px solid #22BC66");
        assert_eq!(style.border.top, 10.0);
        assert_eq!(style.border.right, 18.0);
        assert_eq!(style.border_color, Rgba::rgb(0x22, 0xbc, 0x66));
        assert_eq!(style.border_style, BorderLineStyle::Dashed);

        style.apply_declaration("border-style", "inset");
        assert_eq!(style.border_style, BorderLineStyle::Inset);
    }

    #[test]
    fn parses_border_radius() {
        let mut style = Style::initial();
        style.apply_declaration("border-radius", "12px");
        assert_eq!(style.border_radius, 12.0);
        style.apply_declaration("border-radius", "50%");
        assert!(style.border_radius > 10_000.0);
        assert!(point_in_rounded_rect(
            50.0,
            1.0,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            style.border_radius
        ));
        assert!(!point_in_rounded_rect(
            1.0,
            1.0,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            style.border_radius
        ));
    }

    #[test]
    fn parses_outer_box_shadow() {
        let mut style = Style::initial();
        style.apply_declaration("box-shadow", "0 2px 3px rgba(0, 0, 0, 0.16)");
        assert_eq!(style.box_shadows.len(), 1);
        let shadow = style.box_shadows[0];
        assert_eq!(shadow.offset_x, 0.0);
        assert_eq!(shadow.offset_y, 2.0);
        assert_eq!(shadow.blur_radius, 3.0);
        assert_eq!(shadow.spread, 0.0);
        assert_eq!(shadow.color, Rgba::with_alpha(0, 0, 0, 41));
        assert!(!shadow.inset);
    }

    #[test]
    fn parses_inherited_text_shadow() {
        let mut style = Style::initial();
        style.color = Rgba::rgb(0x11, 0x22, 0x33);
        style.apply_declaration("text-shadow", "0 1px 0 white, 2px 3px #000");
        assert_eq!(style.text_shadows.len(), 2);
        assert_eq!(style.text_shadows[0].offset_y, 1.0);
        assert_eq!(style.text_shadows[0].color, Rgba::WHITE);
        assert_eq!(style.text_shadows[1].offset_x, 2.0);
        assert_eq!(style.text_shadows[1].color, Rgba::BLACK);

        let inherited = Style::from_parent_for_tag(&style, "span");
        assert_eq!(inherited.text_shadows, style.text_shadows);

        style.apply_declaration("text-shadow", "none");
        assert!(style.text_shadows.is_empty());
    }

    #[test]
    fn parses_background_images_from_css_and_html_attributes() {
        let mut style = Style::initial();
        style.apply_declaration("background-image", "url('hero.jpg')");
        assert_eq!(style.background_image_src.as_deref(), Some("hero.jpg"));

        let document = kuchiki::parse_html()
            .one(r#"<table><tr><td background="assets/top.jpg">A</td></tr></table>"#);
        let cell = find_first_tag(&document, "td").expect("td");
        let style = style_for_node(&cell, &Style::initial());
        assert_eq!(
            style.background_image_src.as_deref(),
            Some("assets/top.jpg")
        );
    }

    #[test]
    fn inline_style_uses_css_parser_for_function_values() {
        let document = kuchiki::parse_html()
            .one(r##"<div style='background-image: url("hero;v=1.jpg"); color: #ff0000'>A</div>"##);
        let div = find_first_tag(&document, "div").expect("div");
        let style = style_for_node(&div, &Style::initial());

        assert_eq!(style.background_image_src.as_deref(), Some("hero;v=1.jpg"));
        assert_eq!(style.color, Rgba::rgb(0xff, 0x00, 0x00));
    }

    #[test]
    fn inline_style_important_declarations_win_after_parsing() {
        let document = kuchiki::parse_html()
            .one(r##"<div style="color: #111111 !important; color: #222222">A</div>"##);
        let div = find_first_tag(&document, "div").expect("div");
        let style = style_for_node(&div, &Style::initial());

        assert_eq!(style.color, Rgba::rgb(0x11, 0x11, 0x11));
    }

    #[test]
    fn parses_flex_container_style_model() {
        let mut style = Style::initial();
        style.apply_declaration("display", "flex");
        style.apply_declaration("flex-flow", "column wrap");
        style.apply_declaration("justify-content", "space-between");
        style.apply_declaration("align-items", "center");
        style.apply_declaration("gap", "12px 24px");

        assert_eq!(style.display, Display::Flex);
        assert_eq!(style.flex_direction, FlexDirection::Column);
        assert_eq!(style.flex_wrap, FlexWrap::Wrap);
        assert_eq!(style.justify_content, JustifyContent::SpaceBetween);
        assert_eq!(style.align_items, AlignItems::Center);
        assert_eq!(style.row_gap, 12.0);
        assert_eq!(style.column_gap, 24.0);
    }

    #[test]
    fn parses_flex_item_style_model() {
        let mut style = Style::initial();
        style.apply_declaration("flex", "2 0 40%");
        style.apply_declaration("align-self", "flex-end");

        assert_eq!(style.flex_grow, 2.0);
        assert_eq!(style.flex_shrink, 0.0);
        assert_eq!(style.flex_basis, Some(Length::Percent(0.4)));
        assert_eq!(style.align_self, Some(AlignItems::FlexEnd));
    }

    #[test]
    fn lays_out_flex_row_with_taffy_gap_and_alignment() {
        let layout = layout_for_test(
            r#"<div style="display:flex;width:120px;height:40px;gap:10px;align-items:center">
                <div style="width:20px;height:10px;background:#111"></div>
                <div style="width:30px;height:20px;background:#222"></div>
            </div>"#,
            200,
        );

        let flex = find_layout_with_display(&layout, Display::Flex).expect("flex layout");
        assert_eq!(flex.style.display, Display::Flex);
        assert!((flex.rect.width - 120.0).abs() < 0.1);
        assert!((flex.rect.height - 40.0).abs() < 0.1);
        assert!((flex.children[0].rect.x - flex.rect.x).abs() < 0.1);
        assert!((flex.children[1].rect.x - (flex.rect.x + 30.0)).abs() < 0.1);
        assert!((flex.children[0].rect.y - (flex.rect.y + 15.0)).abs() < 0.1);
        assert!((flex.children[1].rect.y - (flex.rect.y + 10.0)).abs() < 0.1);
    }

    #[test]
    fn lays_out_flex_column_with_taffy_direction() {
        let layout = layout_for_test(
            r#"<div style="display:flex;flex-direction:column;width:80px;gap:5px">
                <div style="width:20px;height:10px"></div>
                <div style="width:30px;height:15px"></div>
            </div>"#,
            200,
        );

        let flex = find_layout_with_display(&layout, Display::Flex).expect("flex layout");
        assert!((flex.rect.height - 30.0).abs() < 0.1);
        assert!((flex.children[0].rect.y - flex.rect.y).abs() < 0.1);
        assert!((flex.children[1].rect.y - (flex.rect.y + 15.0)).abs() < 0.1);
    }

    #[test]
    fn parses_float_and_clear_style_model() {
        let mut style = Style::initial();
        style.apply_declaration("position", "absolute");
        style.apply_declaration("opacity", ".3");
        style.apply_declaration("top", "10px");
        style.apply_declaration("float", "right");
        style.apply_declaration("clear", "both");

        assert_eq!(style.position, Position::Absolute);
        assert!((style.opacity - 0.3).abs() < 0.01);
        assert_eq!(style.inset_top, Some(Length::Px(10.0)));
        assert_eq!(style.float_side, FloatSide::Right);
        assert_eq!(style.clear, Clear::Both);
    }

    #[test]
    fn parses_fixed_table_layout_style_model() {
        let mut style = Style::initial();
        style.apply_declaration("table-layout", "fixed");
        assert!(style.table_layout_fixed);
    }

    #[test]
    fn absolute_positioned_children_do_not_advance_block_flow() {
        let layout = layout_for_test(
            r#"<div style="width:100px">
                <div style="position:absolute;width:100px;height:80px;background:#111"></div>
                <p style="margin:0;height:10px;background:#222"></p>
            </div>"#,
            100,
        );
        let paragraph = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x22, 0x22, 0x22))
        })
        .expect("paragraph");
        let absolute = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("absolute");
        assert!((paragraph.rect.y - 0.0).abs() < 0.1);
        assert!((absolute.rect.y - 0.0).abs() < 0.1);
        assert!((absolute.rect.height - 80.0).abs() < 0.1);
    }

    #[test]
    fn paints_absolute_wrapper_children_without_own_background() {
        let layout = layout_for_test(
            r#"<div style="position:relative;height:80px">
                <div style="position:absolute;left:20px;top:10px">
                    <span style="padding:4px;background:#111;color:#fff">Play</span>
                </div>
            </div>"#,
            200,
        );
        let badge = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("absolute child background");

        assert!((badge.rect.x - 20.0).abs() < 0.1);
        assert!((badge.rect.y - 10.0).abs() < 0.1);
    }

    #[test]
    fn inline_block_absolute_children_are_positioned_against_parent() {
        let layout = layout_for_test(
            r#"<span style="display:inline-block;position:relative;width:80px;height:20px">
                Label<span style="position:absolute;left:0;bottom:-10px;width:80px;height:2px;background:#111"></span>
            </span>"#,
            200,
        );
        let underline = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("absolute underline");
        assert!((underline.rect.x - 0.0).abs() < 0.1);
        assert!(underline.rect.y > 20.0);
    }

    #[test]
    fn float_left_reduces_following_text_line_width() {
        let layout = layout_for_test(
            r#"<div style="width:100px">
                <div style="float:left;width:40px;height:20px;background:#111"></div>
                text next to float
            </div>"#,
            200,
        );

        let float = find_layout_with_float(&layout, FloatSide::Left).expect("float");
        let text = find_text_layout(&layout).expect("text");
        assert!((float.rect.x - text.rect.x + 40.0).abs() < 0.1);
        assert!((text.rect.width - 60.0).abs() < 0.1);
    }

    #[test]
    fn clear_left_moves_block_below_float() {
        let layout = layout_for_test(
            r#"<div style="width:100px">
                <div style="float:left;width:40px;height:20px;background:#111"></div>
                <p style="clear:left;margin:0;height:10px"></p>
            </div>"#,
            200,
        );

        let float = find_layout_with_float(&layout, FloatSide::Left).expect("float");
        let cleared = find_layout_with_clear(&layout, Clear::Left).expect("clear");
        assert!(cleared.rect.y >= float.rect.y + float.rect.height - 0.1);
    }

    #[test]
    fn float_right_reduces_following_text_line_width_and_clear_both_moves_below() {
        let layout = layout_for_test(
            r#"<div style="width:100px">
                <div style="float:right;width:40px;height:20px;background:#111"></div>
                text next to float
                <p style="clear:both;margin:0;height:10px"></p>
            </div>"#,
            200,
        );

        let float = find_layout_with_float(&layout, FloatSide::Right).expect("float");
        let text = find_text_layout(&layout).expect("text");
        let cleared = find_layout_with_clear(&layout, Clear::Both).expect("clear");
        assert!((float.rect.x - (text.rect.x + 60.0)).abs() < 0.1);
        assert!((text.rect.width - 60.0).abs() < 0.1);
        assert!(cleared.rect.y >= float.rect.y + float.rect.height - 0.1);
    }

    #[test]
    fn parses_background_cover_position_and_repeat() {
        let mut style = Style::initial();
        style.apply_declaration(
            "background",
            "#2a3448 url(hero.jpg) no-repeat center top / cover",
        );

        assert_eq!(style.background, Some(Rgba::rgb(0x2a, 0x34, 0x48)));
        assert_eq!(style.background_image_src.as_deref(), Some("hero.jpg"));
        assert_eq!(style.background_repeat, BackgroundRepeat::NoRepeat);
        assert_eq!(style.background_size, BackgroundSize::Cover);
        assert_eq!(
            style.background_position,
            BackgroundPosition {
                x: PositionAxis::Center,
                y: PositionAxis::Start,
            }
        );

        style.apply_declaration("background-size", "contain");
        style.apply_declaration("background-position", "right bottom");
        assert_eq!(style.background_size, BackgroundSize::Contain);
        assert_eq!(
            style.background_position,
            BackgroundPosition {
                x: PositionAxis::End,
                y: PositionAxis::End,
            }
        );
    }

    #[test]
    fn parses_object_fit_cover() {
        let mut style = Style::initial();
        style.apply_declaration("object-fit", "cover");
        style.apply_declaration("object-position", "left top");

        assert_eq!(style.object_fit, ObjectFit::Cover);
        assert_eq!(
            style.object_position,
            ObjectPosition {
                x: PositionAxis::Start,
                y: PositionAxis::Start,
            }
        );
        style.apply_declaration("object-fit", "scale-down");
        assert_eq!(style.object_fit, ObjectFit::ScaleDown);
    }

    #[test]
    fn parses_alpha_color_serializations() {
        assert_eq!(parse_color("#000c"), Some(Rgba::with_alpha(0, 0, 0, 0xcc)));
        assert_eq!(
            parse_color("#11223380"),
            Some(Rgba::with_alpha(0x11, 0x22, 0x33, 0x80))
        );
        assert_eq!(
            parse_color("rgb(0 0 0 / 80%)"),
            Some(Rgba::with_alpha(0, 0, 0, 204))
        );

        let mut style = Style::initial();
        for (name, value) in css_declarations("background: rgba(0,0,0,.8)") {
            style.apply_declaration(&name, &value);
        }
        assert_eq!(style.background, Some(Rgba::with_alpha(0, 0, 0, 204)));
    }

    #[test]
    fn body_color_inherits_to_paragraph_text() {
        let html = build_document(
            "<p>Hello</p>",
            Some("body { color: rgba(0,0,0,.4); }"),
            None,
            200,
        );
        let html = inline_css(&html, 200).unwrap();
        let document = kuchiki::parse_html().one(html);
        let mut font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            system_font_database(),
            cosmic_text::PlatformFallback,
        );
        let mut engine = LayoutEngine::new(
            &mut font_system,
            resource_policy_for_test(),
            Vec::new(),
            Vec::new(),
        );
        let layout = engine.layout_document(&document, 200).unwrap();
        let text = find_text_layout(&layout).expect("text");
        assert_eq!(text.style.color, Rgba::with_alpha(0, 0, 0, 102));
    }

    #[test]
    fn body_inherits_from_html_style() {
        let layout = layout_for_test(
            r#"<html style="-webkit-font-smoothing:antialiased;color:#123456"><body><p>Hello</p></body></html>"#,
            200,
        );
        let text = find_text_layout(&layout).expect("text");

        assert!(text.style.font_hinting_disabled);
        assert_eq!(text.style.color, Rgba::rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn applies_text_transform_to_text_nodes() {
        let layout = layout_for_test(r#"<p style="text-transform: uppercase">Confirm</p>"#, 200);
        let text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "CONFIRM"),
        );
        assert!(text.is_some());
    }

    #[test]
    fn collapses_source_newlines_but_preserves_br_breaks() {
        assert_eq!(normalize_text("Viewed by\n  Someone"), "Viewed by Someone");
        let with_break = format!("Viewed by{HARD_BREAK}Someone");
        assert_eq!(normalize_text(&with_break), "Viewed by\nSomeone");
        assert_eq!(normalize_text(&HARD_BREAK.to_string()), "\n");
    }

    #[test]
    fn preserves_spaces_after_br_with_leading_source_space() {
        let text = format!("Thanks,{HARD_BREAK} [Sender Name] and the [Product Name] team");
        assert_eq!(
            normalize_text(&text),
            "Thanks,\n[Sender Name] and the [Product Name] team"
        );
    }

    #[test]
    fn preserves_non_breaking_space_for_table_spacers() {
        assert_eq!(normalize_text("\u{00a0}"), "\u{00a0}");
        assert_eq!(normalize_text("A\u{00a0} B"), "A\u{00a0} B");
    }

    #[test]
    fn lays_out_table_cells() {
        let layout = layout_for_test(
            r#"<table width="600" cellpadding="10"><tr><td width="200">A</td><td>B</td></tr></table>"#,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert_eq!(table.children.len(), 1);
        assert_eq!(table.children[0].children.len(), 2);
        assert!((table.children[0].children[0].rect.width - 220.0).abs() < 0.1);
        assert!((table.children[0].children[1].rect.width - 380.0).abs() < 0.1);
    }

    #[test]
    fn table_cells_use_cellpadding_attribute() {
        let layout = layout_for_test(
            r#"<table width="100" cellpadding="1"><tr><td><img width="20" height="10" alt=""></td></tr></table>"#,
            100,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");

        assert!((image.rect.x - (cell.rect.x + 1.0)).abs() < 0.1);
        assert!((image.rect.y - (cell.rect.y + 1.0)).abs() < 0.1);
    }

    #[test]
    fn table_cells_use_browser_default_cellpadding() {
        let layout = layout_for_test(
            r#"<table width="100"><tr><td><img width="20" height="10" alt=""></td></tr></table>"#,
            100,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");

        assert!((image.rect.x - (cell.rect.x + 1.0)).abs() < 0.1);
        assert!((image.rect.y - (cell.rect.y + 1.0)).abs() < 0.1);
    }

    #[test]
    fn table_cell_css_padding_overrides_cellpadding_attribute() {
        let layout = layout_for_test(
            r#"<table width="100" cellpadding="1"><tr><td style="padding:0"><img width="20" height="10" alt=""></td></tr></table>"#,
            100,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");

        assert!((image.rect.x - cell.rect.x).abs() < 0.1);
        assert!((image.rect.y - cell.rect.y).abs() < 0.1);
    }

    #[test]
    fn table_cells_inherit_browser_middle_valign_from_rows() {
        let layout = layout_for_test(
            r#"<table width="100" cellpadding="0"><tr><td height="40"><img width="20" height="10" alt=""></td></tr></table>"#,
            100,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");

        assert!((image.rect.y - (cell.rect.y + 15.0)).abs() < 0.1);
    }

    #[test]
    fn fixed_table_layout_uses_first_row_widths() {
        let layout = layout_for_test(
            r#"<table width="300" style="table-layout:fixed"><tr><td>A</td><td>B</td></tr><tr><td width="250">C</td><td>D</td></tr></table>"#,
            300,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        let first_row = &table.children[0];

        assert!((first_row.children[0].rect.width - 150.0).abs() < 0.1);
        assert!((first_row.children[1].rect.width - 150.0).abs() < 0.1);
    }

    #[test]
    fn table_cell_valign_middle_centers_content_in_explicit_height() {
        let layout = layout_for_test(
            r##"<table width="200"><tr><td height="100" valign="middle"><div style="height:20px;background:#111"></div></td></tr></table>"##,
            200,
        );
        let child = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("cell child");
        assert!((child.rect.y - 40.0).abs() < 0.1);
    }

    #[test]
    fn table_cell_vertical_align_attribute_alias_centers_content() {
        let layout = layout_for_test(
            r##"<table width="200"><tr><td height="100" vertical-align="middle"><div style="height:20px;background:#111"></div></td></tr></table>"##,
            200,
        );
        let child = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("cell child");
        assert!((child.rect.y - 40.0).abs() < 0.1);
    }

    #[test]
    fn display_none_table_rows_do_not_occupy_height() {
        let layout = layout_for_test(
            r#"<table><tr style="display:none"><td height="35">&nbsp;</td></tr><tr><td height="20">&nbsp;</td></tr></table>"#,
            200,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");

        assert!(table.rect.height < 30.0);
        assert_eq!(table.children.len(), 1);
    }

    #[test]
    fn table_spacer_cells_keep_non_breaking_space_width() {
        let layout = layout_for_test(
            r#"<table width="600"><tr><td>&nbsp;</td><td width="600">Center</td><td>&nbsp;</td></tr></table>"#,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        let cells = &table.children[0].children;
        assert!(cells[0].rect.width > 1.0);
        assert!(cells[1].rect.width < 600.0);
        assert!(cells[2].rect.width > 1.0);
    }

    #[test]
    fn auto_width_tables_shrink_to_contents() {
        let layout = layout_for_test(
            r##"<table><tr><td bgcolor="#cc7953"><a style="display:inline-block;padding:16px 36px;font-size:16px">Do Something</a></td></tr></table>"##,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!(table.rect.width > 120.0);
        assert!(table.rect.width < 240.0);
    }

    #[test]
    fn auto_width_tables_shrink_block_cell_contents() {
        let layout = layout_for_test(
            r##"<table><tr><td style="padding:15px 25px"><p style="margin:0;font-size:15px;line-height:18px">Update Your Billing Info</p></td></tr></table>"##,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!(table.rect.width > 170.0);
        assert!(table.rect.width < 260.0);
    }

    #[test]
    fn percentage_width_tables_still_fill_parent() {
        let layout = layout_for_test(
            r#"<table width="100%"><tr><td>Do Something</td></tr></table>"#,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!((table.rect.width - 600.0).abs() < 0.1);
    }

    #[test]
    fn adjacent_block_vertical_margins_collapse() {
        let layout = layout_for_test(
            r#"<p style="margin:0 0 20px">A</p><table style="margin:30px 0" width="100"><tr><td>B</td></tr></table>"#,
            300,
        );
        let paragraph = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "A"),
        )
        .expect("paragraph text");
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        let gap = table.rect.y - (paragraph.rect.y + paragraph.rect.height);
        assert!((gap - 30.0).abs() < 0.1);
    }

    #[test]
    fn lays_out_colspan_cells() {
        let layout = layout_for_test(
            r#"<table width="300"><tr><td colspan="2">A</td><td>B</td></tr><tr><td width="100">C</td><td width="50">D</td><td>E</td></tr></table>"#,
            300,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert_eq!(table.children.len(), 2);
        assert_eq!(table.children[0].children.len(), 2);
        assert!((table.children[0].children[0].rect.width - 154.0).abs() < 0.1);
        assert!((table.children[0].children[1].rect.width - 146.0).abs() < 0.1);
    }

    #[test]
    fn list_items_render_markers() {
        let layout = layout_for_test("<ul><li>First</li><li>Second</li></ul>", 200);
        let marker = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "\u{2022}"),
        );
        let text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "First"),
        );
        assert!(marker.is_some());
        assert!(text.is_some());
    }

    #[test]
    fn list_style_none_suppresses_markers() {
        let layout = layout_for_test(
            r#"<ul style="list-style:none"><li>First</li><li style="list-style-type:none">Second</li></ul>"#,
            200,
        );
        let marker = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "\u{2022}" || text == "1."),
        );
        assert!(marker.is_none());
    }

    #[test]
    fn flattened_inline_text_preserves_color_spans() {
        let layout = layout_for_test(
            r##"<p>Open <a href="#" style="color:#2563eb">link</a></p>"##,
            200,
        );
        let rich = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::RichText(_))
        })
        .expect("rich text");
        let LayoutKind::RichText(spans) = &rich.kind else {
            unreachable!();
        };
        assert_eq!(spans_text(spans), "Open link");
        assert_eq!(spans[0].text, "Open ");
        assert_eq!(spans[0].style.color, Rgba::BLACK);
        assert_eq!(spans[1].text, "link");
        assert_eq!(spans[1].style.color, Rgba::rgb(0x25, 0x63, 0xeb));
    }

    #[test]
    fn flattened_inline_text_preserves_font_runs() {
        let layout = layout_for_test(
            r#"<h1 style="font-size:30px;font-family:serif">Open <a style="font-size:20px;font-family:sans-serif;font-weight:400;text-transform:uppercase">link</a></h1>"#,
            300,
        );
        let rich = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::RichText(_))
        })
        .expect("rich text");
        let LayoutKind::RichText(spans) = &rich.kind else {
            unreachable!();
        };
        assert_eq!(spans_text(spans), "Open LINK");
        assert_eq!(spans[0].style.font_size, 30.0);
        assert_eq!(spans[1].style.font_size, 20.0);
        assert_eq!(spans[1].style.font_family.as_deref(), Some("sans-serif"));
        assert_eq!(spans[1].style.font_weight, FontWeight::NORMAL);
    }

    #[test]
    fn nested_table_content_does_not_inherit_outer_align_attribute() {
        let layout = layout_for_test(
            r#"<table width="200"><tr><td align="center"><table width="100"><tr><td>Inner</td></tr></table></td></tr></table>"#,
            200,
        );
        let text =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_))).expect("text");
        assert_eq!(text.style.text_align, TextAlign::Left);
    }

    #[test]
    fn parent_align_centers_nested_table_without_centering_cell_text() {
        let layout = layout_for_test(
            r#"<table width="200"><tr><td align="center"><table width="100"><tr><td>Inner</td></tr></table></td></tr></table>"#,
            200,
        );
        let inner_table = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Table) && (child.rect.width - 100.0).abs() < 0.1
        })
        .expect("inner table");
        assert!((inner_table.rect.x - 50.0).abs() < 0.1);
        let text =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_))).expect("text");
        assert_eq!(text.style.text_align, TextAlign::Left);
    }

    #[test]
    fn table_align_centers_auto_width_image_table() {
        let layout = layout_for_test(
            r#"<table width="640"><tr><td align="left"><table align="center"><tr><td><img width="220" height="35" alt=""></td></tr></table></td></tr></table>"#,
            640,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");
        assert!((image.rect.x - 210.0).abs() < 0.1);
    }

    #[test]
    fn table_align_attribute_does_not_align_cell_text() {
        let layout = layout_for_test(
            r#"<table align="center" width="100"><tr><td>Inner</td></tr></table>"#,
            200,
        );
        let text =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_))).expect("text");
        assert_eq!(text.style.text_align, TextAlign::Left);
    }

    #[test]
    fn table_align_center_offsets_table_horizontally() {
        let layout = layout_for_test(
            r#"<table align="center" width="100"><tr><td>Inner</td></tr></table>"#,
            200,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!((table.rect.x - 50.0).abs() < 0.1);
    }

    #[test]
    fn block_images_do_not_follow_parent_text_align() {
        let layout = layout_for_test(
            r#"<div style="text-align:center"><img width="50" height="20" alt=""></div>"#,
            200,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");
        assert!(image.rect.x < 1.0);
    }

    #[test]
    fn hr_is_laid_out_as_block_separator() {
        let layout = layout_for_test(r#"<div><hr><p style="margin:0">After</p></div>"#, 200);
        let rule = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Block) && child.style.border.top > 0.0
        })
        .expect("hr");
        let text =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_))).expect("text");

        assert!(rule.rect.height >= 1.0);
        assert_eq!(rule.style.border_color, Rgba::rgb(0x80, 0x80, 0x80));
        assert_eq!(rule.style.border_style, BorderLineStyle::Inset);
        assert!(text.rect.y > rule.rect.y + rule.rect.height);
    }

    #[test]
    fn legacy_align_attribute_centers_block_images() {
        let layout = layout_for_test(
            r#"<div align="center"><img width="50" height="20" alt=""></div>"#,
            200,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");
        assert!((image.rect.x - 75.0).abs() < 0.1);
    }

    #[test]
    fn inline_anchor_width_does_not_constrain_wrapped_image() {
        let layout = layout_for_test(
            r#"<div><a style="width:50%"><img style="width:100%" width="640" height="20" alt=""></a></div>"#,
            640,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image");
        assert!((image.rect.width - 640.0).abs() < 0.1);
    }

    #[test]
    fn missing_images_with_empty_alt_have_zero_default_size() {
        let layout = layout_for_test(r#"<img src="missing.png" alt="">"#, 200);
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(None))
        })
        .expect("image");
        assert!(image.rect.width < 0.1);
        assert!(image.rect.height < 0.1);
    }

    #[test]
    fn missing_images_keep_explicit_dimensions() {
        let layout = layout_for_test(
            r#"<img src="missing.png" width="50" height="20" alt="">"#,
            200,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(None))
        })
        .expect("image");
        assert!((image.rect.width - 50.0).abs() < 0.1);
        assert!((image.rect.height - 20.0).abs() < 0.1);
    }

    #[test]
    fn css_image_height_auto_overrides_html_height_attribute() {
        let layout = layout_for_test(
            r##"<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="100" height="40" style="width:50px;height:auto" alt="">"##,
            200,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");

        assert!((image.rect.width - 50.0).abs() < 0.1);
        assert!((image.rect.height - 50.0).abs() < 0.1);
    }

    #[test]
    fn image_width_auto_uses_declared_height_and_aspect_ratio() {
        let layout = layout_for_test(
            r##"<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" height="20" alt="">"##,
            200,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");

        assert!((image.rect.width - 20.0).abs() < 0.1);
        assert!((image.rect.height - 20.0).abs() < 0.1);
    }

    #[test]
    fn image_max_width_clamps_css_width_and_preserves_auto_height() {
        let layout = layout_for_test(
            r##"<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="400" height="400" style="max-width:50%;height:auto" alt="">"##,
            200,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");

        assert!((image.rect.width - 100.0).abs() < 0.1);
        assert!((image.rect.height - 100.0).abs() < 0.1);
    }

    #[test]
    fn image_auto_horizontal_margins_center_fixed_width_images() {
        let layout = layout_for_test(
            r##"<img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="40" height="20" style="margin:auto" alt="">"##,
            200,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");

        assert!((image.rect.x - 80.0).abs() < 0.1);
    }

    #[test]
    fn paragraph_top_margin_collapses_inside_list_items() {
        let layout = layout_for_test(r#"<ol><li><p>First item</p></li></ol>"#, 200);
        let marker = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "1."),
        )
        .expect("marker");
        let text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "First item"),
        )
        .expect("text");

        assert!((marker.rect.y - text.rect.y).abs() < 1.0);
    }

    #[test]
    fn auto_horizontal_margins_center_fixed_width_blocks() {
        let layout = layout_for_test(
            r#"<div style="width:100px;margin:0 auto;background:#000">Inner</div>"#,
            200,
        );
        let block = find_layout(&layout, |child| child.style.background == Some(Rgba::BLACK))
            .expect("block");
        assert!((block.rect.x - 50.0).abs() < 0.1);
    }

    #[test]
    fn content_box_table_cell_width_keeps_padding_outside_content() {
        let layout = layout_for_test(
            r#"<table width="100"><tr><td style="width:32px;padding-left:12px;box-sizing:content-box;white-space:nowrap">4/5</td></tr></table>"#,
            100,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
        let text =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_))).expect("text");
        assert!(cell.rect.width >= 44.0);
        assert!(text.rect.width >= 32.0);
        assert_eq!(text.style.wrap, TextWrap::None);
    }

    #[test]
    fn lays_out_images_inside_inline_links() {
        let layout = layout_for_test(
            r##"<a href="#"><img width="20" height="10" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" alt="logo"></a>"##,
            80,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");
        assert!((image.rect.width - 20.0).abs() < 0.1);
        assert!((image.rect.height - 10.0).abs() < 0.1);
    }

    #[test]
    fn bilinear_image_sampling_blends_neighbor_pixels() {
        let image = ImageData {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };

        let sampled = sample_image_bilinear(&image, 0.5, 0.0);

        assert_eq!(sampled, [128, 128, 128, 255]);
    }

    #[test]
    fn area_image_sampling_averages_downscaled_pixels() {
        let image = ImageData {
            width: 2,
            height: 2,
            rgba: vec![
                0, 0, 0, 255, 100, 100, 100, 255, 200, 200, 200, 255, 255, 255, 255, 255,
            ],
        };

        let sampled = sample_image_area(&image, 0.0, 0.0, 2.0, 2.0);

        assert_eq!(sampled, [139, 139, 139, 255]);
    }

    #[test]
    fn image_rects_are_pixel_snapped_like_blink() {
        let image = ImageData {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        let mut pixmap = Pixmap::new(4, 1).expect("pixmap");

        draw_image_with_fit(
            &mut pixmap,
            1.0,
            Rect::new(0.5, 0.0, 2.0, 1.0),
            &image,
            ImageFitPaint {
                fit: ObjectFit::Fill,
                position: ObjectPosition::default(),
                radius: 0.0,
                opacity: 1.0,
            },
        );

        let data = pixmap.data();
        assert_eq!(&data[0..4], &[0, 0, 0, 0]);
        assert_eq!(&data[4..8], &[255, 0, 0, 255]);
        assert_eq!(&data[8..12], &[255, 0, 0, 255]);
        assert_eq!(&data[12..16], &[0, 0, 0, 0]);
    }

    #[test]
    fn object_fit_cover_crops_source_to_destination_ratio() {
        let image = ImageData {
            width: 4,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255,
                0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        };
        let mut pixmap = Pixmap::new(2, 2).expect("pixmap");

        draw_image_with_fit(
            &mut pixmap,
            1.0,
            Rect::new(0.0, 0.0, 2.0, 2.0),
            &image,
            ImageFitPaint {
                fit: ObjectFit::Cover,
                position: ObjectPosition::default(),
                radius: 0.0,
                opacity: 1.0,
            },
        );

        let data = pixmap.data();
        assert_eq!(&data[0..4], &[0, 255, 0, 255]);
        assert_eq!(&data[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn object_fit_contain_centers_content_rect() {
        let image = ImageData {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        };
        let object_rect = object_fit_rect(
            Rect::new(0.0, 0.0, 4.0, 4.0),
            &image,
            ObjectFit::Contain,
            ObjectPosition::default(),
        );

        assert!((object_rect.x - 0.0).abs() < 0.1);
        assert!((object_rect.y - 1.0).abs() < 0.1);
        assert!((object_rect.width - 4.0).abs() < 0.1);
        assert!((object_rect.height - 2.0).abs() < 0.1);
    }

    #[test]
    fn image_sampling_interpolates_premultiplied_alpha_like_skia() {
        let image = ImageData {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 0],
        };

        let sampled = sample_image_bilinear(&image, 0.5, 0.0);

        assert_eq!(sampled, [254, 0, 0, 128]);
    }

    #[test]
    fn image_opacity_is_applied_during_composite() {
        let image = ImageData {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        let mut pixmap = Pixmap::new(1, 1).expect("pixmap");

        draw_image_with_fit(
            &mut pixmap,
            1.0,
            Rect::new(0.0, 0.0, 1.0, 1.0),
            &image,
            ImageFitPaint {
                fit: ObjectFit::Fill,
                position: ObjectPosition::default(),
                radius: 0.0,
                opacity: 0.5,
            },
        );

        assert_eq!(pixmap.data()[3], 128);
    }

    #[test]
    fn background_images_are_clipped_by_border_radius() {
        let image = ImageData {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        let mut pixmap = Pixmap::new(3, 3).expect("pixmap");

        draw_background_image(
            &mut pixmap,
            1.0,
            Rect::new(0.0, 0.0, 3.0, 3.0),
            &image,
            BackgroundImagePaint {
                repeat: BackgroundRepeat::NoRepeat,
                size: BackgroundSize::Cover,
                position: BackgroundPosition::default(),
                radius: 1.5,
                opacity: 1.0,
            },
        );

        let data = pixmap.data();
        assert!(data[3] < 255);
        assert_eq!(&data[16..20], &[255, 0, 0, 255]);
    }

    #[test]
    fn centers_inline_block_flow_children() {
        let layout = layout_for_test(
            r#"<div style="text-align:center"><a style="display:inline-block;width:20px;height:10px;background:#000"></a></div>"#,
            100,
        );
        let inline_block = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Block)
                && (child.rect.width - 20.0).abs() < 0.1
                && child.style.background == Some(Rgba::BLACK)
        })
        .expect("inline block");
        assert!((inline_block.rect.x - 40.0).abs() < 0.1);
    }

    #[test]
    fn inlined_compound_class_inline_block_keeps_background() {
        let layout = layout_for_test(
            r#"<style>
                .btn { display:inline-block; padding:12px 24px; }
                .btn.btn-primary { background:#f3a333; color:#fff; }
            </style><p><a class="btn btn-primary">Read more</a></p>"#,
            240,
        );
        let button = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0xf3, 0xa3, 0x33))
        })
        .expect("button");
        assert!(button.rect.width > 80.0);
        assert!(button.rect.height > 30.0);
    }

    #[test]
    fn inline_anchor_with_button_box_keeps_background_and_padding() {
        let layout = layout_for_test(
            r#"<style>
                .btn { padding:10px 15px; }
                .btn.btn-primary { border-radius:30px; background:#f3a333; color:#fff; }
            </style><p style="text-align:center"><a class="btn btn-primary">Get Your Order Here!</a></p>"#,
            300,
        );
        let button = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0xf3, 0xa3, 0x33))
        })
        .expect("inline button");

        assert!(button.rect.width > 160.0);
        assert!(button.rect.height > 35.0);
        assert!(button.rect.x > 40.0);
    }

    #[test]
    fn inline_padding_does_not_expand_non_replaced_line_height() {
        let layout = layout_for_test(
            r##"<p style="margin:0;font-size:15px;line-height:27px"><a style="padding:10px 15px;background:#f3a333">Button</a></p>"##,
            300,
        );
        let paragraph = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Block)
                && child
                    .children
                    .iter()
                    .any(|child| child.style.background == Some(Rgba::rgb(0xf3, 0xa3, 0x33)))
        })
        .expect("paragraph");
        let button = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0xf3, 0xa3, 0x33))
        })
        .expect("button");

        assert!((paragraph.rect.height - 27.0).abs() < 0.1);
        assert!(button.rect.height > paragraph.rect.height);
    }

    #[test]
    fn trailing_child_margin_collapses_through_block_but_advances_flow() {
        let layout = layout_for_test(
            r##"<div style="background:#111"><p style="margin:0 0 15px;font-size:16px;line-height:20px">A</p></div><div style="height:10px;background:#222"></div>"##,
            300,
        );
        let first = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("first block");
        let second = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x22, 0x22, 0x22))
        })
        .expect("second block");

        assert!((first.rect.height - 20.0).abs() < 0.1);
        assert!((second.rect.y - 35.0).abs() < 0.1);
    }

    #[test]
    fn trailing_child_margin_collapses_with_parent_bottom_margin() {
        let layout = layout_for_test(
            r##"<div style="margin:0 0 16px"><p style="margin:0 0 16px;font-size:16px;line-height:20px">A</p></div><p style="margin:16px 0 0;font-size:16px;line-height:20px">B</p>"##,
            300,
        );
        let first_text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "A"),
        )
        .expect("first text");
        let second_text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "B"),
        )
        .expect("second text");

        assert!((second_text.rect.y - (first_text.rect.y + 36.0)).abs() < 0.1);
    }

    #[test]
    fn inline_block_uses_parent_strut_without_expanding_replaced_images() {
        let layout = layout_for_test(
            r##"<div style="font-size:15px;line-height:27px"><span style="display:inline-block;font-size:13px;line-height:23.4px;margin-bottom:20px">WELCOME</span><h2 style="margin:0;font-size:28px;line-height:39.2px">Title</h2></div><div style="font-size:16px;line-height:24px;padding:24px;background:#111"><img width="64" height="20" alt="" style="display:inline-block;height:20px;vertical-align:middle"></div>"##,
            300,
        );
        let title = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "Title"),
        )
        .expect("title");
        let logo_wrapper = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("logo wrapper");

        assert!((title.rect.y - 47.0).abs() < 0.1);
        assert!((logo_wrapper.rect.height - 68.0).abs() < 0.1);
    }

    #[test]
    fn lays_out_adjacent_inline_blocks_on_one_row() {
        let layout = layout_for_test(
            r#"<div style="font-size:0"><div style="display:inline-block; width:50%; font-size:16px">A</div>
            <div style="display:inline-block; width:50%; font-size:16px">B</div></div>"#,
            200,
        );
        let a = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "A"),
        )
        .expect("A");
        let b = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "B"),
        )
        .expect("B");
        assert!((a.rect.y - b.rect.y).abs() < 0.1);
        assert!((b.rect.x - a.rect.x - 100.0).abs() < 0.1);
    }

    #[test]
    fn baseline_inline_block_keeps_parent_descent_space() {
        let layout = layout_for_test(
            r##"<table><tr><td style="padding:36px 24px"><a style="display:inline-block"><img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" style="display:block;width:48px;height:75px" alt=""></a></td></tr></table>"##,
            600,
        );
        let cell =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");

        assert!(cell.rect.height >= 150.0);
    }

    #[test]
    fn renders_png_with_text_pixels() {
        let html = build_document(
            r##"<table width="320" cellpadding="12" bgcolor="#f3f4f6"><tr><td><h1>Hello</h1><p style="color:#2563eb">World</p></td></tr></table>"##,
            None,
            None,
            320,
        );
        let request = RenderRequest::defaults_for_html(html, 320, 240, 1.0);
        let mut renderer = MailCanvasRenderer::new(320, 240, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        let decoded = image::load_from_memory(&image.png).unwrap().to_rgba8();
        assert_eq!(decoded.width(), 320);
        assert!(decoded.height() > 40);
        let non_white_pixels = decoded
            .pixels()
            .filter(|pixel| pixel.0 != [255, 255, 255, 255])
            .count();
        assert!(non_white_pixels > 50);
    }

    #[test]
    fn renders_data_url_images() {
        let html = build_document(
            r#"<img width="20" height="10" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" alt="">"#,
            None,
            None,
            40,
        );
        let request = RenderRequest::defaults_for_html(html, 40, 40, 1.0);
        let mut renderer = MailCanvasRenderer::new(40, 40, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert!(image.console_messages.is_empty());
        assert!(image.warnings.is_empty());
        let decoded = image::load_from_memory(&image.png).unwrap().to_rgba8();
        assert_ne!(decoded.get_pixel(5, 5).0, [255, 255, 255, 255]);
    }

    #[test]
    fn remote_images_are_blocked_by_default() {
        let html = build_document(
            r#"<img width="20" height="10" src="https://example.com/pixel.png" alt="">"#,
            None,
            None,
            40,
        );
        let request = RenderRequest::defaults_for_html(html, 40, 40, 1.0);
        let mut renderer = MailCanvasRenderer::new(40, 40, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert_eq!(image.console_messages.len(), 1);
        assert!(
            image.console_messages[0]
                .message
                .contains("remote resources are disabled")
        );
        assert_eq!(image.warnings.len(), 1);
        assert_eq!(image.warnings[0].code, RenderWarningCode::ImageLoadFailed);
        assert_eq!(image.warnings[0].node.as_deref(), Some("img"));
        assert_eq!(
            image.warnings[0].url.as_deref(),
            Some("https://example.com/pixel.png")
        );
    }

    #[test]
    fn renders_raster_pdf() {
        let html = build_document("<p>Hello PDF</p>", None, None, 160);
        let request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        let mut renderer = MailCanvasRenderer::new(160, 120, 1.0).unwrap();
        let pdf = renderer.render_pdf(request).unwrap();
        assert!(pdf.pdf.starts_with(b"%PDF-"));
        assert!(pdf.pdf.len() > 100);
        assert!(pdf.warnings.is_empty());
    }

    #[test]
    fn rejects_content_over_max_height() {
        let html = build_document(
            r#"<div style="height: 120px; background: #000"></div>"#,
            None,
            None,
            160,
        );
        let mut request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        request.max_height = Some(60);
        let mut renderer = MailCanvasRenderer::new(160, 120, 1.0).unwrap();
        let error = renderer.render_png(request).unwrap_err();
        assert!(error.to_string().contains("max-height"));
    }

    #[test]
    fn rejects_zero_width() {
        let request = RenderRequest::defaults_for_html(String::new(), 0, 800, 1.0);
        assert!(validate_request(&request).is_err());
    }

    fn find_layout(
        layout: &LayoutBox,
        predicate: impl Fn(&LayoutBox) -> bool + Copy,
    ) -> Option<&LayoutBox> {
        if predicate(layout) {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_layout(child, predicate))
    }
}
