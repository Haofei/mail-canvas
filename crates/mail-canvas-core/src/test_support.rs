use std::path::PathBuf;

use anyhow::Result;
use cosmic_text::FontSystem;
use kuchiki::traits::TendrilSink as _;
use tiny_skia::Pixmap;

use crate::MailCanvasFontFallback;
use crate::api::{
    DEFAULT_MAX_DECODED_PIXELS, DEFAULT_MAX_DOM_NODES, DEFAULT_MAX_IMAGE_BYTES,
    DEFAULT_MAX_LAYOUT_DEPTH, DEFAULT_MAX_TABLE_CELLS, EmailRenderer, RenderDebugOptions,
    RenderRequest, RenderedImage, RenderedPdf, RenderedRgba, ResourcePolicy,
};
use crate::css::inline_css;
use crate::document::build_document;
use crate::fonts::{FontFamilyIndex, font_database_from_paths, system_font_database};
use crate::layout::{LayoutBox, LayoutEngine, RenderLimits};
use crate::output::OutputBackend;
use crate::render::{scaled_dimension, validate_scale};
use crate::resource::{TestResourceProvider, TestResourceProviderFactory};

pub(crate) struct MailCanvasRenderer {
    inner: crate::RendererCore,
}

impl MailCanvasRenderer {
    pub(crate) fn new(width: u32, viewport_height: u32, scale: f32) -> Result<Self> {
        Self::with_fonts(width, viewport_height, scale, [])
    }

    pub(crate) fn with_fonts(
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
            font_database_from_paths(font_paths)?
        };
        let font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            font_db,
            MailCanvasFontFallback,
        );
        Ok(Self {
            inner: crate::RendererCore::new(font_system),
        })
    }
}

impl EmailRenderer for MailCanvasRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        let output = TestOutputBackend;
        self.inner
            .render_png_with(request, &TestResourceProviderFactory, &output)
    }

    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf> {
        let output = TestOutputBackend;
        self.inner
            .render_pdf_with(request, &TestResourceProviderFactory, &output)
    }
}

struct TestOutputBackend;

impl OutputBackend for TestOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, rendered: &RenderedRgba) -> Result<Vec<u8>> {
        use pdf_writer::{Content, Name, Pdf, Rect as PdfRect, Ref};

        let width = rendered.pixel_width.max(1);
        let height = rendered.pixel_height.max(1);
        let mut rgb = Vec::with_capacity(rendered.rgba.len() / 4 * 3);
        for pixel in rendered.rgba.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
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
            let mut image = pdf.image_xobject(image_id, &rgb);
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

pub(crate) fn resource_policy_for_test() -> TestResourceProvider {
    TestResourceProvider::from_request(&RenderRequest {
        html: String::new(),
        width: 1,
        viewport_height: 1,
        min_height: 1,
        scale: 1.0,
        base_url: None,
        max_height: None,
        resource_policy: ResourcePolicy {
            allow_remote: false,
            https_only: true,
            deny_private_networks: true,
            timeout: std::time::Duration::from_secs(30),
            max_resource_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_total_resource_bytes: 64 * 1024 * 1024,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
            max_resource_count: 128,
        },
        max_dom_nodes: DEFAULT_MAX_DOM_NODES,
        max_layout_depth: DEFAULT_MAX_LAYOUT_DEPTH,
        max_table_cells: DEFAULT_MAX_TABLE_CELLS,
        debug: RenderDebugOptions::none(),
    })
}

pub(crate) fn layout_for_test(html: &str, width: u32) -> LayoutBox {
    let html = inline_css(&build_document(html, None, None, width), width, 800).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new();
    let mut engine = LayoutEngine::new(
        &mut font_system,
        resource_policy_for_test(),
        FontFamilyIndex::default(),
        Vec::new(),
        RenderLimits::default(),
    );
    engine.layout_document(&document, width).unwrap()
}
