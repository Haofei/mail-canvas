#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[cfg(test)]
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
#[cfg(test)]
use cosmic_text::{Attrs, Buffer, Color as TextColor, Metrics, Shaping, Weight as FontWeight};
use cosmic_text::{FontSystem, SwashCache};
use kuchiki::traits::TendrilSink as _;
use tiny_skia::Pixmap;
mod api;
mod css;
mod debug;
mod document;
mod dom;
mod font_catalog;
mod fonts;
mod layout;
mod output;
mod paint;
mod resource;
mod style;
mod table;
mod text;

use api::RenderDiagnostics;
pub use api::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, EmailRenderer,
    RenderDebugOptions, RenderDiagnosticsReport, RenderRequest, RenderWarning, RenderWarningCode,
    RenderedImage, RenderedPdf, ResourcePolicy,
};
#[cfg(test)]
use api::{DEFAULT_MAX_DECODED_PIXELS, DEFAULT_MAX_IMAGE_BYTES};
#[cfg(test)]
use css::css_declarations;
use css::{inline_css, strip_hidden_conditional_comments};
pub use debug::{
    ImageDiagnosticKind, ImageLayoutDiagnostic, IntrinsicSizeSnapshot, LayoutNodeSnapshot,
    LayoutStyleSnapshot, RectSnapshot, RenderDebugSnapshot, TextRectSnapshot,
};
pub use document::{PreparedDocument, build_document};
#[cfg(test)]
use dom::find_first_tag;
use dom::{document_base_url, ensure_dom_node_limit};
pub use fonts::MailCanvasFontFallback;
#[cfg(test)]
use fonts::{
    WebFontFace, font_database_from_paths, font_face_covers_basic_latin, stylesheet_link_urls,
    system_font_database,
};
use fonts::{font_database_families, load_web_fonts_from_html};
#[cfg(test)]
use layout::{LayoutBox, LayoutKind};
pub(crate) use layout::{LayoutEngine, RenderLimits};
pub use output::OutputBackend as RenderOutputBackend;
use output::OutputBackend;
use paint::LayoutPainter;
use paint::fill_rect;
#[cfg(test)]
use paint::{
    ImageFitPaint, apply_text_base_alpha, apply_text_opacity, draw_background_image,
    draw_image_with_fit, object_fit_rect, point_in_rounded_rect, sample_image_area,
    sample_image_bilinear,
};
#[cfg(test)]
use resource::TestResourceProvider;
pub use resource::{ImageData, ResourceProvider, ResourceProviderFactory, repair_png_chunk_crcs};
#[cfg(test)]
use style::BackgroundPosition;
#[cfg(test)]
use style::{
    BackgroundImagePaint, BackgroundRepeat, BackgroundSize, BorderLineStyle, Display, Edges,
    ObjectFit, ObjectPosition, PositionAxis, Style, TextAlign, TextSpan, style_for_node,
};
pub(crate) use style::{Length, Rect, Rgba, parse_font_style, parse_length};
#[cfg(test)]
use text::{normalize_text, rich_text_baseline_leading_offset, spans_text};

const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;
const HARD_BREAK: char = '\u{000B}';

pub struct RendererCore {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl RendererCore {
    pub fn new(font_system: FontSystem) -> Self {
        Self {
            font_system,
            swash_cache: SwashCache::new(),
        }
    }

    pub fn render_png_with<F: ResourceProviderFactory, O: OutputBackend>(
        &mut self,
        request: RenderRequest,
        resource_factory: &F,
        output: &O,
    ) -> Result<RenderedImage> {
        validate_request(&request)?;

        let render_html = strip_hidden_conditional_comments(&request.html);
        let source_document = kuchiki::parse_html().one(render_html.clone());
        ensure_dom_node_limit(&source_document, request.max_dom_nodes)?;
        let document_base = request
            .base_url
            .clone()
            .or_else(|| document_base_url(&source_document));
        let resources = resource_factory.create(&request, document_base.clone());
        let limits = RenderLimits::from_request(&request);
        let mut diagnostics = RenderDiagnostics::default();
        let web_font_faces = load_web_fonts_from_html(
            &render_html,
            document_base.as_ref(),
            &resources,
            self.font_system.db_mut(),
            &mut diagnostics,
        );

        let available_font_families = font_database_families(self.font_system.db());
        let html = inline_css(&render_html, request.width, request.viewport_height)?;
        let document = kuchiki::parse_html().one(html);
        let mut engine = LayoutEngine::new(
            &mut self.font_system,
            resources.clone(),
            available_font_families,
            web_font_faces,
            limits,
        );
        let mut layout = engine.layout_document(&document, request.width)?;
        for warning in std::mem::take(&mut engine.warnings) {
            diagnostics.push_warning(warning);
        }
        drop(engine);
        let assets = resources.take_asset_reports();
        let debug = RenderDebugSnapshot::collect(
            &layout,
            &mut self.font_system,
            request.scale,
            request.debug,
        );

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

        let png = output.encode_png(&pixmap)?;

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
            assets,
            debug,
        })
    }

    pub fn render_pdf_with<F: ResourceProviderFactory, O: OutputBackend>(
        &mut self,
        request: RenderRequest,
        resource_factory: &F,
        output: &O,
    ) -> Result<RenderedPdf> {
        let rendered = self.render_png_with(request, resource_factory, output)?;
        let pdf = output.encode_pdf(&rendered)?;
        Ok(RenderedPdf {
            pdf,
            css_width: rendered.css_width,
            css_height: rendered.css_height,
            pixel_width: rendered.pixel_width,
            pixel_height: rendered.pixel_height,
            scale: rendered.scale,
            console_messages: rendered.console_messages,
            warnings: rendered.warnings,
            assets: rendered.assets,
            debug: rendered.debug,
        })
    }

    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }

    pub fn font_system(&self) -> &FontSystem {
        &self.font_system
    }
}

#[cfg(test)]
pub struct MailCanvasRenderer {
    inner: RendererCore,
}

#[cfg(test)]
impl MailCanvasRenderer {
    pub fn new(width: u32, viewport_height: u32, scale: f32) -> Result<Self> {
        Self::with_fonts(width, viewport_height, scale, [])
    }

    pub fn with_fonts(
        width: u32,
        viewport_height: u32,
        scale: f32,
        font_paths: impl IntoIterator<Item = std::path::PathBuf>,
    ) -> Result<Self> {
        validate_scale(scale)?;
        let _ = scaled_dimension(width, scale, "width")?;
        let _ = scaled_dimension(viewport_height.max(1), scale, "viewport-height")?;
        let font_paths: Vec<std::path::PathBuf> = font_paths.into_iter().collect();
        let font_db = if font_paths.is_empty() {
            system_font_database()
        } else {
            font_database_from_paths(&font_paths)?
        };
        let font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            font_db,
            MailCanvasFontFallback,
        );
        Ok(Self {
            inner: RendererCore::new(font_system),
        })
    }
}

#[cfg(test)]
pub type RustEmailRenderer = MailCanvasRenderer;
#[cfg(test)]
pub type ServoEmailRenderer = MailCanvasRenderer;

#[cfg(test)]
impl EmailRenderer for MailCanvasRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        let output = TestOutputBackend;
        self.inner
            .render_png_with(request, &resource::TestResourceProviderFactory, &output)
    }

    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf> {
        let output = TestOutputBackend;
        self.inner
            .render_pdf_with(request, &resource::TestResourceProviderFactory, &output)
    }
}

#[cfg(test)]
struct TestOutputBackend;

#[cfg(test)]
impl OutputBackend for TestOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, rendered: &RenderedImage) -> Result<Vec<u8>> {
        use anyhow::Context as _;
        use pdf_writer::{Content, Name, Pdf, Rect as PdfRect, Ref};

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
}

// Skia's downscale sampling lands slightly later than a pure source-edge box
// average for the regression image set. Keep this limited to area sampling so
// 1:1 and upscaled images still use the normal bilinear center convention.
fn validate_request(request: &RenderRequest) -> Result<()> {
    if request.width == 0 {
        bail!("width must be greater than zero");
    }
    if request.viewport_height == 0 {
        bail!("viewport-height must be greater than zero");
    }
    if request.max_dom_nodes == 0 {
        bail!("max-dom-nodes must be greater than zero");
    }
    if request.max_table_cells == 0 {
        bail!("max-table-cells must be greater than zero");
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
fn resource_policy_for_test() -> TestResourceProvider {
    TestResourceProvider::from_request(&RenderRequest {
        html: String::new(),
        width: 1,
        viewport_height: 1,
        min_height: 1,
        scale: 1.0,
        base_url: None,
        max_height: None,
        resource_policy: crate::ResourcePolicy {
            allow_remote: false,
            https_only: true,
            deny_private_networks: true,
            timeout: Duration::from_secs(30),
            max_resource_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_total_resource_bytes: 64 * 1024 * 1024,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
            max_resource_count: 128,
        },
        max_dom_nodes: crate::api::DEFAULT_MAX_DOM_NODES,
        max_layout_depth: crate::api::DEFAULT_MAX_LAYOUT_DEPTH,
        max_table_cells: crate::api::DEFAULT_MAX_TABLE_CELLS,
        debug: RenderDebugOptions::none(),
    })
}

#[cfg(test)]
fn layout_for_test(html: &str, width: u32) -> LayoutBox {
    let html = inline_css(&build_document(html, None, None, width), width, 800).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new();
    let mut engine = LayoutEngine::new(
        &mut font_system,
        resource_policy_for_test(),
        Vec::new(),
        Vec::new(),
        RenderLimits::default(),
    );
    engine.layout_document(&document, width).unwrap()
}

#[cfg(test)]
mod tests;
