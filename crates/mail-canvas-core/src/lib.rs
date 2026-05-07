#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use anyhow::{Result, anyhow};
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
mod render;
mod resource;
mod style;
mod table;
#[cfg(test)]
mod test_support;
mod text;

use api::RenderDiagnostics;
pub use api::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, EmailRenderer,
    RenderDebugOptions, RenderDiagnosticsReport, RenderRequest, RenderWarning, RenderWarningCode,
    RenderedImage, RenderedPdf, ResourcePolicy,
};
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
    WebFontFace, font_face_covers_basic_latin, stylesheet_link_urls, system_font_database,
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
use render::{ceil_to_u32, clamp_css_height, scaled_dimension, validate_request};
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
use test_support::{MailCanvasRenderer, layout_for_test, resource_policy_for_test};
#[cfg(test)]
use text::{normalize_text, rich_text_baseline_leading_offset, spans_text};

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
mod tests;
