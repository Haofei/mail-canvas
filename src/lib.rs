#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::fs;
use std::io::Cursor;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::{
    Align as TextAlignMode, Attrs, Buffer, Color as TextColor, Family as FontFamily, FontSystem,
    Metrics, Shaping, Style as FontStyle, SwashCache, Weight as FontWeight, Wrap,
};
use css_inline::CSSInliner;
use data_url::DataUrl;
use image::{ImageReader, Limits};
use kuchiki::{NodeRef, traits::TendrilSink as _};
use pdf_writer::{Content, Name, Pdf, Rect as PdfRect, Ref};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect as SkiaRect, Transform};
use url::Url;

const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;
const MAX_CONSOLE_MESSAGES: usize = 50;
const MAX_CONSOLE_MESSAGE_LEN: usize = 2048;
const MAX_LAYOUT_DEPTH: usize = 64;
const DEFAULT_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;
const HARD_BREAK: char = '\u{000B}';

#[derive(Debug, Clone)]
pub struct PreparedDocument {
    pub html: String,
    pub base_url: Option<Url>,
}

#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub html: String,
    pub width: u32,
    pub viewport_height: u32,
    pub min_height: u32,
    pub scale: f32,
    pub timeout: Duration,
    pub settle: Duration,
    pub base_url: Option<Url>,
    pub max_height: Option<u32>,
    pub allow_remote: bool,
    pub https_only: bool,
    pub max_image_bytes: usize,
    pub max_decoded_pixels: u64,
}

impl RenderRequest {
    pub fn defaults_for_html(html: String, width: u32, viewport_height: u32, scale: f32) -> Self {
        Self {
            html,
            width,
            viewport_height,
            min_height: 1,
            scale,
            timeout: Duration::from_secs(30),
            settle: Duration::ZERO,
            base_url: None,
            max_height: None,
            allow_remote: false,
            https_only: true,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub png: Vec<u8>,
    pub css_width: u32,
    pub css_height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: f32,
    pub content_css_width: u32,
    pub console_messages: Vec<ConsoleMessage>,
}

#[derive(Debug, Clone)]
pub struct RenderedPdf {
    pub pdf: Vec<u8>,
    pub css_width: u32,
    pub css_height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: f32,
    pub console_messages: Vec<ConsoleMessage>,
}

#[derive(Debug, Clone)]
pub struct ConsoleMessage {
    pub level: &'static str,
    pub message: String,
}

pub trait EmailRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage>;
    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf>;
}

pub struct RustEmailRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl RustEmailRenderer {
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
            "Helvetica Neue",
            "Helvetica",
            "Arial",
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
            "Iowan Old Style",
            "Palatino Linotype",
            "Palatino",
            "Georgia",
            "Times New Roman",
            "Times",
            "DejaVu Serif",
            "Noto Serif",
        ],
    )
    .or_else(|| fallback_family.clone());
    let mono = first_available_family(
        db,
        &[
            "Menlo",
            "Monaco",
            "Consolas",
            "Courier New",
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

pub type ServoEmailRenderer = RustEmailRenderer;

impl EmailRenderer for RustEmailRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        validate_request(&request)?;

        let html = inline_css(&request.html, request.width)?;
        let document = kuchiki::parse_html().one(html);
        let mut engine = LayoutEngine::new(
            &mut self.font_system,
            ResourcePolicy::from_request(&request, document_base_url(&document)),
        );
        let mut layout = engine.layout_document(&document, request.width)?;
        let warnings = std::mem::take(&mut engine.warnings);
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
            console_messages: warnings,
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
        })
    }
}

pub fn build_document_from_files(
    html_path: &Path,
    css_path: Option<&Path>,
    base_url: Option<&str>,
    width: u32,
) -> Result<PreparedDocument> {
    let html = fs::read_to_string(html_path)
        .with_context(|| format!("failed to read {}", html_path.display()))?;
    let css = match css_path {
        Some(path) => Some(
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        ),
        None => None,
    };

    let base = match base_url {
        Some(raw) => Some(Url::parse(raw).with_context(|| format!("invalid --base-url {raw}"))?),
        None => {
            let dir = html_path.parent().unwrap_or_else(|| Path::new("."));
            let dir = dir.canonicalize().with_context(|| {
                format!("failed to resolve HTML parent directory: {}", dir.display())
            })?;
            Some(Url::from_directory_path(&dir).map_err(|()| {
                anyhow!(
                    "failed to convert HTML parent directory to base URL: {}",
                    dir.display()
                )
            })?)
        }
    };

    Ok(PreparedDocument {
        html: build_document(&html, css.as_deref(), base.as_ref(), width),
        base_url: base,
    })
}

pub fn build_document(
    source_html: &str,
    css: Option<&str>,
    base_url: Option<&Url>,
    width: u32,
) -> String {
    let head = build_head_markup(css, base_url, width);
    let lower = source_html.to_ascii_lowercase();
    let looks_like_document = lower.contains("<!doctype")
        || lower.contains("<html")
        || lower.contains("<body")
        || lower.contains("<head");

    if !looks_like_document {
        return format!(
            "<!doctype html><html><head>{head}</head><body><div id=\"email-render-root\">{source_html}</div></body></html>"
        );
    }

    inject_head_markup(source_html, &head)
}

fn inline_css(html: &str, viewport_width: u32) -> Result<String> {
    let html = inject_active_media_styles(html, viewport_width);
    CSSInliner::options()
        .load_remote_stylesheets(false)
        .keep_style_tags(false)
        .keep_link_tags(false)
        .apply_width_attributes(true)
        .apply_height_attributes(true)
        .build()
        .inline(&html)
        .context("failed to inline CSS")
}

fn inject_active_media_styles(html: &str, viewport_width: u32) -> String {
    let css = active_media_css(html, viewport_width);
    if css.trim().is_empty() {
        return html.to_string();
    }

    let style = format!("\n<style id=\"email-render-active-media\">\n{css}\n</style>\n");
    inject_head_markup(html, &style)
}

fn active_media_css(html: &str, viewport_width: u32) -> String {
    let mut out = String::new();
    for style in style_blocks(html) {
        append_active_media_css(style, viewport_width, &mut out);
    }
    out
}

fn style_blocks(html: &str) -> Vec<&str> {
    let lower = html.to_ascii_lowercase();
    let mut blocks = Vec::new();
    let mut offset = 0;

    while let Some(start_rel) = lower[offset..].find("<style") {
        let start = offset + start_rel;
        let Some(open_rel) = lower[start..].find('>') else {
            break;
        };
        let content_start = start + open_rel + 1;
        let Some(end_rel) = lower[content_start..].find("</style>") else {
            break;
        };
        let content_end = content_start + end_rel;
        blocks.push(&html[content_start..content_end]);
        offset = content_end + "</style>".len();
    }

    blocks
}

fn append_active_media_css(css: &str, viewport_width: u32, out: &mut String) {
    let lower = css.to_ascii_lowercase();
    let mut offset = 0;

    while let Some(media_rel) = lower[offset..].find("@media") {
        let media_start = offset + media_rel;
        let condition_start = media_start + "@media".len();
        let Some(open_rel) = css[condition_start..].find('{') else {
            break;
        };
        let open = condition_start + open_rel;
        let Some(close) = find_matching_brace(css, open) else {
            break;
        };

        if media_condition_matches(&css[condition_start..open], viewport_width) {
            let body = css[open + 1..close].trim();
            if !body.is_empty() {
                out.push_str(body);
                out.push('\n');
            }
        }
        offset = close + 1;
    }
}

fn find_matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }

    let mut depth = 0usize;
    let mut index = open;
    let mut quote = None;
    let mut in_comment = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                in_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

        if let Some(current_quote) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == current_quote {
                quote = None;
            }
            index += 1;
            continue;
        }

        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            in_comment = true;
            index += 2;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }

    None
}

fn media_condition_matches(condition: &str, viewport_width: u32) -> bool {
    condition
        .split(',')
        .any(|query| single_media_query_matches(query, viewport_width))
}

fn single_media_query_matches(query: &str, viewport_width: u32) -> bool {
    let query = query.to_ascii_lowercase();
    let query = query.trim();
    if query.is_empty()
        || query.contains("not screen")
        || query.contains("prefers-color-scheme")
        || (query.contains("print") && !query.contains("screen") && !query.contains("all"))
    {
        return false;
    }

    let width = viewport_width as f32;
    for max_width in media_width_constraints(query, "max-width") {
        if width > max_width {
            return false;
        }
    }
    for min_width in media_width_constraints(query, "min-width") {
        if width < min_width {
            return false;
        }
    }
    true
}

fn media_width_constraints(query: &str, name: &str) -> Vec<f32> {
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(index_rel) = query[offset..].find(name) {
        let index = offset + index_rel + name.len();
        if let Some(colon_rel) = query[index..].find(':') {
            let value_start = index + colon_rel + 1;
            if let Some(value) = parse_leading_css_number(&query[value_start..]) {
                values.push(value);
            }
            offset = value_start;
        } else {
            break;
        }
    }
    values
}

fn parse_leading_css_number(value: &str) -> Option<f32> {
    let value = value.trim_start();
    let end = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-'))
        .map(|(index, ch)| index + ch.len_utf8())
        .last()?;
    value[..end]
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn build_head_markup(css: Option<&str>, base_url: Option<&Url>, width: u32) -> String {
    let mut head = String::new();
    head.push_str("<meta charset=\"utf-8\">\n");
    if let Some(base) = base_url {
        head.push_str("<base href=\"");
        head.push_str(&escape_attr(base.as_str()));
        head.push_str("\">\n");
    }
    head.push_str("<style id=\"email-render-defaults\">\n");
    head.push_str("html, body { margin: 0; padding: 0; }\n");
    head.push_str(&format!(
        "body {{ width: {width}px; min-width: {width}px; overflow: visible; background: #fff; }}\n"
    ));
    head.push_str("#email-render-root { width: 100%; }\n");
    head.push_str("table { border-collapse: separate; border-spacing: 0; }\n");
    head.push_str("img { display: block; }\n");
    head.push_str("</style>\n");
    if let Some(css) = css {
        head.push_str("<style id=\"email-render-css\">\n");
        head.push_str(css);
        head.push_str("\n</style>\n");
    }
    head
}

fn inject_head_markup(source_html: &str, head: &str) -> String {
    let lower = source_html.to_ascii_lowercase();

    if let Some(index) = lower.find("</head>") {
        let mut out = String::with_capacity(source_html.len() + head.len());
        out.push_str(&source_html[..index]);
        out.push_str(head);
        out.push_str(&source_html[index..]);
        return out;
    }

    if let Some(index) = lower.find("<html") {
        if let Some(close_offset) = source_html[index..].find('>') {
            let insert_at = index + close_offset + 1;
            let mut out = String::with_capacity(source_html.len() + head.len() + 13);
            out.push_str(&source_html[..insert_at]);
            out.push_str("<head>");
            out.push_str(head);
            out.push_str("</head>");
            out.push_str(&source_html[insert_at..]);
            return out;
        }
    }

    format!("<!doctype html><html><head>{head}</head>{source_html}</html>")
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Clone)]
struct ResourcePolicy {
    base_url: Option<Url>,
    allow_remote: bool,
    https_only: bool,
    timeout: Duration,
    max_image_bytes: usize,
    max_decoded_pixels: u64,
}

impl ResourcePolicy {
    fn from_request(request: &RenderRequest, document_base_url: Option<Url>) -> Self {
        Self {
            base_url: request.base_url.clone().or(document_base_url),
            allow_remote: request.allow_remote,
            https_only: request.https_only,
            timeout: if request.timeout.is_zero() {
                Duration::from_secs(8)
            } else {
                request.timeout
            },
            max_image_bytes: request.max_image_bytes.max(1),
            max_decoded_pixels: request.max_decoded_pixels.max(1),
        }
    }
}

fn document_base_url(document: &NodeRef) -> Option<Url> {
    let base = find_first_tag(document, "base")?;
    let href = attr(&base, "href")?;
    Url::parse(&href).ok()
}

fn load_image(src: &str, policy: &ResourcePolicy) -> Result<ImageData> {
    let bytes = load_resource_bytes(src, policy)?;
    decode_image_bytes(&bytes, policy)
}

fn load_resource_bytes(src: &str, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if src.trim_start().starts_with("data:") {
        let data_url =
            DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
        let (bytes, _) = data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
        ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
        return Ok(bytes);
    }

    let url = Url::parse(src)
        .or_else(|_| {
            policy
                .base_url
                .as_ref()
                .ok_or(url::ParseError::RelativeUrlWithoutBase)
                .and_then(|base| base.join(src))
        })
        .with_context(|| format!("failed to resolve resource URL {src}"))?;

    match url.scheme() {
        "file" => load_file_url(&url, policy),
        "https" | "http" => load_remote_url(&url, policy),
        scheme => bail!("unsupported resource URL scheme: {scheme}"),
    }
}

fn load_file_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    let path = url
        .to_file_path()
        .map_err(|()| anyhow!("invalid file URL: {url}"))?;
    if let Some(base) = &policy.base_url {
        if base.scheme() == "file" {
            if let Ok(root) = base.to_file_path() {
                let root = root.canonicalize().unwrap_or(root);
                let target = path.canonicalize().unwrap_or(path.clone());
                if !target.starts_with(&root) {
                    bail!(
                        "file resource is outside the base directory: {}",
                        target.display()
                    );
                }
            }
        }
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    Ok(bytes)
}

fn load_remote_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if !policy.allow_remote {
        bail!("remote resources are disabled");
    }
    if policy.https_only && url.scheme() != "https" {
        bail!("non-HTTPS remote resource rejected");
    }
    reject_private_host(url)?;

    let agent = ureq::Agent::config_builder()
        .https_only(policy.https_only)
        .max_redirects(3)
        .timeout_global(Some(policy.timeout))
        .build()
        .new_agent();
    let mut response = agent
        .get(url.as_str())
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(policy.max_image_bytes as u64)
        .read_to_vec()
        .with_context(|| format!("failed to read response body from {url}"))?;
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    Ok(bytes)
}

fn reject_private_host(url: &Url) -> Result<()> {
    let Some(host) = url.host_str() else {
        bail!("remote resource missing host");
    };
    if host.eq_ignore_ascii_case("localhost") {
        bail!("localhost resource rejected");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let rejected = match ip {
            IpAddr::V4(ip) => {
                ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
            }
            IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
            }
        };
        if rejected {
            bail!("private or local remote resource rejected");
        }
    }
    Ok(())
}

fn ensure_resource_size(len: usize, max_len: usize) -> Result<()> {
    if len > max_len {
        bail!("resource is too large: {len} bytes > {max_len} bytes");
    }
    Ok(())
}

fn decode_image_bytes(bytes: &[u8], policy: &ResourcePolicy) -> Result<ImageData> {
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    let max_side = policy.max_decoded_pixels.min(u64::from(u32::MAX)) as u32;
    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(policy.max_decoded_pixels.saturating_mul(4));
    reader.limits(limits);
    let image = reader
        .with_guessed_format()
        .context("failed to guess image format")?
        .decode()
        .context("failed to decode image")?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > policy.max_decoded_pixels {
        bail!(
            "decoded image is too large: {pixels} pixels > {} pixels",
            policy.max_decoded_pixels
        );
    }
    Ok(ImageData {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

struct LayoutEngine<'a> {
    font_system: &'a mut FontSystem,
    resources: ResourcePolicy,
    warnings: Vec<ConsoleMessage>,
}

impl<'a> LayoutEngine<'a> {
    fn new(font_system: &'a mut FontSystem, resources: ResourcePolicy) -> Self {
        Self {
            font_system,
            resources,
            warnings: Vec::new(),
        }
    }

    fn layout_document(&mut self, document: &NodeRef, width: u32) -> Result<LayoutBox> {
        let root_node = find_first_tag(document, "body").unwrap_or_else(|| document.clone());
        let initial = Style::initial();
        let root_style = if root_node.as_element().is_some() {
            style_for_node(&root_node, &initial)
        } else {
            initial
        };

        let viewport_width = width as f32;
        let layout_width = root_style
            .resolve_width(viewport_width)
            .unwrap_or(viewport_width)
            .max(1.0);
        let (children, height) =
            self.layout_children(&root_node, &root_style, 0.0, 0.0, layout_width, 0)?;

        Ok(LayoutBox {
            kind: LayoutKind::Block,
            rect: Rect::new(0.0, 0.0, layout_width, height.max(1.0)),
            style: root_style,
            children,
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
    ) -> Result<(Vec<LayoutBox>, f32)> {
        if depth > MAX_LAYOUT_DEPTH {
            self.push_warning(
                "warn",
                "maximum layout depth reached; truncated nested content",
            );
            return Ok((Vec::new(), 0.0));
        }

        let mut children = Vec::new();
        let mut cursor_y = y;
        let mut text = String::new();
        let parent_tag = element_tag(node);
        let mut ordered_list_index = 1usize;

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                append_text(&mut text, &text_node.borrow());
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };

            if is_metadata_tag(&tag) {
                continue;
            }
            if tag == "br" {
                text.push(HARD_BREAK);
                continue;
            }

            let child_style = style_for_node(&child, style);
            if child_style.display == Display::None {
                continue;
            }

            if child_style.display == Display::Inline
                && tag != "img"
                && inline_can_flatten(&child, &child_style)
            {
                append_text(&mut text, &text_content(&child));
                continue;
            }

            self.flush_text(&mut text, style, x, &mut cursor_y, width, &mut children)?;
            let child_display = child_style.display;
            let list_marker = if tag == "li" {
                match parent_tag.as_deref() {
                    Some("ol") => {
                        let marker = format!("{ordered_list_index}.");
                        ordered_list_index += 1;
                        Some(marker)
                    }
                    Some("ul") => Some("\u{2022}".to_string()),
                    _ => None,
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
                if is_inline_flow(child_display, &tag) {
                    align_inline_flow(&mut flow.node, style.text_align, width);
                }
                cursor_y += flow.advance;
                children.push(flow.node);
            }
        }

        self.flush_text(&mut text, style, x, &mut cursor_y, width, &mut children)?;
        Ok((children, cursor_y - y))
    }

    fn flush_text(
        &mut self,
        text: &mut String,
        style: &Style,
        x: f32,
        cursor_y: &mut f32,
        width: f32,
        children: &mut Vec<LayoutBox>,
    ) -> Result<()> {
        let normalized = normalize_text(text);
        text.clear();

        if normalized.is_empty() {
            return Ok(());
        }

        let normalized = apply_text_transform(&normalized, style.text_transform);
        let height = self.measure_text_height(&normalized, width, style)?;
        children.push(LayoutBox {
            kind: LayoutKind::Text(normalized),
            rect: Rect::new(x, *cursor_y, width, height),
            style: style.clone(),
            children: Vec::new(),
        });
        *cursor_y += height;
        Ok(())
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
            Display::Table => self.layout_table(node, style, x, y, containing_width, depth),
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
        let (children, content_height) =
            self.layout_children(node, &style, inner_x, inner_y, preferred_inner_width, depth)?;
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let rect_width = if explicit_width.is_some() {
            max_outer_width
        } else {
            (preferred_inner_width + style.padding.horizontal() + style.border.horizontal())
                .max(1.0)
        };
        let rect_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, rect_width, rect_height),
                style,
                children,
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

        let (children, content_height) =
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth)?;
        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let rect_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(min_height)
            .max(0.0);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + style.margin.bottom,
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
        let table_width = style
            .resolve_width(containing_width)
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, table_width);
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border.left + style.padding.left;
        let content_y = rect_y + style.border.top + style.padding.top;
        let content_width = style.inner_width_for_outer(table_width);

        let grid = build_table_grid(node);
        if grid.rows.is_empty() {
            return self.layout_block(node, style, x, y, containing_width, depth);
        }

        let mut row_boxes = Vec::new();
        let mut row_y = content_y;
        let spacing = if style.border_collapse == BorderCollapse::Collapse {
            0.0
        } else {
            style.cell_spacing.max(0.0)
        };
        let column_widths = self.resolve_table_column_widths(&grid, &style, content_width, spacing);

        for row in grid.rows {
            let row_style = style_for_node(&row.node, &style);
            if row.cells.is_empty() {
                continue;
            }

            let mut cell_boxes = Vec::with_capacity(row.cells.len());
            let mut row_height: f32 = 0.0;

            for cell in row.cells {
                let mut cell_style = style_for_node(&cell.node, &row_style);
                if cell_style.padding.is_zero() && style.cell_padding > 0.0 {
                    cell_style.padding = Edges::all(style.cell_padding);
                }

                let cell_x = content_x + column_offset(&column_widths, cell.col, spacing);
                let cell_width = spanned_width(&column_widths, cell.col, cell.colspan, spacing);
                let cell_inner_x = cell_x + cell_style.border.left + cell_style.padding.left;
                let cell_inner_y = row_y + cell_style.border.top + cell_style.padding.top;
                let cell_inner_width =
                    (cell_width - cell_style.padding.horizontal() - cell_style.border.horizontal())
                        .max(1.0);
                let (children, content_height) = self.layout_children(
                    &cell.node,
                    &cell_style,
                    cell_inner_x,
                    cell_inner_y,
                    cell_inner_width,
                    depth + 1,
                )?;
                let explicit_height = cell_style.resolve_height(0.0).unwrap_or(0.0);
                let cell_height =
                    (content_height + cell_style.padding.vertical() + cell_style.border.vertical())
                        .max(explicit_height)
                        .max(1.0);
                row_height = row_height.max(cell_height);
                cell_boxes.push(LayoutBox {
                    kind: LayoutKind::Cell,
                    rect: Rect::new(cell_x, row_y, cell_width, cell_height),
                    style: cell_style,
                    children,
                });
            }

            for cell in &mut cell_boxes {
                let delta = (row_height - cell.rect.height).max(0.0);
                let offset_y = match cell.style.vertical_align {
                    VerticalAlign::Top => 0.0,
                    VerticalAlign::Middle => delta / 2.0,
                    VerticalAlign::Bottom => delta,
                };
                if offset_y > 0.0 {
                    translate_layout_children(cell, 0.0, offset_y);
                }
                cell.rect.height = row_height;
            }

            row_boxes.push(LayoutBox {
                kind: LayoutKind::Row,
                rect: Rect::new(content_x, row_y, content_width, row_height),
                style: row_style,
                children: cell_boxes,
            });
            row_y += row_height + spacing;
        }

        let content_height = (row_y - content_y - spacing).max(0.0);
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let table_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);

        Ok(Some(FlowBox {
            advance: style.margin.top + table_height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                children: row_boxes,
            },
        }))
    }

    fn resolve_table_column_widths(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        table_width: f32,
        spacing: f32,
    ) -> Vec<f32> {
        let count = grid.column_count.max(1);
        let available = (table_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = vec![None; count];
        let mut fixed_total = 0.0;

        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width.and_then(|width| width.resolve(available)) {
                let width = width.max(1.0);
                widths[col] = Some(width);
            }
        }

        for row in &grid.rows {
            for cell in &row.cells {
                let style = style_for_node(&cell.node, table_style);
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
                }
            }
        }

        for width in widths.iter().flatten() {
            fixed_total += *width;
        }
        let flexible = widths.iter().filter(|width| width.is_none()).count();
        let flexible_width = if flexible > 0 {
            ((available - fixed_total).max(flexible as f32)) / flexible as f32
        } else {
            0.0
        };

        widths
            .into_iter()
            .map(|width| width.unwrap_or(flexible_width).max(1.0))
            .collect()
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
                    "warn",
                    &format!("failed to load image {src}: {error}; drew placeholder"),
                );
                None
            }
        });
        let natural_width = image.as_ref().map_or(320.0, |image| image.width as f32);
        let natural_height = image.as_ref().map_or(32.0, |image| image.height as f32);
        let width = style
            .resolve_width(containing_width)
            .or_else(|| {
                attr(node, "width").and_then(|value| {
                    parse_length(&value).and_then(|length| length.resolve(containing_width))
                })
            })
            .unwrap_or(natural_width.min(containing_width))
            .max(1.0);
        let height = style
            .resolve_height(width)
            .or_else(|| {
                attr(node, "height")
                    .and_then(|value| parse_length(&value).and_then(|length| length.resolve(width)))
            })
            .unwrap_or_else(|| {
                if natural_width > 0.0 {
                    (width / natural_width) * natural_height
                } else {
                    natural_height
                }
            })
            .max(1.0);

        FlowBox {
            advance: style.margin.top + height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Image(image),
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
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
        let marker_width = (style.font_size * 1.75).max(20.0).min(inner_width);
        let content_x = inner_x + marker_width;
        let content_width = (inner_width - marker_width).max(1.0);

        let (mut children, content_height) =
            self.layout_children(node, &style, content_x, inner_y, content_width, depth)?;
        let mut marker_style = style.clone();
        marker_style.text_align = TextAlign::Right;
        children.insert(
            0,
            LayoutBox {
                kind: LayoutKind::Text(marker),
                rect: Rect::new(
                    inner_x,
                    inner_y,
                    (marker_width - 6.0).max(1.0),
                    style.line_height,
                ),
                style: marker_style,
                children: Vec::new(),
            },
        );

        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let rect_height = (content_height.max(style.line_height)
            + style.padding.vertical()
            + style.border.vertical())
        .max(min_height)
        .max(0.0);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                children,
            },
        }))
    }

    fn layout_hr(&mut self, mut style: Style, x: f32, y: f32, containing_width: f32) -> FlowBox {
        let width = style
            .resolve_width(containing_width)
            .unwrap_or(containing_width);
        let height = style
            .resolve_height(0.0)
            .unwrap_or_else(|| style.border.max_width().max(1.0))
            .max(1.0);
        if style.background.is_none() {
            style.background = Some(style.border_color);
        }

        FlowBox {
            advance: style.margin.top + height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
                style,
                children: Vec::new(),
            },
        }
    }

    fn measure_text_height(&mut self, text: &str, width: f32, style: &Style) -> Result<f32> {
        let metrics = Metrics::new(style.font_size.max(1.0), style.line_height.max(1.0));
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
        Ok(height.max(style.line_height))
    }

    fn measure_text_width(&mut self, text: &str, style: &Style) -> f32 {
        let metrics = Metrics::new(style.font_size.max(1.0), style.line_height.max(1.0));
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

            let child_style = style_for_node(&child, style);
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
                    .unwrap_or(containing_width)
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
                    .map_or(320.0, |image| image.width as f32)
                    .min(containing_width)
            })
            .max(1.0)
    }

    fn push_warning(&mut self, level: &'static str, message: &str) {
        if self.warnings.len() >= MAX_CONSOLE_MESSAGES {
            return;
        }

        let mut message = message.to_string();
        if message.len() > MAX_CONSOLE_MESSAGE_LEN {
            message.truncate(MAX_CONSOLE_MESSAGE_LEN);
            message.push_str("... (truncated)");
        }
        self.warnings.push(ConsoleMessage { level, message });
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
        if let Some(background) = layout.style.background {
            fill_style_rect(
                self.pixmap,
                self.scale,
                layout.rect,
                background,
                layout.style.border_radius,
            );
        }
        if layout.style.border.max_width() > 0.0 {
            stroke_style_border(
                self.pixmap,
                self.scale,
                layout.rect,
                layout.style.border,
                layout.style.border_color,
                layout.style.border_radius,
            );
        }

        match &layout.kind {
            LayoutKind::Text(text) => self.paint_text(layout.rect, &layout.style, text),
            LayoutKind::Image(Some(image)) => self.paint_image(layout.rect, image),
            LayoutKind::Image(None) => self.paint_image_placeholder(layout.rect),
            LayoutKind::Block | LayoutKind::Table | LayoutKind::Row | LayoutKind::Cell => {}
        }

        for child in &layout.children {
            self.paint(child);
        }
    }

    fn paint_text(&mut self, rect: Rect, style: &Style, text: &str) {
        let metrics = Metrics::new(
            (style.font_size * self.scale).max(1.0),
            (style.line_height * self.scale).max(1.0),
        );
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(
            self.font_system,
            Some((rect.width * self.scale).max(1.0)),
            Some((rect.height * self.scale).max(1.0)),
        );
        buffer.set_text(
            self.font_system,
            text,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(style.text_align.to_cosmic()),
        );

        let origin_x = (rect.x * self.scale).round() as i32;
        let origin_y = (rect.y * self.scale).round() as i32;
        let color = TextColor::rgba(style.color.r, style.color.g, style.color.b, style.color.a);
        buffer.draw(
            self.font_system,
            self.swash_cache,
            color,
            |x, y, width, height, color| {
                blend_text_rect(
                    self.pixmap,
                    origin_x + x,
                    origin_y + y,
                    width,
                    height,
                    color,
                );
            },
        );
    }

    fn paint_image_placeholder(&mut self, rect: Rect) {
        fill_rect(self.pixmap, self.scale, rect, Rgba::rgb(0xe5, 0xe7, 0xeb));
        stroke_rect(
            self.pixmap,
            self.scale,
            rect,
            1.0,
            Rgba::rgb(0x9c, 0xa3, 0xaf),
        );
    }

    fn paint_image(&mut self, rect: Rect, image: &ImageData) {
        draw_image(self.pixmap, self.scale, rect, image);
    }
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
    Image(Option<ImageData>),
}

#[derive(Debug)]
struct FlowBox {
    node: LayoutBox,
    advance: f32,
}

#[derive(Debug, Clone)]
struct Style {
    display: Display,
    width: Option<Length>,
    min_width: Option<Length>,
    max_width: Option<Length>,
    height: Option<Length>,
    min_height: Option<Length>,
    max_height: Option<Length>,
    margin: Edges,
    margin_left_auto: bool,
    margin_right_auto: bool,
    padding: Edges,
    background: Option<Rgba>,
    color: Rgba,
    font_family: Option<String>,
    font_weight: FontWeight,
    font_style: FontStyle,
    font_size: f32,
    line_height: f32,
    line_height_factor: Option<f32>,
    letter_spacing: f32,
    text_align: TextAlign,
    text_transform: TextTransform,
    vertical_align: VerticalAlign,
    wrap: TextWrap,
    box_sizing: BoxSizing,
    border: Edges,
    border_radius: f32,
    border_color: Rgba,
    border_collapse: BorderCollapse,
    cell_padding: f32,
    cell_spacing: f32,
}

impl Style {
    fn initial() -> Self {
        Self {
            display: Display::Block,
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            padding: Edges::ZERO,
            background: None,
            color: Rgba::BLACK,
            font_family: None,
            font_weight: FontWeight::NORMAL,
            font_style: FontStyle::Normal,
            font_size: 16.0,
            line_height: 22.4,
            line_height_factor: Some(1.4),
            letter_spacing: 0.0,
            text_align: TextAlign::Left,
            text_transform: TextTransform::None,
            vertical_align: VerticalAlign::Top,
            wrap: TextWrap::WordOrGlyph,
            box_sizing: BoxSizing::BorderBox,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: Rgba::BLACK,
            border_collapse: BorderCollapse::Separate,
            cell_padding: 0.0,
            cell_spacing: 0.0,
        }
    }

    fn from_parent_for_tag(parent: &Self, tag: &str) -> Self {
        let mut style = Self {
            display: default_display(tag),
            width: None,
            min_width: None,
            max_width: None,
            height: None,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            padding: Edges::ZERO,
            background: None,
            color: parent.color,
            font_family: parent.font_family.clone(),
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            font_size: parent.font_size,
            line_height: parent.line_height,
            line_height_factor: parent.line_height_factor,
            letter_spacing: parent.letter_spacing,
            text_align: if tag == "table" {
                TextAlign::Left
            } else {
                parent.text_align
            },
            text_transform: parent.text_transform,
            vertical_align: VerticalAlign::Top,
            wrap: parent.wrap,
            box_sizing: parent.box_sizing,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: parent.border_color,
            border_collapse: BorderCollapse::Separate,
            cell_padding: 0.0,
            cell_spacing: 0.0,
        };

        match tag {
            "h1" => style.set_font_size(32.0),
            "h2" => style.set_font_size(24.0),
            "h3" => style.set_font_size(20.0),
            "small" => style.set_font_size(parent.font_size * 0.85),
            "p" => style.margin.bottom = 16.0,
            "ul" | "ol" => {
                style.margin.top = 16.0;
                style.margin.bottom = 16.0;
                style.padding.left = 40.0;
            }
            "hr" => {
                style.margin.top = 8.0;
                style.margin.bottom = 8.0;
                style.border = Edges::all(1.0);
                style.border_color = Rgba::rgb(0xcb, 0xcc, 0xcf);
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
        }
    }

    fn apply_declaration(&mut self, name: &str, value: &str) {
        let value = strip_important(value);
        match name {
            "display" => {
                if let Some(display) = parse_display(value) {
                    self.display = display;
                }
            }
            "width" => self.width = parse_length(value),
            "min-width" => self.min_width = parse_length(value),
            "max-width" => self.max_width = parse_length(value),
            "height" => self.height = parse_length(value),
            "min-height" => self.min_height = parse_length(value),
            "max-height" => self.max_height = parse_length(value),
            "margin" => {
                if let Some((edges, left_auto, right_auto)) =
                    parse_margin_edges(value, self.font_size)
                {
                    self.margin = edges;
                    self.margin_left_auto = left_auto;
                    self.margin_right_auto = right_auto;
                }
            }
            "margin-top" => {
                self.margin.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-right" => {
                self.margin_right_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-bottom" => {
                self.margin.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-left" => {
                self.margin_left_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding" => {
                if let Some(edges) = parse_edges_with_font(value, self.font_size) {
                    self.padding = edges;
                }
            }
            "padding-top" => {
                self.padding.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding-right" => {
                self.padding.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding-bottom" => {
                self.padding.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding-left" => {
                self.padding.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "background" | "background-color" => {
                if let Some(color) = parse_color(value) {
                    self.background = Some(color);
                }
            }
            "color" => {
                if let Some(color) = parse_color(value) {
                    self.color = color;
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
                }
            }
            "font-weight" => self.font_weight = parse_font_weight(value),
            "font-style" => self.font_style = parse_font_style(value),
            "line-height" => {
                if let Some((line_height, factor)) =
                    parse_line_height_declaration(value, self.font_size)
                {
                    self.line_height = line_height.max(1.0);
                    self.line_height_factor = factor;
                }
            }
            "letter-spacing" => {
                self.letter_spacing = if value.trim().eq_ignore_ascii_case("normal") {
                    0.0
                } else {
                    parse_css_length(value, self.font_size, true).unwrap_or(0.0)
                };
            }
            "text-align" | "align" => {
                if let Some(align) = parse_text_align(value) {
                    self.text_align = align;
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
            "border" => apply_border(self, value),
            "border-radius" => self.border_radius = parse_radius(value).unwrap_or(0.0).max(0.0),
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
            "border-top" => apply_border_side(self, BorderSide::Top, value),
            "border-right" => apply_border_side(self, BorderSide::Right, value),
            "border-bottom" => apply_border_side(self, BorderSide::Bottom, value),
            "border-left" => apply_border_side(self, BorderSide::Left, value),
            "border-collapse" => {
                if value.trim().eq_ignore_ascii_case("collapse") {
                    self.border_collapse = BorderCollapse::Collapse;
                }
            }
            "border-spacing" => self.cell_spacing = parse_px(value).unwrap_or(0.0),
            _ => {}
        }
    }

    fn resolve_width(&self, containing_width: f32) -> Option<f32> {
        let mut width = self.width.and_then(|width| width.resolve(containing_width));
        if let Some(min_width) = self
            .min_width
            .and_then(|width| width.resolve(containing_width))
        {
            width = Some(width.unwrap_or(min_width).max(min_width));
        }
        if let Some(max_width) = self
            .max_width
            .and_then(|width| width.resolve(containing_width))
        {
            if let Some(current) = width {
                width = Some(current.min(max_width));
            }
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
        let family = match self.font_family.as_deref().map(str::to_ascii_lowercase) {
            Some(family) if family == "serif" => FontFamily::Serif,
            Some(family) if family == "monospace" => FontFamily::Monospace,
            Some(family) if family == "sans-serif" => FontFamily::SansSerif,
            Some(_) => self
                .font_family
                .as_deref()
                .map_or(FontFamily::SansSerif, FontFamily::Name),
            None => FontFamily::SansSerif,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(self.font_weight)
            .style(self.font_style);
        if self.letter_spacing == 0.0 {
            attrs
        } else {
            attrs.letter_spacing(self.letter_spacing / self.font_size.max(1.0))
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Display {
    Block,
    Inline,
    InlineBlock,
    Table,
    TableRow,
    TableCell,
    None,
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
enum BoxSizing {
    BorderBox,
    ContentBox,
}

#[derive(Debug, Clone)]
struct ImageData {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
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
        }
    }
    if tag == "table" {
        if let Some(cell_padding) = attrs.get("cellpadding").and_then(parse_px) {
            style.cell_padding = cell_padding;
        }
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
        let mut declarations = Vec::new();
        for declaration in style_attr.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            declarations.push((
                name.trim().to_ascii_lowercase(),
                value.trim().to_string(),
                declaration_is_important(value),
            ));
        }
        for important in [false, true] {
            for (name, value, is_important) in &declarations {
                if *is_important == important {
                    style.apply_declaration(name, value);
                }
            }
        }
    }

    style
}

fn declaration_is_important(value: &str) -> bool {
    value.trim().to_ascii_lowercase().ends_with("!important")
}

fn default_display(tag: &str) -> Display {
    match tag {
        "html" | "body" | "div" | "p" | "section" | "article" | "header" | "footer" | "main"
        | "center" | "blockquote" | "ul" | "ol" | "li" | "h1" | "h2" | "h3" | "h4" | "h5"
        | "h6" => Display::Block,
        "table" => Display::Table,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        "script" | "style" | "head" | "meta" | "link" | "title" | "base" => Display::None,
        _ => Display::Inline,
    }
}

fn parse_display(value: &str) -> Option<Display> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Some(Display::Block),
        "inline" => Some(Display::Inline),
        "inline-block" => Some(Display::InlineBlock),
        "table" => Some(Display::Table),
        "table-row" => Some(Display::TableRow),
        "table-cell" => Some(Display::TableCell),
        "none" => Some(Display::None),
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
    value.split_whitespace().next().and_then(parse_px)
}

fn parse_color(value: &str) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_color(hex);
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
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgba::rgb(r, g, b))
        }
        _ => None,
    }
}

fn parse_rgb_function(value: &str) -> Option<Rgba> {
    let start = value.find('(')?;
    let end = value.rfind(')')?;
    let channels: Vec<&str> = value[start + 1..end].split(',').collect();
    if channels.len() < 3 {
        return None;
    }
    let r = channels[0].trim().parse().ok()?;
    let g = channels[1].trim().parse().ok()?;
    let b = channels[2].trim().parse().ok()?;
    let a = channels
        .get(3)
        .and_then(|alpha| alpha.trim().parse::<f32>().ok())
        .map(|alpha| (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
        .unwrap_or(255);
    Some(Rgba::with_alpha(r, g, b, a))
}

fn parse_line_height_declaration(value: &str, font_size: f32) -> Option<(f32, Option<f32>)> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return Some((font_size * 1.4, Some(1.4)));
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (font_size * value / 100.0, None));
    }
    if let Some(length) = parse_css_length(value, font_size, false) {
        return Some((length, None));
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|scale| (font_size * scale, Some(scale)))
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
        "top" | "text-top" => Some(VerticalAlign::Top),
        "middle" => Some(VerticalAlign::Middle),
        "bottom" | "text-bottom" | "baseline" => Some(VerticalAlign::Bottom),
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
    let candidates: Vec<String> = value
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
        .collect();

    if let Some(first) = candidates.first() {
        if let Some(generic) = generic_font_family(first) {
            return Some(generic.to_string());
        }
    }
    for family in &candidates {
        if is_safe_system_font(family) {
            return Some(family.clone());
        }
    }
    for family in &candidates {
        if let Some(generic) = generic_font_family(family) {
            return Some(generic.to_string());
        }
    }
    candidates.into_iter().next()
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

fn is_inline_flow(display: Display, tag: &str) -> bool {
    tag == "img" || matches!(display, Display::Inline | Display::InlineBlock)
}

fn align_inline_flow(layout: &mut LayoutBox, align: TextAlign, containing_width: f32) {
    let free = (containing_width - layout.rect.width).max(0.0);
    let dx = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => free / 2.0,
        TextAlign::Right => free,
    };
    if dx > 0.0 {
        translate_layout(layout, dx, 0.0);
    }
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
            | Display::Table
            | Display::TableRow
            | Display::TableCell => return false,
        }
    }
    true
}

fn append_text(out: &mut String, text: &str) {
    out.push_str(text);
}

fn normalize_text(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;

    for ch in text.chars() {
        if ch == HARD_BREAK {
            while out.ends_with(' ') {
                out.pop();
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            pending_space = false;
        } else if ch.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push(ch);
            pending_space = false;
        }
    }

    out.trim_matches(|ch: char| ch != '\n' && ch.is_whitespace())
        .to_string()
}

fn fill_style_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba, radius: f32) {
    if radius <= 0.0 {
        fill_rect(pixmap, scale, rect, color);
        return;
    }
    fill_rounded_rect(pixmap, scale, rect, color, radius);
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
    _radius: f32,
) {
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

fn draw_image(pixmap: &mut Pixmap, scale: f32, rect: Rect, image: &ImageData) {
    if rect.width <= 0.0
        || rect.height <= 0.0
        || image.width == 0
        || image.height == 0
        || image.rgba.is_empty()
    {
        return;
    }

    let x0 = (rect.x * scale).round() as i32;
    let y0 = (rect.y * scale).round() as i32;
    let target_width = (rect.width * scale).round().max(1.0) as i32;
    let target_height = (rect.height * scale).round().max(1.0) as i32;
    let x1 = x0.saturating_add(target_width);
    let y1 = y0.saturating_add(target_height);

    let pixmap_width = pixmap.width() as i32;
    let pixmap_height = pixmap.height() as i32;
    let data = pixmap.data_mut();

    let start_x = x0.max(0);
    let start_y = y0.max(0);
    let end_x = x1.min(pixmap_width);
    let end_y = y1.min(pixmap_height);
    if start_x >= end_x || start_y >= end_y {
        return;
    }

    for py in start_y..end_y {
        let src_y = ((py - y0) as f32 + 0.5) * image.height as f32 / target_height as f32 - 0.5;
        for px in start_x..end_x {
            let src_x = ((px - x0) as f32 + 0.5) * image.width as f32 / target_width as f32 - 0.5;
            let [r, g, b, a] = sample_image_bilinear(image, src_x, src_y);
            let dst_index = ((py as u32 * pixmap_width as u32 + px as u32) * 4) as usize;
            composite_pixel(&mut data[dst_index..dst_index + 4], r, g, b, a);
        }
    }
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

    let p00 = image_pixel(image, x0, y0);
    let p10 = image_pixel(image, x1, y0);
    let p01 = image_pixel(image, x0, y1);
    let p11 = image_pixel(image, x1, y1);
    let mut sampled = [0; 4];

    for channel in 0..4 {
        let top = lerp(p00[channel] as f32, p10[channel] as f32, tx);
        let bottom = lerp(p01[channel] as f32, p11[channel] as f32, tx);
        sampled[channel] = lerp(top, bottom, ty).round().clamp(0.0, 255.0) as u8;
    }

    sampled
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

fn raster_pdf_from_png(rendered: &RenderedImage) -> Result<Vec<u8>> {
    let width = rendered.pixel_width.max(1);
    let height = rendered.pixel_height.max(1);
    let rgb = image::load_from_memory(&rendered.png)
        .context("failed to decode rendered PNG for PDF output")?
        .to_rgb8();
    let mut pdf = Pdf::new();
    let catalog_id = Ref::new(1);
    let page_tree_id = Ref::new(2);
    let page_id = Ref::new(3);
    let image_id = Ref::new(4);
    let content_id = Ref::new(5);

    pdf.catalog(catalog_id).pages(page_tree_id);
    pdf.pages(page_tree_id).kids([page_id]).count(1);

    {
        let mut page = pdf.page(page_id);
        page.parent(page_tree_id);
        page.media_box(PdfRect::new(0.0, 0.0, width as f32, height as f32));
        page.resources().x_objects().pair(Name(b"Im1"), image_id);
        page.contents(content_id);
    }

    {
        let mut image = pdf.image_xobject(image_id, rgb.as_raw());
        image.width(width as i32);
        image.height(height as i32);
        image.color_space().device_rgb();
        image.bits_per_component(8);
    }

    let mut content = Content::new();
    content.save_state();
    content.transform([width as f32, 0.0, 0.0, height as f32, 0.0, 0.0]);
    content.x_object(Name(b"Im1"));
    content.restore_state();
    pdf.stream(content_id, &content.finish());

    Ok(pdf.finish())
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
    let mut engine = LayoutEngine::new(&mut font_system, resource_policy_for_test());
    engine.layout_document(&document, width).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!((parse_line_height_declaration("1.625", 16.0).unwrap().0 - 26.0).abs() < 0.1);
        assert!((parse_line_height_declaration("150%", 16.0).unwrap().0 - 24.0).abs() < 0.1);
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
    fn parses_em_spacing_against_current_font_size() {
        let edges = parse_edges_with_font(".4em 0 1.1875em", 16.0).unwrap();
        assert!((edges.top - 6.4).abs() < 0.1);
        assert!((edges.bottom - 19.0).abs() < 0.1);
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
        style.apply_declaration("border-top", "10px solid #22BC66");
        style.apply_declaration("border-right", "18px solid #22BC66");
        assert_eq!(style.border.top, 10.0);
        assert_eq!(style.border.right, 18.0);
        assert_eq!(style.border_color, Rgba::rgb(0x22, 0xbc, 0x66));
    }

    #[test]
    fn parses_border_radius() {
        let mut style = Style::initial();
        style.apply_declaration("border-radius", "12px");
        assert_eq!(style.border_radius, 12.0);
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
    fn lays_out_table_cells() {
        let layout = layout_for_test(
            r#"<table width="600" cellpadding="10"><tr><td width="200">A</td><td>B</td></tr></table>"#,
            600,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert_eq!(table.children.len(), 1);
        assert_eq!(table.children[0].children.len(), 2);
        assert!((table.children[0].children[0].rect.width - 200.0).abs() < 0.1);
        assert!((table.children[0].children[1].rect.width - 400.0).abs() < 0.1);
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
        assert!((table.children[0].children[0].rect.width - 150.0).abs() < 0.1);
        assert!((table.children[0].children[1].rect.width - 150.0).abs() < 0.1);
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
    fn renders_png_with_text_pixels() {
        let html = build_document(
            r##"<table width="320" cellpadding="12" bgcolor="#f3f4f6"><tr><td><h1>Hello</h1><p style="color:#2563eb">World</p></td></tr></table>"##,
            None,
            None,
            320,
        );
        let request = RenderRequest::defaults_for_html(html, 320, 240, 1.0);
        let mut renderer = RustEmailRenderer::new(320, 240, 1.0).unwrap();
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
        let mut renderer = RustEmailRenderer::new(40, 40, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert!(image.console_messages.is_empty());
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
        let mut renderer = RustEmailRenderer::new(40, 40, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert_eq!(image.console_messages.len(), 1);
        assert!(
            image.console_messages[0]
                .message
                .contains("remote resources are disabled")
        );
    }

    #[test]
    fn renders_raster_pdf() {
        let html = build_document("<p>Hello PDF</p>", None, None, 160);
        let request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        let mut renderer = RustEmailRenderer::new(160, 120, 1.0).unwrap();
        let pdf = renderer.render_pdf(request).unwrap();
        assert!(pdf.pdf.starts_with(b"%PDF-"));
        assert!(pdf.pdf.len() > 100);
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
        let mut renderer = RustEmailRenderer::new(160, 120, 1.0).unwrap();
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
