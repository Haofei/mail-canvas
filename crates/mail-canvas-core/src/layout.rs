pub(crate) struct RenderLimits {
    max_layout_depth: usize,
    max_table_cells: usize,
}

impl RenderLimits {
    fn from_request(request: &RenderRequest) -> Self {
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
    available_font_families: Vec<String>,
    web_font_faces: Vec<WebFontFace>,
    warnings: Vec<RenderWarning>,
}

impl<'a, R: ResourceProvider> LayoutEngine<'a, R> {
    fn new(
        font_system: &'a mut FontSystem,
        resources: R,
        available_font_families: Vec<String>,
        web_font_faces: Vec<WebFontFace>,
        limits: RenderLimits,
    ) -> Self {
        Self {
            font_system,
            resources,
            limits,
            available_font_families,
            web_font_faces,
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

    fn layout_document(&mut self, document: &NodeRef, width: u32) -> Result<LayoutBox> {
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
        let content = self.layout_children(&root_node, &root_style, 0.0, 0.0, layout_width, 0)?;

        Ok(LayoutBox {
            kind: LayoutKind::Block,
            rect: Rect::new(0.0, 0.0, layout_width, content.advance.max(1.0)),
            style: root_style,
            debug: LayoutDebugMeta::for_node(&root_node, "body"),
            children: content.children,
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
        let mut floats = Vec::new();

        for child in node.children() {
            if let Some(text_node) = child.as_text() {
                let text_value = text_node.borrow();
                if text_value.chars().any(|ch| !is_collapsible_whitespace(ch)) {
                    last_inline_block_fallback = false;
                }
                if !inline_row.is_empty() && text_value.chars().all(is_collapsible_whitespace) {
                    continue;
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
                append_text_span(&mut text, &HARD_BREAK.to_string(), style);
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

            if child_style.display == Display::Inline
                && tag != "img"
                && !inline_style_has_own_box(&child_style)
                && !child_is_inline_block_fallback
            {
                last_inline_block_fallback = false;
                append_inline_spans(&child, &child_style, &mut text);
                continue;
            }

            let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
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
            let child_display = child_style.display;
            let child_float_side = child_style.float_side;
            let child_clear = child_style.clear;
            if child_clear != Clear::None {
                cursor_y = cursor_y.max(clear_float_y(&floats, child_clear));
                previous_margin_bottom = None;
            }
            let child_is_inline_flow = is_inline_flow(&tag, &child_style);
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
            let flow = if let Some(marker) = list_marker {
                self.layout_list_item(
                    &child,
                    child_style,
                    marker,
                    Rect::new(x, cursor_y, width, 0.0),
                    depth + 1,
                )?
            } else {
                self.layout_element_with_style(&child, child_style, x, cursor_y, width, depth + 1)?
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
                    let inline_flow_width =
                        (flow.node.rect.width + flow.node.style.margin.horizontal()).max(1.0);
                    if inline_row_width > 0.0
                        && inline_row_width + inline_flow_width > width + f32::EPSILON
                    {
                        flush_inline_row(
                            &mut inline_row,
                            &mut inline_row_width,
                            &mut inline_row_height,
                            style,
                            width,
                            &mut cursor_y,
                            &mut children,
                        );
                    }
                    if (cursor_y - flow_start_y).abs() > f32::EPSILON {
                        translate_layout(&mut flow.node, 0.0, cursor_y - flow_start_y);
                    }
                    if inline_row_width > 0.0 {
                        translate_layout(&mut flow.node, inline_row_width, 0.0);
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
                align_table_child_to_parent_text(&mut flow.node, style, x, width);
                align_image_child_to_legacy_align(&mut flow.node, style, x, width);
                let margin_overlap = if can_collapse_sibling_margin(child_display) {
                    previous_margin_bottom
                        .map(|previous: f32| previous.min(flow.node.style.margin.top))
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                if margin_overlap > 0.0 {
                    translate_layout(&mut flow.node, 0.0, -margin_overlap);
                }
                cursor_y += flow.advance - margin_overlap;
                previous_margin_bottom =
                    can_collapse_sibling_margin(child_display).then_some(collapsible_margin_bottom);
                last_inline_block_fallback = child_is_inline_block_fallback;
                children.push(flow.node);
            }
        }

        let (text_x, text_width) = float_adjusted_line(x, width, cursor_y, &floats);
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
        let float_bottom = floats
            .iter()
            .map(|float| float.rect.y + float.rect.height)
            .fold(cursor_y, f32::max);
        Ok(LayoutChildren {
            children,
            advance: float_bottom - y,
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

        let plain_text = spans_text(&normalized);
        let matches_parent_style = text_spans_match_style(&normalized, style);
        let height = if matches_parent_style {
            self.measure_text_height(&plain_text, width, style)?
        } else {
            self.measure_rich_text_height(&normalized, width, style)?
        };
        let kind = if matches_parent_style {
            LayoutKind::Text(plain_text)
        } else {
            LayoutKind::RichText(normalized)
        };
        let debug = match &kind {
            LayoutKind::Text(text) => LayoutDebugMeta::for_text(text),
            LayoutKind::RichText(spans) => LayoutDebugMeta::for_text(&spans_text(spans)),
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
                    self.layout_block(node, inline_style, x, y, containing_width, depth)
                }
            }
            Display::InlineBlock => {
                self.layout_inline_block(node, style, x, y, containing_width, depth)
            }
            _ => self.layout_block(node, style, x, y, containing_width, depth),
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
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth)?;
        let explicit_height = style.resolve_height(0.0).unwrap_or(0.0);
        let rect_height = (content.advance + style.padding.vertical() + style.border.vertical())
            .max(explicit_height)
            .max(1.0);
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
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, rect_width, rect_height),
                style,
                debug: LayoutDebugMeta::for_node(node, "div"),
                children: content.children,
            },
        }))
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

        let mut content =
            self.layout_children(node, &style, inner_x, inner_y, inner_width, depth)?;
        let min_height = style.resolve_height(0.0).unwrap_or(0.0);
        let collapsed_trailing_margin = if block_allows_trailing_margin_collapse(&style) {
            content.trailing_collapsible_margin.min(content.advance)
        } else {
            0.0
        };
        let content_box_height = (content.advance - collapsed_trailing_margin).max(0.0);
        let rect_height = (content_box_height + style.padding.vertical() + style.border.vertical())
            .max(min_height)
            .max(0.0);
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
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: LayoutDebugMeta::for_node(node, "div"),
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
                    LayoutKind::Text(text) => LayoutDebugMeta::for_text(text),
                    LayoutKind::RichText(spans) => LayoutDebugMeta::for_text(&spans_text(spans)),
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
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: LayoutDebugMeta::for_node(node, "div"),
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
            return self.layout_block(node, style, x, y, containing_width, depth);
        }

        let max_table_width = (containing_width - style.margin.horizontal()).max(1.0);
        let spacing = if style.border_collapse == BorderCollapse::Collapse {
            0.0
        } else {
            style.cell_spacing.max(0.0)
        };
        let table_width = if let Some(width) = style.resolve_width(containing_width) {
            style.outer_width_for_declared(width)
        } else {
            self.preferred_table_outer_width(&grid, &style, max_table_width, spacing)?
                .min(max_table_width)
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

            let mut cell_boxes = Vec::with_capacity(row.cells.len());
            let mut row_height: f32 = 0.0;

            for cell in row.cells {
                let mut cell_style = self.style_for_node(&cell.node, &row_style);
                if cell_style.display == Display::None {
                    continue;
                }
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
                        debug: LayoutDebugMeta::for_node(&cell.node, "td"),
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
                debug: LayoutDebugMeta::for_node(&row.node, "tr"),
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
            node: LayoutBox {
                kind: LayoutKind::Table,
                rect: Rect::new(rect_x, rect_y, table_width, table_height),
                style,
                debug: LayoutDebugMeta::for_node(node, "table"),
                children: row_boxes,
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
                let uses_intrinsic_fixed_width =
                    style.width.as_ref().is_some_and(length_is_intrinsic_fixed)
                        || table_cell_is_spacer(&cell.node)
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
                    if table_cell_is_spacer(&cell.node) && col < minimums.len() {
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
            node: LayoutBox {
                kind: LayoutKind::Image(image),
                rect: Rect::new(
                    x + style.horizontal_offset(containing_width, width),
                    y + style.margin.top,
                    width,
                    height,
                ),
                style,
                debug: LayoutDebugMeta::for_image_node(node),
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
            self.layout_children(node, &style, content_x, inner_y, content_width, depth)?;
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
                debug: LayoutDebugMeta::for_marker(),
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
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(rect_x, rect_y, outer_width, rect_height),
                style,
                debug: LayoutDebugMeta::for_node(node, "li"),
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
            node: LayoutBox {
                kind: LayoutKind::Block,
                rect: Rect::new(x + style.margin.left, y + style.margin.top, width, height),
                style,
                debug: LayoutDebugMeta::for_tag("hr"),
                children: Vec::new(),
            },
        }
    }

    fn measure_text_height(&mut self, text: &str, width: f32, style: &Style) -> Result<f32> {
        let line_height = resolved_line_height_from_db(self.font_system.db(), style);
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
        width.ceil()
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
        width.ceil()
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
                    child_style.outer_width_for_declared(width)
                } else {
                    self.preferred_table_outer_width(
                        &grid,
                        &child_style,
                        containing_width,
                        spacing,
                    )?
                    .min(containing_width)
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

pub(crate) fn float_adjusted_line(x: f32, width: f32, y: f32, floats: &[PlacedFloat]) -> (f32, f32) {
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

pub(crate) fn float_placement_y(floats: &[PlacedFloat], x: f32, width: f32, y: f32, needed_width: f32) -> f32 {
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

pub(crate) fn taffy_leaf_style(style: &Style, measured_width: f32, measured_height: f32) -> TaffyStyle {
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

#[derive(Debug, Clone)]
pub(crate) struct LayoutBox {
    kind: LayoutKind,
    rect: Rect,
    style: Style,
    debug: LayoutDebugMeta,
    children: Vec<LayoutBox>,
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
pub(crate) struct FlowBox {
    node: LayoutBox,
    advance: f32,
    collapsible_margin_bottom: f32,
}

#[derive(Debug, Default)]
pub(crate) struct LayoutChildren {
    children: Vec<LayoutBox>,
    advance: f32,
    trailing_collapsible_margin: f32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LayoutDebugMeta {
    tag: String,
    id: Option<String>,
    class_name: Option<String>,
    text: String,
    src: Option<String>,
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

fn normalize_preview_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect()
}
