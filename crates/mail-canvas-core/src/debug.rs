use cosmic_text::{Buffer, FontSystem, Metrics, Shaping};
use serde::Serialize;

use crate::ImageData;
use crate::api::RenderDebugOptions;
use crate::layout::{LayoutBox, LayoutKind, normalize_preview_text};
use crate::paint::{background_tile_size, object_fit_rect, positioned_offset};
use crate::style::{
    BackgroundPosition, BackgroundRepeat, BackgroundSize, Display, ObjectFit, ObjectPosition,
    PositionAxis, Rect, Style, TextAlign, VerticalAlign,
};
use crate::text::{
    resolved_line_height_from_db, rich_text_baseline_leading_offset, rich_text_style_spans,
    spans_text, wrap_width_adjustment,
};

#[derive(Debug, Clone, Serialize)]
pub struct RenderDebugSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutNodeSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_rects: Vec<TextRectSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_diagnostics: Vec<ImageLayoutDiagnostic>,
}

impl RenderDebugSnapshot {
    pub(crate) fn collect(
        layout: &LayoutBox,
        font_system: &mut FontSystem,
        scale: f32,
        options: RenderDebugOptions,
    ) -> Option<Self> {
        if !options.any() {
            return None;
        }

        Some(Self {
            layout: options.layout.then(|| layout_snapshot(layout)),
            text_rects: if options.text_rects {
                collect_text_rects(layout, font_system, scale)
            } else {
                Vec::new()
            },
            image_diagnostics: if options.image_diagnostics {
                collect_image_diagnostics(layout)
            } else {
                Vec::new()
            },
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct RectSnapshot {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutStyleSnapshot {
    pub display: String,
    pub font_size: f32,
    pub line_height: f32,
    pub text_align: String,
    pub vertical_align: String,
    pub object_fit: String,
    pub object_position: String,
    pub background_image: bool,
    pub background_size: String,
    pub background_position: String,
    pub background_repeat: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutNodeSnapshot {
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    pub rect: RectSnapshot,
    pub style: LayoutStyleSnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<LayoutNodeSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextRectSnapshot {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    pub rect: RectSnapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageDiagnosticKind {
    Img,
    Background,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntrinsicSizeSnapshot {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageLayoutDiagnostic {
    pub kind: ImageDiagnosticKind,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    pub intrinsic: IntrinsicSizeSnapshot,
    pub css_rect: RectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_fit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_position: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_position: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background_repeat: Option<String>,
    pub draw_rect: RectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_crop: Option<RectSnapshot>,
}

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

fn collect_text_rects(
    layout: &LayoutBox,
    font_system: &mut FontSystem,
    scale: f32,
) -> Vec<TextRectSnapshot> {
    let mut rects = Vec::new();
    collect_text_rects_into(layout, font_system, scale, &mut rects);
    rects
}

fn collect_text_rects_into(
    layout: &LayoutBox,
    font_system: &mut FontSystem,
    scale: f32,
    out: &mut Vec<TextRectSnapshot>,
) {
    match &layout.kind {
        LayoutKind::Text(text) => collect_text_line_rects(
            TextLineRectContext {
                rect: layout.rect,
                style: &layout.style,
                text,
                scale,
                origin_y_extra: 0.0,
            },
            font_system,
            out,
            |buffer, font_system| {
                buffer.set_text(
                    font_system,
                    text,
                    &layout.style.text_attrs(),
                    Shaping::Advanced,
                    Some(layout.style.text_align.to_cosmic()),
                );
            },
        ),
        LayoutKind::RichText(spans) => {
            let baseline_offset = rich_text_baseline_leading_offset(spans, &layout.style);
            let text = spans_text(spans);
            collect_text_line_rects(
                TextLineRectContext {
                    rect: layout.rect,
                    style: &layout.style,
                    text: &text,
                    scale,
                    origin_y_extra: baseline_offset,
                },
                font_system,
                out,
                |buffer, font_system| {
                    let rich_spans =
                        rich_text_style_spans(spans, font_system.db(), scale, &layout.style);
                    buffer.set_rich_text(
                        font_system,
                        rich_spans,
                        &layout.style.text_attrs(),
                        Shaping::Advanced,
                        Some(layout.style.text_align.to_cosmic()),
                    );
                },
            );
        }
        _ => {}
    }
    for child in &layout.children {
        collect_text_rects_into(child, font_system, scale, out);
    }
}

struct TextLineRectContext<'a> {
    rect: Rect,
    scale: f32,
    origin_y_extra: f32,
    style: &'a Style,
    text: &'a str,
}

fn collect_text_line_rects(
    context: TextLineRectContext<'_>,
    font_system: &mut FontSystem,
    out: &mut Vec<TextRectSnapshot>,
    set_text: impl FnOnce(&mut Buffer, &mut FontSystem),
) {
    let TextLineRectContext {
        rect,
        scale,
        origin_y_extra,
        style,
        text,
    } = context;
    let line_height = resolved_line_height_from_db(font_system.db(), style);
    let metrics = Metrics::new(
        (style.font_size * scale).max(1.0),
        (line_height * scale).max(1.0),
    );
    let mut buffer = Buffer::new_empty(metrics);
    buffer.set_wrap(font_system, style.wrap.to_cosmic());
    let effective_width =
        (rect.width * wrap_width_adjustment(style.font_family.as_deref()) * scale).max(1.0);
    buffer.set_size(
        font_system,
        Some(effective_width),
        Some((rect.height * scale).max(1.0)),
    );
    set_text(&mut buffer, font_system);

    for run in buffer.layout_runs() {
        if run.glyphs.is_empty() {
            continue;
        }
        let x0 = run
            .glyphs
            .iter()
            .map(|glyph| glyph.x)
            .fold(f32::INFINITY, f32::min);
        let x1 = run
            .glyphs
            .iter()
            .map(|glyph| glyph.x + glyph.w)
            .fold(f32::NEG_INFINITY, f32::max);
        if !x0.is_finite() || !x1.is_finite() || x1 <= x0 {
            continue;
        }
        let start = run
            .glyphs
            .iter()
            .map(|glyph| glyph.start)
            .min()
            .unwrap_or(0)
            .min(run.text.len());
        let end = run
            .glyphs
            .iter()
            .map(|glyph| glyph.end)
            .max()
            .unwrap_or(run.text.len())
            .min(run.text.len());
        let line_text = run.text.get(start..end).unwrap_or(text);
        out.push(TextRectSnapshot {
            text: normalize_preview_text(line_text),
            rect: rect_snapshot(Rect::new(
                rect.x + x0 / scale,
                rect.y + origin_y_extra + run.line_top / scale,
                (x1 - x0) / scale,
                run.line_height / scale,
            )),
        });
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
