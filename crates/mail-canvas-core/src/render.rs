use anyhow::{Result, anyhow, bail};
use cosmic_text::{FontSystem, SwashCache};
use kuchiki::traits::TendrilSink as _;
use tiny_skia::{Color, Pixmap};

use crate::api::{RenderDiagnostics, RenderRequest};
use crate::css::{inline_css_from_stripped_html, strip_hidden_conditional_comments};
use crate::debug::RenderDebugSnapshot;
use crate::dom::{document_base_url, ensure_dom_node_limit};
use crate::fonts::{font_database_families, load_web_fonts_from_html};
use crate::layout::{LayoutEngine, RenderLimits};
use crate::output::OutputBackend;
use crate::paint::LayoutPainter;
use crate::resource::{ResourceProvider, ResourceProviderFactory};
use crate::{ConsoleMessage, RenderWarning, RenderedImage, RenderedPdf, RenderedRgba};

const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;

pub struct RendererCore {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

struct RenderedPixmap {
    pixmap: Pixmap,
    css_width: u32,
    css_height: u32,
    pixel_width: u32,
    pixel_height: u32,
    scale: f32,
    content_css_width: u32,
    console_messages: Vec<ConsoleMessage>,
    warnings: Vec<RenderWarning>,
    assets: Vec<crate::AssetReport>,
    debug: Option<RenderDebugSnapshot>,
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
        let rendered = self.render_pixmap_with(request, resource_factory)?;
        let png = output.encode_png(&rendered.pixmap)?;

        Ok(RenderedImage {
            png,
            css_width: rendered.css_width,
            css_height: rendered.css_height,
            pixel_width: rendered.pixel_width,
            pixel_height: rendered.pixel_height,
            scale: rendered.scale,
            content_css_width: rendered.content_css_width,
            console_messages: rendered.console_messages,
            warnings: rendered.warnings,
            assets: rendered.assets,
            debug: rendered.debug,
        })
    }

    pub fn render_rgba_with<F: ResourceProviderFactory>(
        &mut self,
        request: RenderRequest,
        resource_factory: &F,
    ) -> Result<RenderedRgba> {
        let rendered = self.render_pixmap_with(request, resource_factory)?;
        let rgba = rendered.pixmap.take();

        Ok(RenderedRgba {
            rgba,
            css_width: rendered.css_width,
            css_height: rendered.css_height,
            pixel_width: rendered.pixel_width,
            pixel_height: rendered.pixel_height,
            scale: rendered.scale,
            content_css_width: rendered.content_css_width,
            console_messages: rendered.console_messages,
            warnings: rendered.warnings,
            assets: rendered.assets,
            debug: rendered.debug,
        })
    }

    fn render_pixmap_with<F: ResourceProviderFactory>(
        &mut self,
        request: RenderRequest,
        resource_factory: &F,
    ) -> Result<RenderedPixmap> {
        validate_request(&request)?;

        let render_html = strip_hidden_conditional_comments(&request.html);
        let source_document = kuchiki::parse_html().one(render_html.as_str());
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
            &source_document,
            document_base.as_ref(),
            &resources,
            self.font_system.db_mut(),
            &mut diagnostics,
        );

        let available_font_families = font_database_families(self.font_system.db());
        let html =
            inline_css_from_stripped_html(&render_html, request.width, request.viewport_height)?;
        let document = kuchiki::parse_html().one(html);
        let mut engine = LayoutEngine::new(
            &mut self.font_system,
            resources.clone(),
            available_font_families,
            web_font_faces,
            limits,
            request.debug.any(),
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

        pixmap.fill(Color::WHITE);

        let mut painter = LayoutPainter {
            pixmap: &mut pixmap,
            font_system: &mut self.font_system,
            swash_cache: &mut self.swash_cache,
            scale: request.scale,
        };
        painter.paint(&layout);

        Ok(RenderedPixmap {
            pixmap,
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
        let rendered = self.render_rgba_with(request, resource_factory)?;
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

pub(crate) fn validate_request(request: &RenderRequest) -> Result<()> {
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

pub(crate) fn validate_scale(scale: f32) -> Result<()> {
    if !scale.is_finite() || scale <= 0.0 {
        bail!("scale must be a finite positive number");
    }
    Ok(())
}

pub(crate) fn scaled_dimension(value: u32, scale: f32, label: &str) -> Result<u32> {
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

pub(crate) fn clamp_css_height(
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

pub(crate) fn ceil_to_u32(value: f32) -> Result<u32> {
    if !value.is_finite() || value < 0.0 {
        bail!("invalid layout size: {value}");
    }
    let value = value.ceil();
    if value > u32::MAX as f32 {
        bail!("layout size is too large: {value}");
    }
    Ok(value.max(1.0) as u32)
}
