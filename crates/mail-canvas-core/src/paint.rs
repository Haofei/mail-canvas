use cosmic_text::{Buffer, Color as TextColor, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::Pixmap;

use crate::ImageData;
use crate::layout::{LayoutBox, LayoutKind};
use crate::style::{BackgroundImagePaint, BorderLineStyle, Rect, Style, TextSpan, with_opacity};
use crate::text::{
    resolved_line_height_from_db, rich_text_baseline_leading_offset, wrap_width_adjustment,
};
use crate::{
    ImageFitPaint, blend_text_rect, draw_background_image, draw_image_with_fit, fill_style_rect,
    needs_synthetic_bold_paint, paint_box_shadow, rich_text_style_spans, stroke_style_border,
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
