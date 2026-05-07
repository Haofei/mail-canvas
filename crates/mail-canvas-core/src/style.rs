use cosmic_text::{
    Align as TextAlignMode, Attrs, Color as TextColor, Metrics, Style as FontStyle,
    Weight as FontWeight, Wrap,
};
use kuchiki::NodeRef;

use crate::ImageData;
use crate::css::{css_declarations, first_css_url, unquote_css_value};
use crate::fonts::WebFontFace;
use crate::text::{
    normal_line_height_fallback, parse_line_height_declaration, resolved_line_height_from_db,
    resolved_line_height_from_run_db, text_style_attrs,
};

#[derive(Debug, Clone)]
pub(crate) struct TextSpan {
    pub(crate) text: String,
    pub(crate) style: TextRunStyle,
}

impl TextSpan {
    pub(crate) fn from_style(text: String, style: &Style) -> Self {
        Self {
            text,
            style: TextRunStyle::from_style(style),
        }
    }

    pub(crate) fn with_run_style(text: String, style: TextRunStyle) -> Self {
        Self { text, style }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextRunStyle {
    pub(crate) color: Rgba,
    pub(crate) font_family: Option<String>,
    pub(crate) font_weight: FontWeight,
    pub(crate) font_face_weight: Option<FontWeight>,
    pub(crate) font_style: FontStyle,
    pub(crate) font_size: f32,
    pub(crate) line_height: f32,
    pub(crate) line_height_factor: Option<f32>,
    pub(crate) line_height_normal: bool,
    pub(crate) font_hinting_disabled: bool,
    pub(crate) letter_spacing: f32,
    pub(crate) text_transform: TextTransform,
}

impl TextRunStyle {
    pub(crate) fn from_style(style: &Style) -> Self {
        Self {
            color: style.color,
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            font_face_weight: style.font_face_weight,
            font_style: style.font_style,
            font_size: style.font_size,
            line_height: style.line_height,
            line_height_factor: style.line_height_factor,
            line_height_normal: style.line_height_normal,
            font_hinting_disabled: style.font_hinting_disabled,
            letter_spacing: style.letter_spacing,
            text_transform: style.text_transform,
        }
    }

    pub(crate) fn text_attrs(&self) -> Attrs<'_> {
        text_style_attrs(
            self.font_family.as_deref(),
            self.font_weight,
            self.font_face_weight,
            self.font_style,
            self.font_hinting_disabled,
            self.letter_spacing,
            self.font_size,
        )
    }

    pub(crate) fn text_attrs_for_span(
        &self,
        db: &fontdb::Database,
        scale: f32,
        parent_style: &Style,
    ) -> Attrs<'_> {
        let attrs = self.text_attrs().color(TextColor::rgba(
            self.color.r,
            self.color.g,
            self.color.b,
            self.color.a,
        ));
        if !self.needs_own_metrics(db, parent_style) {
            return attrs;
        }

        let font_size = (self.font_size * scale).max(1.0);
        let line_height = (resolved_line_height_from_run_db(db, self) * scale).max(1.0);
        attrs.metrics(Metrics::new(font_size, line_height))
    }

    pub(crate) fn needs_own_metrics(&self, db: &fontdb::Database, parent_style: &Style) -> bool {
        if (self.font_size - parent_style.font_size).abs() > 0.01 {
            return true;
        }
        let run_line_height = resolved_line_height_from_run_db(db, self);
        let parent_line_height = resolved_line_height_from_db(db, parent_style);
        (run_line_height - parent_line_height).abs() > 0.01
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Style {
    pub(crate) display: Display,
    pub(crate) width: Option<Length>,
    pub(crate) width_auto: bool,
    pub(crate) min_width: Option<Length>,
    pub(crate) max_width: Option<Length>,
    pub(crate) height: Option<Length>,
    pub(crate) height_auto: bool,
    pub(crate) min_height: Option<Length>,
    pub(crate) max_height: Option<Length>,
    pub(crate) margin: Edges,
    pub(crate) margin_left_auto: bool,
    pub(crate) margin_right_auto: bool,
    pub(crate) margin_top_em: Option<f32>,
    pub(crate) margin_bottom_em: Option<f32>,
    pub(crate) padding: Edges,
    pub(crate) padding_percent: RelativeEdges,
    pub(crate) padding_explicit: EdgeFlags,
    pub(crate) background: Option<Rgba>,
    pub(crate) background_image: Option<ImageData>,
    pub(crate) background_image_src: Option<String>,
    pub(crate) background_repeat: BackgroundRepeat,
    pub(crate) background_size: BackgroundSize,
    pub(crate) background_position: BackgroundPosition,
    pub(crate) object_fit: ObjectFit,
    pub(crate) object_position: ObjectPosition,
    pub(crate) opacity: f32,
    pub(crate) color: Rgba,
    pub(crate) box_shadows: Vec<BoxShadow>,
    pub(crate) text_shadows: Vec<BoxShadow>,
    pub(crate) font_family: Option<String>,
    pub(crate) font_weight: FontWeight,
    pub(crate) font_face_weight: Option<FontWeight>,
    pub(crate) font_style: FontStyle,
    pub(crate) font_size: f32,
    pub(crate) line_height: f32,
    pub(crate) line_height_factor: Option<f32>,
    pub(crate) line_height_normal: bool,
    pub(crate) font_hinting_disabled: bool,
    pub(crate) letter_spacing: f32,
    pub(crate) text_align: TextAlign,
    pub(crate) align_from_attribute: bool,
    pub(crate) text_transform: TextTransform,
    pub(crate) vertical_align: VerticalAlign,
    pub(crate) wrap: TextWrap,
    pub(crate) list_style_type: ListStyleType,
    pub(crate) box_sizing: BoxSizing,
    pub(crate) position: Position,
    pub(crate) inset_top: Option<Length>,
    pub(crate) inset_right: Option<Length>,
    pub(crate) inset_bottom: Option<Length>,
    pub(crate) inset_left: Option<Length>,
    pub(crate) flex_direction: FlexDirection,
    pub(crate) flex_wrap: FlexWrap,
    pub(crate) justify_content: JustifyContent,
    pub(crate) align_items: AlignItems,
    pub(crate) align_self: Option<AlignItems>,
    pub(crate) row_gap: f32,
    pub(crate) column_gap: f32,
    pub(crate) flex_grow: f32,
    pub(crate) flex_shrink: f32,
    pub(crate) flex_basis: Option<Length>,
    pub(crate) float_side: FloatSide,
    pub(crate) clear: Clear,
    pub(crate) border: Edges,
    pub(crate) border_radius: f32,
    pub(crate) border_color: Rgba,
    pub(crate) border_style: BorderLineStyle,
    pub(crate) border_collapse: BorderCollapse,
    pub(crate) table_layout_fixed: bool,
    pub(crate) cell_padding: Edges,
    pub(crate) cell_spacing: f32,
}

impl Style {
    pub(crate) fn initial() -> Self {
        Self {
            display: Display::Block,
            width: None,
            width_auto: false,
            min_width: None,
            max_width: None,
            height: None,
            height_auto: false,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            margin_top_em: None,
            margin_bottom_em: None,
            padding: Edges::ZERO,
            padding_percent: RelativeEdges::NONE,
            padding_explicit: EdgeFlags::NONE,
            background: None,
            background_image: None,
            background_image_src: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_size: BackgroundSize::Auto,
            background_position: BackgroundPosition::default(),
            object_fit: ObjectFit::Fill,
            object_position: ObjectPosition::default(),
            opacity: 1.0,
            color: Rgba::BLACK,
            box_shadows: Vec::new(),
            text_shadows: Vec::new(),
            font_family: None,
            font_weight: FontWeight::NORMAL,
            font_face_weight: None,
            font_style: FontStyle::Normal,
            font_size: 16.0,
            line_height: normal_line_height_fallback(16.0),
            line_height_factor: None,
            line_height_normal: true,
            font_hinting_disabled: false,
            letter_spacing: 0.0,
            text_align: TextAlign::Left,
            align_from_attribute: false,
            text_transform: TextTransform::None,
            vertical_align: VerticalAlign::Baseline,
            wrap: TextWrap::WordOrGlyph,
            list_style_type: ListStyleType::Disc,
            box_sizing: BoxSizing::ContentBox,
            position: Position::Static,
            inset_top: None,
            inset_right: None,
            inset_bottom: None,
            inset_left: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: None,
            row_gap: 0.0,
            column_gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            float_side: FloatSide::None,
            clear: Clear::None,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: Rgba::BLACK,
            border_style: BorderLineStyle::None,
            border_collapse: BorderCollapse::Separate,
            table_layout_fixed: false,
            cell_padding: Edges::ZERO,
            cell_spacing: 0.0,
        }
    }

    pub(crate) fn from_parent_for_tag(parent: &Self, tag: &str) -> Self {
        let mut style = Self {
            display: default_display(tag),
            width: None,
            width_auto: false,
            min_width: None,
            max_width: None,
            height: None,
            height_auto: false,
            min_height: None,
            max_height: None,
            margin: Edges::ZERO,
            margin_left_auto: false,
            margin_right_auto: false,
            margin_top_em: None,
            margin_bottom_em: None,
            padding: Edges::ZERO,
            padding_percent: RelativeEdges::NONE,
            padding_explicit: EdgeFlags::NONE,
            background: None,
            background_image: None,
            background_image_src: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_size: BackgroundSize::Auto,
            background_position: BackgroundPosition::default(),
            object_fit: ObjectFit::Fill,
            object_position: ObjectPosition::default(),
            opacity: 1.0,
            color: parent.color,
            box_shadows: Vec::new(),
            text_shadows: parent.text_shadows.clone(),
            font_family: parent.font_family.clone(),
            font_weight: parent.font_weight,
            font_face_weight: parent.font_face_weight,
            font_style: parent.font_style,
            font_size: parent.font_size,
            line_height: parent.line_height,
            line_height_factor: parent.line_height_factor,
            line_height_normal: parent.line_height_normal,
            font_hinting_disabled: parent.font_hinting_disabled,
            letter_spacing: parent.letter_spacing,
            text_align: if tag == "table" {
                TextAlign::Left
            } else {
                parent.text_align
            },
            align_from_attribute: if tag == "table" {
                false
            } else {
                parent.align_from_attribute
            },
            text_transform: parent.text_transform,
            vertical_align: default_vertical_align(tag, parent.vertical_align),
            wrap: parent.wrap,
            list_style_type: parent.list_style_type,
            box_sizing: parent.box_sizing,
            position: Position::Static,
            inset_top: None,
            inset_right: None,
            inset_bottom: None,
            inset_left: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            align_self: None,
            row_gap: 0.0,
            column_gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: None,
            float_side: FloatSide::None,
            clear: Clear::None,
            border: Edges::ZERO,
            border_radius: 0.0,
            border_color: parent.border_color,
            border_style: BorderLineStyle::None,
            border_collapse: BorderCollapse::Separate,
            table_layout_fixed: false,
            cell_padding: Edges::ZERO,
            cell_spacing: 0.0,
        };

        match tag {
            "h1" => {
                style.set_font_size(parent.font_size * 2.0);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(0.67, 0.67);
            }
            "h2" => {
                style.set_font_size(parent.font_size * 1.5);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(0.83, 0.83);
            }
            "h3" => {
                style.set_font_size(parent.font_size * 1.17);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.0, 1.0);
            }
            "h4" => {
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.33, 1.33);
            }
            "h5" => {
                style.set_font_size(parent.font_size * 0.83);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(1.67, 1.67);
            }
            "h6" => {
                style.set_font_size(parent.font_size * 0.67);
                style.font_weight = FontWeight::BOLD;
                style.set_default_em_margins(2.33, 2.33);
            }
            "small" => style.set_font_size(parent.font_size * 0.85),
            "p" => {
                style.set_default_em_margins(1.0, 1.0);
            }
            "ul" => {
                style.set_default_em_margins(1.0, 1.0);
                style.padding.left = 40.0;
                style.list_style_type = ListStyleType::Disc;
            }
            "ol" => {
                style.set_default_em_margins(1.0, 1.0);
                style.padding.left = 40.0;
                style.list_style_type = ListStyleType::Decimal;
            }
            "hr" => {
                style.margin.top = 8.0;
                style.margin.bottom = 8.0;
                style.border = Edges::all(1.0);
                style.border_color = Rgba::rgb(0x80, 0x80, 0x80);
                style.border_style = BorderLineStyle::Inset;
            }
            "strong" | "b" => style.font_weight = FontWeight::BOLD,
            "em" | "i" => style.font_style = FontStyle::Italic,
            "th" => {
                style.text_align = TextAlign::Center;
                style.font_weight = FontWeight::BOLD;
            }
            _ => {}
        }

        style
    }

    pub(crate) fn set_font_size(&mut self, font_size: f32) {
        self.font_size = font_size.max(1.0);
        if let Some(factor) = self.line_height_factor {
            self.line_height = self.font_size * factor;
        } else if self.line_height_normal {
            self.line_height = normal_line_height_fallback(self.font_size);
        }
        if let Some(factor) = self.margin_top_em {
            self.margin.top = self.font_size * factor;
        }
        if let Some(factor) = self.margin_bottom_em {
            self.margin.bottom = self.font_size * factor;
        }
    }

    pub(crate) fn set_default_em_margins(&mut self, top: f32, bottom: f32) {
        self.margin_top_em = Some(top);
        self.margin_bottom_em = Some(bottom);
        self.margin.top = self.font_size * top;
        self.margin.bottom = self.font_size * bottom;
    }

    pub(crate) fn finalize_border(&mut self) {
        if self.border_style == BorderLineStyle::None {
            self.border = Edges::ZERO;
        }
    }

    pub(crate) fn resolved_padding(&self, basis: f32) -> Edges {
        Edges {
            top: self
                .padding_percent
                .top
                .map(|percent| basis.max(0.0) * percent)
                .unwrap_or(self.padding.top),
            right: self
                .padding_percent
                .right
                .map(|percent| basis.max(0.0) * percent)
                .unwrap_or(self.padding.right),
            bottom: self
                .padding_percent
                .bottom
                .map(|percent| basis.max(0.0) * percent)
                .unwrap_or(self.padding.bottom),
            left: self
                .padding_percent
                .left
                .map(|percent| basis.max(0.0) * percent)
                .unwrap_or(self.padding.left),
        }
    }

    pub(crate) fn set_padding_edge(&mut self, edge: &str, value: &str) {
        let parsed = parse_box_length(value, self.font_size, true);
        let (absolute, percent) = match parsed {
            Some(Length::Percent(value)) => (0.0, Some(value)),
            Some(Length::Px(value)) => (value, None),
            Some(Length::Inherit) | None => (0.0, None),
        };
        match edge {
            "top" => {
                self.padding.top = absolute;
                self.padding_percent.top = percent;
                self.padding_explicit.top = true;
            }
            "right" => {
                self.padding.right = absolute;
                self.padding_percent.right = percent;
                self.padding_explicit.right = true;
            }
            "bottom" => {
                self.padding.bottom = absolute;
                self.padding_percent.bottom = percent;
                self.padding_explicit.bottom = true;
            }
            "left" => {
                self.padding.left = absolute;
                self.padding_percent.left = percent;
                self.padding_explicit.left = true;
            }
            _ => {}
        }
    }

    pub(crate) fn apply_declaration(&mut self, name: &str, value: &str) {
        let value = strip_important(value);
        match name {
            "display" => {
                if let Some(display) = parse_display(value) {
                    self.display = display;
                }
            }
            "width" => {
                self.width_auto = value.trim().eq_ignore_ascii_case("auto");
                self.width = parse_length(value);
            }
            "min-width" => self.min_width = parse_length(value),
            "max-width" => self.max_width = parse_length(value),
            "height" => {
                self.height_auto = value.trim().eq_ignore_ascii_case("auto");
                self.height = parse_length(value);
            }
            "min-height" => self.min_height = parse_length(value),
            "max-height" => self.max_height = parse_length(value),
            "margin" => {
                if let Some((edges, left_auto, right_auto)) =
                    parse_margin_edges(value, self.font_size)
                {
                    self.margin_top_em = None;
                    self.margin_bottom_em = None;
                    self.margin = edges;
                    self.margin_left_auto = left_auto;
                    self.margin_right_auto = right_auto;
                }
            }
            "margin-top" => {
                self.margin_top_em = None;
                self.margin.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-right" => {
                self.margin_right_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-bottom" => {
                self.margin_bottom_em = None;
                self.margin.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "margin-left" => {
                self.margin_left_auto = value.trim().eq_ignore_ascii_case("auto");
                self.margin.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "padding" => {
                if let Some(edges) = parse_edge_lengths_with_font(value, self.font_size, true) {
                    self.padding = Edges {
                        top: absolute_length_or_zero(edges.top),
                        right: absolute_length_or_zero(edges.right),
                        bottom: absolute_length_or_zero(edges.bottom),
                        left: absolute_length_or_zero(edges.left),
                    };
                    self.padding_percent = RelativeEdges {
                        top: percent_length(edges.top),
                        right: percent_length(edges.right),
                        bottom: percent_length(edges.bottom),
                        left: percent_length(edges.left),
                    };
                    self.padding_explicit = EdgeFlags::ALL;
                }
            }
            "padding-top" => self.set_padding_edge("top", value),
            "padding-right" => self.set_padding_edge("right", value),
            "padding-bottom" => self.set_padding_edge("bottom", value),
            "padding-left" => self.set_padding_edge("left", value),
            "background" => {
                if let Some(color) = parse_color(value) {
                    self.background = Some(color);
                }
                if let Some(src) = parse_background_image(value) {
                    self.background_image_src = Some(src);
                    self.background_image = None;
                } else if background_shorthand_removes_image(value) {
                    self.background_image_src = None;
                    self.background_image = None;
                }
                self.background_repeat = parse_background_repeat(value).unwrap_or_default();
                self.background_size = parse_background_size_from_shorthand(value);
                self.background_position = parse_background_position_from_shorthand(value);
            }
            "background-repeat" => {
                if let Some(repeat) = parse_background_repeat(value) {
                    self.background_repeat = repeat;
                }
            }
            "background-size" => {
                if let Some(size) = parse_background_size(value) {
                    self.background_size = size;
                }
            }
            "background-position" => {
                if let Some(position) = parse_background_position(value) {
                    self.background_position = position;
                }
            }
            "background-color" => {
                if let Some(color) = parse_color(value) {
                    self.background = Some(color);
                }
            }
            "background-image" => {
                self.background_image_src = parse_background_image(value);
                self.background_image = None;
            }
            "object-fit" => {
                if let Some(object_fit) = parse_object_fit(value) {
                    self.object_fit = object_fit;
                }
            }
            "object-position" => {
                if let Some(object_position) = parse_object_position(value) {
                    self.object_position = object_position;
                }
            }
            "opacity" => {
                if let Ok(opacity) = value.trim().parse::<f32>() {
                    if opacity.is_finite() {
                        self.opacity = opacity.clamp(0.0, 1.0);
                    }
                }
            }
            "color" => {
                if let Some(color) = parse_color(value) {
                    self.color = color;
                }
            }
            "box-shadow" => {
                if let Some(shadows) = parse_box_shadow(value, self.font_size, self.color) {
                    self.box_shadows = shadows;
                }
            }
            "text-shadow" => {
                if let Some(shadows) = parse_text_shadow(value, self.font_size, self.color) {
                    self.text_shadows = shadows;
                }
            }
            "font-size" => {
                if let Some(font_size) = parse_font_size(value, self.font_size) {
                    self.set_font_size(font_size);
                }
            }
            "font-family" => {
                if let Some(font_family) = parse_font_family(value) {
                    self.font_family = Some(font_family);
                    self.font_face_weight = None;
                }
            }
            "font-weight" if !is_inherit_keyword(value) => {
                self.font_weight = parse_font_weight(value);
            }
            "font-style" if !is_inherit_keyword(value) => {
                self.font_style = parse_font_style(value);
            }
            "line-height" => {
                if let Some(line_height) = parse_line_height_declaration(value, self.font_size) {
                    self.line_height = line_height.height.max(1.0);
                    self.line_height_factor = line_height.factor;
                    self.line_height_normal = line_height.normal;
                }
            }
            "letter-spacing" => {
                self.letter_spacing = if value.trim().eq_ignore_ascii_case("normal") {
                    0.0
                } else {
                    parse_css_length(value, self.font_size, true).unwrap_or(0.0)
                };
            }
            "-webkit-font-smoothing" => {
                self.font_hinting_disabled = value.trim().eq_ignore_ascii_case("antialiased");
            }
            "text-rendering" if value.trim().eq_ignore_ascii_case("geometricprecision") => {
                self.font_hinting_disabled = true;
            }
            "text-align" | "align" => {
                if let Some(align) = parse_text_align(value) {
                    self.text_align = align;
                    self.align_from_attribute = false;
                }
            }
            "text-transform" => {
                if let Some(transform) = parse_text_transform(value) {
                    self.text_transform = transform;
                }
            }
            "vertical-align" => {
                if let Some(align) = parse_vertical_align(value) {
                    self.vertical_align = align;
                }
            }
            "white-space" if value.trim().eq_ignore_ascii_case("nowrap") => {
                self.wrap = TextWrap::None;
            }
            "list-style" | "list-style-type" => {
                if let Some(list_style_type) = parse_list_style_type(value) {
                    self.list_style_type = list_style_type;
                }
            }
            "word-break" if value.trim().eq_ignore_ascii_case("break-all") => {
                self.wrap = TextWrap::Glyph;
            }
            "word-break" if value.trim().eq_ignore_ascii_case("break-word") => {
                self.wrap = TextWrap::WordOrGlyph;
            }
            "overflow-wrap" | "word-wrap" => {
                if matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "break-word" | "anywhere"
                ) {
                    self.wrap = TextWrap::WordOrGlyph;
                }
            }
            "box-sizing" => {
                if let Some(box_sizing) = parse_box_sizing(value) {
                    self.box_sizing = box_sizing;
                }
            }
            "position" => {
                if let Some(position) = parse_position(value) {
                    self.position = position;
                }
            }
            "top" => self.inset_top = parse_length(value),
            "right" => self.inset_right = parse_length(value),
            "bottom" => self.inset_bottom = parse_length(value),
            "left" => self.inset_left = parse_length(value),
            "flex-direction" => {
                if let Some(direction) = parse_flex_direction(value) {
                    self.flex_direction = direction;
                }
            }
            "flex-wrap" => {
                if let Some(wrap) = parse_flex_wrap(value) {
                    self.flex_wrap = wrap;
                }
            }
            "flex-flow" => apply_flex_flow(self, value),
            "justify-content" => {
                if let Some(justify) = parse_justify_content(value) {
                    self.justify_content = justify;
                }
            }
            "align-items" => {
                if let Some(align) = parse_align_items(value) {
                    self.align_items = align;
                }
            }
            "align-self" => {
                self.align_self = parse_align_items(value);
            }
            "gap" => {
                if let Some((row_gap, column_gap)) = parse_gap(value, self.font_size) {
                    self.row_gap = row_gap;
                    self.column_gap = column_gap;
                }
            }
            "row-gap" => {
                self.row_gap = parse_css_length(value, self.font_size, false).unwrap_or(0.0);
            }
            "column-gap" => {
                self.column_gap = parse_css_length(value, self.font_size, false).unwrap_or(0.0);
            }
            "flex" => apply_flex(self, value),
            "flex-grow" => self.flex_grow = parse_flex_factor(value).unwrap_or(self.flex_grow),
            "flex-shrink" => {
                self.flex_shrink = parse_flex_factor(value).unwrap_or(self.flex_shrink)
            }
            "flex-basis" => self.flex_basis = parse_length(value),
            "float" => {
                if let Some(float_side) = parse_float_side(value) {
                    self.float_side = float_side;
                }
            }
            "clear" => {
                if let Some(clear) = parse_clear(value) {
                    self.clear = clear;
                }
            }
            "border" => apply_border(self, value),
            "border-radius" => self.border_radius = parse_radius(value).unwrap_or(0.0).max(0.0),
            "border-style" => {
                if let Some(border_style) = parse_border_line_style(value) {
                    self.border_style = border_style;
                }
            }
            "border-width" => {
                self.border = parse_edges(value).unwrap_or(Edges::ZERO);
            }
            "border-top-width" => {
                self.border.top = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-right-width" => {
                self.border.right = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-bottom-width" => {
                self.border.bottom = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-left-width" => {
                self.border.left = parse_css_length(value, self.font_size, true).unwrap_or(0.0);
            }
            "border-color" => {
                if let Some(color) = parse_color(value) {
                    self.border_color = color;
                }
            }
            "border-top-style"
            | "border-right-style"
            | "border-bottom-style"
            | "border-left-style" => {
                if let Some(border_style) = parse_border_line_style(value) {
                    self.border_style = border_style;
                }
            }
            "border-top" => apply_border_side(self, BorderSide::Top, value),
            "border-right" => apply_border_side(self, BorderSide::Right, value),
            "border-bottom" => apply_border_side(self, BorderSide::Bottom, value),
            "border-left" => apply_border_side(self, BorderSide::Left, value),
            "border-collapse" if value.trim().eq_ignore_ascii_case("collapse") => {
                self.border_collapse = BorderCollapse::Collapse;
            }
            "table-layout" => {
                self.table_layout_fixed = value.trim().eq_ignore_ascii_case("fixed");
            }
            "border-spacing" => self.cell_spacing = parse_px(value).unwrap_or(0.0),
            _ => {}
        }
    }

    pub(crate) fn resolve_width(&self, containing_width: f32) -> Option<f32> {
        let mut width = self.width.and_then(|width| width.resolve(containing_width));
        width = width.map(|width| self.constrain_width(width, containing_width));
        width
    }

    pub(crate) fn constrain_width(&self, width: f32, containing_width: f32) -> f32 {
        let mut width = width;
        if let Some(min_width) = self
            .min_width
            .and_then(|width| width.resolve(containing_width))
        {
            width = width.max(min_width);
        }
        if let Some(max_width) = self
            .max_width
            .and_then(|width| width.resolve(containing_width))
        {
            width = width.min(max_width);
        }
        width
    }

    pub(crate) fn constrain_outer_width(&self, outer_width: f32, containing_width: f32) -> f32 {
        let mut outer_width = outer_width;
        if let Some(min_width) = self
            .min_width
            .and_then(|width| width.resolve(containing_width))
        {
            outer_width = outer_width.max(self.outer_width_for_declared(min_width));
        }
        if let Some(max_width) = self
            .max_width
            .and_then(|width| width.resolve(containing_width))
        {
            outer_width = outer_width.min(self.outer_width_for_declared(max_width));
        }
        outer_width
    }

    pub(crate) fn resolve_height(&self, basis: f32) -> Option<f32> {
        let mut height = self.height.and_then(|height| height.resolve(basis));
        if let Some(min_height) = self.min_height.and_then(|height| height.resolve(basis)) {
            height = Some(height.unwrap_or(min_height).max(min_height));
        }
        if let Some(max_height) = self.max_height.and_then(|height| height.resolve(basis)) {
            height = Some(height.unwrap_or(max_height).min(max_height));
        }
        height
    }

    pub(crate) fn outer_width_for_declared(&self, width: f32) -> f32 {
        match self.box_sizing {
            BoxSizing::BorderBox => width,
            BoxSizing::ContentBox => width + self.padding.horizontal() + self.border.horizontal(),
        }
    }

    pub(crate) fn inner_width_for_outer(&self, width: f32) -> f32 {
        (width - self.padding.horizontal() - self.border.horizontal()).max(1.0)
    }

    pub(crate) fn apply_table_cell_padding(&mut self, padding: Edges) {
        if !self.padding_explicit.top && padding.top > 0.0 {
            self.padding.top = padding.top;
        }
        if !self.padding_explicit.right && padding.right > 0.0 {
            self.padding.right = padding.right;
        }
        if !self.padding_explicit.bottom && padding.bottom > 0.0 {
            self.padding.bottom = padding.bottom;
        }
        if !self.padding_explicit.left && padding.left > 0.0 {
            self.padding.left = padding.left;
        }
    }

    pub(crate) fn horizontal_offset(&self, containing_width: f32, outer_width: f32) -> f32 {
        let fixed_left = if self.margin_left_auto {
            0.0
        } else {
            self.margin.left
        };
        let fixed_right = if self.margin_right_auto {
            0.0
        } else {
            self.margin.right
        };
        let free = (containing_width - outer_width - fixed_left - fixed_right).max(0.0);
        if self.margin_left_auto && self.margin_right_auto {
            fixed_left + free / 2.0
        } else if self.margin_left_auto {
            fixed_left + free
        } else {
            fixed_left
        }
    }

    pub(crate) fn text_attrs(&self) -> Attrs<'_> {
        text_style_attrs(
            self.font_family.as_deref(),
            self.font_weight,
            self.font_face_weight,
            self.font_style,
            self.font_hinting_disabled,
            self.letter_spacing,
            self.font_size,
        )
    }
}

pub(crate) fn strip_important(value: &str) -> &str {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix("!important") {
        return value[..stripped.len()].trim();
    }
    value
}

pub(crate) fn is_inherit_keyword(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "inherit" | "unset"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Display {
    Block,
    Inline,
    InlineBlock,
    InlineTable,
    Flex,
    Table,
    TableRow,
    TableCell,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JustifyContent {
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AlignItems {
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatSide {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clear {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListStyleType {
    Disc,
    Decimal,
    None,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PlacedFloat {
    pub(crate) side: FloatSide,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Length {
    Px(f32),
    Percent(f32),
    Inherit,
}

impl Length {
    pub(crate) fn resolve(self, basis: f32) -> Option<f32> {
        match self {
            Self::Px(value) => Some(value),
            Self::Percent(value) if basis.is_finite() && basis > 0.0 => Some(basis * value),
            Self::Percent(_) => None,
            Self::Inherit if basis.is_finite() && basis > 0.0 => Some(basis),
            Self::Inherit => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Edges {
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) left: f32,
}

impl Edges {
    pub(crate) const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    pub(crate) fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub(crate) fn horizontal(self) -> f32 {
        self.left + self.right
    }

    pub(crate) fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    pub(crate) fn max_width(self) -> f32 {
        self.top.max(self.right).max(self.bottom).max(self.left)
    }

    pub(crate) fn is_zero(self) -> bool {
        self.top == 0.0 && self.right == 0.0 && self.bottom == 0.0 && self.left == 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct RelativeEdges {
    pub(crate) top: Option<f32>,
    pub(crate) right: Option<f32>,
    pub(crate) bottom: Option<f32>,
    pub(crate) left: Option<f32>,
}

impl RelativeEdges {
    pub(crate) const NONE: Self = Self {
        top: None,
        right: None,
        bottom: None,
        left: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ResolvedEdgeLengths {
    pub(crate) top: Length,
    pub(crate) right: Length,
    pub(crate) bottom: Length,
    pub(crate) left: Length,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeFlags {
    pub(crate) top: bool,
    pub(crate) right: bool,
    pub(crate) bottom: bool,
    pub(crate) left: bool,
}

impl EdgeFlags {
    pub(crate) const NONE: Self = Self {
        top: false,
        right: false,
        bottom: false,
        left: false,
    };

    pub(crate) const ALL: Self = Self {
        top: true,
        right: true,
        bottom: true,
        left: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgba {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

impl Rgba {
    pub(crate) const BLACK: Self = Self::rgb(0, 0, 0);
    pub(crate) const WHITE: Self = Self::rgb(255, 255, 255);

    pub(crate) const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub(crate) const fn with_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BoxShadow {
    pub(crate) offset_x: f32,
    pub(crate) offset_y: f32,
    pub(crate) blur_radius: f32,
    pub(crate) spread: f32,
    pub(crate) color: Rgba,
    pub(crate) inset: bool,
}

pub(crate) fn with_opacity(color: Rgba, opacity: f32) -> Rgba {
    if opacity >= 1.0 {
        return color;
    }
    Rgba {
        a: ((f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8),
        ..color
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextAlign {
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub(crate) fn to_cosmic(self) -> TextAlignMode {
        match self {
            Self::Left => TextAlignMode::Left,
            Self::Center => TextAlignMode::Center,
            Self::Right => TextAlignMode::Right,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerticalAlign {
    Baseline,
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextWrap {
    None,
    WordOrGlyph,
    Glyph,
}

impl TextWrap {
    pub(crate) fn to_cosmic(self) -> Wrap {
        match self {
            Self::None => Wrap::None,
            Self::WordOrGlyph => Wrap::WordOrGlyph,
            Self::Glyph => Wrap::Glyph,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorderCollapse {
    Separate,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorderLineStyle {
    None,
    Solid,
    Dashed,
    Inset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoxSizing {
    BorderBox,
    ContentBox,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum BackgroundRepeat {
    #[default]
    Repeat,
    NoRepeat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundSize {
    Auto,
    Cover,
    Contain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ObjectPosition {
    pub(crate) x: PositionAxis,
    pub(crate) y: PositionAxis,
}

impl Default for ObjectPosition {
    fn default() -> Self {
        Self {
            x: PositionAxis::Center,
            y: PositionAxis::Center,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BackgroundPosition {
    pub(crate) x: PositionAxis,
    pub(crate) y: PositionAxis,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BackgroundImagePaint {
    pub(crate) repeat: BackgroundRepeat,
    pub(crate) size: BackgroundSize,
    pub(crate) position: BackgroundPosition,
    pub(crate) radius: f32,
    pub(crate) opacity: f32,
}

impl Default for BackgroundPosition {
    fn default() -> Self {
        Self {
            x: PositionAxis::Start,
            y: PositionAxis::Start,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PositionAxis {
    Start,
    Center,
    End,
}

impl PositionAxis {
    pub(crate) fn factor(self) -> f32 {
        match self {
            Self::Start => 0.0,
            Self::Center => 0.5,
            Self::End => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    pub(crate) const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

pub(crate) fn style_for_node(node: &NodeRef, parent: &Style) -> Style {
    style_for_node_with_fonts(node, parent, &[], &[])
}

pub(crate) fn style_for_node_with_fonts(
    node: &NodeRef,
    parent: &Style,
    available_font_families: &[String],
    web_font_faces: &[WebFontFace],
) -> Style {
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
    if let Some(background) = attrs.get("bgcolor").and_then(parse_html_color_attribute) {
        style.background = Some(background);
    }
    if let Some(border_color) = attrs
        .get("bordercolor")
        .and_then(parse_html_color_attribute)
    {
        style.border_color = border_color;
    }
    if let Some(background_image) = attrs.get("background") {
        style.background_image_src = Some(background_image.trim().to_string());
        style.background_image = None;
    }
    if matches!(tag.as_str(), "td" | "th") && attrs.get("nowrap").is_some() {
        style.wrap = TextWrap::None;
    }
    if let Some(raw_align) = attrs.get("align") {
        if tag == "table" {
            match parse_text_align(raw_align) {
                Some(TextAlign::Left) => {
                    style.float_side = FloatSide::Left;
                }
                Some(TextAlign::Center) => {
                    style.margin_left_auto = true;
                    style.margin_right_auto = true;
                }
                Some(TextAlign::Right) => {
                    style.float_side = FloatSide::Right;
                }
                _ => {}
            }
        } else if let Some(align) = parse_text_align(raw_align) {
            style.text_align = align;
            style.align_from_attribute = true;
        }
    }
    if let Some(vertical_align) = attrs
        .get("valign")
        .or_else(|| attrs.get("vertical-align"))
        .and_then(parse_vertical_align)
    {
        style.vertical_align = vertical_align;
    }
    if tag == "table" {
        style.cell_padding = attrs
            .get("cellpadding")
            .and_then(parse_px)
            .map(Edges::all)
            .unwrap_or(Edges::all(1.0));
        if let Some(cell_spacing) = attrs.get("cellspacing").and_then(parse_px) {
            style.cell_spacing = cell_spacing;
        }
        if let Some(border) = attrs.get("border").and_then(parse_px) {
            if border > 0.0 {
                style.border = Edges::all(border);
                style.border_style = BorderLineStyle::Solid;
            }
        }
    }
    if let Some(style_attr) = attrs.get("style") {
        for (name, value) in css_declarations(style_attr) {
            match name.as_str() {
                "font-family" => {
                    if let Some(selection) =
                        parse_font_family_selection(&value, available_font_families, web_font_faces)
                    {
                        style.font_family = Some(selection.family);
                        style.font_face_weight = selection.forced_weight;
                    }
                }
                "font-weight" if is_inherit_keyword(&value) => {
                    style.font_weight = parent.font_weight;
                }
                "font-style" if is_inherit_keyword(&value) => {
                    style.font_style = parent.font_style;
                }
                _ => style.apply_declaration(&name, &value),
            }
        }
    }
    style.finalize_border();
    style
}

pub(crate) fn default_display(tag: &str) -> Display {
    match tag {
        "html" | "body" | "div" | "p" | "section" | "article" | "header" | "footer" | "main"
        | "center" | "blockquote" | "ul" | "ol" | "li" | "h1" | "h2" | "h3" | "h4" | "h5"
        | "h6" | "hr" => Display::Block,
        "table" => Display::Table,
        "thead" | "tbody" | "tfoot" => Display::Block,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        "script" | "style" | "head" | "meta" | "link" | "title" | "base" => Display::None,
        _ => Display::Inline,
    }
}

pub(crate) fn default_vertical_align(tag: &str, parent: VerticalAlign) -> VerticalAlign {
    match tag {
        "thead" | "tbody" | "tfoot" | "tr" => VerticalAlign::Middle,
        "td" | "th" => parent,
        _ => VerticalAlign::Baseline,
    }
}

pub(crate) fn parse_display(value: &str) -> Option<Display> {
    match value.trim().to_ascii_lowercase().as_str() {
        "block" => Some(Display::Block),
        "inline" => Some(Display::Inline),
        "inline-block" => Some(Display::InlineBlock),
        "inline-table" => Some(Display::InlineTable),
        "flex" | "inline-flex" => Some(Display::Flex),
        "table" => Some(Display::Table),
        "table-row" => Some(Display::TableRow),
        "table-cell" => Some(Display::TableCell),
        "none" => Some(Display::None),
        _ => None,
    }
}

pub(crate) fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "row" => Some(FlexDirection::Row),
        "row-reverse" => Some(FlexDirection::RowReverse),
        "column" => Some(FlexDirection::Column),
        "column-reverse" => Some(FlexDirection::ColumnReverse),
        _ => None,
    }
}

pub(crate) fn parse_flex_wrap(value: &str) -> Option<FlexWrap> {
    match value.trim().to_ascii_lowercase().as_str() {
        "nowrap" => Some(FlexWrap::NoWrap),
        "wrap" => Some(FlexWrap::Wrap),
        "wrap-reverse" => Some(FlexWrap::WrapReverse),
        _ => None,
    }
}

pub(crate) fn parse_justify_content(value: &str) -> Option<JustifyContent> {
    match value.trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" | "left" => Some(JustifyContent::FlexStart),
        "end" | "flex-end" | "right" => Some(JustifyContent::FlexEnd),
        "center" => Some(JustifyContent::Center),
        "space-between" => Some(JustifyContent::SpaceBetween),
        "space-around" => Some(JustifyContent::SpaceAround),
        "space-evenly" => Some(JustifyContent::SpaceEvenly),
        _ => None,
    }
}

pub(crate) fn parse_align_items(value: &str) -> Option<AlignItems> {
    match value.trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" => Some(AlignItems::FlexStart),
        "end" | "flex-end" => Some(AlignItems::FlexEnd),
        "center" => Some(AlignItems::Center),
        "baseline" => Some(AlignItems::Baseline),
        "stretch" | "normal" => Some(AlignItems::Stretch),
        _ => None,
    }
}

pub(crate) fn parse_gap(value: &str, font_size: f32) -> Option<(f32, f32)> {
    if value.trim().eq_ignore_ascii_case("normal") {
        return Some((0.0, 0.0));
    }
    let mut parts = value.split_whitespace();
    let row_gap = parse_css_length(parts.next()?, font_size, false)?;
    let column_gap = parts
        .next()
        .and_then(|value| parse_css_length(value, font_size, false))
        .unwrap_or(row_gap);
    Some((row_gap.max(0.0), column_gap.max(0.0)))
}

pub(crate) fn apply_flex_flow(style: &mut Style, value: &str) {
    for token in value.split_whitespace() {
        if let Some(direction) = parse_flex_direction(token) {
            style.flex_direction = direction;
        } else if let Some(wrap) = parse_flex_wrap(token) {
            style.flex_wrap = wrap;
        }
    }
}

pub(crate) fn apply_flex(style: &mut Style, value: &str) {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        style.flex_grow = 0.0;
        style.flex_shrink = 0.0;
        style.flex_basis = None;
        return;
    }
    if value.eq_ignore_ascii_case("auto") {
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
        style.flex_basis = None;
        return;
    }
    if value.eq_ignore_ascii_case("initial") {
        style.flex_grow = 0.0;
        style.flex_shrink = 1.0;
        style.flex_basis = None;
        return;
    }

    let mut numbers = Vec::new();
    let mut basis = None;
    for token in value.split_whitespace() {
        if let Some(factor) = parse_flex_factor(token) {
            numbers.push(factor);
        } else if token.eq_ignore_ascii_case("auto") {
            basis = None;
        } else if let Some(length) = parse_length(token) {
            basis = Some(length);
        }
    }

    if let Some(grow) = numbers.first().copied() {
        style.flex_grow = grow;
        style.flex_shrink = numbers.get(1).copied().unwrap_or(1.0);
        style.flex_basis = basis.or(Some(Length::Percent(0.0)));
    } else if basis.is_some() {
        style.flex_basis = basis;
    }
}

pub(crate) fn parse_flex_factor(value: &str) -> Option<f32> {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

pub(crate) fn parse_position(value: &str) -> Option<Position> {
    match value.trim().to_ascii_lowercase().as_str() {
        "static" => Some(Position::Static),
        "relative" => Some(Position::Relative),
        "absolute" => Some(Position::Absolute),
        "fixed" => Some(Position::Fixed),
        _ => None,
    }
}

pub(crate) fn parse_list_style_type(value: &str) -> Option<ListStyleType> {
    let lower = value.trim().to_ascii_lowercase();
    if lower.split_whitespace().any(|token| token == "none") {
        return Some(ListStyleType::None);
    }
    if lower
        .split_whitespace()
        .any(|token| matches!(token, "decimal" | "decimal-leading-zero"))
    {
        return Some(ListStyleType::Decimal);
    }
    if lower
        .split_whitespace()
        .any(|token| matches!(token, "disc" | "circle" | "square"))
    {
        return Some(ListStyleType::Disc);
    }
    None
}

pub(crate) fn parse_float_side(value: &str) -> Option<FloatSide> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(FloatSide::None),
        "left" => Some(FloatSide::Left),
        "right" => Some(FloatSide::Right),
        _ => None,
    }
}

pub(crate) fn parse_clear(value: &str) -> Option<Clear> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(Clear::None),
        "left" => Some(Clear::Left),
        "right" => Some(Clear::Right),
        "both" => Some(Clear::Both),
        _ => None,
    }
}

pub(crate) fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    if matches!(value.to_ascii_lowercase().as_str(), "inherit" | "unset") {
        return Some(Length::Inherit);
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

pub(crate) fn parse_px(value: &str) -> Option<f32> {
    parse_css_length(value, 16.0, true)
}

pub(crate) fn parse_css_length(value: &str, font_size: f32, allow_unitless: bool) -> Option<f32> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let (number, multiplier) = if let Some(number) = lower.strip_suffix("rem") {
        (number, 16.0)
    } else if let Some(number) = lower.strip_suffix("em") {
        (number, font_size.max(1.0))
    } else if let Some(number) = lower.strip_suffix("px") {
        (number, 1.0)
    } else if let Some(number) = lower.strip_suffix("pt") {
        (number, 96.0 / 72.0)
    } else if allow_unitless {
        (lower.as_str(), 1.0)
    } else {
        return None;
    };

    number
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value * multiplier)
}

pub(crate) fn parse_font_size(value: &str, parent_font_size: f32) -> Option<f32> {
    parse_css_length(value, parent_font_size, false).or_else(|| {
        value
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && *value == 0.0)
    })
}

pub(crate) fn parse_edges(value: &str) -> Option<Edges> {
    parse_edges_with_font(value, 16.0)
}

pub(crate) fn parse_margin_edges(value: &str, font_size: f32) -> Option<(Edges, bool, bool)> {
    let values: Vec<(&str, f32, bool)> = value
        .split_whitespace()
        .filter_map(|token| {
            let is_auto = token.eq_ignore_ascii_case("auto");
            parse_css_length(token, font_size, true)
                .or(Some(0.0).filter(|_| is_auto))
                .map(|length| (token, length, is_auto))
        })
        .collect();

    let expanded = match values.as_slice() {
        [all] => [all, all, all, all],
        [vertical, horizontal] => [vertical, horizontal, vertical, horizontal],
        [top, horizontal, bottom] => [top, horizontal, bottom, horizontal],
        [top, right, bottom, left, ..] => [top, right, bottom, left],
        _ => return None,
    };

    Some((
        Edges {
            top: expanded[0].1,
            right: expanded[1].1,
            bottom: expanded[2].1,
            left: expanded[3].1,
        },
        expanded[3].2,
        expanded[1].2,
    ))
}

pub(crate) fn parse_edges_with_font(value: &str, font_size: f32) -> Option<Edges> {
    let values: Vec<f32> = value
        .split_whitespace()
        .filter_map(|token| {
            parse_css_length(token, font_size, true).or(Some(0.0).filter(|_| token == "auto"))
        })
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

pub(crate) fn parse_edge_lengths_with_font(
    value: &str,
    font_size: f32,
    allow_unitless: bool,
) -> Option<ResolvedEdgeLengths> {
    let values: Vec<Length> = value
        .split_whitespace()
        .filter_map(|token| parse_box_length(token, font_size, allow_unitless))
        .collect();

    let expanded = match values.as_slice() {
        [all] => [*all, *all, *all, *all],
        [vertical, horizontal] => [*vertical, *horizontal, *vertical, *horizontal],
        [top, horizontal, bottom] => [*top, *horizontal, *bottom, *horizontal],
        [top, right, bottom, left, ..] => [*top, *right, *bottom, *left],
        _ => return None,
    };

    Some(ResolvedEdgeLengths {
        top: expanded[0],
        right: expanded[1],
        bottom: expanded[2],
        left: expanded[3],
    })
}

pub(crate) fn parse_box_length(
    value: &str,
    font_size: f32,
    allow_unitless: bool,
) -> Option<Length> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if value.eq_ignore_ascii_case("auto") || value.is_empty() {
        return None;
    }
    if matches!(value.to_ascii_lowercase().as_str(), "inherit" | "unset") {
        return Some(Length::Inherit);
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| Length::Percent(value / 100.0));
    }
    parse_css_length(value, font_size, allow_unitless).map(Length::Px)
}

pub(crate) fn absolute_length_or_zero(length: Length) -> f32 {
    match length {
        Length::Px(value) => value,
        Length::Percent(_) | Length::Inherit => 0.0,
    }
}

pub(crate) fn percent_length(length: Length) -> Option<f32> {
    match length {
        Length::Percent(value) => Some(value),
        Length::Px(_) | Length::Inherit => None,
    }
}

pub(crate) fn parse_radius(value: &str) -> Option<f32> {
    let token = value.split_whitespace().next()?.trim();
    if let Some(percent) = token.strip_suffix('%') {
        let percent = percent.trim().parse::<f32>().ok()?;
        return (percent > 0.0).then_some(if percent >= 50.0 {
            1_000_000.0
        } else {
            percent
        });
    }
    parse_px(token)
}

pub(crate) fn parse_color(value: &str) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        if let Some(color) = parse_hex_color(hex) {
            return Some(color);
        }
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

pub(crate) fn parse_html_color_attribute(value: &str) -> Option<Rgba> {
    let value = value.trim().trim_matches(['"', '\'']);
    parse_color(value).or_else(|| parse_hex_color(value))
}

pub(crate) fn parse_box_shadow(
    value: &str,
    font_size: f32,
    default_color: Rgba,
) -> Option<Vec<BoxShadow>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }

    let mut shadows = Vec::new();
    for shadow in split_css_top_level_list(value, ',') {
        let mut lengths = Vec::new();
        let mut color = None;
        let mut inset = false;
        for token in css_top_level_whitespace_tokens(shadow) {
            if token.eq_ignore_ascii_case("inset") {
                inset = true;
                continue;
            }
            if let Some(parsed_color) = parse_color(&token) {
                color = Some(parsed_color);
                continue;
            }
            if let Some(length) = parse_css_length(&token, font_size, true) {
                lengths.push(length);
            }
        }
        if lengths.len() < 2 {
            continue;
        }
        shadows.push(BoxShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lengths.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(default_color),
            inset,
        });
    }

    Some(shadows)
}

pub(crate) fn parse_text_shadow(
    value: &str,
    font_size: f32,
    default_color: Rgba,
) -> Option<Vec<BoxShadow>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }

    let mut shadows = Vec::new();
    for shadow in split_css_top_level_list(value, ',') {
        let mut lengths = Vec::new();
        let mut color = None;
        for token in css_top_level_whitespace_tokens(shadow) {
            if let Some(parsed_color) = parse_color(&token) {
                color = Some(parsed_color);
                continue;
            }
            if let Some(length) = parse_css_length(&token, font_size, true) {
                lengths.push(length);
            }
        }
        if lengths.len() < 2 {
            continue;
        }
        shadows.push(BoxShadow {
            offset_x: lengths[0],
            offset_y: lengths[1],
            blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: 0.0,
            color: color.unwrap_or(default_color),
            inset: false,
        });
    }

    Some(shadows)
}

pub(crate) fn split_css_top_level_list(value: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut paren_depth = 0usize;

    for (index, ch) in value.char_indices() {
        if let Some(current_quote) = quote {
            if ch == current_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ if ch == separator && paren_depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(value[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

pub(crate) fn css_top_level_whitespace_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut quote = None;
    let mut paren_depth = 0usize;

    for (index, ch) in value.char_indices() {
        if start.is_none() && !ch.is_whitespace() {
            start = Some(index);
        }

        if let Some(current_quote) = quote {
            if ch == current_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => paren_depth += 1,
            ')' => paren_depth = paren_depth.saturating_sub(1),
            _ if ch.is_whitespace() && paren_depth == 0 => {
                if let Some(token_start) = start.take() {
                    tokens.push(value[token_start..index].trim().to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(token_start) = start {
        tokens.push(value[token_start..].trim().to_string());
    }
    tokens
        .into_iter()
        .filter(|token| !token.is_empty())
        .collect()
}

pub(crate) fn parse_background_image(value: &str) -> Option<String> {
    let value = strip_important(value).trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    first_css_url(value).map(|url| unquote_css_value(&url))
}

pub(crate) fn background_shorthand_removes_image(value: &str) -> bool {
    let value = strip_important(value).trim();
    value.eq_ignore_ascii_case("none") || !value.to_ascii_lowercase().contains("url(")
}

pub(crate) fn parse_background_repeat(value: &str) -> Option<BackgroundRepeat> {
    let lower = strip_important(value).to_ascii_lowercase();
    if lower.contains("no-repeat") {
        Some(BackgroundRepeat::NoRepeat)
    } else if lower.contains("repeat") {
        Some(BackgroundRepeat::Repeat)
    } else {
        None
    }
}

pub(crate) fn parse_background_size(value: &str) -> Option<BackgroundSize> {
    let value = strip_important(value).trim().to_ascii_lowercase();
    match value.split_whitespace().next()? {
        "auto" => Some(BackgroundSize::Auto),
        "cover" => Some(BackgroundSize::Cover),
        "contain" => Some(BackgroundSize::Contain),
        _ => None,
    }
}

pub(crate) fn parse_background_size_from_shorthand(value: &str) -> BackgroundSize {
    strip_important(value)
        .split_once('/')
        .and_then(|(_, size)| parse_background_size(size))
        .unwrap_or(BackgroundSize::Auto)
}

pub(crate) fn parse_object_fit(value: &str) -> Option<ObjectFit> {
    match strip_important(value).trim().to_ascii_lowercase().as_str() {
        "fill" => Some(ObjectFit::Fill),
        "contain" => Some(ObjectFit::Contain),
        "cover" => Some(ObjectFit::Cover),
        "none" => Some(ObjectFit::None),
        "scale-down" => Some(ObjectFit::ScaleDown),
        _ => None,
    }
}

pub(crate) fn parse_object_position(value: &str) -> Option<ObjectPosition> {
    let position = parse_position_keywords(value)?;
    Some(ObjectPosition {
        x: position.x,
        y: position.y,
    })
}

pub(crate) fn parse_background_position_from_shorthand(value: &str) -> BackgroundPosition {
    let position = strip_important(value)
        .split_once('/')
        .map_or(value, |(position, _)| position);
    parse_background_position(position).unwrap_or_default()
}

pub(crate) fn parse_background_position(value: &str) -> Option<BackgroundPosition> {
    parse_position_keywords(value)
}

pub(crate) fn parse_position_keywords(value: &str) -> Option<BackgroundPosition> {
    let mut x = None;
    let mut y = None;
    let mut saw_keyword = false;

    for keyword in background_position_keywords(value) {
        saw_keyword = true;
        match keyword {
            PositionKeyword::Left => x = Some(PositionAxis::Start),
            PositionKeyword::Right => x = Some(PositionAxis::End),
            PositionKeyword::Top => y = Some(PositionAxis::Start),
            PositionKeyword::Bottom => y = Some(PositionAxis::End),
            PositionKeyword::Center => {
                if x.is_none() {
                    x = Some(PositionAxis::Center);
                } else if y.is_none() {
                    y = Some(PositionAxis::Center);
                }
            }
        }
    }

    saw_keyword.then_some(BackgroundPosition {
        x: x.unwrap_or(PositionAxis::Center),
        y: y.unwrap_or(PositionAxis::Center),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PositionKeyword {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

pub(crate) fn background_position_keywords(value: &str) -> Vec<PositionKeyword> {
    css_ident_tokens_without_functions(value)
        .into_iter()
        .filter_map(|token| match token.as_str() {
            "left" => Some(PositionKeyword::Left),
            "right" => Some(PositionKeyword::Right),
            "top" => Some(PositionKeyword::Top),
            "bottom" => Some(PositionKeyword::Bottom),
            "center" => Some(PositionKeyword::Center),
            _ => None,
        })
        .collect()
}

pub(crate) fn css_ident_tokens_without_functions(value: &str) -> Vec<String> {
    let mut scrubbed = String::with_capacity(value.len());
    let mut paren_depth = 0usize;
    for ch in strip_important(value).chars() {
        match ch {
            '(' => {
                paren_depth += 1;
                scrubbed.push(' ');
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                scrubbed.push(' ');
            }
            _ if paren_depth > 0 => scrubbed.push(' '),
            ',' | '/' => scrubbed.push(' '),
            _ => scrubbed.push(ch),
        }
    }

    scrubbed
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

pub(crate) fn parse_color_token(value: &str) -> Option<Rgba> {
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
        "transparent" => Some(Rgba::with_alpha(0, 0, 0, 0)),
        _ => None,
    }
}

pub(crate) fn parse_hex_color(hex: &str) -> Option<Rgba> {
    match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Rgba::rgb(r, g, b))
        }
        4 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            let a = u8::from_str_radix(&hex[3..4].repeat(2), 16).ok()?;
            Some(Rgba::with_alpha(r, g, b, a))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Rgba::rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some(Rgba::with_alpha(r, g, b, a))
        }
        _ => None,
    }
}

pub(crate) fn parse_rgb_function(value: &str) -> Option<Rgba> {
    let start = value.find('(')?;
    let end = value.rfind(')')?;
    let body = value[start + 1..end].trim();
    if body.contains(',') {
        let channels: Vec<&str> = body.split(',').collect();
        if channels.len() < 3 {
            return None;
        }
        let r = parse_rgb_channel(channels[0])?;
        let g = parse_rgb_channel(channels[1])?;
        let b = parse_rgb_channel(channels[2])?;
        let a = channels
            .get(3)
            .and_then(|alpha| parse_alpha_channel(alpha))
            .unwrap_or(255);
        return Some(Rgba::with_alpha(r, g, b, a));
    }

    let (channels, alpha) = body
        .split_once('/')
        .map_or((body, None), |(channels, alpha)| (channels, Some(alpha)));
    let mut channels = channels.split_whitespace();
    let r = parse_rgb_channel(channels.next()?)?;
    let g = parse_rgb_channel(channels.next()?)?;
    let b = parse_rgb_channel(channels.next()?)?;
    let a = alpha.and_then(parse_alpha_channel).unwrap_or(255);
    Some(Rgba::with_alpha(r, g, b, a))
}

pub(crate) fn parse_rgb_channel(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (value.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(0.0, 255.0) as u8)
}

pub(crate) fn parse_alpha_channel(value: &str) -> Option<u8> {
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (value.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

pub(crate) fn parse_text_align(value: &str) -> Option<TextAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" | "start" => Some(TextAlign::Left),
        "center" | "middle" => Some(TextAlign::Center),
        "right" | "end" => Some(TextAlign::Right),
        _ => None,
    }
}

pub(crate) fn parse_text_transform(value: &str) -> Option<TextTransform> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(TextTransform::None),
        "uppercase" => Some(TextTransform::Uppercase),
        "lowercase" => Some(TextTransform::Lowercase),
        "capitalize" => Some(TextTransform::Capitalize),
        _ => None,
    }
}

pub(crate) fn parse_vertical_align(value: &str) -> Option<VerticalAlign> {
    match value.trim().to_ascii_lowercase().as_str() {
        "baseline" => Some(VerticalAlign::Baseline),
        "top" | "text-top" => Some(VerticalAlign::Top),
        "center" | "middle" => Some(VerticalAlign::Middle),
        "bottom" | "text-bottom" => Some(VerticalAlign::Bottom),
        _ => None,
    }
}

pub(crate) fn parse_box_sizing(value: &str) -> Option<BoxSizing> {
    match value.trim().to_ascii_lowercase().as_str() {
        "border-box" => Some(BoxSizing::BorderBox),
        "content-box" => Some(BoxSizing::ContentBox),
        _ => None,
    }
}

pub(crate) fn parse_font_family(value: &str) -> Option<String> {
    parse_font_family_with_available(value, &[])
}

pub(crate) fn parse_font_family_with_available(
    value: &str,
    available_font_families: &[String],
) -> Option<String> {
    parse_font_family_selection(value, available_font_families, &[])
        .map(|selection| selection.family)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontFamilySelection {
    pub(crate) family: String,
    pub(crate) forced_weight: Option<FontWeight>,
}

pub(crate) fn parse_font_family_selection(
    value: &str,
    available_font_families: &[String],
    web_font_faces: &[WebFontFace],
) -> Option<FontFamilySelection> {
    if font_family_value_has_invalid_unquoted_colon(value) {
        return None;
    }

    let candidates = parse_font_family_candidates(value);

    if let Some(first) = candidates.first() {
        if let Some(generic) = generic_font_family(first) {
            return Some(FontFamilySelection {
                family: generic.to_string(),
                forced_weight: None,
            });
        }
    }

    for family in &candidates {
        if let Some(selection) = web_font_selection_for_family(family, web_font_faces) {
            return Some(selection);
        }
        if available_font_families
            .iter()
            .any(|available| available.eq_ignore_ascii_case(family))
        {
            return Some(FontFamilySelection {
                family: family.clone(),
                forced_weight: None,
            });
        }
    }

    if !available_font_families.is_empty() {
        for family in &candidates {
            if let Some(generic) = generic_font_family(family) {
                return Some(FontFamilySelection {
                    family: generic.to_string(),
                    forced_weight: None,
                });
            }
        }
        for family in &candidates {
            if let Some(generic) = crate::font_catalog::safe_system_font_generic(family) {
                return Some(FontFamilySelection {
                    family: generic.to_string(),
                    forced_weight: None,
                });
            }
        }
    }

    for family in &candidates {
        if is_safe_system_font(family) {
            return Some(FontFamilySelection {
                family: family.clone(),
                forced_weight: None,
            });
        }
    }
    for family in &candidates {
        if let Some(generic) = generic_font_family(family) {
            return Some(FontFamilySelection {
                family: generic.to_string(),
                forced_weight: None,
            });
        }
    }
    candidates
        .into_iter()
        .next()
        .map(|family| FontFamilySelection {
            family,
            forced_weight: None,
        })
}

pub(crate) fn font_family_value_has_invalid_unquoted_colon(value: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        match quote {
            Some(current) if ch == current => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ':' => return true,
            None => {}
        }
    }
    false
}

pub(crate) fn web_font_selection_for_family(
    family: &str,
    web_font_faces: &[WebFontFace],
) -> Option<FontFamilySelection> {
    let mut matched = web_font_faces
        .iter()
        .filter(|face| face.css_family.eq_ignore_ascii_case(family));
    let first = matched.next()?;
    let mut weights = vec![first.weight];
    for face in matched {
        if !weights.iter().any(|weight| weight.0 == face.weight.0) {
            weights.push(face.weight);
        }
    }

    Some(FontFamilySelection {
        family: first.actual_family.clone(),
        forced_weight: (weights.len() == 1).then_some(weights[0]),
    })
}

pub(crate) fn parse_font_family_candidates(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|candidate| {
            let family = candidate.trim().trim_matches('"').trim_matches('\'').trim();
            if family.is_empty()
                || matches!(
                    family.to_ascii_lowercase().as_str(),
                    "inherit" | "initial" | "unset"
                )
            {
                None
            } else {
                Some(family.to_string())
            }
        })
        .collect()
}

pub(crate) use crate::font_catalog::generic_font_family;
pub(crate) use crate::font_catalog::is_safe_system_font;

pub(crate) fn parse_font_weight(value: &str) -> FontWeight {
    match value.trim().to_ascii_lowercase().as_str() {
        "bold" | "bolder" => FontWeight::BOLD,
        "normal" | "lighter" => FontWeight::NORMAL,
        raw => raw
            .parse::<u16>()
            .ok()
            .map(FontWeight)
            .unwrap_or(FontWeight::NORMAL),
    }
}

pub(crate) fn parse_font_style(value: &str) -> FontStyle {
    match value.trim().to_ascii_lowercase().as_str() {
        "italic" => FontStyle::Italic,
        "oblique" => FontStyle::Oblique,
        _ => FontStyle::Normal,
    }
}

pub(crate) fn apply_border(style: &mut Style, value: &str) {
    if value.contains("none") {
        style.border = Edges::ZERO;
        style.border_style = BorderLineStyle::None;
        return;
    }
    if let Some(border_style) = parse_border_line_style(value) {
        style.border_style = border_style;
    }

    let mut saw_width = false;
    for token in value.split_whitespace() {
        if let Some(width) = parse_px(token) {
            style.border = Edges::all(width);
            saw_width = true;
        }
        if let Some(color) = parse_color(token) {
            style.border_color = color;
        }
    }

    if !saw_width && !value.trim().is_empty() {
        style.border = Edges::all(1.0);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum BorderSide {
    Top,
    Right,
    Bottom,
    Left,
}

pub(crate) fn apply_border_side(style: &mut Style, side: BorderSide, value: &str) {
    if value.contains("none") {
        set_border_side(&mut style.border, side, 0.0);
        style.border_style = BorderLineStyle::None;
        return;
    }
    if let Some(border_style) = parse_border_line_style(value) {
        if style.border_style == BorderLineStyle::None
            || !matches!(border_style, BorderLineStyle::Solid)
        {
            style.border_style = border_style;
        }
    }

    let mut saw_width = false;
    for token in value.split_whitespace() {
        if let Some(width) = parse_px(token) {
            set_border_side(&mut style.border, side, width);
            saw_width = true;
        }
        if let Some(color) = parse_color(token) {
            style.border_color = color;
        }
    }

    if !saw_width && !value.trim().is_empty() {
        set_border_side(&mut style.border, side, 1.0);
    }
}

pub(crate) fn parse_border_line_style(value: &str) -> Option<BorderLineStyle> {
    for token in value.split_whitespace() {
        match token.to_ascii_lowercase().as_str() {
            "none" | "hidden" => return Some(BorderLineStyle::None),
            "dashed" | "dotted" => return Some(BorderLineStyle::Dashed),
            "inset" | "groove" => return Some(BorderLineStyle::Inset),
            "solid" => return Some(BorderLineStyle::Solid),
            _ => {}
        }
    }
    None
}

pub(crate) fn set_border_side(border: &mut Edges, side: BorderSide, width: f32) {
    match side {
        BorderSide::Top => border.top = width,
        BorderSide::Right => border.right = width,
        BorderSide::Bottom => border.bottom = width,
        BorderSide::Left => border.left = width,
    }
}
