use cosmic_text::{
    Buffer, Color as TextColor, FontSystem, Metrics, Shaping, SwashCache, Weight as FontWeight,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect as SkiaRect, Transform};

use crate::ImageData;
use crate::api::DEFAULT_MAX_DECODED_PIXELS;
use crate::layout::{LayoutBox, LayoutKind};
use crate::style::{
    BackgroundImagePaint, BackgroundRepeat, BackgroundSize, BorderLineStyle, BoxShadow, Edges,
    ObjectFit, ObjectPosition, PositionAxis, Rect, Rgba, Style, TextSpan, with_opacity,
};
use crate::text::{
    resolved_line_height_from_db, rich_text_baseline_leading_offset, rich_text_style_spans,
    wrap_width_adjustment,
};

pub(crate) struct LayoutPainter<'a> {
    pub(crate) pixmap: &'a mut Pixmap,
    pub(crate) font_system: &'a mut FontSystem,
    pub(crate) swash_cache: &'a mut SwashCache,
    pub(crate) scale: f32,
}

impl LayoutPainter<'_> {
    pub(crate) fn paint(&mut self, layout: &LayoutBox) {
        self.paint_with_opacity(layout, 1.0);
    }

    pub(crate) fn paint_with_opacity(&mut self, layout: &LayoutBox, parent_opacity: f32) {
        let paints_own_box = !matches!(layout.kind, LayoutKind::Text(_) | LayoutKind::RichText(_));
        let opacity = if paints_own_box {
            parent_opacity * layout.style.opacity
        } else {
            parent_opacity
        }
        .clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        if paints_own_box {
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
            if layout.style.border.max_width() > 0.0
                && layout.style.border_style != BorderLineStyle::None
            {
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

    pub(crate) fn paint_text(&mut self, rect: Rect, style: &Style, text: &str, opacity: f32) {
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

    pub(crate) fn paint_rich_text(
        &mut self,
        rect: Rect,
        style: &Style,
        spans: &[TextSpan],
        opacity: f32,
    ) {
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

    pub(crate) fn paint_text_buffer(
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
        let effective_width =
            (rect.width * wrap_width_adjustment(style.font_family.as_deref()) * self.scale)
                .max(1.0);
        buffer.set_size(
            self.font_system,
            Some(effective_width),
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

    pub(crate) fn paint_text_runs(
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

    pub(crate) fn paint_image(
        &mut self,
        rect: Rect,
        style: &Style,
        image: &ImageData,
        opacity: f32,
    ) {
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

    pub(crate) fn paint_background_image(
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
pub(crate) struct PaintTextRunOptions {
    pub(crate) color: TextColor,
    pub(crate) opacity: f32,
    pub(crate) synthetic_bold: bool,
    pub(crate) use_glyph_color: bool,
}

pub(crate) fn apply_text_opacity(color: TextColor, opacity: f32) -> TextColor {
    if opacity >= 1.0 {
        return color;
    }
    let (r, g, b, a) = color.as_rgba_tuple();
    let a = (a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
    TextColor::rgba(r, g, b, a)
}

pub(crate) fn apply_text_base_alpha(mask_color: TextColor, base_color: TextColor) -> TextColor {
    let (r, g, b, a) = mask_color.as_rgba_tuple();
    let (_, _, _, base_a) = base_color.as_rgba_tuple();
    if base_a == 255 {
        return mask_color;
    }
    let a = ((u16::from(a) * u16::from(base_a) + 127) / 255) as u8;
    TextColor::rgba(r, g, b, a)
}

fn needs_synthetic_bold_paint(style: &Style) -> bool {
    style.font_weight.0 >= FontWeight::SEMIBOLD.0
        && style
            .font_face_weight
            .is_some_and(|face_weight| face_weight.0 < FontWeight::SEMIBOLD.0)
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
pub(crate) fn fill_rect(pixmap: &mut Pixmap, scale: f32, rect: Rect, color: Rgba) {
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
pub(crate) fn draw_image_with_fit(
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
pub(crate) fn draw_background_image(
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
pub(crate) fn background_tile_size(
    rect: Rect,
    image: &ImageData,
    size: BackgroundSize,
) -> (f32, f32) {
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
pub(crate) fn positioned_offset(origin: f32, available: f32, size: f32, axis: PositionAxis) -> f32 {
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
pub(crate) fn object_fit_rect(
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
pub(crate) fn point_in_rounded_rect(x: f32, y: f32, rect: Rect, radius: f32) -> bool {
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
pub(crate) fn sample_image_bilinear(image: &ImageData, x: f32, y: f32) -> [u8; 4] {
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
pub(crate) fn sample_image_area(image: &ImageData, x0: f32, y0: f32, x1: f32, y1: f32) -> [u8; 4] {
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ImageFitPaint {
    pub(crate) fit: ObjectFit,
    pub(crate) position: ObjectPosition,
    pub(crate) radius: f32,
    pub(crate) opacity: f32,
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

const IMAGE_AREA_SAMPLE_PHASE: f32 = 0.25;
