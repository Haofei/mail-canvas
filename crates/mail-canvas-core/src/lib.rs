#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[cfg(test)]
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
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
mod api;
mod css;
mod document;
mod dom;
mod font_catalog;
mod fonts;
mod output;
mod resource;
mod table;
mod text;

#[cfg(test)]
use api::DEFAULT_MAX_IMAGE_BYTES;
pub use api::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, EmailRenderer,
    ImageDiagnosticKind, ImageLayoutDiagnostic, IntrinsicSizeSnapshot, LayoutNodeSnapshot,
    LayoutStyleSnapshot, RectSnapshot, RenderDiagnosticsReport, RenderRequest, RenderWarning,
    RenderWarningCode, RenderedImage, RenderedPdf, ResourcePolicy, TextRectSnapshot,
};
use api::{DEFAULT_MAX_DECODED_PIXELS, RenderDiagnostics};
use css::{
    css_declarations, first_css_url, inline_css, strip_hidden_conditional_comments,
    unquote_css_value,
};
pub use document::{PreparedDocument, build_document};
use dom::{
    attr, document_base_url, element_tag, ensure_dom_node_limit, find_first_tag, is_metadata_tag,
};
pub use fonts::MailCanvasFontFallback;
use fonts::{WebFontFace, font_database_families, load_web_fonts_from_html};
#[cfg(test)]
use fonts::{
    font_database_from_paths, font_face_covers_basic_latin, stylesheet_link_urls,
    system_font_database,
};
pub use output::OutputBackend as RenderOutputBackend;
use output::OutputBackend;
#[cfg(test)]
use resource::TestResourceProvider;
pub use resource::{ImageData, ResourceProvider, ResourceProviderFactory, repair_png_chunk_crcs};
use table::{
    TableGrid, build_table_grid, column_offset, distribute_fixed_table_column_widths,
    length_is_intrinsic_fixed, spanned_width,
};
use text::{
    blink_font_descent_from_db, normal_line_height_fallback, parse_line_height_declaration,
    resolved_line_height_from_db, resolved_line_height_from_run_db,
    rich_text_baseline_leading_offset, text_style_attrs, wrap_width_adjustment,
};
#[cfg(test)]
use text::{
    blink_mac_ascent_hack_applies, blink_web_standard_family_ascent_adjustment, fontdb_family,
};

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
        let html = inline_css(&render_html, request.width)?;
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
        let layout_snapshot = layout_snapshot(&layout);
        let text_rects = collect_text_rects(&layout);
        let image_diagnostics = collect_image_diagnostics(&layout);

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
            layout: layout_snapshot,
            text_rects,
            image_diagnostics,
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
            layout: rendered.layout,
            text_rects: rendered.text_rects,
            image_diagnostics: rendered.image_diagnostics,
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

include!("layout.rs");
include!("paint.rs");
include!("style.rs");

fn layout_snapshot(layout: &LayoutBox) -> LayoutNodeSnapshot {
    LayoutNodeSnapshot {
        tag: layout.debug.tag.clone(),
        id: layout.debug.id.clone(),
        class_name: layout.debug.class_name.clone(),
        text: layout.debug.text.clone(),
        rect: rect_snapshot(layout.rect),
        style: layout_style_snapshot(&layout.style),
        children: layout.children.iter().map(layout_snapshot).collect(),
    }
}

fn collect_text_rects(layout: &LayoutBox) -> Vec<TextRectSnapshot> {
    let mut rects = Vec::new();
    collect_text_rects_into(layout, &mut rects);
    rects
}

fn collect_text_rects_into(layout: &LayoutBox, out: &mut Vec<TextRectSnapshot>) {
    match &layout.kind {
        LayoutKind::Text(text) => out.push(TextRectSnapshot {
            text: normalize_preview_text(text),
            rect: rect_snapshot(layout.rect),
        }),
        LayoutKind::RichText(spans) => out.push(TextRectSnapshot {
            text: normalize_preview_text(&spans_text(spans)),
            rect: rect_snapshot(layout.rect),
        }),
        _ => {}
    }
    for child in &layout.children {
        collect_text_rects_into(child, out);
    }
}

fn collect_image_diagnostics(layout: &LayoutBox) -> Vec<ImageLayoutDiagnostic> {
    let mut items = Vec::new();
    collect_image_diagnostics_into(layout, &mut items);
    items
}

fn collect_image_diagnostics_into(layout: &LayoutBox, out: &mut Vec<ImageLayoutDiagnostic>) {
    if let LayoutKind::Image(Some(image)) = &layout.kind {
        let draw_rect = object_fit_rect(
            layout.rect,
            image,
            layout.style.object_fit,
            layout.style.object_position,
        );
        let source_crop = source_crop_for_draw(draw_rect, layout.rect, image);
        out.push(ImageLayoutDiagnostic {
            kind: ImageDiagnosticKind::Img,
            tag: layout.debug.tag.clone(),
            id: layout.debug.id.clone(),
            class_name: layout.debug.class_name.clone(),
            src: layout.debug.src.clone(),
            intrinsic: IntrinsicSizeSnapshot {
                width: image.width,
                height: image.height,
            },
            css_rect: rect_snapshot(layout.rect),
            object_fit: Some(object_fit_name(layout.style.object_fit)),
            object_position: Some(object_position_name(layout.style.object_position)),
            background_size: None,
            background_position: None,
            background_repeat: None,
            draw_rect: rect_snapshot(draw_rect),
            source_crop: source_crop.map(rect_snapshot),
        });
    }

    if let Some(image) = &layout.style.background_image {
        let (draw_rect, source_crop) =
            background_image_diagnostic_geometry(layout.rect, &layout.style, image);
        out.push(ImageLayoutDiagnostic {
            kind: ImageDiagnosticKind::Background,
            tag: layout.debug.tag.clone(),
            id: layout.debug.id.clone(),
            class_name: layout.debug.class_name.clone(),
            src: layout.style.background_image_src.clone(),
            intrinsic: IntrinsicSizeSnapshot {
                width: image.width,
                height: image.height,
            },
            css_rect: rect_snapshot(layout.rect),
            object_fit: None,
            object_position: None,
            background_size: Some(background_size_name(layout.style.background_size)),
            background_position: Some(background_position_name(layout.style.background_position)),
            background_repeat: Some(background_repeat_name(layout.style.background_repeat)),
            draw_rect: rect_snapshot(draw_rect),
            source_crop: source_crop.map(rect_snapshot),
        });
    }

    for child in &layout.children {
        collect_image_diagnostics_into(child, out);
    }
}

fn rect_snapshot(rect: Rect) -> RectSnapshot {
    RectSnapshot {
        x: round_snapshot(rect.x),
        y: round_snapshot(rect.y),
        width: round_snapshot(rect.width),
        height: round_snapshot(rect.height),
    }
}

fn round_snapshot(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

fn layout_style_snapshot(style: &Style) -> LayoutStyleSnapshot {
    LayoutStyleSnapshot {
        display: display_name(style.display),
        font_size: round_snapshot(style.font_size),
        line_height: round_snapshot(style.line_height),
        text_align: text_align_name(style.text_align),
        vertical_align: vertical_align_name(style.vertical_align),
        object_fit: object_fit_name(style.object_fit),
        object_position: object_position_name(style.object_position),
        background_image: style.background_image.is_some(),
        background_size: background_size_name(style.background_size),
        background_position: background_position_name(style.background_position),
        background_repeat: background_repeat_name(style.background_repeat),
    }
}

fn display_name(display: Display) -> String {
    match display {
        Display::None => "none",
        Display::Block => "block",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::InlineTable => "inline-table",
        Display::Flex => "flex",
        Display::Table => "table",
        Display::TableRow => "table-row",
        Display::TableCell => "table-cell",
    }
    .to_string()
}

fn text_align_name(text_align: TextAlign) -> String {
    match text_align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
    }
    .to_string()
}

fn vertical_align_name(vertical_align: VerticalAlign) -> String {
    match vertical_align {
        VerticalAlign::Top => "top",
        VerticalAlign::Middle => "middle",
        VerticalAlign::Bottom => "bottom",
        VerticalAlign::Baseline => "baseline",
    }
    .to_string()
}

fn object_fit_name(object_fit: ObjectFit) -> String {
    match object_fit {
        ObjectFit::Fill => "fill",
        ObjectFit::Contain => "contain",
        ObjectFit::Cover => "cover",
        ObjectFit::None => "none",
        ObjectFit::ScaleDown => "scale-down",
    }
    .to_string()
}

fn object_position_name(position: ObjectPosition) -> String {
    format!(
        "{} {}",
        position_axis_name(position.x),
        position_axis_name(position.y)
    )
}

fn background_size_name(size: BackgroundSize) -> String {
    match size {
        BackgroundSize::Auto => "auto",
        BackgroundSize::Cover => "cover",
        BackgroundSize::Contain => "contain",
    }
    .to_string()
}

fn background_repeat_name(repeat: BackgroundRepeat) -> String {
    match repeat {
        BackgroundRepeat::Repeat => "repeat",
        BackgroundRepeat::NoRepeat => "no-repeat",
    }
    .to_string()
}

fn background_position_name(position: BackgroundPosition) -> String {
    format!(
        "{} {}",
        position_axis_name(position.x),
        position_axis_name(position.y)
    )
}

fn position_axis_name(axis: PositionAxis) -> String {
    if (axis.factor() - 0.0).abs() < f32::EPSILON {
        "start".to_string()
    } else if (axis.factor() - 0.5).abs() < f32::EPSILON {
        "center".to_string()
    } else if (axis.factor() - 1.0).abs() < f32::EPSILON {
        "end".to_string()
    } else {
        format!("{:.3}", round_snapshot(axis.factor()))
    }
}

fn source_crop_for_draw(draw_rect: Rect, clip_rect: Rect, image: &ImageData) -> Option<Rect> {
    let visible = intersect_rect(draw_rect, clip_rect)?;
    let sx = (visible.x - draw_rect.x) * image.width as f32 / draw_rect.width.max(1.0);
    let sy = (visible.y - draw_rect.y) * image.height as f32 / draw_rect.height.max(1.0);
    let sw = visible.width * image.width as f32 / draw_rect.width.max(1.0);
    let sh = visible.height * image.height as f32 / draw_rect.height.max(1.0);
    Some(Rect::new(sx, sy, sw, sh))
}

fn background_image_diagnostic_geometry(
    rect: Rect,
    style: &Style,
    image: &ImageData,
) -> (Rect, Option<Rect>) {
    let (tile_width, tile_height) = background_tile_size(rect, image, style.background_size);
    let tile_rect = Rect::new(
        positioned_offset(rect.x, rect.width, tile_width, style.background_position.x),
        positioned_offset(
            rect.y,
            rect.height,
            tile_height,
            style.background_position.y,
        ),
        tile_width,
        tile_height,
    );
    let source_crop = source_crop_for_draw(tile_rect, rect, image);
    (tile_rect, source_crop)
}

fn intersect_rect(left: Rect, right: Rect) -> Option<Rect> {
    let x0 = left.x.max(right.x);
    let y0 = left.y.max(right.y);
    let x1 = (left.x + left.width).min(right.x + right.width);
    let y1 = (left.y + left.height).min(right.y + right.height);
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
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
    matches!(style.display, Display::InlineBlock | Display::InlineTable)
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
        Display::InlineBlock | Display::InlineTable
            if !matches!(layout.kind, LayoutKind::Image(_)) =>
        {
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

fn table_cell_is_spacer(node: &NodeRef) -> bool {
    let text = text_content(node);
    text.chars().any(|ch| ch == '\u{00a0}')
        && text
            .chars()
            .all(|ch| ch == '\u{00a0}' || is_collapsible_whitespace(ch))
}

fn cell_contains_only_intrinsic_fixed_replaced_content(node: &NodeRef, style: &Style) -> bool {
    let mut saw_replaced = false;
    cell_contains_only_intrinsic_fixed_replaced_content_inner(node, style, &mut saw_replaced)
        && saw_replaced
}

fn cell_contains_only_intrinsic_fixed_replaced_content_inner(
    node: &NodeRef,
    style: &Style,
    saw_replaced: &mut bool,
) -> bool {
    for child in node.children() {
        if let Some(text) = child.as_text() {
            if !text.borrow().chars().all(is_collapsible_whitespace) {
                return false;
            }
            continue;
        }

        let Some(tag) = element_tag(&child) else {
            continue;
        };
        if is_metadata_tag(&tag) || tag == "br" {
            continue;
        }
        let child_style = style_for_node(&child, style);
        if child_style.display == Display::None {
            continue;
        }
        if tag == "img" {
            *saw_replaced = true;
            if child_style
                .width
                .is_some_and(|width| matches!(width, Length::Percent(_)))
            {
                return false;
            }
            continue;
        }
        if !matches!(child_style.display, Display::Inline) {
            return false;
        }
        if !cell_contains_only_intrinsic_fixed_replaced_content_inner(
            &child,
            &child_style,
            saw_replaced,
        ) {
            return false;
        }
    }
    true
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
            | Display::InlineTable
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

fn text_spans_are_only_collapsible_whitespace(spans: &[TextSpan]) -> bool {
    spans.is_empty()
        || spans
            .iter()
            .all(|span| span.text.chars().all(is_collapsible_whitespace))
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

#[cfg(test)]
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
        settle: Duration::ZERO,
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
    })
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
        RenderLimits::default(),
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
    fn media_rule_overrides_table_width_attribute() {
        let html = build_document(
            r#"<table class="floater" width="280"><tr><td>Hello</td></tr></table>"#,
            Some("@media all and (max-width: 600px) { .floater { width: 320px !important; } }"),
            None,
            600,
        );
        let inlined = inline_css(&html, 600).unwrap();
        assert!(inlined.contains("width: 320px"));
    }

    #[test]
    fn active_media_rule_can_stack_inline_tables() {
        let layout = layout_for_test(
            r#"
            <style>
              @media all and (max-width: 600px) { .floater { width: 320px !important; } }
            </style>
            <div style="font-size:0">
              <table class="floater" style="display:inline-table" width="280"><tr><td>A</td></tr></table>
              <table class="floater" style="display:inline-table" width="280"><tr><td>B</td></tr></table>
            </div>
            "#,
            600,
        );
        let tables: Vec<&LayoutBox> = collect_layouts(&layout, &|child| {
            matches!(child.kind, LayoutKind::Table) && (child.rect.width - 320.0).abs() < 0.1
        });
        assert_eq!(tables.len(), 2);
        assert!(tables[1].rect.y >= tables[0].rect.y + tables[0].rect.height - 0.1);
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
    fn unavailable_safe_system_font_uses_declared_generic_fallback() {
        let available = vec!["Arimo".to_string(), "Noto Sans".to_string()];
        let family =
            parse_font_family_with_available("Arial, Helvetica, sans-serif", &available).unwrap();
        assert_eq!(family, "sans-serif");

        let family =
            parse_font_family_with_available("Georgia, Times New Roman, serif", &available)
                .unwrap();
        assert_eq!(family, "serif");

        let family =
            parse_font_family_with_available("Trebuchet MS, Verdana, Tahoma", &available).unwrap();
        assert_eq!(family, "sans-serif");
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
    fn generic_font_families_use_registered_generic_slots() {
        assert_eq!(fontdb_family(None), fontdb::Family::Serif);
        assert_eq!(fontdb_family(Some("sans-serif")), fontdb::Family::SansSerif);
        assert_eq!(fontdb_family(Some("serif")), fontdb::Family::Serif);
    }

    #[test]
    fn mail_canvas_fallback_uses_symbol_fonts_for_missing_glyphs() {
        let mut font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            system_font_database(),
            MailCanvasFontFallback,
        );
        let mut buffer = Buffer::new_empty(Metrics::new(20.0, 24.0));
        buffer.set_size(&mut font_system, Some(240.0), Some(48.0));
        buffer.set_text(
            &mut font_system,
            "Submit ⇒",
            &Attrs::new().family(cosmic_text::Family::SansSerif),
            Shaping::Advanced,
            None,
        );

        let arrow = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .find(|glyph| "Submit ⇒"[glyph.start..glyph.end].contains('⇒'))
            .expect("arrow glyph");
        let face = font_system.db().face(arrow.font_id).expect("font face");
        assert_ne!(arrow.glyph_id, 0);
        assert!(
            face.families
                .iter()
                .any(|(family, _)| family.eq_ignore_ascii_case("Noto Sans Math")),
            "expected Noto Sans Math fallback, got {:?}",
            face.families
        );
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
    fn border_width_without_visible_style_does_not_affect_layout() {
        let layout = layout_for_test(
            r#"<a style="display:block;border-left-width:40px;border-right-width:40px;padding:10px 40px;background:#cfe2f3">Learn more</a>"#,
            300,
        );
        let link = find_layout(&layout, |child| child.debug.tag == "a").expect("link");
        assert_eq!(link.style.border, Edges::ZERO);
        assert_eq!(link.style.border_style, BorderLineStyle::None);
        let text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "Learn more"),
        )
        .expect("text");
        assert!((text.rect.x - 40.0).abs() < 0.1);
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
    fn parses_bare_hex_html_color_attributes() {
        let document = kuchiki::parse_html()
            .one(r#"<table bgcolor="5c9085" bordercolor="ffffff"><tr><td>A</td></tr></table>"#);
        let table = find_first_tag(&document, "table").expect("table");
        let style = style_for_node(&table, &Style::initial());

        assert_eq!(style.background, Some(Rgba::rgb(0x5c, 0x90, 0x85)));
        assert_eq!(style.border_color, Rgba::WHITE);
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
            MailCanvasFontFallback,
        );
        let mut engine = LayoutEngine::new(
            &mut font_system,
            resource_policy_for_test(),
            Vec::new(),
            Vec::new(),
            RenderLimits::default(),
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
        let with_empty_line = format!("Viewed by{HARD_BREAK}{HARD_BREAK}Someone");
        assert_eq!(normalize_text(&with_empty_line), "Viewed by\n\nSomeone");
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
    fn explicit_auto_layout_tables_expand_for_fixed_image_grid_min_width() {
        let layout = layout_for_test(
            r#"<table align="center" width="600" cellpadding="0" cellspacing="0">
                <tr><td style="padding:0 20px">
                  <table width="560" cellpadding="0" cellspacing="0">
                    <tr>
                      <td><img width="100" height="100" alt=""></td>
                      <td><img width="100" height="100" alt=""></td>
                      <td><img width="100" height="100" alt=""></td>
                      <td><img width="100" height="100" alt=""></td>
                      <td><img width="100" height="100" alt=""></td>
                      <td><img width="100" height="100" alt=""></td>
                    </tr>
                  </table>
                </td></tr>
              </table>"#,
            800,
        );
        let tables: Vec<&LayoutBox> =
            collect_layouts(&layout, &|child| matches!(child.kind, LayoutKind::Table));

        assert!(
            (tables[0].rect.width - 640.0).abs() < 0.1,
            "outer table width: {}",
            tables[0].rect.width
        );
        assert!(
            (tables[1].rect.width - 600.0).abs() < 0.1,
            "inner table width: {}",
            tables[1].rect.width
        );
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
    fn table_cell_valign_center_aliases_middle() {
        let layout = layout_for_test(
            r##"<table width="200"><tr><td height="100" valign="center"><div style="height:20px;background:#111"></div></td></tr></table>"##,
            200,
        );
        let child = find_layout(&layout, |child| {
            child.style.background == Some(Rgba::rgb(0x11, 0x11, 0x11))
        })
        .expect("cell child");
        assert!((child.rect.y - 40.0).abs() < 0.1);
    }

    #[test]
    fn table_cell_nowrap_attribute_disables_wrapping() {
        let layout = layout_for_test(
            r#"<table width="40"><tr><td nowrap>Alpha Beta</td></tr></table>"#,
            40,
        );
        let text = find_text_layout(&layout).expect("text");
        assert_eq!(text.style.wrap, TextWrap::None);
    }

    #[test]
    fn table_bordercolor_attribute_sets_border_color() {
        let layout = layout_for_test(
            r##"<table border="2" bordercolor="#123456"><tr><td>Cell</td></tr></table>"##,
            200,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert_eq!(table.style.border_color, Rgba::rgb(0x12, 0x34, 0x56));
        assert_eq!(table.style.border.left, 2.0);
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
    fn auto_width_table_honors_min_width() {
        let layout = layout_for_test(
            r#"<table style="min-width:120px"><tr><td>Go</td></tr></table>"#,
            200,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!(table.rect.width >= 120.0);
    }

    #[test]
    fn auto_width_table_honors_max_width() {
        let layout = layout_for_test(
            r#"<table style="max-width:100px"><tr><td style="white-space:nowrap">Alpha Beta Gamma</td></tr></table>"#,
            200,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!(table.rect.width <= 100.1);
    }

    #[test]
    fn auto_width_table_measures_flattened_inline_child_style() {
        let layout = layout_for_test(
            r#"<table><tr><td style="padding:12px 24px"><a style="font-size:17px;line-height:120%">View on GitHub</a></td></tr></table>"#,
            240,
        );
        let table =
            find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
        assert!(table.rect.width > 150.0);
    }

    #[test]
    fn inline_table_participates_in_inline_flow() {
        let layout = layout_for_test(
            r#"<div><table style="display:inline-table" width="80"><tr><td>A</td></tr></table><table style="display:inline-table" width="80"><tr><td>B</td></tr></table></div>"#,
            240,
        );
        let tables: Vec<&LayoutBox> = collect_layouts(&layout, &|child| {
            matches!(child.kind, LayoutKind::Table) && (child.rect.width - 80.0).abs() < 0.1
        });
        assert_eq!(tables.len(), 2);
        assert!(tables[1].rect.x >= tables[0].rect.x + tables[0].rect.width - 0.1);
    }

    #[test]
    fn inline_anchor_with_block_image_and_br_does_not_insert_blank_line() {
        let layout = layout_for_test(
            r#"<table border="0" cellpadding="0" cellspacing="0" width="320"><tr><td align="center" valign="top" style="padding:30px 15px 0; font-size:17px; font-weight:400; line-height:160%; font-family:sans-serif; color:#000000;"><a target="_blank" style="text-decoration:none; font-size:17px; line-height:160%;" href="https://example.com"><img width="250" height="142" alt="" style="color:#000000; font-size:10px; margin:0; padding:0; outline:none; text-decoration:none; border:none; display:block; margin-bottom:8px;" /><b style="color:#0B5073; text-decoration:underline;">Gerenal template</b></a><br/>The&nbsp;perfect choice for any purpose of a&nbsp;message.</td></tr></table>"#,
            320,
        );
        let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell))
            .expect("cell layout");
        assert!(
            cell.rect.height < 270.0,
            "unexpected cell height: {}, children: {:?}",
            cell.rect.height,
            cell.children
                .iter()
                .map(|child| (&child.kind, child.rect))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn inline_block_flow_does_not_double_count_padding() {
        let layout = layout_for_test(
            r#"
            <div style="width:433px;text-align:right;font-size:0">
              <a style="display:inline-block;padding:5px 10px 5px 20px;font-size:16px">Home</a><a style="display:inline-block;padding:5px 10px 5px 20px;font-size:16px">Product</a><a style="display:inline-block;padding:5px 10px 5px 20px;font-size:16px">About Us</a><a style="display:inline-block;padding:5px 10px 5px 20px;font-size:16px">Blog</a>
            </div>
            "#,
            433,
        );
        let links: Vec<&LayoutBox> = collect_layouts(&layout, &|child| child.debug.tag == "a");
        assert_eq!(links.len(), 4);
        let first_y = links[0].rect.y;
        assert!(
            links.iter().all(|link| (link.rect.y - first_y).abs() < 0.1),
            "inline-block links should stay on one line: {:?}",
            links
                .iter()
                .map(|link| (link.debug.text.as_str(), link.rect))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn percentage_width_table_cells_do_not_shrink_single_column_tables() {
        let layout = layout_for_test(
            r#"<table width="600" border="0" cellpadding="0" cellspacing="0"><tr><td style="padding-left:6.25%;padding-right:6.25%;width:87.5%">Header</td></tr></table>"#,
            600,
        );
        let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell))
            .expect("cell layout");
        assert!(
            (cell.rect.width - 600.0).abs() < 0.1,
            "cell width: {}",
            cell.rect.width
        );
    }

    #[test]
    fn percentage_padding_in_table_cells_reduces_text_content_width() {
        let layout = layout_for_test(
            r#"<table width="600" border="0" cellpadding="0" cellspacing="0"><tr><td style="padding-left:6.25%;padding-right:6.25%;font-size:17px;line-height:160%;">More than 50% of total email opens occurred on a mobile device and this copy should wrap like email clients do.</td></tr></table>"#,
            600,
        );
        let text = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Text(_)))
            .expect("text layout");
        assert!((text.rect.x - 37.5).abs() < 0.1, "text x: {}", text.rect.x);
        assert!(
            (text.rect.width - 525.0).abs() < 0.1,
            "text width: {}",
            text.rect.width
        );
    }

    #[test]
    fn percentage_image_width_resolves_against_column_width_not_html_width_attr() {
        let layout = layout_for_test(
            r#"<table width="600" border="0" cellpadding="0" cellspacing="0"><tr><td style="padding-top:20px"><a style="text-decoration:none" href="https://example.com"><img width="530" alt="" style="width:88.33%;max-width:530px;display:block" /></a></td></tr></table>"#,
            600,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image layout");
        assert!(
            (image.rect.width - 529.98).abs() < 1.0,
            "image width: {}",
            image.rect.width
        );
    }

    #[test]
    fn hero_image_width_stays_full_after_percent_width_rows() {
        let layout = layout_for_test(
            r#"<table width="600" border="0" cellpadding="0" cellspacing="0">
                <tr><td class="header" style="padding-bottom:6px;padding-left:6.25%;padding-right:6.25%;width:87.5%;font-size:30px;font-weight:700;line-height:130%">Explore responsive email templates</td></tr>
                <tr><td class="subheader" style="padding-bottom:3px;padding-left:6.25%;padding-right:6.25%;width:87.5%;font-size:18px;font-weight:300;line-height:150%">Available on GitHub and CodePen</td></tr>
                <tr><td class="hero" style="padding-top:20px"><a style="text-decoration:none" href="https://example.com"><img width="530" alt="" style="width:88.33%;max-width:530px;display:block" /></a></td></tr>
            </table>"#,
            600,
        );
        let image = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_)))
            .expect("image layout");
        assert!(
            (image.rect.width - 529.98).abs() < 1.0,
            "image width: {}",
            image.rect.width
        );
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
    fn nested_table_width_inherit_fills_parent_cell() {
        let layout = layout_for_test(
            r#"<table width="200"><tr><td style="padding:10px"><table style="width:inherit"><tr><td>Inner</td></tr></table></td></tr></table>"#,
            200,
        );
        let nested = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Table) && (child.rect.width - 180.0).abs() < 0.1
        })
        .expect("nested table");
        assert!((nested.rect.width - 180.0).abs() < 0.1);
    }

    #[test]
    fn inherited_width_inner_table_keeps_expected_auto_columns() {
        let layout = layout_for_test(
            r#"
            <table width="490"><tr><td>
              <table style="width: inherit; margin: 0; padding: 0; border-collapse: collapse; border-spacing: 0;">
                <tr>
                  <td style="padding-top: 30px; padding-right: 20px;"><img width="50" height="50" alt=""></td>
                  <td style="font-size: 17px; font-weight: 400; line-height: 160%; padding-top: 25px; font-family: sans-serif;">
                    <b>Highly compatible</b><br/>Tested on the most popular email clients for web, desktop and mobile. Checklist included.
                  </td>
                </tr>
              </table>
            </td></tr></table>
            "#,
            490,
        );
        let cells: Vec<&LayoutBox> =
            collect_layouts(&layout, &|child| matches!(child.kind, LayoutKind::Cell));
        let cell_widths: Vec<f32> = cells.iter().map(|cell| cell.rect.width).collect();
        let image_cell = cells
            .iter()
            .copied()
            .find(|cell| cell.rect.width < 100.0)
            .unwrap_or_else(|| panic!("image cell widths: {:?}", cell_widths));
        let text_cell = cells
            .iter()
            .copied()
            .find(|cell| cell.rect.width > 300.0 && cell.rect.width < 450.0)
            .unwrap_or_else(|| panic!("text cell widths: {:?}", cell_widths));
        assert!(
            (image_cell.rect.width - 70.0).abs() < 4.0,
            "image/text widths: {:?}",
            cell_widths
        );
        assert!(
            (text_cell.rect.width - 420.0).abs() < 4.0,
            "image/text widths: {:?}",
            cell_widths
        );
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
    fn legacy_aligned_tables_float_side_by_side() {
        let layout = layout_for_test(
            r##"
            <div style="width:590px;background:#eee">
              <table align="left" width="240"><tr><td><img width="220" height="40" alt=""></td></tr></table>
              <table align="left" width="340"><tr><td><div style="height:30px;background:#111"></div></td></tr></table>
            </div>
            "##,
            640,
        );
        let tables: Vec<&LayoutBox> =
            collect_layouts(&layout, &|child| matches!(child.kind, LayoutKind::Table));
        let floated_tables: Vec<&LayoutBox> = tables
            .into_iter()
            .filter(|table| table.style.float_side == FloatSide::Left)
            .collect();
        assert_eq!(floated_tables.len(), 2);
        assert!((floated_tables[0].rect.x - 0.0).abs() < 0.1);
        assert!((floated_tables[1].rect.x - 240.0).abs() < 0.1);
        assert!((floated_tables[1].rect.y - floated_tables[0].rect.y).abs() < 0.1);
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
    fn inline_block_tables_keep_table_cell_row_layout() {
        let layout = layout_for_test(
            r##"<table class="social-table" style="display:inline-block"><tbody><tr><td><a><img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="32" height="32" alt=""></a></td><td><a><img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="32" height="32" alt=""></a></td><td><a><img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="32" height="32" alt=""></a></td><td><a><img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" width="32" height="32" alt=""></a></td></tr></tbody></table>"##,
            200,
        );
        let social_table = find_layout(&layout, |child| {
            child.debug.class_name.as_deref() == Some("social-table")
        })
        .expect("social table");
        let images: Vec<&LayoutBox> = collect_layouts(&layout, &|child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        });

        assert!((social_table.rect.height - 34.0).abs() < 0.1);
        assert_eq!(images.len(), 4);
        assert!(
            images
                .windows(2)
                .all(|pair| pair[1].rect.x > pair[0].rect.x)
        );
        assert!(
            images
                .windows(2)
                .all(|pair| (pair[1].rect.y - pair[0].rect.y).abs() < 0.1)
        );
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
    fn inline_image_and_text_share_one_line_inside_block_link() {
        let layout = layout_for_test(
            r##"<a style="display:block;font-size:14px;line-height:20px"><img width="16" height="16" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAAAAAAALAAAAAABAAEAAAIBRAA7" alt="Phone" style="display:inline-block;padding-right:10px">987-654-321</a>"##,
            140,
        );
        let image = find_layout(&layout, |child| {
            matches!(child.kind, LayoutKind::Image(Some(_)))
        })
        .expect("image");
        let text = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "987-654-321"),
        )
        .expect("text");
        assert!(
            (text.rect.y - image.rect.y).abs() < 1.0,
            "image y {}, text y {}",
            image.rect.y,
            text.rect.y
        );
        assert!(text.rect.x > image.rect.x + image.rect.width);
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
    fn mixed_inline_text_and_inline_blocks_share_one_row() {
        let layout = layout_for_test(
            r#"<div style="font-size:0;text-align:center">
                <a style="display:inline-block;padding:5px;font-size:14px">HOW TO BOOK?</a>
                <span style="font-size:14px">·</span>
                <a style="display:inline-block;padding:5px;font-size:14px">ABOUT THE EVENT</a>
                <span style="font-size:14px">·</span>
                <a style="display:inline-block;padding:5px;font-size:14px">CONTACT</a>
            </div>"#,
            650,
        );
        let first = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "HOW TO BOOK?"),
        )
        .expect("first link");
        let second = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "ABOUT THE EVENT"),
        )
        .expect("second link");
        let third = find_layout(
            &layout,
            |child| matches!(child.kind, LayoutKind::Text(ref text) if text == "CONTACT"),
        )
        .expect("third link");

        assert!((first.rect.y - second.rect.y).abs() < 0.1);
        assert!((second.rect.y - third.rect.y).abs() < 0.1);
        assert!(first.rect.x < second.rect.x);
        assert!(second.rect.x < third.rect.x);
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
        assert_eq!(image.assets.len(), 1);
        assert_eq!(image.assets[0].kind, AssetKind::Image);
        assert_eq!(image.assets[0].status, AssetStatus::Loaded);
        assert_eq!(image.assets[0].source, Some(AssetSource::DataUrl));
        assert_eq!(image.assets[0].initiator.as_deref(), Some("img"));
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
        assert_eq!(image.assets.len(), 1);
        assert_eq!(image.assets[0].kind, AssetKind::Image);
        assert_eq!(image.assets[0].status, AssetStatus::Blocked);
        assert_eq!(image.assets[0].source, Some(AssetSource::Remote));
        assert_eq!(image.assets[0].initiator.as_deref(), Some("img"));
    }

    #[test]
    fn blocked_stylesheet_is_reported_in_assets() {
        let html = build_document(
            r#"<div>Hello</div>"#,
            Some(r#"@import url("https://example.com/email.css");"#),
            None,
            200,
        );
        let request = RenderRequest::defaults_for_html(html, 200, 120, 1.0);
        let mut renderer = MailCanvasRenderer::new(200, 120, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert_eq!(image.warnings.len(), 1);
        assert_eq!(
            image.warnings[0].code,
            RenderWarningCode::StylesheetLoadFailed
        );
        assert_eq!(image.assets.len(), 1);
        assert_eq!(image.assets[0].kind, AssetKind::Stylesheet);
        assert_eq!(image.assets[0].status, AssetStatus::Blocked);
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
        assert!(pdf.assets.is_empty());
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
    fn rejects_document_over_max_dom_nodes() {
        let html = build_document("<p>Hello</p>", None, None, 160);
        let mut request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        request.max_dom_nodes = 1;
        let mut renderer = MailCanvasRenderer::new(160, 120, 1.0).unwrap();
        let error = renderer.render_png(request).unwrap_err();
        assert!(error.to_string().contains("max-dom-nodes"));
    }

    #[test]
    fn rejects_table_over_max_table_cells() {
        let html = build_document(
            "<table><tr><td>A</td><td>B</td></tr></table>",
            None,
            None,
            160,
        );
        let mut request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        request.max_table_cells = 1;
        let mut renderer = MailCanvasRenderer::new(160, 120, 1.0).unwrap();
        let error = renderer.render_png(request).unwrap_err();
        assert!(error.to_string().contains("max-table-cells"));
    }

    #[test]
    fn layout_depth_limit_emits_structured_warning() {
        let html = build_document(
            "<table><tr><td><p>Nested</p></td></tr></table>",
            None,
            None,
            160,
        );
        let mut request = RenderRequest::defaults_for_html(html, 160, 120, 1.0);
        request.max_layout_depth = 0;
        let mut renderer = MailCanvasRenderer::new(160, 120, 1.0).unwrap();
        let image = renderer.render_png(request).unwrap();
        assert!(
            image
                .warnings
                .iter()
                .any(|warning| warning.code == RenderWarningCode::LayoutLimitReached)
        );
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

    fn collect_layouts<'a>(
        layout: &'a LayoutBox,
        predicate: &impl Fn(&LayoutBox) -> bool,
    ) -> Vec<&'a LayoutBox> {
        let mut out = Vec::new();
        collect_layouts_inner(layout, predicate, &mut out);
        out
    }

    fn collect_layouts_inner<'a>(
        layout: &'a LayoutBox,
        predicate: &impl Fn(&LayoutBox) -> bool,
        out: &mut Vec<&'a LayoutBox>,
    ) {
        if predicate(layout) {
            out.push(layout);
        }
        for child in &layout.children {
            collect_layouts_inner(child, predicate, out);
        }
    }
}
