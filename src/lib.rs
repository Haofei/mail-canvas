use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::{
    Align as TextAlignMode, Attrs, Buffer, Color as TextColor, FontSystem, Metrics, Shaping,
    SwashCache,
};
use css_inline::CSSInliner;
use kuchiki::{NodeRef, traits::TendrilSink as _};
use tiny_skia::{Paint, Pixmap, Rect as SkiaRect, Transform};
use url::Url;

const MAX_RENDER_PIXELS_PER_AXIS: u32 = 16_384;
const MAX_CONSOLE_MESSAGES: usize = 50;
const MAX_CONSOLE_MESSAGE_LEN: usize = 2048;
const MAX_LAYOUT_DEPTH: usize = 64;

#[derive(Debug, Clone)]
pub struct PreparedDocument {
    pub html: String,
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
pub struct ConsoleMessage {
    pub level: &'static str,
    pub message: String,
}

pub trait EmailRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage>;
}

pub struct RustEmailRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl RustEmailRenderer {
    pub fn new(width: u32, viewport_height: u32, scale: f32) -> Result<Self> {
        validate_scale(scale)?;
        let _ = scaled_dimension(width, scale, "width")?;
        let _ = scaled_dimension(viewport_height.max(1), scale, "viewport-height")?;

        Ok(Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        })
    }
}

pub type ServoEmailRenderer = RustEmailRenderer;

impl EmailRenderer for RustEmailRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        validate_request(&request)?;

        let html = inline_css(&request.html)?;
        let document = kuchiki::parse_html().one(html);
        let mut engine = LayoutEngine::new(&mut self.font_system);
        let mut layout = engine.layout_document(&document, request.width)?;
        let warnings = std::mem::take(&mut engine.warnings);
        drop(engine);

        let css_height = clamp_css_height(
            ceil_to_u32(layout.rect.height)?,
            request.min_height,
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

fn inline_css(html: &str) -> Result<String> {
    CSSInliner::options()
        .load_remote_stylesheets(false)
        .keep_style_tags(false)
        .keep_link_tags(false)
        .apply_width_attributes(true)
        .apply_height_attributes(true)
        .build()
        .inline(html)
        .context("failed to inline CSS")
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

struct LayoutEngine<'a> {
    font_system: &'a mut FontSystem,
    warnings: Vec<ConsoleMessage>,
}

impl<'a> LayoutEngine<'a> {
    fn new(font_system: &'a mut FontSystem) -> Self {
        Self {
            font_system,
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
            .width
            .and_then(|length| length.resolve(viewport_width))
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
                text.push('\n');
                continue;
            }

            let child_style = style_for_node(&child, style);
            if child_style.display == Display::None {
                continue;
            }

            if child_style.display == Display::Inline && tag != "img" {
                append_text(&mut text, &text_content(&child));
                continue;
            }

            self.flush_text(&mut text, style, x, &mut cursor_y, width, &mut children)?;
            if let Some(flow) =
                self.layout_element_with_style(&child, child_style, x, cursor_y, width, depth + 1)?
            {
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

        match style.display {
            Display::None => Ok(None),
            Display::Table => self.layout_table(node, style, x, y, containing_width, depth),
            _ => self.layout_block(node, style, x, y, containing_width, depth),
        }
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
            .width
            .and_then(|width| width.resolve(containing_width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let rect_x = x + style.margin.left;
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border_width + style.padding.left;
        let inner_y = rect_y + style.border_width + style.padding.top;
        let inner_width =
            (outer_width - style.padding.horizontal() - style.border_width.mul_add(2.0, 0.0))
                .max(1.0);

        let (children, content_height) =
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth)?;
        let min_height = style
            .height
            .and_then(|height| height.resolve(0.0))
            .unwrap_or(0.0);
        let rect_height = (content_height + style.padding.vertical() + style.border_width * 2.0)
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
            .width
            .and_then(|width| width.resolve(containing_width))
            .unwrap_or(containing_width - style.margin.horizontal())
            .max(1.0);
        let rect_x = x + style.margin.left;
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border_width + style.padding.left;
        let content_y = rect_y + style.border_width + style.padding.top;
        let content_width =
            (table_width - style.padding.horizontal() - style.border_width * 2.0).max(1.0);

        let rows = collect_rows(node);
        if rows.is_empty() {
            return self.layout_block(node, style, x, y, containing_width, depth);
        }

        let mut row_boxes = Vec::new();
        let mut row_y = content_y;
        let spacing = style.cell_spacing.max(0.0);

        for row in rows {
            let row_style = style_for_node(&row, &style);
            let cells = collect_cells(&row);
            if cells.is_empty() {
                continue;
            }

            let widths = self.resolve_cell_widths(&cells, &row_style, content_width, spacing);
            let mut cell_x = content_x;
            let mut cell_boxes = Vec::with_capacity(cells.len());
            let mut row_height: f32 = 0.0;

            for (cell, cell_width) in cells.into_iter().zip(widths) {
                let mut cell_style = style_for_node(&cell, &row_style);
                if cell_style.padding.is_zero() && style.cell_padding > 0.0 {
                    cell_style.padding = Edges::all(style.cell_padding);
                }

                let cell_inner_x = cell_x + cell_style.border_width + cell_style.padding.left;
                let cell_inner_y = row_y + cell_style.border_width + cell_style.padding.top;
                let cell_inner_width =
                    (cell_width - cell_style.padding.horizontal() - cell_style.border_width * 2.0)
                        .max(1.0);
                let (children, content_height) = self.layout_children(
                    &cell,
                    &cell_style,
                    cell_inner_x,
                    cell_inner_y,
                    cell_inner_width,
                    depth + 1,
                )?;
                let explicit_height = cell_style
                    .height
                    .and_then(|height| height.resolve(0.0))
                    .unwrap_or(0.0);
                let cell_height = (content_height
                    + cell_style.padding.vertical()
                    + cell_style.border_width * 2.0)
                    .max(explicit_height)
                    .max(1.0);
                row_height = row_height.max(cell_height);
                cell_boxes.push(LayoutBox {
                    kind: LayoutKind::Cell,
                    rect: Rect::new(cell_x, row_y, cell_width, cell_height),
                    style: cell_style,
                    children,
                });
                cell_x += cell_width + spacing;
            }

            for cell in &mut cell_boxes {
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
        let explicit_height = style
            .height
            .and_then(|height| height.resolve(0.0))
            .unwrap_or(0.0);
        let table_height = (content_height + style.padding.vertical() + style.border_width * 2.0)
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

    fn resolve_cell_widths(
        &mut self,
        cells: &[NodeRef],
        parent_style: &Style,
        row_width: f32,
        spacing: f32,
    ) -> Vec<f32> {
        let count = cells.len().max(1);
        let available = (row_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = Vec::with_capacity(cells.len());
        let mut fixed_total = 0.0;
        let mut flexible = 0usize;

        for cell in cells {
            let style = style_for_node(cell, parent_style);
            if let Some(width) = style.width.and_then(|width| width.resolve(available)) {
                let width = width.max(1.0);
                fixed_total += width;
                widths.push(Some(width));
            } else {
                flexible += 1;
                widths.push(None);
            }
        }

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
        let width = style
            .width
            .and_then(|width| width.resolve(containing_width))
            .unwrap_or(containing_width.min(320.0))
            .max(1.0);
        let height = style
            .height
            .and_then(|height| height.resolve(width))
            .unwrap_or(32.0)
            .max(1.0);

        if let Some(src) = attr(node, "src") {
            self.push_warning(
                "warn",
                &format!("image loading is not implemented; drew placeholder for {src}"),
            );
        }

        FlowBox {
            advance: style.margin.top + height + style.margin.bottom,
            node: LayoutBox {
                kind: LayoutKind::Image,
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
                style,
                children: Vec::new(),
            },
        }
    }

    fn measure_text_height(&mut self, text: &str, width: f32, style: &Style) -> Result<f32> {
        let metrics = Metrics::new(style.font_size.max(1.0), style.line_height.max(1.0));
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_size(self.font_system, Some(width.max(1.0)), None);
        buffer.set_text(
            self.font_system,
            text,
            &Attrs::new(),
            Shaping::Advanced,
            Some(style.text_align.to_cosmic()),
        );

        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            height = height.max(run.line_top + run.line_height);
        }
        Ok(height.max(style.line_height))
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
            fill_rect(self.pixmap, self.scale, layout.rect, background);
        }
        if layout.style.border_width > 0.0 {
            stroke_rect(
                self.pixmap,
                self.scale,
                layout.rect,
                layout.style.border_width,
                layout.style.border_color,
            );
        }

        match &layout.kind {
            LayoutKind::Text(text) => self.paint_text(layout.rect, &layout.style, text),
            LayoutKind::Image => self.paint_image_placeholder(layout.rect),
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
        buffer.set_size(
            self.font_system,
            Some((rect.width * self.scale).max(1.0)),
            Some((rect.height * self.scale).max(1.0)),
        );
        buffer.set_text(
            self.font_system,
            text,
            &Attrs::new(),
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
    Image,
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
    height: Option<Length>,
    margin: Edges,
    padding: Edges,
    background: Option<Rgba>,
    color: Rgba,
    font_size: f32,
    line_height: f32,
    text_align: TextAlign,
    border_width: f32,
    border_color: Rgba,
    cell_padding: f32,
    cell_spacing: f32,
}

impl Style {
    fn initial() -> Self {
        Self {
            display: Display::Block,
            width: None,
            height: None,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            background: None,
            color: Rgba::BLACK,
            font_size: 16.0,
            line_height: 22.4,
            text_align: TextAlign::Left,
            border_width: 0.0,
            border_color: Rgba::BLACK,
            cell_padding: 0.0,
            cell_spacing: 0.0,
        }
    }

    fn from_parent_for_tag(parent: &Self, tag: &str) -> Self {
        let mut style = Self {
            display: default_display(tag),
            width: None,
            height: None,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            background: None,
            color: parent.color,
            font_size: parent.font_size,
            line_height: parent.line_height,
            text_align: parent.text_align,
            border_width: 0.0,
            border_color: parent.border_color,
            cell_padding: 0.0,
            cell_spacing: 0.0,
        };

        match tag {
            "h1" => style.set_font_size(32.0),
            "h2" => style.set_font_size(24.0),
            "h3" => style.set_font_size(20.0),
            "small" => style.set_font_size(parent.font_size * 0.85),
            "p" => style.margin.bottom = 16.0,
            "th" => style.text_align = TextAlign::Center,
            _ => {}
        }

        style
    }

    fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size.max(1.0);
        self.line_height = self.font_size * 1.4;
    }

    fn apply_declaration(&mut self, name: &str, value: &str) {
        match name {
            "display" => {
                if let Some(display) = parse_display(value) {
                    self.display = display;
                }
            }
            "width" | "min-width" => self.width = parse_length(value),
            "height" | "min-height" => self.height = parse_length(value),
            "margin" => {
                if let Some(edges) = parse_edges(value) {
                    self.margin = edges;
                }
            }
            "margin-top" => self.margin.top = parse_px(value).unwrap_or(0.0),
            "margin-right" => self.margin.right = parse_px(value).unwrap_or(0.0),
            "margin-bottom" => self.margin.bottom = parse_px(value).unwrap_or(0.0),
            "margin-left" => self.margin.left = parse_px(value).unwrap_or(0.0),
            "padding" => {
                if let Some(edges) = parse_edges(value) {
                    self.padding = edges;
                }
            }
            "padding-top" => self.padding.top = parse_px(value).unwrap_or(0.0),
            "padding-right" => self.padding.right = parse_px(value).unwrap_or(0.0),
            "padding-bottom" => self.padding.bottom = parse_px(value).unwrap_or(0.0),
            "padding-left" => self.padding.left = parse_px(value).unwrap_or(0.0),
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
                if let Some(font_size) = parse_px(value) {
                    self.set_font_size(font_size);
                }
            }
            "line-height" => {
                if let Some(line_height) = parse_line_height(value, self.font_size) {
                    self.line_height = line_height.max(1.0);
                }
            }
            "text-align" | "align" => {
                if let Some(align) = parse_text_align(value) {
                    self.text_align = align;
                }
            }
            "border" => apply_border(self, value),
            "border-width" => self.border_width = parse_px(value).unwrap_or(0.0),
            "border-color" => {
                if let Some(color) = parse_color(value) {
                    self.border_color = color;
                }
            }
            "border-spacing" => self.cell_spacing = parse_px(value).unwrap_or(0.0),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Display {
    Block,
    Inline,
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

    const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
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
    if let Some(align) = attrs.get("align").and_then(parse_text_align) {
        style.text_align = align;
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
                style.border_width = border;
            }
        }
    }
    if let Some(style_attr) = attrs.get("style") {
        for declaration in style_attr.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            style.apply_declaration(&name.trim().to_ascii_lowercase(), value.trim());
        }
    }

    style
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
        "inline" | "inline-block" => Some(Display::Inline),
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
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    let number = value
        .strip_suffix("px")
        .or_else(|| value.strip_suffix("PX"))
        .unwrap_or(value)
        .trim();
    number.parse::<f32>().ok().filter(|value| value.is_finite())
}

fn parse_edges(value: &str) -> Option<Edges> {
    let values: Vec<f32> = value
        .split_whitespace()
        .filter_map(|token| parse_px(token).or(Some(0.0).filter(|_| token == "auto")))
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
        "transparent" => Some(Rgba::rgba(0, 0, 0, 0)),
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
    Some(Rgba::rgba(r, g, b, a))
}

fn parse_line_height(value: &str, font_size: f32) -> Option<f32> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("normal") {
        return Some(font_size * 1.4);
    }
    if let Some(px) = parse_px(value) {
        return Some(px);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|scale| font_size * scale)
}

fn parse_text_align(value: &str) -> Option<TextAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Some(TextAlign::Left),
        "center" | "middle" => Some(TextAlign::Center),
        "right" | "end" => Some(TextAlign::Right),
        _ => None,
    }
}

fn apply_border(style: &mut Style, value: &str) {
    if value.contains("none") {
        style.border_width = 0.0;
        return;
    }

    for token in value.split_whitespace() {
        if let Some(width) = parse_px(token) {
            style.border_width = width;
        }
        if let Some(color) = parse_color(token) {
            style.border_color = color;
        }
    }

    if style.border_width == 0.0 && !value.trim().is_empty() {
        style.border_width = 1.0;
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
        return "\n".to_string();
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

fn normalize_text(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;

    for ch in text.chars() {
        if ch == '\n' {
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

    out.trim().to_string()
}

fn fill_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba) {
    if color.a == 0 || rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let Some(rect) = SkiaRect::from_xywh(
        rect.x * scale,
        rect.y * scale,
        rect.width * scale,
        rect.height * scale,
    ) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
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

fn clamp_css_height(measured_height: u32, min_height: u32, scale: f32) -> Result<u32> {
    let requested = measured_height.max(min_height).max(1);
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
fn layout_for_test(html: &str, width: u32) -> LayoutBox {
    let html = inline_css(&build_document(html, None, None, width)).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new();
    let mut engine = LayoutEngine::new(&mut font_system);
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
        let inlined = inline_css(&html).unwrap();
        assert!(inlined.contains("style=\"color: #f00;\""));
        assert!(!inlined.contains("email-render-css"));
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
    fn renders_png_with_text_pixels() {
        let html = build_document(
            r##"<table width="320" cellpadding="12" bgcolor="#f3f4f6"><tr><td><h1>Hello</h1><p style="color:#2563eb">World</p></td></tr></table>"##,
            None,
            None,
            320,
        );
        let request = RenderRequest {
            html,
            width: 320,
            viewport_height: 240,
            min_height: 1,
            scale: 1.0,
            timeout: Duration::from_secs(1),
            settle: Duration::ZERO,
        };
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
    fn rejects_zero_width() {
        let request = RenderRequest {
            html: String::new(),
            width: 0,
            viewport_height: 800,
            min_height: 1,
            scale: 1.0,
            timeout: Duration::from_secs(1),
            settle: Duration::ZERO,
        };
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
