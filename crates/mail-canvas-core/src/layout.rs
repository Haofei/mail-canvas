use anyhow::Result;
use cosmic_text::{Align as TextAlignMode, Buffer, FontSystem, Metrics, Shaping, Wrap};
use kuchiki::NodeRef;
use taffy::geometry::{Rect as TaffyRect, Size as TaffySize};
use taffy::prelude::{
    AlignItems as TaffyAlignItems, AvailableSpace, Dimension as TaffyDimension,
    Display as TaffyDisplay, FlexDirection as TaffyFlexDirection, FlexWrap as TaffyFlexWrap,
    JustifyContent as TaffyJustifyContent, NodeId as TaffyNodeId, Style as TaffyStyle, TaffyTree,
};
use taffy::style_helpers::{auto as taffy_auto, length as taffy_length, percent as taffy_percent};

use crate::api::{self, RenderRequest, RenderWarning, RenderWarningCode};
use crate::dom::{attr, element_tag, find_first_tag, is_metadata_tag};
use crate::fonts::{FontFamilyIndex, WebFontFace};
use crate::resource::ResourceProvider;
use crate::style::{
    AlignItems, BorderCollapse, BoxSizing, Clear, Display, FlexDirection, FlexWrap, FloatSide,
    JustifyContent, Length, ListStyleType, PlacedFloat, Position, Rect, Style, TextAlign, TextSpan,
    TextWrap, VerticalAlign, parse_length, style_for_node, style_for_node_with_fonts,
};
use crate::table::{
    TableGrid, build_table_grid, column_offset, distribute_fixed_table_column_widths,
    length_is_intrinsic_fixed, spanned_width,
};
use crate::text::{
    append_inline_spans, append_text_span, blink_font_descent_from_db, is_collapsible_whitespace,
    normalize_text_spans, resolved_line_height_from_db, resolved_line_height_from_run_db,
    rich_text_style_spans, spans_text, text_content, text_spans_are_only_collapsible_whitespace,
    text_spans_match_style, wrap_width_adjustment,
};
use crate::{HARD_BREAK_STR, ImageData};

type CssTableCell = (NodeRef, Style);
type CssTableRow = (NodeRef, Style, Vec<CssTableCell>);

pub(crate) struct RenderLimits {
    max_layout_depth: usize,
    max_table_cells: usize,
}

impl RenderLimits {
    pub(crate) fn from_request(request: &RenderRequest) -> Self {
        Self {
            max_layout_depth: request.max_layout_depth,
            max_table_cells: request.max_table_cells,
        }
    }
}

#[cfg(test)]
impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_layout_depth: api::DEFAULT_MAX_LAYOUT_DEPTH,
            max_table_cells: api::DEFAULT_MAX_TABLE_CELLS,
        }
    }
}

pub(crate) struct LayoutEngine<'a, R: ResourceProvider> {
    font_system: &'a mut FontSystem,
    resources: R,
    limits: RenderLimits,
    available_font_families: FontFamilyIndex,
    web_font_faces: Vec<WebFontFace>,
    collect_debug_meta: bool,
    pub(crate) warnings: Vec<RenderWarning>,
}

impl<'a, R: ResourceProvider> LayoutEngine<'a, R> {
    pub(crate) fn new(
        font_system: &'a mut FontSystem,
        resources: R,
        available_font_families: FontFamilyIndex,
        web_font_faces: Vec<WebFontFace>,
        limits: RenderLimits,
        collect_debug_meta: bool,
    ) -> Self {
        Self {
            font_system,
            resources,
            limits,
            available_font_families,
            web_font_faces,
            collect_debug_meta,
            warnings: Vec::new(),
        }
    }

    fn style_for_node(&self, node: &NodeRef, parent: &Style) -> Style {
        let mut style = style_for_node_with_fonts(
            node,
            parent,
            &self.available_font_families,
            &self.web_font_faces,
        );
        self.load_style_background(&mut style);
        style
    }

    fn load_style_background(&self, style: &mut Style) {
        let Some(src) = style.background_image_src.as_deref() else {
            return;
        };
        if src.is_empty() {
            return;
        }
        if let Ok(image) = self.resources.load_image(src, "background-image") {
            style.background_image = Some(image);
        }
    }

    fn debug_for_node(&self, node: &NodeRef, fallback_tag: &str) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_node(node, fallback_tag)
        } else {
            LayoutDebugMeta::default()
        }
    }

    fn debug_for_tag(&self, tag: &str) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_tag(tag)
        } else {
            LayoutDebugMeta::default()
        }
    }

    fn debug_for_text(&self, text: &str) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_text(text)
        } else {
            LayoutDebugMeta::default()
        }
    }

    fn debug_for_text_spans(&self, spans: &[TextSpan]) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_text(&spans_text(spans))
        } else {
            LayoutDebugMeta::default()
        }
    }

    fn debug_for_marker(&self) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_marker()
        } else {
            LayoutDebugMeta::default()
        }
    }

    fn debug_for_image_node(&self, node: &NodeRef) -> LayoutDebugMeta {
        if self.collect_debug_meta {
            LayoutDebugMeta::for_image_node(node)
        } else {
            LayoutDebugMeta::default()
        }
    }

    pub(crate) fn layout_document(&mut self, document: &NodeRef, width: u32) -> Result<LayoutBox> {
        let root_node = find_first_tag(document, "body").unwrap_or_else(|| document.clone());
        let initial = Style::initial();
        let parent_style = if element_tag(&root_node).as_deref() == Some("body") {
            find_first_tag(document, "html")
                .map(|html| self.style_for_node(&html, &initial))
                .unwrap_or_else(|| initial.clone())
        } else {
            initial.clone()
        };
        let root_style = if root_node.as_element().is_some() {
            self.style_for_node(&root_node, &parent_style)
        } else {
            initial
        };

        let viewport_width = width as f32;
        let layout_width = root_style
            .resolve_width(viewport_width)
            .unwrap_or(viewport_width)
            .max(1.0);
        let content =
            self.layout_children(&root_node, &root_style, 0.0, 0.0, layout_width, 0, &[])?;

        Ok(LayoutBox {
            kind: LayoutKind::Block,
            rect: Rect::new(0.0, 0.0, layout_width, content.advance.max(1.0)),
            style: root_style,
            debug: self.debug_for_node(&root_node, "body"),
            children: content.children,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_children(
        &mut self,
        node: &NodeRef,
        style: &Style,
        x: f32,
        y: f32,
        width: f32,
        depth: usize,
        inherited_floats: &[PlacedFloat],
    ) -> Result<LayoutChildren> {
        if depth > self.limits.max_layout_depth {
            self.push_warning(RenderWarning::new(
                RenderWarningCode::LayoutLimitReached,
                "maximum layout depth reached; truncated nested content",
            ));
            return Ok(LayoutChildren::default());
        }

        let mut children = Vec::new();
        let mut cursor_y = y;
        let mut text = Vec::new();
        let parent_tag = element_tag(node);
        let mut ordered_list_index = 1usize;
        let mut previous_margin_bottom = None;
        let mut inline_row = Vec::new();
        let mut inline_row_width = 0.0;
        let mut inline_row_height = 0.0;
        let mut last_inline_block_fallback = false;
        let parent_line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let inherited_float_count = inherited_floats.len();
        let mut floats = inherited_floats.to_vec();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                let text_value = text_node.borrow();
                if text_value.chars().any(|ch| !is_collapsible_whitespace(ch)) {
                    last_inline_block_fallback = false;
                }
                if !inline_row.is_empty() && text_value.chars().all(is_collapsible_whitespace) {
                    continue;
                }
                append_text_span(&mut text, &text_value, style);
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };

            if is_metadata_tag(&tag) {
                continue;
            }
            if tag == "br" {
                if last_inline_block_fallback
                    && inline_row.is_empty()
                    && text_spans_are_only_collapsible_whitespace(&text)
                {
                    text.clear();
                    last_inline_block_fallback = false;
                    continue;
                }
                last_inline_block_fallback = false;
                let row_was_flushed = flush_inline_row(
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    style,
                    width,
                    &mut cursor_y,
                    &mut children,
                );
                if row_was_flushed {
                    previous_margin_bottom = None;
                }
                if row_was_flushed && text.is_empty() {
                    continue;
                }
                append_text_span(&mut text, HARD_BREAK_STR, style);
                continue;
            }

            let mut child_style = self.style_for_node(&child, style);
            if parent_tag.as_deref() == Some("li") && tag == "p" {
                child_style.margin.top = 0.0;
            }
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }

            let child_is_inline_block_fallback = child_style.display == Display::Inline
                && tag != "img"
                && !inline_style_has_own_box(&child_style)
                && !inline_can_flatten(&child, &child_style);
            if child_is_inline_block_fallback
                && inline_needs_inline_block_container(&child, &child_style)
            {
                child_style.display = Display::InlineBlock;
            }

            if child_style.display == Display::Inline
                && tag != "img"
                && !inline_style_has_own_box(&child_style)
                && !child_is_inline_block_fallback
            {
                last_inline_block_fallback = false;
                append_inline_spans(&child, &child_style, &mut text);
                continue;
            }

            let child_display = child_style.display;
            let child_float_side = child_style.float_side;
            let child_clear = child_style.clear;
            let child_is_inline_flow = is_inline_flow(&tag, &child_style);
            let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
            if child_is_inline_flow || !inline_row.is_empty() {
                if self.push_text_inline_row(
                    &mut text,
                    style,
                    text_x,
                    text_width,
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    &mut cursor_y,
                    &mut children,
                )? {
                    previous_margin_bottom = None;
                }
            } else if self.flush_text(
                &mut text,
                style,
                text_x,
                &mut cursor_y,
                text_width,
                &mut children,
            )? {
                previous_margin_bottom = None;
            }
            if child_clear != Clear::None {
                cursor_y = cursor_y.max(clear_float_y(&floats, child_clear));
                previous_margin_bottom = None;
            }
            if !child_is_inline_flow
                && flush_inline_row(
                    &mut inline_row,
                    &mut inline_row_width,
                    &mut inline_row_height,
                    style,
                    width,
                    &mut cursor_y,
                    &mut children,
                )
            {
                previous_margin_bottom = None;
            }
            let child_should_avoid_floats = child_style.resolve_width(width).is_some()
                && !block_establishes_float_container(&child_style);
            if child_float_side == FloatSide::None
                && !child_is_inline_flow
                && child_should_avoid_floats
            {
                let placed_y = block_flow_placement_y(&child_style, x, width, cursor_y, &floats);
                if placed_y > cursor_y {
                    cursor_y = placed_y;
                    previous_margin_bottom = None;
                }
            }
            let list_marker = if tag == "li" {
                match child_style.list_style_type {
                    ListStyleType::None => None,
                    ListStyleType::Decimal => {
                        let marker = format!("{ordered_list_index}.");
                        ordered_list_index += 1;
                        Some(marker)
                    }
                    ListStyleType::Disc => match parent_tag.as_deref() {
                        Some("ol") => {
                            let marker = format!("{ordered_list_index}.");
                            ordered_list_index += 1;
                            Some(marker)
                        }
                        Some("ul") => Some("\u{2022}".to_string()),
                        _ => None,
                    },
                }
            } else {
                None
            };
            let flow_start_y = cursor_y;
            let child_inherited_floats = if child_float_side == FloatSide::None
                && !child_is_inline_flow
                && !block_establishes_float_container(&child_style)
            {
                floats.as_slice()
            } else {
                &[]
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
                self.layout_element_with_style_and_floats(
                    &child,
                    child_style,
                    x,
                    cursor_y,
                    width,
                    depth + 1,
                    child_inherited_floats,
                )?
            };
            if let Some(flow) = flow {
                let mut flow = flow;
                let collapsible_margin_bottom = flow.collapsible_margin_bottom;
                if child_float_side != FloatSide::None {
                    let occupied_width =
                        (flow.node.rect.width + flow.node.style.margin.horizontal()).max(1.0);
                    let occupied_height = flow.advance.max(flow.node.rect.height).max(1.0);
                    let float_y = float_placement_y(&floats, x, width, cursor_y, occupied_width);
                    let (left_offset, right_offset) =
                        float_offsets_at_y(x, width, float_y, &floats);
                    let occupied_x = match child_float_side {
                        FloatSide::Left => x + left_offset,
                        FloatSide::Right => x + width - right_offset - occupied_width,
                        FloatSide::None => x,
                    };
                    let target_x = occupied_x + flow.node.style.margin.left;
                    let target_y = float_y + flow.node.style.margin.top;
                    let dx = target_x - flow.node.rect.x;
                    let dy = target_y - flow.node.rect.y;
                    translate_layout(&mut flow.node, dx, dy);
                    translate_placed_floats(&mut flow.escaped_floats, dx, dy);
                    floats.push(PlacedFloat {
                        side: child_float_side,
                        rect: Rect::new(occupied_x, float_y, occupied_width, occupied_height),
                    });
                    previous_margin_bottom = None;
                    last_inline_block_fallback = child_is_inline_block_fallback;
                    children.push(flow.node);
                    continue;
                }
                if child_is_inline_flow {
                    let (mut line_x, line_width) = float_adjusted_line(x, width, cursor_y, &floats);
                    let replaced_padding = if matches!(flow.node.kind, LayoutKind::Image(_)) {
                        flow.node.style.padding.horizontal() + flow.node.style.border.horizontal()
                    } else {
                        0.0
                    };
                    let inline_flow_width = (flow.node.rect.width
                        + replaced_padding
                        + flow.node.style.margin.horizontal())
                    .max(1.0);
                    if inline_row_width > 0.0
                        && inline_row_width + inline_flow_width > line_width + f32::EPSILON
                    {
                        flush_inline_row(
                            &mut inline_row,
                            &mut inline_row_width,
                            &mut inline_row_height,
                            style,
                            line_width,
                            &mut cursor_y,
                            &mut children,
                        );
                        let (next_line_x, _) = float_adjusted_line(x, width, cursor_y, &floats);
                        line_x = next_line_x;
                    }
                    if (cursor_y - flow_start_y).abs() > f32::EPSILON {
                        let dy = cursor_y - flow_start_y;
                        translate_layout(&mut flow.node, 0.0, dy);
                        translate_placed_floats(&mut flow.escaped_floats, 0.0, dy);
                    }
                    let inline_dx = line_x - x + inline_row_width;
                    if inline_dx.abs() > f32::EPSILON {
                        translate_layout(&mut flow.node, inline_dx, 0.0);
                        translate_placed_floats(&mut flow.escaped_floats, inline_dx, 0.0);
                    }
                    inline_row_width += inline_flow_width;
                    let baseline_descent = if flow.node.style.vertical_align
                        == VerticalAlign::Baseline
                        && inline_flow_uses_bottom_edge_baseline(&flow.node)
                    {
                        blink_font_descent_from_db(self.font_system.db(), style)
                            .unwrap_or(style.font_size * 0.25)
                    } else {
                        0.0
                    };
                    let line_advance = inline_flow_line_advance(
                        &flow.node,
                        flow.advance,
                        parent_line_height,
                        self.font_system.db(),
                    );
                    inline_row_height = inline_row_height.max(line_advance + baseline_descent);
                    inline_row.push(flow.node);
                    previous_margin_bottom = None;
                    last_inline_block_fallback = false;
                    continue;
                }
                let before_align_x = flow.node.rect.x;
                let before_align_y = flow.node.rect.y;
                align_table_child_to_parent_text(&mut flow.node, style, x, width);
                align_block_child_to_legacy_align_attribute(&mut flow.node, style, x, width);
                let aligned_dx = flow.node.rect.x - before_align_x;
                let aligned_dy = flow.node.rect.y - before_align_y;
                if aligned_dx.abs() > f32::EPSILON || aligned_dy.abs() > f32::EPSILON {
                    translate_placed_floats(&mut flow.escaped_floats, aligned_dx, aligned_dy);
                }
                let margin_overlap = if can_collapse_sibling_margin(child_display) {
                    previous_margin_bottom
                        .map(|previous: f32| previous.min(flow.node.style.margin.top))
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                if margin_overlap > 0.0 {
                    translate_layout(&mut flow.node, 0.0, -margin_overlap);
                    translate_placed_floats(&mut flow.escaped_floats, 0.0, -margin_overlap);
                }
                floats.extend(flow.escaped_floats);
                cursor_y += flow.advance - margin_overlap;
                previous_margin_bottom =
                    can_collapse_sibling_margin(child_display).then_some(collapsible_margin_bottom);
                last_inline_block_fallback = child_is_inline_block_fallback;
                children.push(flow.node);
            }
        }

        let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
        if !inline_row.is_empty()
            && self.push_text_inline_row(
                &mut text,
                style,
                text_x,
                text_width,
                &mut inline_row,
                &mut inline_row_width,
                &mut inline_row_height,
                &mut cursor_y,
                &mut children,
            )?
        {
            previous_margin_bottom = None;
        }
        if self.flush_text(
            &mut text,
            style,
            text_x,
            &mut cursor_y,
            text_width,
            &mut children,
        )? {
            previous_margin_bottom = None;
        }
        if flush_inline_row(
            &mut inline_row,
            &mut inline_row_width,
            &mut inline_row_height,
            style,
            width,
            &mut cursor_y,
            &mut children,
        ) {
            previous_margin_bottom = None;
        }
        let local_float_bottom = floats[inherited_float_count..]
            .iter()
            .map(|float| float.rect.y + float.rect.height)
            .fold(cursor_y, f32::max);
        Ok(LayoutChildren {
            children,
            advance: local_float_bottom - y,
            in_flow_advance: cursor_y - y,
            floats: floats.into_iter().skip(inherited_float_count).collect(),
            trailing_collapsible_margin: previous_margin_bottom.unwrap_or(0.0),
        })
    }

    fn flush_text(
        &mut self,
        text: &mut Vec<TextSpan>,
        style: &Style,
        x: f32,
        cursor_y: &mut f32,
        width: f32,
        children: &mut Vec<LayoutBox>,
    ) -> Result<bool> {
        let normalized = normalize_text_spans(text);
        text.clear();

        if normalized.is_empty() {
            return Ok(false);
        }

        let matches_parent_style = text_spans_match_style(&normalized, style);
        let (height, kind) = if matches_parent_style {
            let plain_text = spans_text(&normalized);
            (
                self.measure_text_height(&plain_text, width, style)?,
                LayoutKind::Text(plain_text),
            )
        } else {
            (
                self.measure_rich_text_height(&normalized, width, style)?,
                LayoutKind::RichText(normalized),
            )
        };
        let debug = match &kind {
            LayoutKind::Text(text) => self.debug_for_text(text),
            LayoutKind::RichText(spans) => self.debug_for_text_spans(spans),
            _ => LayoutDebugMeta::default(),
        };
        children.push(LayoutBox {
            kind,
            rect: Rect::new(x, *cursor_y, width, height),
            style: style.clone(),
            debug,
            children: Vec::new(),
        });
        *cursor_y += height;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_text_inline_row(
        &mut self,
        text: &mut Vec<TextSpan>,
        style: &Style,
        x: f32,
        width: f32,
        inline_row: &mut Vec<LayoutBox>,
        inline_row_width: &mut f32,
        inline_row_height: &mut f32,
        cursor_y: &mut f32,
        children: &mut Vec<LayoutBox>,
    ) -> Result<bool> {
        let normalized = normalize_text_spans(text);
        text.clear();

        if normalized.is_empty() {
            return Ok(false);
        }

        let matches_parent_style = text_spans_match_style(&normalized, style);
        let (text_width, height, kind) = if matches_parent_style {
            let plain_text = spans_text(&normalized);
            (
                self.measure_text_width(&plain_text, style)
                    .min(width.max(1.0)),
                resolved_line_height_from_db(self.font_system.db(), style),
                LayoutKind::Text(plain_text),
            )
        } else {
            (
                self.measure_rich_text_width(&normalized, style)
                    .min(width.max(1.0)),
                normalized
                    .iter()
                    .map(|span| {
                        resolved_line_height_from_run_db(self.font_system.db(), &span.style)
                    })
                    .fold(
                        resolved_line_height_from_db(self.font_system.db(), style),
                        f32::max,
                    ),
                LayoutKind::RichText(normalized),
            )
        };
        if *inline_row_width > 0.0 && *inline_row_width + text_width > width + f32::EPSILON {
            flush_inline_row(
                inline_row,
                inline_row_width,
                inline_row_height,
                style,
                width,
                cursor_y,
                children,
            );
        }

        let debug = match &kind {
            LayoutKind::Text(text) => self.debug_for_text(text),
            LayoutKind::RichText(spans) => self.debug_for_text_spans(spans),
            _ => LayoutDebugMeta::default(),
        };

        inline_row.push(LayoutBox {
            kind,
            rect: Rect::new(x + *inline_row_width, *cursor_y, text_width, height),
            style: style.clone(),
            debug,
            children: Vec::new(),
        });
        *inline_row_width += text_width;
        *inline_row_height = inline_row_height.max(height);
        Ok(true)
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
        self.layout_element_with_style_and_floats(node, style, x, y, containing_width, depth, &[])
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_element_with_style_and_floats(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
        inherited_floats: &[PlacedFloat],
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
        if tag == "table" && matches!(style.display, Display::InlineBlock) {
            return self.layout_table(node, style, x, y, containing_width, depth);
        }

        match style.display {
            Display::None => Ok(None),
            Display::Flex => self.layout_flex(node, style, x, y, containing_width, depth),
            Display::InlineTable => self.layout_table(node, style, x, y, containing_width, depth),
            Display::Table => self.layout_table(node, style, x, y, containing_width, depth),
            Display::Inline => {
                if inline_style_has_own_box(&style) {
                    self.layout_inline_block(node, style, x, y, containing_width, depth)
                } else {
                    let mut inline_style = style;
                    inline_style.width = None;
                    inline_style.min_width = None;
                    inline_style.max_width = None;
                    self.layout_block(
                        node,
                        inline_style,
                        x,
                        y,
                        containing_width,
                        depth,
                        inherited_floats,
                    )
                }
            }
            Display::InlineBlock => {
                self.layout_inline_block(node, style, x, y, containing_width, depth)
            }
            _ => self.layout_block(node, style, x, y, containing_width, depth, inherited_floats),
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
            .min(style.constrain_outer_width(f32::MAX, containing_width))
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

        let rect_width = if explicit_width.is_some() {
            max_outer_width
        } else {
            style
                .constrain_outer_width(
                    preferred_inner_width + style.padding.horizontal() + style.border.horizontal(),
                    containing_width,
                )
                .max(1.0)
        };
        let inner_width = style.inner_width_for_outer(rect_width);
        let rect_x = x + style.horizontal_offset(containing_width, rect_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let mut content =
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth, &[])?;
        let rect_height = style
            .constrain_outer_height(
                content.advance + style.padding.vertical() + style.border.vertical(),
                0.0,
            )
            .max(0.0);
        if style.overflow_hidden && rect_height <= 0.5 {
            content.children.clear();
        }
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, rect_width, rect_height),
            &mut content.children,
            depth,
        )?;

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, rect_width, rect_height),
                style,
                debug: self.debug_for_node(node, "div"),
                children: content.children,
            },
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_block(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
        inherited_floats: &[PlacedFloat],
    ) -> Result<Option<FlowBox>> {
        if let Some(flow) =
            self.layout_css_table_cells(node, style.clone(), x, y, containing_width, depth)?
        {
            return Ok(Some(flow));
        }

        let outer_width = style
            .resolve_width(containing_width)
            .map(|width| style.outer_width_for_declared(width))
            .unwrap_or(containing_width - style.margin.horizontal());
        let outer_width = style
            .constrain_outer_width(outer_width, containing_width)
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, outer_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let inner_width = style.inner_width_for_outer(outer_width);

        let mut content = self.layout_children(
            node,
            &style,
            inner_x,
            inner_y,
            inner_width,
            depth,
            inherited_floats,
        )?;
        let collapsed_trailing_margin = if block_allows_trailing_margin_collapse(&style) {
            content.trailing_collapsible_margin.min(content.advance)
        } else {
            0.0
        };
        let contains_descendant_floats = block_establishes_float_container(&style);
        let height_advance = if contains_descendant_floats {
            content.advance
        } else {
            content.in_flow_advance
        };
        let content_box_height = (height_advance - collapsed_trailing_margin).max(0.0);
        let rect_height = style
            .constrain_outer_height(
                content_box_height + style.padding.vertical() + style.border.vertical(),
                0.0,
            )
            .max(0.0);
        if style.overflow_hidden && rect_height <= 0.5 {
            content.children.clear();
        }
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, outer_width, rect_height),
            &mut content.children,
            depth,
        )?;

        let collapsed_bottom_margin = style.margin.bottom.max(collapsed_trailing_margin);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsed_bottom_margin,
            collapsible_margin_bottom: collapsed_bottom_margin,
            escaped_floats: if contains_descendant_floats {
                Vec::new()
            } else {
                content.floats
            },
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: self.debug_for_node(node, "div"),
                children: content.children,
            },
        }))
    }

    fn layout_flex(
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
            .unwrap_or(containing_width - style.margin.horizontal());
        let outer_width = style
            .constrain_outer_width(outer_width, containing_width)
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, outer_width);
        let rect_y = y + style.margin.top;
        let inner_x = rect_x + style.border.left + style.padding.left;
        let inner_y = rect_y + style.border.top + style.padding.top;
        let inner_width = style.inner_width_for_outer(outer_width);
        let explicit_height = style.resolve_height(0.0);
        let explicit_inner_height = explicit_height
            .map(|height| (height - style.padding.vertical() - style.border.vertical()).max(0.0));

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        taffy.disable_rounding();
        let mut child_nodes: Vec<TaffyNodeId> = Vec::new();
        let mut flex_items: Vec<(TaffyNodeId, LayoutBox)> = Vec::new();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                let text_value = text_node.borrow();
                if text_value.chars().all(is_collapsible_whitespace) {
                    continue;
                }
                let normalized =
                    normalize_text_spans(&[TextSpan::from_style(text_value.to_string(), &style)]);
                let plain_text = spans_text(&normalized);
                let matches_parent_style = text_spans_match_style(&normalized, &style);
                let item_width = if matches_parent_style {
                    self.measure_text_width(&plain_text, &style)
                } else {
                    self.measure_rich_text_width(&normalized, &style)
                }
                .max(1.0);
                let item_height = if matches_parent_style {
                    self.measure_text_height(&plain_text, item_width, &style)?
                } else {
                    self.measure_rich_text_height(&normalized, item_width, &style)?
                };
                let kind = if matches_parent_style {
                    LayoutKind::Text(plain_text)
                } else {
                    LayoutKind::RichText(normalized)
                };
                let debug = match &kind {
                    LayoutKind::Text(text) => self.debug_for_text(text),
                    LayoutKind::RichText(spans) => self.debug_for_text_spans(spans),
                    _ => LayoutDebugMeta::default(),
                };
                let item = LayoutBox {
                    kind,
                    rect: Rect::new(0.0, 0.0, item_width, item_height),
                    style: style.clone(),
                    debug,
                    children: Vec::new(),
                };
                let node_id =
                    taffy.new_leaf(taffy_leaf_style(&item.style, item_width, item_height))?;
                child_nodes.push(node_id);
                flex_items.push((node_id, item));
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }

            let child_style = self.style_for_node(&child, &style);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }
            let Some(flow) = self.layout_element_with_style(
                &child,
                child_style,
                0.0,
                0.0,
                inner_width,
                depth + 1,
            )?
            else {
                continue;
            };

            let mut item = flow.node;
            let item_width = item.rect.width.max(1.0);
            let item_height = item.rect.height.max(1.0);
            let item_x = item.rect.x;
            let item_y = item.rect.y;
            translate_layout(&mut item, -item_x, -item_y);
            let node_id = taffy.new_leaf(taffy_leaf_style(&item.style, item_width, item_height))?;
            child_nodes.push(node_id);
            flex_items.push((node_id, item));
        }

        let root = taffy.new_with_children(
            taffy_flex_container_style(&style, inner_width, explicit_inner_height),
            &child_nodes,
        )?;
        taffy.compute_layout(
            root,
            TaffySize {
                width: AvailableSpace::Definite(inner_width),
                height: AvailableSpace::MaxContent,
            },
        )?;
        let root_layout = *taffy.layout(root)?;
        let mut children = Vec::with_capacity(flex_items.len());
        for (node_id, mut item) in flex_items {
            let layout = *taffy.layout(node_id)?;
            translate_layout(
                &mut item,
                inner_x + layout.location.x,
                inner_y + layout.location.y,
            );
            children.push(item);
        }

        let min_height = explicit_height.unwrap_or(0.0);
        let rect_height =
            (root_layout.size.height + style.padding.vertical() + style.border.vertical())
                .max(min_height)
                .max(0.0);
        self.append_absolute_children(
            node,
            &style,
            Rect::new(rect_x, rect_y, outer_width, rect_height),
            &mut children,
            depth,
        )?;

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: self.debug_for_node(node, "div"),
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
        let grid = build_table_grid(node, self.limits.max_table_cells)?;
        if grid.rows.is_empty() {
            if let Some(flow) =
                self.layout_css_table_rows(node, style.clone(), x, y, containing_width, depth)?
            {
                return Ok(Some(flow));
            }
            if let Some(flow) =
                self.layout_css_table_cells(node, style.clone(), x, y, containing_width, depth)?
            {
                return Ok(Some(flow));
            }
            return self.layout_block(node, style, x, y, containing_width, depth, &[]);
        }

        let max_table_width = (containing_width - style.margin.horizontal()).max(1.0);
        let spacing = if style.border_collapse == BorderCollapse::Collapse {
            0.0
        } else {
            style.cell_spacing.max(0.0)
        };
        let table_width = if let Some(width) = style.resolve_width(containing_width) {
            let declared = table_outer_width_for_declared(&style, width);
            if !can_expand_declared_table_width(&style) {
                declared
            } else {
                declared.max(
                    self.fixed_replaced_table_min_outer_width(&grid, &style, declared, spacing)?,
                )
            }
        } else {
            self.preferred_auto_table_outer_width(&grid, &style, max_table_width, spacing)?
        };
        let table_width = style
            .constrain_outer_width(table_width, containing_width)
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, table_width);
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border.left + style.padding.left;
        let content_y = rect_y + style.border.top + style.padding.top;
        let content_width = style.inner_width_for_outer(table_width);

        let mut row_boxes = Vec::new();
        let mut row_y = content_y;
        let column_widths =
            self.resolve_table_column_widths(&grid, &style, content_width, spacing)?;

        for row in grid.rows {
            let row_style = self.style_for_node(&row.node, &style);
            if row_style.display == Display::None {
                continue;
            }
            if row.cells.is_empty() {
                continue;
            }

            let mut styled_cells = Vec::with_capacity(row.cells.len());
            for cell in row.cells {
                let cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
                styled_cells.push((cell, cell_style));
            }
            if styled_cells.is_empty() {
                continue;
            }

            if styled_cells
                .iter()
                .all(|(_, cell_style)| cell_style.display != Display::TableCell)
            {
                let row_is_inline_flow = styled_cells.iter().all(|(cell, cell_style)| {
                    element_tag(&cell.node)
                        .as_deref()
                        .is_some_and(|tag| is_inline_flow(tag, cell_style))
                });
                let mut row_children = Vec::with_capacity(styled_cells.len());
                let mut row_height = 0.0_f32;

                if row_is_inline_flow {
                    let mut row_width = 0.0_f32;
                    for (cell, cell_style) in styled_cells {
                        let Some(flow) = self.layout_element_with_style(
                            &cell.node,
                            cell_style,
                            0.0,
                            0.0,
                            content_width,
                            depth + 1,
                        )?
                        else {
                            continue;
                        };
                        let mut child = flow.node;
                        let child_outer_width = child.style.margin.horizontal() + child.rect.width;
                        let child_target_x = content_x
                            + row_width
                            + if child.style.margin_left_auto {
                                0.0
                            } else {
                                child.style.margin.left
                            };
                        let child_target_y = row_y
                            + if child.style.margin.top > 0.0 {
                                child.style.margin.top
                            } else {
                                0.0
                            };
                        let dx = child_target_x - child.rect.x;
                        let dy = child_target_y - child.rect.y;
                        translate_layout(&mut child, dx, dy);
                        row_width += child_outer_width;
                        row_height = row_height.max(flow.advance.max(child.rect.height));
                        row_children.push(child);
                    }
                    let free = (content_width - row_width).max(0.0);
                    let dx = match row_style.text_align {
                        TextAlign::Left => 0.0,
                        TextAlign::Center => free / 2.0,
                        TextAlign::Right => free,
                    };
                    if dx > 0.0 {
                        for child in &mut row_children {
                            translate_layout(child, dx, 0.0);
                        }
                    }
                    row_height = row_height.max(1.0);
                } else {
                    let mut block_y = row_y;
                    for (cell, cell_style) in styled_cells {
                        let Some(flow) = self.layout_element_with_style(
                            &cell.node,
                            cell_style,
                            content_x,
                            block_y,
                            content_width,
                            depth + 1,
                        )?
                        else {
                            continue;
                        };
                        block_y += flow.advance;
                        row_children.push(flow.node);
                    }
                    row_height = (block_y - row_y).max(1.0);
                }

                if row_children.is_empty() {
                    continue;
                }
                row_boxes.push(LayoutBox {
                    kind: LayoutKind::Row,
                    rect: Rect::new(content_x, row_y, content_width, row_height),
                    style: row_style,
                    debug: self.debug_for_node(&row.node, "tr"),
                    children: row_children,
                });
                row_y += row_height + spacing;
                continue;
            }

            let mut cell_boxes = Vec::with_capacity(styled_cells.len());
            let mut row_height: f32 = 0.0;

            for (cell, mut cell_style) in styled_cells {
                cell_style.apply_table_cell_padding(style.cell_padding);

                let cell_x = content_x + column_offset(&column_widths, cell.col, spacing);
                let cell_width = spanned_width(&column_widths, cell.col, cell.colspan, spacing);
                let cell_padding = cell_style.resolved_padding(cell_width);
                let cell_inner_x = cell_x + cell_style.border.left + cell_padding.left;
                let cell_inner_y = row_y + cell_style.border.top + cell_padding.top;
                let cell_inner_width =
                    (cell_width - cell_padding.horizontal() - cell_style.border.horizontal())
                        .max(1.0);
                let content = self.layout_children(
                    &cell.node,
                    &cell_style,
                    cell_inner_x,
                    cell_inner_y,
                    cell_inner_width,
                    depth + 1,
                    &[],
                )?;
                let explicit_height = cell_style.resolve_height(0.0).unwrap_or(0.0);
                let natural_cell_height =
                    (content.advance + cell_padding.vertical() + cell_style.border.vertical())
                        .max(1.0);
                let cell_height = natural_cell_height.max(explicit_height).max(1.0);
                row_height = row_height.max(cell_height);
                cell_boxes.push((
                    cell.node.clone(),
                    LayoutBox {
                        kind: LayoutKind::Cell,
                        rect: Rect::new(cell_x, row_y, cell_width, cell_height),
                        style: cell_style,
                        debug: self.debug_for_node(&cell.node, "td"),
                        children: content.children,
                    },
                    natural_cell_height,
                ));
            }

            if cell_boxes.is_empty() {
                continue;
            }

            for (cell_node, cell, natural_cell_height) in &mut cell_boxes {
                let delta = (row_height - *natural_cell_height).max(0.0);
                let offset_y = match cell.style.vertical_align {
                    VerticalAlign::Baseline | VerticalAlign::Top => 0.0,
                    VerticalAlign::Middle => delta / 2.0,
                    VerticalAlign::Bottom => delta,
                };
                if offset_y > 0.0 {
                    translate_layout_children(cell, 0.0, offset_y);
                }
                cell.rect.height = row_height;
                self.append_absolute_children(
                    cell_node,
                    &cell.style,
                    cell.rect,
                    &mut cell.children,
                    depth + 1,
                )?;
            }

            row_boxes.push(LayoutBox {
                kind: LayoutKind::Row,
                rect: Rect::new(content_x, row_y, content_width, row_height),
                style: row_style,
                debug: self.debug_for_node(&row.node, "tr"),
                children: cell_boxes.into_iter().map(|(_, cell, _)| cell).collect(),
            });
            row_y += row_height + spacing;
        }

        let content_height = (row_y - content_y - spacing).max(0.0);
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let table_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);

        let collapsible_margin_bottom = style.margin.bottom;
        Ok(Some(FlowBox {
            advance: style.margin.top + table_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                debug: self.debug_for_node(node, "table"),
                children: row_boxes,
            },
        }))
    }

    fn layout_css_table_rows(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let mut rows = Vec::new();
        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                if text_node
                    .borrow()
                    .chars()
                    .any(|ch| !is_collapsible_whitespace(ch))
                {
                    return Ok(None);
                }
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            let row_style = self.style_for_node(&child, &style);
            if row_style.display == Display::None {
                continue;
            }
            if matches!(row_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }
            if row_style.display != Display::TableRow {
                return Ok(None);
            }
            let Some(cells) = self.css_table_row_cells(&child, &row_style) else {
                return Ok(None);
            };
            if !cells.is_empty() {
                rows.push((child, row_style, cells));
            }
        }

        if rows.is_empty() {
            return Ok(None);
        }

        let max_table_width = (containing_width - style.margin.horizontal()).max(1.0);
        let table_width = style
            .resolve_width(containing_width)
            .map(|width| table_outer_width_for_declared(&style, width))
            .unwrap_or(max_table_width);
        let table_width = style
            .constrain_outer_width(table_width, containing_width)
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, table_width);
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border.left + style.padding.left;
        let content_y = rect_y + style.border.top + style.padding.top;
        let content_width = style.inner_width_for_outer(table_width).max(1.0);
        let column_widths = css_table_row_column_widths(&rows, content_width);

        let mut row_boxes = Vec::with_capacity(rows.len());
        let mut row_y = content_y;
        for (row_node, row_style, cells) in rows {
            let mut cell_boxes = Vec::with_capacity(cells.len());
            let mut row_height: f32 = 0.0;
            let mut cursor_x = content_x;

            for (idx, (cell_node, cell_style)) in cells.iter().enumerate() {
                let cell_width = column_widths.get(idx).copied().unwrap_or(1.0).max(1.0);
                let cell_padding = cell_style.resolved_padding(cell_width);
                let cell_inner_x = cursor_x + cell_style.border.left + cell_padding.left;
                let cell_inner_y = row_y + cell_style.border.top + cell_padding.top;
                let cell_inner_width =
                    (cell_width - cell_padding.horizontal() - cell_style.border.horizontal())
                        .max(1.0);
                let content = self.layout_children(
                    cell_node,
                    cell_style,
                    cell_inner_x,
                    cell_inner_y,
                    cell_inner_width,
                    depth + 1,
                    &[],
                )?;
                let explicit_height = cell_style.resolve_height(0.0).unwrap_or(0.0);
                let natural_cell_height =
                    (content.advance + cell_padding.vertical() + cell_style.border.vertical())
                        .max(1.0);
                let cell_height = natural_cell_height.max(explicit_height).max(1.0);
                row_height = row_height.max(cell_height);
                cell_boxes.push((
                    cell_node.clone(),
                    LayoutBox {
                        kind: LayoutKind::Cell,
                        rect: Rect::new(cursor_x, row_y, cell_width, cell_height),
                        style: cell_style.clone(),
                        debug: self.debug_for_node(cell_node, "td"),
                        children: content.children,
                    },
                    natural_cell_height,
                ));
                cursor_x += cell_width;
            }

            if cell_boxes.is_empty() {
                continue;
            }

            for (cell_node, cell, natural_cell_height) in &mut cell_boxes {
                let delta = (row_height - *natural_cell_height).max(0.0);
                let offset_y = match cell.style.vertical_align {
                    VerticalAlign::Baseline | VerticalAlign::Top => 0.0,
                    VerticalAlign::Middle => delta / 2.0,
                    VerticalAlign::Bottom => delta,
                };
                if offset_y > 0.0 {
                    translate_layout_children(cell, 0.0, offset_y);
                }
                cell.rect.height = row_height;
                self.append_absolute_children(
                    cell_node,
                    &cell.style,
                    cell.rect,
                    &mut cell.children,
                    depth + 1,
                )?;
            }

            row_boxes.push(LayoutBox {
                kind: LayoutKind::Row,
                rect: Rect::new(content_x, row_y, content_width, row_height),
                style: row_style,
                debug: self.debug_for_node(&row_node, "tr"),
                children: cell_boxes.into_iter().map(|(_, cell, _)| cell).collect(),
            });
            row_y += row_height;
        }

        if row_boxes.is_empty() {
            return Ok(None);
        }

        let content_height = (row_y - content_y).max(0.0);
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let table_height = (content_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);
        let collapsible_margin_bottom = style.margin.bottom;

        Ok(Some(FlowBox {
            advance: style.margin.top + table_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                debug: self.debug_for_node(node, "table"),
                children: row_boxes,
            },
        }))
    }

    fn css_table_row_cells(
        &mut self,
        row_node: &NodeRef,
        row_style: &Style,
    ) -> Option<Vec<CssTableCell>> {
        let mut cells = Vec::new();
        for child in row_node.children() {
            if let Some(text_node) = child.as_text() {
                if text_node
                    .borrow()
                    .chars()
                    .any(|ch| !is_collapsible_whitespace(ch))
                {
                    return None;
                }
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            let child_style = self.style_for_node(&child, row_style);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }
            if child_style.display != Display::TableCell {
                return None;
            }
            cells.push((child, child_style));
        }
        Some(cells)
    }

    fn layout_css_table_cells(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let mut cell_nodes = Vec::new();
        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                if text_node
                    .borrow()
                    .chars()
                    .any(|ch| !is_collapsible_whitespace(ch))
                {
                    return Ok(None);
                }
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            let child_style = self.style_for_node(&child, &style);
            if child_style.display == Display::None {
                continue;
            }
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                continue;
            }
            if child_style.display != Display::TableCell {
                return Ok(None);
            }
            cell_nodes.push((child, child_style));
        }

        if cell_nodes.is_empty() {
            return Ok(None);
        }

        let max_table_width = (containing_width - style.margin.horizontal()).max(1.0);
        let table_width = style
            .resolve_width(containing_width)
            .map(|width| table_outer_width_for_declared(&style, width))
            .unwrap_or(max_table_width);
        let table_width = style
            .constrain_outer_width(table_width, containing_width)
            .max(1.0);
        let rect_x = x + style.horizontal_offset(containing_width, table_width);
        let rect_y = y + style.margin.top;
        let content_x = rect_x + style.border.left + style.padding.left;
        let content_y = rect_y + style.border.top + style.padding.top;
        let content_width = style.inner_width_for_outer(table_width).max(1.0);

        let column_widths = css_table_cell_widths(&cell_nodes, content_width);
        let mut cell_boxes = Vec::with_capacity(cell_nodes.len());
        let mut row_height: f32 = 0.0;
        let mut cursor_x = content_x;

        for ((cell_node, cell_style), cell_width) in cell_nodes.iter().zip(column_widths.iter()) {
            let cell_width = (*cell_width).max(1.0);
            let cell_padding = cell_style.resolved_padding(cell_width);
            let cell_inner_x = cursor_x + cell_style.border.left + cell_padding.left;
            let cell_inner_y = content_y + cell_style.border.top + cell_padding.top;
            let cell_inner_width =
                (cell_width - cell_padding.horizontal() - cell_style.border.horizontal()).max(1.0);
            let content = self.layout_children(
                cell_node,
                cell_style,
                cell_inner_x,
                cell_inner_y,
                cell_inner_width,
                depth + 1,
                &[],
            )?;
            let explicit_height = cell_style.resolve_height(0.0).unwrap_or(0.0);
            let natural_cell_height =
                (content.advance + cell_padding.vertical() + cell_style.border.vertical()).max(1.0);
            let cell_height = natural_cell_height.max(explicit_height).max(1.0);
            row_height = row_height.max(cell_height);
            cell_boxes.push((
                cell_node.clone(),
                LayoutBox {
                    kind: LayoutKind::Cell,
                    rect: Rect::new(cursor_x, content_y, cell_width, cell_height),
                    style: cell_style.clone(),
                    debug: self.debug_for_node(cell_node, "td"),
                    children: content.children,
                },
                natural_cell_height,
            ));
            cursor_x += cell_width;
        }

        for (cell_node, cell, natural_cell_height) in &mut cell_boxes {
            let delta = (row_height - *natural_cell_height).max(0.0);
            let offset_y = match cell.style.vertical_align {
                VerticalAlign::Baseline | VerticalAlign::Top => 0.0,
                VerticalAlign::Middle => delta / 2.0,
                VerticalAlign::Bottom => delta,
            };
            if offset_y > 0.0 {
                translate_layout_children(cell, 0.0, offset_y);
            }
            cell.rect.height = row_height;
            self.append_absolute_children(
                cell_node,
                &cell.style,
                cell.rect,
                &mut cell.children,
                depth + 1,
            )?;
        }

        let row_box = LayoutBox {
            kind: LayoutKind::Row,
            rect: Rect::new(content_x, content_y, content_width, row_height),
            style: style.clone(),
            debug: self.debug_for_tag("tr"),
            children: cell_boxes.into_iter().map(|(_, cell, _)| cell).collect(),
        };
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let table_height = (row_height + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);
        let collapsible_margin_bottom = style.margin.bottom;

        Ok(Some(FlowBox {
            advance: style.margin.top + table_height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                debug: self.debug_for_node(node, "table"),
                children: vec![row_box],
            },
        }))
    }

    fn append_absolute_children(
        &mut self,
        node: &NodeRef,
        parent_style: &Style,
        containing_rect: Rect,
        children: &mut Vec<LayoutBox>,
        depth: usize,
    ) -> Result<()> {
        let mut absolute_children = Vec::new();
        for child in node.children() {
            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            let mut child_style = self.style_for_node(&child, parent_style);
            if child_style.display == Display::None
                || !matches!(child_style.position, Position::Absolute | Position::Fixed)
            {
                continue;
            }
            let Some(flow) =
                self.layout_absolute_child(&child, &mut child_style, containing_rect, depth + 1)?
            else {
                continue;
            };
            absolute_children.push(flow.node);
        }
        if !absolute_children.is_empty() {
            children.splice(0..0, absolute_children);
        }
        Ok(())
    }

    fn layout_absolute_child(
        &mut self,
        child: &NodeRef,
        child_style: &mut Style,
        containing_rect: Rect,
        depth: usize,
    ) -> Result<Option<FlowBox>> {
        let left = child_style
            .inset_left
            .and_then(|length| length.resolve(containing_rect.width));
        let right = child_style
            .inset_right
            .and_then(|length| length.resolve(containing_rect.width));
        let top = child_style
            .inset_top
            .and_then(|length| length.resolve(containing_rect.height));
        let bottom = child_style
            .inset_bottom
            .and_then(|length| length.resolve(containing_rect.height));

        if child_style.width.is_none() {
            if let (Some(left), Some(right)) = (left, right) {
                child_style.width =
                    Some(Length::Px((containing_rect.width - left - right).max(0.0)));
            }
        }
        if child_style.height.is_none() {
            if let (Some(top), Some(bottom)) = (top, bottom) {
                child_style.height =
                    Some(Length::Px((containing_rect.height - top - bottom).max(0.0)));
            }
        }

        let resolved_width = child_style
            .resolve_width(containing_rect.width)
            .map(|width| child_style.outer_width_for_declared(width))
            .unwrap_or(containing_rect.width)
            .max(1.0);
        let x = if let Some(left) = left {
            containing_rect.x + left
        } else if let Some(right) = right {
            containing_rect.x + containing_rect.width - right - resolved_width
        } else {
            containing_rect.x
        };

        let resolved_height = child_style.resolve_height(containing_rect.height);
        let y = if let Some(top) = top {
            containing_rect.y + top
        } else if let (Some(bottom), Some(height)) = (bottom, resolved_height) {
            containing_rect.y + containing_rect.height - bottom - height
        } else {
            containing_rect.y
        };

        self.layout_element_with_style(child, child_style.clone(), x, y, resolved_width, depth)
    }

    fn preferred_table_outer_width(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        max_outer_width: f32,
        spacing: f32,
    ) -> Result<f32> {
        let count = grid.column_count.max(1);
        let max_content_width = table_style.inner_width_for_outer(max_outer_width);
        let available =
            (max_content_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = vec![0.0_f32; count];

        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width.and_then(|width| width.resolve(available)) {
                widths[col] = widths[col].max(width.max(1.0));
            }
        }

        for row in &grid.rows {
            let row_style = self.style_for_node(&row.node, table_style);
            if row_style.display == Display::None {
                continue;
            }
            for cell in &row.cells {
                let mut cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
                cell_style.apply_table_cell_padding(table_style.cell_padding);

                let preferred = if let Some(width) =
                    cell_style.width.and_then(|width| width.resolve(available))
                {
                    cell_style.outer_width_for_declared(width)
                } else {
                    self.preferred_content_width(&cell.node, &cell_style, available)?
                        + cell_style.padding.horizontal()
                        + cell_style.border.horizontal()
                }
                .max(0.0);
                let per_col = ((preferred - spacing * cell.colspan.saturating_sub(1) as f32)
                    / cell.colspan as f32)
                    .max(0.0);
                for col in cell.col..cell.col + cell.colspan {
                    if col < widths.len() {
                        widths[col] = widths[col].max(per_col);
                    }
                }
            }
        }

        let content_width = widths.iter().sum::<f32>() + spacing * count.saturating_sub(1) as f32;
        Ok(
            (content_width + table_style.padding.horizontal() + table_style.border.horizontal())
                .max(1.0),
        )
    }

    fn preferred_auto_table_outer_width(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        max_outer_width: f32,
        spacing: f32,
    ) -> Result<f32> {
        if table_style.float_side != FloatSide::None && !table_grid_has_non_spacer_text(grid) {
            let intrinsic = self.fixed_replaced_table_min_outer_width(
                grid,
                table_style,
                max_outer_width,
                spacing,
            )?;
            if intrinsic > 1.0 {
                return Ok(intrinsic.min(max_outer_width).max(1.0));
            }
        }

        Ok(self
            .preferred_table_outer_width(grid, table_style, max_outer_width, spacing)?
            .min(max_outer_width))
    }

    fn resolve_table_column_widths(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        table_width: f32,
        spacing: f32,
    ) -> Result<Vec<f32>> {
        let count = grid.column_count.max(1);
        let available = (table_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        if count == 1 {
            return Ok(vec![available]);
        }
        let mut widths = vec![None; count];
        let mut preferreds = vec![0.0_f32; count];
        let mut minimums = vec![0.0_f32; count];
        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width
                .filter(length_is_intrinsic_fixed)
                .and_then(|width| width.resolve(available))
            {
                let width = width.max(1.0);
                widths[col] = Some(width);
            }
        }

        if table_style.table_layout_fixed {
            for row in &grid.rows {
                let row_style = self.style_for_node(&row.node, table_style);
                if row_style.display == Display::None {
                    continue;
                }
                for cell in &row.cells {
                    let mut style = self.style_for_node(&cell.node, &row_style);
                    if style.display == Display::None {
                        continue;
                    }
                    style.apply_table_cell_padding(table_style.cell_padding);
                    if let Some(width) = style.width.and_then(|width| width.resolve(available)) {
                        let outer_width = style.outer_width_for_declared(width);
                        let per_col = ((outer_width
                            - spacing * cell.colspan.saturating_sub(1) as f32)
                            / cell.colspan as f32)
                            .max(1.0);
                        for col in cell.col..cell.col + cell.colspan {
                            if col < widths.len() {
                                widths[col] = Some(widths[col].unwrap_or(0.0).max(per_col));
                            }
                        }
                    }
                }
                break;
            }

            return Ok(distribute_fixed_table_column_widths(widths, available));
        }

        for row in &grid.rows {
            let row_style = self.style_for_node(&row.node, table_style);
            if row_style.display == Display::None {
                continue;
            }
            let single_cell_spacer_row = row.cells.len() == 1 && count > 1;
            for cell in &row.cells {
                let mut style = self.style_for_node(&cell.node, &row_style);
                if style.display == Display::None {
                    continue;
                }
                style.apply_table_cell_padding(table_style.cell_padding);
                let preferred =
                    if let Some(width) = style.width.and_then(|width| width.resolve(available)) {
                        style.outer_width_for_declared(width)
                    } else {
                        self.preferred_content_width(&cell.node, &style, available)?
                            + style.padding.horizontal()
                            + style.border.horizontal()
                    }
                    .max(0.0);
                let per_col = ((preferred - spacing * cell.colspan.saturating_sub(1) as f32)
                    / cell.colspan as f32)
                    .max(0.0);
                let spacer_cell = table_cell_is_spacer(&cell.node);
                let uses_intrinsic_fixed_width =
                    style.width.as_ref().is_some_and(length_is_intrinsic_fixed)
                        || style.wrap == TextWrap::None
                        || cell_contains_only_intrinsic_fixed_replaced_content(&cell.node, &style);
                if uses_intrinsic_fixed_width {
                    for col in cell.col..cell.col + cell.colspan {
                        if col < widths.len() {
                            widths[col] = Some(widths[col].unwrap_or(0.0).max(per_col.max(1.0)));
                        }
                    }
                }
                for col in cell.col..cell.col + cell.colspan {
                    if col < preferreds.len() {
                        preferreds[col] = preferreds[col].max(per_col);
                    }
                    if spacer_cell && !single_cell_spacer_row && col < minimums.len() {
                        minimums[col] = minimums[col].max(per_col);
                    }
                }
            }
        }

        let mut resolved: Vec<f32> = widths
            .iter()
            .zip(preferreds.iter().zip(&minimums))
            .map(|(width, (preferred, minimum))| {
                width.unwrap_or_else(|| preferred.max(*minimum)).max(1.0)
            })
            .collect();

        let explicit_total: f32 = widths.iter().flatten().sum();
        let flexible_minimum: f32 = widths
            .iter()
            .zip(resolved.iter())
            .filter_map(|(width, resolved)| width.is_none().then_some(*resolved))
            .sum();

        if explicit_total + flexible_minimum > available {
            let target_flexible = (available - explicit_total).max(0.0);
            if target_flexible > 0.0 && flexible_minimum > 0.0 {
                let scale = target_flexible / flexible_minimum;
                for (index, width) in widths.iter().enumerate() {
                    if width.is_none() {
                        resolved[index] = (resolved[index] * scale).max(1.0);
                    }
                }
            } else {
                let scale = available / (explicit_total + flexible_minimum).max(1.0);
                for width in &mut resolved {
                    *width = (*width * scale).max(1.0);
                }
            }
            return Ok(resolved);
        }

        let resolved_total: f32 = resolved.iter().sum();
        let flexible = widths.iter().filter(|width| width.is_none()).count();
        if flexible > 0 && resolved_total < available {
            let extra = (available - resolved_total) / flexible as f32;
            for (index, width) in widths.iter().enumerate() {
                if width.is_none() {
                    resolved[index] += extra;
                }
            }
        }

        Ok(resolved)
    }

    fn layout_image(
        &mut self,
        node: &NodeRef,
        style: Style,
        x: f32,
        y: f32,
        containing_width: f32,
    ) -> FlowBox {
        let image =
            attr(node, "src").and_then(|src| match self.resources.load_image(&src, "img") {
                Ok(image) => Some(image),
                Err(error) => {
                    self.push_warning(
                        RenderWarning::new(
                            RenderWarningCode::ImageLoadFailed,
                            format!("failed to load image {src}: {error}; left image box empty"),
                        )
                        .with_node("img")
                        .with_url(src),
                    );
                    None
                }
            });
        let natural_width = image.as_ref().map_or(0.0, |image| image.width as f32);
        let natural_height = image.as_ref().map_or(0.0, |image| image.height as f32);
        let min_size = if image.is_some() { 1.0 } else { 0.0 };
        let declared_width = style.resolve_width(containing_width).or_else(|| {
            if style.width_auto {
                None
            } else {
                attr(node, "width").and_then(|value| {
                    parse_length(&value).and_then(|length| length.resolve(containing_width))
                })
            }
        });
        let declared_height = style
            .resolve_height(declared_width.unwrap_or(containing_width))
            .or_else(|| {
                if style.height_auto {
                    None
                } else {
                    attr(node, "height").and_then(|value| {
                        parse_length(&value).and_then(|length| {
                            length.resolve(declared_width.unwrap_or(containing_width))
                        })
                    })
                }
            });
        let mut width = declared_width
            .or_else(|| {
                declared_height.and_then(|height| {
                    (natural_height > 0.0).then_some((height / natural_height) * natural_width)
                })
            })
            .unwrap_or(natural_width.min(containing_width))
            .max(min_size);
        width = style.constrain_width(width, containing_width).max(min_size);
        let height = declared_height
            .or_else(|| {
                if natural_width > 0.0 {
                    Some((width / natural_width) * natural_height)
                } else {
                    None
                }
            })
            .unwrap_or(natural_height)
            .max(min_size);

        let collapsible_margin_bottom = style.margin.bottom;
        FlowBox {
            advance: style.margin.top + height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Image(image),
                rect: Rect::new(
                    x + style.horizontal_offset(containing_width, width),
                    y + style.margin.top,
                    width,
                    height,
                ),
                style,
                debug: self.debug_for_image_node(node),
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
        let marker_width = (style.font_size * 1.5).max(18.0).min(inner_width);
        let content_x = inner_x;
        let content_width = inner_width;

        let content =
            self.layout_children(node, &style, content_x, inner_y, content_width, depth, &[])?;
        let mut marker_style = style.clone();
        marker_style.text_align = TextAlign::Right;
        let mut children = content.children;
        children.insert(
            0,
            LayoutBox {
                kind: LayoutKind::Text(marker),
                rect: Rect::new(
                    inner_x - marker_width,
                    inner_y,
                    (marker_width - 6.0).max(1.0),
                    resolved_line_height_from_db(self.font_system.db(), &style),
                ),
                style: marker_style,
                debug: self.debug_for_marker(),
                children: Vec::new(),
            },
        );

        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let line_height = resolved_line_height_from_db(self.font_system.db(), &style);
        let collapsed_trailing_margin = if block_allows_trailing_margin_collapse(&style) {
            content.trailing_collapsible_margin.min(content.advance)
        } else {
            0.0
        };
        let content_box_height = (content.advance - collapsed_trailing_margin).max(0.0);
        let rect_height = (content_box_height.max(line_height)
            + style.padding.vertical()
            + style.border.vertical())
        .max(min_height)
        .max(0.0);
        let collapsed_bottom_margin = style.margin.bottom.max(collapsed_trailing_margin);

        Ok(Some(FlowBox {
            advance: style.margin.top + rect_height + collapsed_bottom_margin,
            collapsible_margin_bottom: collapsed_bottom_margin,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: self.debug_for_node(node, "li"),
                children,
            },
        }))
    }

    fn layout_hr(&mut self, style: Style, x: f32, y: f32, containing_width: f32) -> FlowBox {
        let width = style
            .resolve_width(containing_width)
            .unwrap_or(containing_width);
        let content_height = style.resolve_height(0.0).unwrap_or(0.0).max(0.0);
        let height = (content_height + style.border.vertical()).max(1.0);

        let collapsible_margin_bottom = style.margin.bottom;
        FlowBox {
            advance: style.margin.top + height + collapsible_margin_bottom,
            collapsible_margin_bottom,
            escaped_floats: Vec::new(),
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
                style,
                debug: self.debug_for_tag("hr"),
                children: Vec::new(),
            },
        }
    }

    fn measure_text_height(&mut self, text: &str, width: f32, style: &Style) -> Result<f32> {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        if text.chars().all(|ch| ch == '\n') {
            return Ok(line_height * text.chars().count().max(1) as f32);
        }
        let effective_width =
            (width.max(1.0) * wrap_width_adjustment(style.font_family.as_deref())).max(1.0);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(self.font_system, Some(effective_width), None);
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
        Ok(height.max(line_height))
    }

    fn measure_rich_text_height(
        &mut self,
        spans: &[TextSpan],
        width: f32,
        style: &Style,
    ) -> Result<f32> {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let plain_text = spans_text(spans);
        if plain_text.chars().all(|ch| ch == '\n') {
            return Ok(line_height * plain_text.chars().count().max(1) as f32);
        }
        let effective_width =
            (width.max(1.0) * wrap_width_adjustment(style.font_family.as_deref())).max(1.0);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let rich_spans = rich_text_style_spans(spans, self.font_system.db(), 1.0, style);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, style.wrap.to_cosmic());
        buffer.set_size(self.font_system, Some(effective_width), None);
        buffer.set_rich_text(
            self.font_system,
            rich_spans,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(style.text_align.to_cosmic()),
        );

        let mut height: f32 = 0.0;
        for run in buffer.layout_runs() {
            height = height.max(run.line_top + run.line_height);
        }
        Ok(height.max(line_height))
    }

    fn measure_text_width(&mut self, text: &str, style: &Style) -> f32 {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
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
        (width + letter_spacing_preferred_width_padding(text, style.letter_spacing)).ceil()
    }

    fn measure_rich_text_width(&mut self, spans: &[TextSpan], style: &Style) -> f32 {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
        let metrics = Metrics::new(style.font_size.max(1.0), line_height.max(1.0));
        let rich_spans = rich_text_style_spans(spans, self.font_system.db(), 1.0, style);
        let mut buffer = Buffer::new_empty(metrics);
        buffer.set_wrap(self.font_system, Wrap::None);
        buffer.set_size(self.font_system, None, None);
        buffer.set_rich_text(
            self.font_system,
            rich_spans,
            &style.text_attrs(),
            Shaping::Advanced,
            Some(TextAlignMode::Left),
        );

        let mut width: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
        }
        (width + rich_text_letter_spacing_preferred_width_padding(spans)).ceil()
    }

    fn preferred_content_width(
        &mut self,
        node: &NodeRef,
        style: &Style,
        containing_width: f32,
    ) -> Result<f32> {
        let mut max_width: f32 = 0.0;
        let mut inline_spans: Vec<TextSpan> = Vec::new();

        let flush_inline_spans =
            |renderer: &mut Self, spans: &mut Vec<TextSpan>, max_width: &mut f32| {
                let normalized = normalize_text_spans(spans);
                spans.clear();
                if normalized.is_empty() {
                    return;
                }
                let width = if text_spans_match_style(&normalized, style) {
                    renderer.measure_text_width(&spans_text(&normalized), style)
                } else {
                    renderer.measure_rich_text_width(&normalized, style)
                };
                *max_width = max_width.max(width);
            };

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                append_text_span(&mut inline_spans, &text_node.borrow(), style);
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) {
                continue;
            }
            if tag == "br" {
                flush_inline_spans(self, &mut inline_spans, &mut max_width);
                continue;
            }

            let child_style = self.style_for_node(&child, style);
            if child_style.display == Display::None {
                continue;
            }

            if child_style.display == Display::Inline
                && tag != "img"
                && inline_can_flatten(&child, &child_style)
            {
                append_inline_spans(&child, &child_style, &mut inline_spans);
                continue;
            }

            flush_inline_spans(self, &mut inline_spans, &mut max_width);

            let child_width = if tag == "img" {
                self.preferred_image_width(&child, &child_style, containing_width)
            } else if child_style.display == Display::InlineBlock {
                self.preferred_content_width(&child, &child_style, containing_width)?
                    + child_style.padding.horizontal()
                    + child_style.border.horizontal()
            } else if matches!(child_style.display, Display::Table | Display::InlineTable) {
                let grid = build_table_grid(&child, self.limits.max_table_cells)?;
                let spacing = if child_style.border_collapse == BorderCollapse::Collapse {
                    0.0
                } else {
                    child_style.cell_spacing.max(0.0)
                };
                if let Some(width) = child_style.resolve_width(containing_width) {
                    let declared = table_outer_width_for_declared(&child_style, width);
                    if !can_expand_declared_table_width(&child_style) {
                        declared
                    } else {
                        self.fixed_replaced_table_min_outer_width(
                            &grid,
                            &child_style,
                            declared,
                            spacing,
                        )?
                        .max(declared)
                    }
                } else {
                    self.preferred_auto_table_outer_width(
                        &grid,
                        &child_style,
                        containing_width,
                        spacing,
                    )?
                }
            } else {
                child_style
                    .resolve_width(containing_width)
                    .map(|width| child_style.outer_width_for_declared(width))
                    .unwrap_or(
                        self.preferred_content_width(&child, &child_style, containing_width)?
                            + child_style.padding.horizontal()
                            + child_style.border.horizontal(),
                    )
            };
            max_width = max_width.max(child_width);
        }

        flush_inline_spans(self, &mut inline_spans, &mut max_width);
        Ok(max_width.max(1.0))
    }

    fn fixed_replaced_table_min_outer_width(
        &mut self,
        grid: &TableGrid,
        table_style: &Style,
        max_outer_width: f32,
        spacing: f32,
    ) -> Result<f32> {
        let count = grid.column_count.max(1);
        let max_content_width = table_style.inner_width_for_outer(max_outer_width);
        let available =
            (max_content_width - spacing * count.saturating_sub(1) as f32).max(count as f32);
        let mut widths = vec![0.0_f32; count];
        let mut blockified_row_width: f32 = 0.0;

        for (col, width) in grid.col_widths.iter().enumerate().take(count) {
            if let Some(width) = width
                .filter(length_is_intrinsic_fixed)
                .and_then(|width| width.resolve(available))
            {
                widths[col] = widths[col].max(width.max(1.0));
            }
        }

        for row in &grid.rows {
            let row_style = self.style_for_node(&row.node, table_style);
            if row_style.display == Display::None {
                continue;
            }
            let mut styled_cells = Vec::with_capacity(row.cells.len());
            for cell in &row.cells {
                let mut cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
                cell_style.apply_table_cell_padding(table_style.cell_padding);
                styled_cells.push((cell, cell_style));
            }
            if styled_cells.is_empty() {
                continue;
            }

            if styled_cells
                .iter()
                .all(|(_, cell_style)| cell_style.display != Display::TableCell)
            {
                let mut row_width: f32 = 0.0;
                for (cell, cell_style) in styled_cells {
                    let intrinsic = if cell_style
                        .width
                        .as_ref()
                        .is_some_and(length_is_intrinsic_fixed)
                        && cell_contains_only_intrinsic_fixed_replaced_content(
                            &cell.node,
                            &cell_style,
                        ) {
                        cell_style
                            .width
                            .and_then(|width| width.resolve(available))
                            .map(|width| cell_style.outer_width_for_declared(width))
                    } else {
                        self.fixed_replaced_content_min_width(&cell.node, &cell_style, available)?
                            .map(|width| {
                                width
                                    + cell_style.padding.horizontal()
                                    + cell_style.border.horizontal()
                            })
                    };
                    if let Some(intrinsic) = intrinsic {
                        row_width = row_width.max(intrinsic.max(0.0));
                    }
                }
                blockified_row_width = blockified_row_width.max(row_width);
                continue;
            }

            for (cell, cell_style) in styled_cells {
                let intrinsic = if cell_style
                    .width
                    .as_ref()
                    .is_some_and(length_is_intrinsic_fixed)
                    && cell_contains_only_intrinsic_fixed_replaced_content(&cell.node, &cell_style)
                {
                    cell_style
                        .width
                        .and_then(|width| width.resolve(available))
                        .map(|width| cell_style.outer_width_for_declared(width))
                } else {
                    self.fixed_replaced_content_min_width(&cell.node, &cell_style, available)?
                        .map(|width| {
                            width + cell_style.padding.horizontal() + cell_style.border.horizontal()
                        })
                };
                let Some(intrinsic) = intrinsic else {
                    continue;
                };
                let per_col = ((intrinsic - spacing * cell.colspan.saturating_sub(1) as f32)
                    / cell.colspan as f32)
                    .max(0.0);
                for col in cell.col..cell.col + cell.colspan {
                    if col < widths.len() {
                        widths[col] = widths[col].max(per_col);
                    }
                }
            }
        }

        let content_width = (widths.iter().sum::<f32>() + spacing * count.saturating_sub(1) as f32)
            .max(blockified_row_width);
        Ok(content_width + table_style.padding.horizontal() + table_style.border.horizontal())
    }

    fn fixed_replaced_content_min_width(
        &mut self,
        node: &NodeRef,
        style: &Style,
        containing_width: f32,
    ) -> Result<Option<f32>> {
        let mut saw_fixed = false;
        let mut max_width = 0.0_f32;

        for child in node.children() {
            if let Some(text) = child.as_text() {
                if !text.borrow().chars().all(is_collapsible_whitespace) {
                    return Ok(None);
                }
                continue;
            }

            let Some(tag) = element_tag(&child) else {
                continue;
            };
            if is_metadata_tag(&tag) || tag == "br" {
                continue;
            }
            let child_style = self.style_for_node(&child, style);
            if child_style.display == Display::None {
                continue;
            }

            let width = if tag == "img" {
                if child_style
                    .width
                    .is_some_and(|width| matches!(width, Length::Percent(_)))
                {
                    None
                } else {
                    Some(self.preferred_image_width(&child, &child_style, containing_width))
                }
            } else if matches!(child_style.display, Display::Table | Display::InlineTable) {
                let grid = build_table_grid(&child, self.limits.max_table_cells)?;
                let spacing = if child_style.border_collapse == BorderCollapse::Collapse {
                    0.0
                } else {
                    child_style.cell_spacing.max(0.0)
                };
                let intrinsic = self.fixed_replaced_table_min_outer_width(
                    &grid,
                    &child_style,
                    containing_width,
                    spacing,
                )?;
                let declared = child_style
                    .resolve_width(containing_width)
                    .map(|width| table_outer_width_for_declared(&child_style, width));
                if child_style.width.is_some_and(is_px_length) {
                    Some(declared.map_or(intrinsic, |declared| declared.max(intrinsic)))
                } else {
                    (intrinsic > 1.0).then_some(intrinsic)
                }
            } else if matches!(
                child_style.display,
                Display::Inline | Display::InlineBlock | Display::Block
            ) {
                self.fixed_replaced_content_min_width(&child, &child_style, containing_width)?
                    .map(|width| {
                        width + child_style.padding.horizontal() + child_style.border.horizontal()
                    })
            } else {
                return Ok(None);
            };

            if let Some(width) = width {
                saw_fixed = true;
                max_width = max_width.max(width);
            }
        }

        Ok(saw_fixed.then_some(max_width.max(1.0)))
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
                    .and_then(|src| self.resources.load_image(&src, "img").ok())
                    .map_or(0.0, |image| image.width as f32)
                    .min(containing_width)
            })
            .max(0.0)
    }

    fn push_warning(&mut self, warning: RenderWarning) {
        if self.warnings.len() < api::MAX_RENDER_WARNINGS {
            self.warnings.push(warning);
        }
    }
}

fn can_expand_declared_table_width(style: &Style) -> bool {
    !style.table_layout_fixed
        && style.box_sizing == BoxSizing::ContentBox
        && style.width.is_some_and(is_px_length)
}

fn table_outer_width_for_declared(style: &Style, width: f32) -> f32 {
    match style.box_sizing {
        BoxSizing::BorderBox => width,
        BoxSizing::ContentBox => width.max(style.padding.horizontal() + style.border.horizontal()),
    }
}

fn is_px_length(length: Length) -> bool {
    matches!(length, Length::Px(_))
}

pub(crate) fn taffy_flex_container_style(
    style: &Style,
    inner_width: f32,
    inner_height: Option<f32>,
) -> TaffyStyle {
    TaffyStyle {
        display: TaffyDisplay::Flex,
        size: TaffySize {
            width: taffy_length(inner_width),
            height: inner_height.map_or_else(taffy_auto, taffy_length),
        },
        flex_direction: taffy_flex_direction(style.flex_direction),
        flex_wrap: taffy_flex_wrap(style.flex_wrap),
        justify_content: Some(taffy_justify_content(style.justify_content)),
        align_items: Some(taffy_align_items(style.align_items)),
        gap: TaffySize {
            width: taffy_length(style.column_gap),
            height: taffy_length(style.row_gap),
        },
        ..Default::default()
    }
}

pub(crate) fn float_adjusted_line(
    x: f32,
    width: f32,
    y: f32,
    floats: &[PlacedFloat],
) -> (f32, f32) {
    let (left_offset, right_offset) = float_offsets_at_y(x, width, y, floats);
    let line_x = x + left_offset;
    let line_width = (width - left_offset - right_offset).max(1.0);
    (line_x, line_width)
}

pub(crate) fn float_offsets_at_y(x: f32, width: f32, y: f32, floats: &[PlacedFloat]) -> (f32, f32) {
    let mut left_offset: f32 = 0.0;
    let mut right_offset: f32 = 0.0;
    for float in floats.iter().filter(|float| float_intersects_y(float, y)) {
        match float.side {
            FloatSide::Left => left_offset = left_offset.max(float.rect.x + float.rect.width - x),
            FloatSide::Right => right_offset = right_offset.max(x + width - float.rect.x),
            FloatSide::None => {}
        }
    }
    (left_offset.min(width), right_offset.min(width))
}

pub(crate) fn float_placement_y(
    floats: &[PlacedFloat],
    x: f32,
    width: f32,
    y: f32,
    needed_width: f32,
) -> f32 {
    let mut candidate_y = y;
    loop {
        let (left_offset, right_offset) = float_offsets_at_y(x, width, candidate_y, floats);
        if width - left_offset - right_offset >= needed_width {
            return candidate_y;
        }
        let Some(next_y) = floats
            .iter()
            .filter(|float| float_intersects_y(float, candidate_y))
            .map(|float| float.rect.y + float.rect.height)
            .min_by(|a, b| a.total_cmp(b))
        else {
            return candidate_y;
        };
        if next_y <= candidate_y {
            return candidate_y;
        }
        candidate_y = next_y;
    }
}

fn block_flow_placement_y(
    style: &Style,
    x: f32,
    width: f32,
    y: f32,
    floats: &[PlacedFloat],
) -> f32 {
    if floats.is_empty() {
        return y;
    }
    let outer_width = style
        .resolve_width(width)
        .map(|declared| style.outer_width_for_declared(declared))
        .unwrap_or_else(|| width - style.margin.horizontal())
        .max(1.0)
        .min(width.max(1.0));
    float_placement_y(floats, x, width, y, outer_width)
}

pub(crate) fn clear_float_y(floats: &[PlacedFloat], clear: Clear) -> f32 {
    floats
        .iter()
        .filter(|float| match clear {
            Clear::None => false,
            Clear::Left => float.side == FloatSide::Left,
            Clear::Right => float.side == FloatSide::Right,
            Clear::Both => matches!(float.side, FloatSide::Left | FloatSide::Right),
        })
        .map(|float| float.rect.y + float.rect.height)
        .fold(0.0, f32::max)
}

pub(crate) fn float_intersects_y(float: &PlacedFloat, y: f32) -> bool {
    y >= float.rect.y && y < float.rect.y + float.rect.height
}

fn translate_placed_floats(floats: &mut [PlacedFloat], dx: f32, dy: f32) {
    for float in floats {
        float.rect.x += dx;
        float.rect.y += dy;
    }
}

fn block_establishes_float_container(style: &Style) -> bool {
    matches!(style.display, Display::Flex | Display::InlineBlock)
        || style.float_side != FloatSide::None
}

pub(crate) fn taffy_leaf_style(
    style: &Style,
    measured_width: f32,
    measured_height: f32,
) -> TaffyStyle {
    TaffyStyle {
        size: TaffySize {
            width: taffy_length(measured_width),
            height: taffy_length(measured_height),
        },
        min_size: TaffySize {
            width: taffy_dimension(style.min_width),
            height: taffy_dimension(style.min_height),
        },
        max_size: TaffySize {
            width: taffy_dimension(style.max_width),
            height: taffy_dimension(style.max_height),
        },
        margin: TaffyRect {
            left: if style.margin_left_auto {
                taffy_auto()
            } else {
                taffy_length(style.margin.left)
            },
            right: if style.margin_right_auto {
                taffy_auto()
            } else {
                taffy_length(style.margin.right)
            },
            top: taffy_length(style.margin.top),
            bottom: taffy_length(style.margin.bottom),
        },
        align_self: style.align_self.map(taffy_align_items),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: taffy_dimension(style.flex_basis),
        ..Default::default()
    }
}

pub(crate) fn taffy_dimension(length: Option<Length>) -> TaffyDimension {
    match length {
        Some(Length::Px(value)) => taffy_length(value),
        Some(Length::Percent(value)) => taffy_percent(value),
        Some(Length::Inherit) => taffy_percent(1.0),
        None => taffy_auto(),
    }
}

pub(crate) fn taffy_flex_direction(direction: FlexDirection) -> TaffyFlexDirection {
    match direction {
        FlexDirection::Row => TaffyFlexDirection::Row,
        FlexDirection::RowReverse => TaffyFlexDirection::RowReverse,
        FlexDirection::Column => TaffyFlexDirection::Column,
        FlexDirection::ColumnReverse => TaffyFlexDirection::ColumnReverse,
    }
}

pub(crate) fn taffy_flex_wrap(wrap: FlexWrap) -> TaffyFlexWrap {
    match wrap {
        FlexWrap::NoWrap => TaffyFlexWrap::NoWrap,
        FlexWrap::Wrap => TaffyFlexWrap::Wrap,
        FlexWrap::WrapReverse => TaffyFlexWrap::WrapReverse,
    }
}

pub(crate) fn taffy_justify_content(justify: JustifyContent) -> TaffyJustifyContent {
    match justify {
        JustifyContent::FlexStart => TaffyJustifyContent::FlexStart,
        JustifyContent::FlexEnd => TaffyJustifyContent::FlexEnd,
        JustifyContent::Center => TaffyJustifyContent::Center,
        JustifyContent::SpaceBetween => TaffyJustifyContent::SpaceBetween,
        JustifyContent::SpaceAround => TaffyJustifyContent::SpaceAround,
        JustifyContent::SpaceEvenly => TaffyJustifyContent::SpaceEvenly,
    }
}

pub(crate) fn taffy_align_items(align: AlignItems) -> TaffyAlignItems {
    match align {
        AlignItems::FlexStart => TaffyAlignItems::FlexStart,
        AlignItems::FlexEnd => TaffyAlignItems::FlexEnd,
        AlignItems::Center => TaffyAlignItems::Center,
        AlignItems::Baseline => TaffyAlignItems::Baseline,
        AlignItems::Stretch => TaffyAlignItems::Stretch,
    }
}

fn css_table_cell_widths(cells: &[CssTableCell], content_width: f32) -> Vec<f32> {
    let count = cells.len().max(1);
    let mut widths = vec![None; count];
    let mut fixed_total = 0.0_f32;
    let mut auto_count = 0usize;

    for (idx, (_, style)) in cells.iter().enumerate() {
        if let Some(width) = style.width.and_then(|width| width.resolve(content_width)) {
            let outer_width = style.outer_width_for_declared(width).max(1.0);
            widths[idx] = Some(outer_width);
            fixed_total += outer_width;
        } else {
            auto_count += 1;
        }
    }

    if fixed_total > content_width + f32::EPSILON {
        let scale = content_width / fixed_total;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(0.0) * scale)
            .collect();
    }

    if auto_count > 0 {
        let auto_width = ((content_width - fixed_total).max(0.0) / auto_count as f32).max(1.0);
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(auto_width))
            .collect();
    }

    if fixed_total > 0.0 && fixed_total < content_width {
        let slack = content_width - fixed_total;
        return widths
            .into_iter()
            .map(|width| {
                let width = width.unwrap_or(0.0);
                width + slack * (width / fixed_total)
            })
            .collect();
    }

    widths
        .into_iter()
        .map(|width| width.unwrap_or(content_width / count as f32))
        .collect()
}

fn css_table_row_column_widths(rows: &[CssTableRow], content_width: f32) -> Vec<f32> {
    let count = rows
        .iter()
        .map(|(_, _, cells)| cells.len())
        .max()
        .unwrap_or(1)
        .max(1);
    let mut widths = vec![None; count];
    let mut fixed_total = 0.0_f32;

    for (_, _, cells) in rows {
        for (idx, (_, style)) in cells.iter().enumerate() {
            let Some(width) = style.width.and_then(|width| width.resolve(content_width)) else {
                continue;
            };
            let outer_width = style.outer_width_for_declared(width).max(1.0);
            let previous = widths[idx].unwrap_or(0.0_f32);
            if outer_width > previous {
                fixed_total += outer_width - previous;
                widths[idx] = Some(outer_width);
            }
        }
    }

    if fixed_total > content_width + f32::EPSILON {
        let scale = content_width / fixed_total;
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(0.0) * scale)
            .collect();
    }

    let auto_count = widths.iter().filter(|width| width.is_none()).count();
    if auto_count > 0 {
        let auto_width = ((content_width - fixed_total).max(0.0) / auto_count as f32).max(1.0);
        return widths
            .into_iter()
            .map(|width| width.unwrap_or(auto_width))
            .collect();
    }

    widths
        .into_iter()
        .map(|width| width.unwrap_or(1.0))
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct LayoutBox {
    pub(crate) kind: LayoutKind,
    pub(crate) rect: Rect,
    pub(crate) style: Style,
    pub(crate) debug: LayoutDebugMeta,
    pub(crate) children: Vec<LayoutBox>,
}

#[derive(Debug, Clone)]
pub(crate) enum LayoutKind {
    Block,
    Table,
    Row,
    Cell,
    Text(String),
    RichText(Vec<TextSpan>),
    Image(Option<ImageData>),
}

#[derive(Debug)]
struct FlowBox {
    node: LayoutBox,
    advance: f32,
    collapsible_margin_bottom: f32,
    escaped_floats: Vec<PlacedFloat>,
}

#[derive(Debug, Default)]
struct LayoutChildren {
    children: Vec<LayoutBox>,
    advance: f32,
    in_flow_advance: f32,
    floats: Vec<PlacedFloat>,
    trailing_collapsible_margin: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LayoutDebugMeta {
    pub(crate) tag: String,
    pub(crate) id: Option<String>,
    pub(crate) class_name: Option<String>,
    pub(crate) text: String,
    pub(crate) src: Option<String>,
}

impl LayoutDebugMeta {
    fn for_node(node: &NodeRef, fallback_tag: &str) -> Self {
        let tag = element_tag(node).unwrap_or_else(|| fallback_tag.to_string());
        let text = normalize_preview_text(&text_content(node));
        Self {
            tag,
            id: attr(node, "id"),
            class_name: attr(node, "class"),
            text,
            src: None,
        }
    }

    fn for_tag(tag: &str) -> Self {
        Self {
            tag: tag.to_string(),
            ..Self::default()
        }
    }

    fn for_text(text: &str) -> Self {
        Self {
            tag: "#text".to_string(),
            text: normalize_preview_text(text),
            ..Self::default()
        }
    }

    fn for_marker() -> Self {
        Self::for_tag("::marker")
    }

    fn for_image_node(node: &NodeRef) -> Self {
        let mut meta = Self::for_node(node, "img");
        meta.src = attr(node, "src");
        if meta.text.is_empty() {
            meta.text = normalize_preview_text(&attr(node, "alt").unwrap_or_default());
        }
        meta
    }
}

pub(crate) fn normalize_preview_text(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;

    for word in text.split_whitespace() {
        if chars >= 120 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
            chars += 1;
            if chars >= 120 {
                break;
            }
        }
        for ch in word.chars() {
            if chars >= 120 {
                break;
            }
            out.push(ch);
            chars += 1;
        }
    }

    out
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
fn letter_spacing_preferred_width_padding(text: &str, letter_spacing: f32) -> f32 {
    if letter_spacing <= 0.0 || !text.chars().any(|ch| !ch.is_whitespace()) {
        return 0.0;
    }
    // Blink's inline preferred width leaves a little room for positive tracking
    // at fragment edges. Without this, tightly fitted email OTP blocks can wrap
    // the final digit even though the browser keeps the tracked run on one line.
    letter_spacing
}
fn rich_text_letter_spacing_preferred_width_padding(spans: &[TextSpan]) -> f32 {
    spans
        .iter()
        .filter(|span| span.text.chars().any(|ch| !ch.is_whitespace()))
        .map(|span| span.style.letter_spacing)
        .fold(0.0_f32, f32::max)
        .max(0.0)
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
fn align_block_child_to_legacy_align_attribute(
    child: &mut LayoutBox,
    parent_style: &Style,
    container_x: f32,
    container_width: f32,
) {
    if !parent_style.align_from_attribute || !legacy_align_attribute_applies_to_child(child) {
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
fn legacy_align_attribute_applies_to_child(child: &LayoutBox) -> bool {
    match child.kind {
        LayoutKind::Image(_) => true,
        LayoutKind::Block => !child.style.margin_left_auto && !child.style.margin_right_auto,
        _ => false,
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
fn table_cell_is_spacer(node: &NodeRef) -> bool {
    let text = text_content(node);
    let mut has_nbsp = false;
    for ch in text.chars() {
        if ch == '\u{00a0}' {
            has_nbsp = true;
        } else if !is_collapsible_whitespace(ch) {
            return false;
        }
    }
    has_nbsp
}
fn table_grid_has_non_spacer_text(grid: &TableGrid) -> bool {
    grid.rows.iter().any(|row| {
        row.cells.iter().any(|cell| {
            !table_cell_is_spacer(&cell.node)
                && text_content(&cell.node)
                    .chars()
                    .any(|ch| !is_collapsible_whitespace(ch))
        })
    })
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
fn inline_needs_inline_block_container(node: &NodeRef, style: &Style) -> bool {
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
        let child_style = style_for_node(&child, style);
        if tag == "img" {
            return child_style.float_side == FloatSide::None
                && matches!(child_style.display, Display::Inline | Display::InlineBlock);
        }

        match child_style.display {
            Display::InlineBlock | Display::InlineTable => return true,
            Display::Inline => {
                if inline_needs_inline_block_container(&child, &child_style) {
                    return true;
                }
            }
            Display::None
            | Display::Block
            | Display::Flex
            | Display::Table
            | Display::TableRow
            | Display::TableCell => {}
        }
    }
    false
}
