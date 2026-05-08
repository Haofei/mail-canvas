use super::*;
use crate::layout::LayoutDebugMeta;
use crate::style::{
    AlignItems, Clear, FlexDirection, FlexWrap, FloatSide, JustifyContent, Position, TextWrap,
    parse_color, parse_edges_with_font, parse_font_family, parse_font_family_selection,
    parse_font_family_with_available,
};
use crate::text::{
    blink_web_standard_ascent_adjustment_applies, blink_web_standard_family_ascent_adjustment,
    fontdb_family, normal_line_height_fallback, parse_line_height_declaration,
};

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
    assert!(html.find("email-render-defaults") < html.find("<title>x</title>"));
}

#[test]
fn injects_uppercase_document_head_without_lowercase_copy() {
    let html = build_document(
        "<HTML><HEAD><title>x</title></HEAD><BODY>Hi</BODY></HTML>",
        None,
        None,
        640,
    );
    assert!(html.contains("<title>x</title>"));
    assert!(html.contains("email-render-defaults"));
    assert!(html.find("email-render-defaults") < html.find("<title>x</title>"));
}

#[test]
fn renderer_defaults_precede_author_head_styles() {
    let html = build_document(
        "<html><head><style>img { display:inline }</style></head><body><img></body></html>",
        None,
        None,
        640,
    );

    assert!(html.find("email-render-defaults") < html.find("img { display:inline }"));
}

#[test]
fn inlines_css_before_rendering() {
    let html = build_document(
        "<p class=\"x\">Hello</p>",
        Some(".x { color: #f00; }"),
        None,
        600,
    );
    let inlined = inline_css(&html, 600, 800).unwrap();
    assert!(
        inlined.contains("color:red") || inlined.contains("color: red"),
        "{inlined}"
    );
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
    let inlined = inline_css(&html, 600, 800).unwrap();
    assert!(
        inlined.contains("text-shadow:0 1px 0 #fff")
            || inlined.contains("text-shadow: 0 1px 0 #fff")
            || inlined.contains("text-shadow:0 1px #fff")
            || inlined.contains("text-shadow: 0 1px #fff")
            || inlined.contains("text-shadow:0 1px 0 white")
            || inlined.contains("text-shadow: 0 1px 0 white"),
        "{inlined}"
    );
}

#[test]
fn inliner_ignores_hidden_mso_conditional_styles() {
    let html = build_document(
        r#"<style>.x { color: red; }</style><!--[if mso]><style>.x { color: blue; }</style><![endif]--><p class="x">Hello</p>"#,
        None,
        None,
        600,
    );
    let inlined = inline_css(&html, 600, 800).unwrap();
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
    let inlined = inline_css(&html, 600, 800).unwrap();
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
    let inlined = inline_css(&html, 600, 800).unwrap();
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
    let inlined = inline_css(&html, 600, 800).unwrap();
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
fn css_display_table_cells_share_one_row() {
    let layout = layout_for_test(
        r#"
        <div style="display:table;width:600px">
          <div style="display:table-cell;width:33.333333%;vertical-align:top">
            <img width="100" height="50" alt="">
          </div>
          <div style="display:table-cell;width:33.333333%;vertical-align:top">
            <img width="100" height="70" alt="">
          </div>
          <div style="display:table-cell;width:33.333333%;vertical-align:top">
            <img width="100" height="60" alt="">
          </div>
        </div>
        "#,
        800,
    );
    let tables: Vec<&LayoutBox> = collect_layouts(&layout, &|child| {
        matches!(child.kind, LayoutKind::Table) && child.style.display == Display::Table
    });
    let table = tables.last().expect("css table");
    assert_eq!(table.children.len(), 1);
    let row = &table.children[0];
    assert_eq!(row.children.len(), 3);
    assert!((table.rect.width - 600.0).abs() < 0.1);
    assert!(
        table.rect.height < 80.0,
        "cells should occupy one row instead of stacking, got height {}",
        table.rect.height
    );
    assert!((row.children[0].rect.width - 200.0).abs() < 0.5);
    assert!((row.children[1].rect.x - row.children[0].rect.x - 200.0).abs() < 0.5);
    assert!((row.children[2].rect.x - row.children[0].rect.x - 400.0).abs() < 0.5);
}

#[test]
fn blockified_table_cells_stack_within_their_row() {
    let layout = layout_for_test(
        r#"
        <table width="262" style="border-collapse:collapse">
          <tr>
            <th style="display:block;padding:0" width="262"><img width="262" height="40" alt=""></th>
            <th style="display:block;padding:0" width="262"><img width="262" height="80" alt=""></th>
          </tr>
        </table>
        "#,
        600,
    );
    let table =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
    let row = &table.children[0];
    assert_eq!(row.children.len(), 2);
    assert!((row.children[0].rect.x - row.children[1].rect.x).abs() < 0.1);
    assert!(row.children[1].rect.y >= row.children[0].rect.y + row.children[0].rect.height - 0.1);
    assert!(
        table.rect.height >= 120.0 && table.rect.height < 125.0,
        "blockified cells should stack vertically, got table height {}",
        table.rect.height
    );
}

#[test]
fn blockified_nested_table_cells_do_not_expand_parent_table_width() {
    let layout = layout_for_test(
        r#"
        <table width="600" style="background:#231e15">
          <tr>
            <td style="padding:0 28px">
              <table width="544" style="table-layout:fixed">
                <tr>
                  <th style="padding:0">
                    <table width="262">
                      <tr>
                        <th style="display:block;padding:0" width="262"><img width="262" height="40" alt=""></th>
                        <th style="display:block;padding:0;background:#b42855" width="262"><img width="262" height="80" alt=""></th>
                      </tr>
                    </table>
                  </th>
                  <th width="20" style="padding:0">&nbsp;</th>
                  <th style="padding:0">
                    <table width="262">
                      <tr>
                        <th style="display:block;padding:0" width="262"><img width="262" height="40" alt=""></th>
                        <th style="display:block;padding:0;background:#ff7346" width="262"><img width="262" height="80" alt=""></th>
                      </tr>
                    </table>
                  </th>
                </tr>
              </table>
            </td>
          </tr>
        </table>
        "#,
        800,
    );
    let root_table =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
    assert!(
        (root_table.rect.width - 600.0).abs() < 0.1,
        "blockified nested cells should not expand parent background to {}",
        root_table.rect.width
    );
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
fn blink_web_standard_ascent_adjustment_matches_web_standard_families() {
    assert!(blink_web_standard_ascent_adjustment_applies(Some(
        "Helvetica"
    )));
    assert!(blink_web_standard_ascent_adjustment_applies(Some(
        "SANS-SERIF"
    )));
    assert!(blink_web_standard_ascent_adjustment_applies(Some("serif")));
    assert!(blink_web_standard_ascent_adjustment_applies(None));
    assert!(!blink_web_standard_ascent_adjustment_applies(Some(
        "Helvetica Neue"
    )));
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
    let edges = parse_edges_with_font(".4EM 0 1.1875em", 16.0).unwrap();
    assert!((edges.top - 6.4).abs() < 0.1);
    assert!((edges.bottom - 19.0).abs() < 0.1);

    let mut style = Style::initial();
    style.apply_declaration("width", "120PX");
    assert_eq!(style.width, Some(Length::Px(120.0)));
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
fn heading_font_size_em_uses_parent_font_size() {
    let layout = layout_for_test(
        r#"<div style="font-size:16px"><h2 style="font-size:4.5em;line-height:0.85em;margin:0">Title</h2></div>"#,
        600,
    );
    let title = find_layout(&layout, |child| child.debug.tag == "h2").expect("heading");

    assert!((title.style.font_size - 72.0).abs() < 0.1);
    assert!((title.style.line_height - 61.2).abs() < 0.1);
}

#[test]
fn entity_quoted_inline_font_family_preserves_following_declarations() {
    let layout = layout_for_test(
        r#"<table width="500"><tr><td style="font-size:0"><a style="display:block;font-family:&quot;Roboto Condensed&quot;, Helvetica, Arial, sans-serif;font-size:18px;line-height:100%;padding:18px 0;background:#231F20;color:#fff">CTA</a></td></tr></table>"#,
        600,
    );
    let link = find_layout(&layout, |child| child.debug.tag == "a").expect("link");

    assert!((link.style.font_size - 18.0).abs() < 0.1);
    assert!(
        link.rect.height >= 50.0,
        "button link should keep text and vertical padding, got {}",
        link.rect.height
    );
}

#[test]
fn inherited_font_weight_keeps_parent_weight() {
    let mut parent = Style::initial();
    parent.font_weight = FontWeight::BOLD;
    let mut child = Style::from_parent_for_tag(&parent, "h2");
    child.apply_declaration("font-weight", "INHERIT");
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
    let available = FontFamilyIndex::from_families(["Nunito Sans"]);
    let family = parse_font_family_with_available(
        r#""Nunito Sans", Helvetica, Arial, sans-serif"#,
        &available,
    )
    .unwrap();
    assert_eq!(family, "Nunito Sans");
}

#[test]
fn unavailable_safe_system_font_uses_declared_generic_fallback() {
    let available = FontFamilyIndex::from_families(["Arimo", "Noto Sans"]);
    let family =
        parse_font_family_with_available("Arial, Helvetica, sans-serif", &available).unwrap();
    assert_eq!(family, "sans-serif");

    let family =
        parse_font_family_with_available("Georgia, Times New Roman, serif", &available).unwrap();
    assert_eq!(family, "serif");

    let family =
        parse_font_family_with_available("Trebuchet MS, Verdana, Tahoma", &available).unwrap();
    assert_eq!(family, "sans-serif");
}

#[test]
fn invalid_font_family_declaration_is_ignored() {
    assert!(parse_font_family(r#"" undefined: IowanOldStyle" undefined: , P052, serif"#).is_none());
    assert!(parse_font_family("INITIAL").is_none());
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
    let selection = parse_font_family_selection(
        r#""Merriweather", Georgia, serif"#,
        &FontFamilyIndex::default(),
        &faces,
    )
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
    let selection = parse_font_family_selection(
        r#""Work Sans", Arial, sans-serif"#,
        &FontFamilyIndex::default(),
        &faces,
    )
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
    let available = FontFamilyIndex::from_families(["Georgia"]);
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
fn mail_canvas_fallback_uses_color_emoji_font_for_emoji() {
    let mut font_system = FontSystem::new_with_locale_and_db_and_fallback(
        "en-US".to_string(),
        system_font_database(),
        MailCanvasFontFallback,
    );
    let mut buffer = Buffer::new_empty(Metrics::new(20.0, 24.0));
    buffer.set_size(&mut font_system, Some(240.0), Some(48.0));
    buffer.set_text(
        &mut font_system,
        "React 😍",
        &Attrs::new().family(cosmic_text::Family::SansSerif),
        Shaping::Advanced,
        None,
    );

    let emoji = buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .find(|glyph| "React 😍"[glyph.start..glyph.end].contains('😍'))
        .expect("emoji glyph");
    let face = font_system.db().face(emoji.font_id).expect("font face");
    assert_ne!(emoji.glyph_id, 0);
    assert!(
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case("Noto Color Emoji")),
        "expected Noto Color Emoji fallback, got {:?}",
        face.families
    );
}

#[test]
fn important_longhand_declarations_override_later_shorthand() {
    let layout = layout_for_test(
        r#"<div style="padding-left: 24px !important; padding: 48px; background: #000">Hello</div>"#,
        200,
    );
    let block =
        find_layout(&layout, |child| child.style.background == Some(Rgba::BLACK)).expect("block");
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
    style.apply_declaration("border-top", "10px DASHED #22BC66");
    style.apply_declaration("border-right", "18px SOLID #22BC66");
    assert_eq!(style.border.top, 10.0);
    assert_eq!(style.border.right, 18.0);
    assert_eq!(style.border_color, Rgba::rgb(0x22, 0xbc, 0x66));
    assert_eq!(style.border_style, BorderLineStyle::Dashed);

    style.apply_declaration("border-style", "INSET");
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
    style.apply_declaration(
        "background",
        "#fff URL('hero-2.jpg') center/cover no-repeat",
    );
    assert_eq!(style.background_image_src.as_deref(), Some("hero-2.jpg"));

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
        .one(r##"<div style="color: #111111 !IMPORTANT; color: #222222">A</div>"##);
    let div = find_first_tag(&document, "div").expect("div");
    let style = style_for_node(&div, &Style::initial());

    assert_eq!(style.color, Rgba::rgb(0x11, 0x11, 0x11));
}

#[test]
fn parses_flex_container_style_model() {
    let mut style = Style::initial();
    style.apply_declaration("display", "FLEX");
    style.apply_declaration("flex-flow", "COLUMN WRAP");
    style.apply_declaration("justify-content", "SPACE-BETWEEN");
    style.apply_declaration("align-items", "CENTER");
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
    style.apply_declaration("align-self", "FLEX-END");

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
    style.apply_declaration("position", "ABSOLUTE");
    style.apply_declaration("opacity", ".3");
    style.apply_declaration("top", "10px");
    style.apply_declaration("float", "RIGHT");
    style.apply_declaration("clear", "BOTH");

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
        "#2a3448 url(hero.jpg) NO-REPEAT CENTER TOP / COVER",
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
    style.apply_declaration("background-position", "RIGHT BOTTOM");
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
fn parses_background_size_percent_width() {
    let mut style = Style::initial();
    style.apply_declaration("background-size", "100%");

    assert_eq!(
        style.background_size,
        BackgroundSize::Explicit {
            width: Some(Length::Percent(1.0)),
            height: None,
        }
    );
}

#[test]
fn parses_object_fit_cover() {
    let mut style = Style::initial();
    style.apply_declaration("object-fit", "COVER");
    style.apply_declaration("object-position", "LEFT TOP");

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
    assert_eq!(parse_color("#ABC"), Some(Rgba::rgb(0xaa, 0xbb, 0xcc)));
    assert_eq!(parse_color("#000c"), Some(Rgba::with_alpha(0, 0, 0, 0xcc)));
    assert_eq!(
        parse_color("#11223380"),
        Some(Rgba::with_alpha(0x11, 0x22, 0x33, 0x80))
    );
    assert_eq!(
        parse_color("rgb(0 0 0 / 80%)"),
        Some(Rgba::with_alpha(0, 0, 0, 204))
    );
    assert_eq!(parse_color("RGB(0, 128, 0)"), Some(Rgba::rgb(0, 128, 0)));
    assert_eq!(
        parse_color("TRANSPARENT"),
        Some(Rgba::with_alpha(0, 0, 0, 0))
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
    let html = inline_css(&html, 200, 800).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new_with_locale_and_db_and_fallback(
        "en-US".to_string(),
        system_font_database(),
        MailCanvasFontFallback,
    );
    let mut engine = LayoutEngine::new(
        &mut font_system,
        resource_policy_for_test(),
        FontFamilyIndex::default(),
        Vec::new(),
        RenderLimits::default(),
        true,
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
    let layout = layout_for_test(r#"<p style="text-transform: UPPERCASE">Confirm</p>"#, 200);
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
    let with_trailing_break = format!("Viewed by{HARD_BREAK}");
    assert_eq!(normalize_text(&with_trailing_break), "Viewed by");
    let with_trailing_empty_line = format!("Viewed by{HARD_BREAK}{HARD_BREAK}");
    assert_eq!(normalize_text(&with_trailing_empty_line), "Viewed by\n");
    let with_empty_line = format!("Viewed by{HARD_BREAK}{HARD_BREAK}Someone");
    assert_eq!(normalize_text(&with_empty_line), "Viewed by\n\nSomeone");
}

#[test]
fn paragraph_with_single_br_uses_one_line_height() {
    let layout = layout_for_test(
        r#"<p style="margin:0;font-size:16px;line-height:24px"><br></p>"#,
        300,
    );
    let paragraph = find_layout(&layout, |child| child.debug.tag == "p").expect("paragraph");

    assert!(
        (paragraph.rect.height - 24.0).abs() < 0.1,
        "paragraph height: {}",
        paragraph.rect.height
    );
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
fn declared_table_width_resolves_percentage_image_against_table_width() {
    let layout = layout_for_test(
        r#"<table align="center" style="width:700px" cellpadding="0" cellspacing="0">
            <tr><td><img width="700" height="450" style="width:100%;height:auto" alt=""></td></tr>
          </table>"#,
        800,
    );
    let table =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");

    assert!(
        (table.rect.width - 700.0).abs() < 0.1,
        "table width: {}",
        table.rect.width
    );
    assert!(
        (table.rect.x - 50.0).abs() < 0.1,
        "table x: {}",
        table.rect.x
    );
    assert!(
        (image.rect.width - 700.0).abs() < 0.1,
        "image width: {}",
        image.rect.width
    );
}

#[test]
fn declared_table_width_does_not_expand_for_plain_fixed_width_text_cell() {
    let layout = layout_for_test(
        r#"<table align="center" style="width:600px" cellpadding="0" cellspacing="0">
            <tr><td style="padding:0 40px">
              <table width="100%" cellpadding="0" cellspacing="0">
                <tr><td width="570"><h1 style="margin:0;font-size:32px;line-height:38.4px">Choose a cruise that grows your faith</h1></td></tr>
              </table>
            </td></tr>
          </table>"#,
        800,
    );
    let tables: Vec<&LayoutBox> =
        collect_layouts(&layout, &|child| matches!(child.kind, LayoutKind::Table));
    let heading = find_layout(&layout, |child| child.debug.tag == "h1").expect("heading");

    assert!(
        (tables[0].rect.width - 600.0).abs() < 0.1,
        "outer table width: {}",
        tables[0].rect.width
    );
    assert!(
        heading.rect.width <= 520.1,
        "heading should use the padded cell content width, got {}",
        heading.rect.width
    );
}

#[test]
fn inline_linked_images_participate_in_same_inline_row() {
    let layout = layout_for_test(
        r##"<table width="300" cellpadding="0" cellspacing="0"><tr><td style="font-size:20px;line-height:28px">
            <a href="#"><img width="26" height="26" style="display:inline-block" alt="One"></a>&nbsp;&nbsp;
            <a href="#"><img width="20" height="24" style="display:inline-block" alt="Two"></a>&nbsp;&nbsp;
            <a href="#"><img width="26" height="26" style="display:inline-block" alt="Three"></a>
          </td></tr></table>"##,
        300,
    );
    let images: Vec<&LayoutBox> =
        collect_layouts(&layout, &|child| matches!(child.kind, LayoutKind::Image(_)));

    assert_eq!(images.len(), 3);
    assert!(
        (images[0].rect.y - images[1].rect.y).abs() < 0.1,
        "first two images should share a row: {} vs {}",
        images[0].rect.y,
        images[1].rect.y
    );
    assert!(
        (images[1].rect.y - images[2].rect.y).abs() < 0.1,
        "last two images should share a row: {} vs {}",
        images[1].rect.y,
        images[2].rect.y
    );
}

#[test]
fn table_cells_use_cellpadding_attribute() {
    let layout = layout_for_test(
        r#"<table width="100" cellpadding="1"><tr><td><img width="20" height="10" alt=""></td></tr></table>"#,
        100,
    );
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");

    assert!((image.rect.x - (cell.rect.x + 1.0)).abs() < 0.1);
    assert!((image.rect.y - (cell.rect.y + 1.0)).abs() < 0.1);
}

#[test]
fn table_cells_use_browser_default_cellpadding() {
    let layout = layout_for_test(
        r#"<table width="100"><tr><td><img width="20" height="10" alt=""></td></tr></table>"#,
        100,
    );
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");

    assert!((image.rect.x - (cell.rect.x + 1.0)).abs() < 0.1);
    assert!((image.rect.y - (cell.rect.y + 1.0)).abs() < 0.1);
}

#[test]
fn table_cell_css_padding_overrides_cellpadding_attribute() {
    let layout = layout_for_test(
        r#"<table width="100" cellpadding="1"><tr><td style="padding:0"><img width="20" height="10" alt=""></td></tr></table>"#,
        100,
    );
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");

    assert!((image.rect.x - cell.rect.x).abs() < 0.1);
    assert!((image.rect.y - cell.rect.y).abs() < 0.1);
}

#[test]
fn table_cells_inherit_browser_middle_valign_from_rows() {
    let layout = layout_for_test(
        r#"<table width="100" cellpadding="0"><tr><td height="40"><img width="20" height="10" alt=""></td></tr></table>"#,
        100,
    );
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");

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
fn word_break_break_word_allows_emergency_wrapping() {
    let layout = layout_for_test(
        r#"<table width="80"><tr><td style="word-break:break-word">Supercalifragilisticexpialidocious</td></tr></table>"#,
        80,
    );
    let text = find_text_layout(&layout).expect("text");
    assert_eq!(text.style.wrap, TextWrap::WordOrGlyph);
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
fn colspan_spacer_does_not_freeze_auto_table_columns() {
    let layout = layout_for_test(
        r#"<table width="600" cellpadding="0" cellspacing="0">
            <tr><td colspan="2" style="font-size:0;line-height:1">&nbsp;</td></tr>
            <tr>
              <td style="width:125px;padding:0 28px"><img width="125" height="35" alt=""></td>
              <td style="padding:0 28px;text-align:right"><table align="right" style="float:right"><tr><td style="padding:8px 16px;font-size:10px;line-height:10px"><a style="display:block;font-size:10px;line-height:10px">Log in</a></td></tr></table></td>
            </tr>
          </table>"#,
        800,
    );
    let table =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
    let cells = &table.children[1].children;
    assert!((cells[0].rect.width - 181.0).abs() < 0.1);
    assert!(
        cells[1].rect.width > 300.0,
        "auto column should receive the remaining table width, got {}",
        cells[1].rect.width
    );
}

#[test]
fn single_cell_spacer_row_does_not_freeze_later_multicolumn_content() {
    let layout = layout_for_test(
        r#"<table width="360" cellpadding="0" cellspacing="0">
            <tr><td height="40">&nbsp;</td></tr>
            <tr>
              <td style="font-size:22px;line-height:28px;text-align:center">
                Customer engagement is not a one-time action
              </td>
              <td height="32">&nbsp;</td>
            </tr>
          </table>"#,
        390,
    );
    let table =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Table)).expect("table");
    let cells = &table.children[1].children;

    assert!(
        cells[0].rect.width > 300.0,
        "content column should not be squeezed by a prior spacer row, got {}",
        cells[0].rect.width
    );
    assert!(cells[1].rect.width < 20.0);
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
    let cell =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell layout");
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
fn inline_anchor_wrapping_inline_block_child_stays_inline_flow() {
    let layout = layout_for_test(
        r#"
        <div style="width:570px;text-align:center;font-size:14px;line-height:24px">
          <a style="text-decoration:none;color:#666"><span style="width:165px;display:inline-block;padding:8px 3px;border:1px solid #ccc;margin:10px;vertical-align:top">Follow Lower Haight</span></a>
          <a style="text-decoration:none;color:#666"><span style="width:165px;display:inline-block;padding:8px 3px;border:1px solid #ccc;margin:10px;vertical-align:top">Follow Mission</span></a>
          <a style="text-decoration:none;color:#666"><span style="width:165px;display:inline-block;padding:8px 3px;border:1px solid #ccc;margin:10px;vertical-align:top">Follow Hayes Valley</span></a>
        </div>
        "#,
        570,
    );
    let anchors: Vec<&LayoutBox> = collect_layouts(&layout, &|child| child.debug.tag == "a");
    assert_eq!(anchors.len(), 3);
    assert!(
        (anchors[0].rect.y - anchors[1].rect.y).abs() < 0.1,
        "first two inline anchors should share a row: {:?}",
        anchors
            .iter()
            .map(|anchor| (anchor.debug.text.as_str(), anchor.rect))
            .collect::<Vec<_>>()
    );
    assert!(
        anchors[1].rect.x > anchors[0].rect.x + 100.0,
        "second anchor should be placed to the right of the first: {:?}",
        anchors
            .iter()
            .map(|anchor| (anchor.debug.text.as_str(), anchor.rect))
            .collect::<Vec<_>>()
    );
}

#[test]
fn anonymous_css_table_cells_flow_horizontally() {
    let layout = layout_for_test(
        r#"
        <div style="width:600px">
          <div class="column" style="display:table-cell;width:300px;vertical-align:top">Left</div>
          <div class="column" style="display:table-cell;width:300px;vertical-align:top">Right</div>
        </div>
        "#,
        600,
    );
    let columns: Vec<&LayoutBox> = collect_layouts(&layout, &|child| {
        child.debug.class_name == Some("column".to_string())
    });
    assert_eq!(columns.len(), 2);
    assert!(
        (columns[0].rect.y - columns[1].rect.y).abs() < 0.1,
        "anonymous table cells should share a row: {:?}",
        columns
            .iter()
            .map(|column| (column.debug.text.as_str(), column.rect))
            .collect::<Vec<_>>()
    );
    assert!(
        columns[1].rect.x > columns[0].rect.x + 250.0,
        "second cell should be to the right of the first: {:?}",
        columns
            .iter()
            .map(|column| (column.debug.text.as_str(), column.rect))
            .collect::<Vec<_>>()
    );
}

#[test]
fn percentage_width_table_cells_do_not_shrink_single_column_tables() {
    let layout = layout_for_test(
        r#"<table width="600" border="0" cellpadding="0" cellspacing="0"><tr><td style="padding-left:6.25%;padding-right:6.25%;width:87.5%">Header</td></tr></table>"#,
        600,
    );
    let cell =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell layout");
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
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");
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
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");
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
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");
    assert!((image.rect.x - 75.0).abs() < 0.1);
}

#[test]
fn legacy_align_attribute_centers_shrink_block_children() {
    let layout = layout_for_test(
        r##"<table width="200" cellpadding="0" cellspacing="0"><tr><td align="center"><div style="max-width:50px;height:10px;background:#000"></div></td></tr></table>"##,
        200,
    );
    let block = find_layout(&layout, |child| {
        matches!(child.kind, LayoutKind::Block) && child.style.background == Some(Rgba::BLACK)
    })
    .expect("shrink block");
    assert!((block.rect.x - 75.0).abs() < 0.1);
}

#[test]
fn legacy_align_attribute_centers_block_image_wrappers() {
    let layout = layout_for_test(
        r#"<table width="200" cellpadding="0" cellspacing="0"><tr><td align="center"><div style="max-width:50px"><img width="100%" height="20" alt="" style="display:block;width:100%"></div></td></tr></table>"#,
        200,
    );
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");
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
    let image =
        find_layout(&layout, |child| matches!(child.kind, LayoutKind::Image(_))).expect("image");
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
    let block =
        find_layout(&layout, |child| child.style.background == Some(Rgba::BLACK)).expect("block");
    assert!((block.rect.x - 50.0).abs() < 0.1);
}

#[test]
fn content_box_table_cell_width_keeps_padding_outside_content() {
    let layout = layout_for_test(
        r#"<table width="100"><tr><td style="width:32px;padding-left:12px;box-sizing:CONTENT-BOX;white-space:nowrap">4/5</td></tr></table>"#,
        100,
    );
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");
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
        rgba: vec![0, 0, 0, 255, 255, 255, 255, 255].into(),
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
        ]
        .into(),
    };

    let sampled = sample_image_area(&image, 0.0, 0.0, 2.0, 2.0);

    assert_eq!(sampled, [139, 139, 139, 255]);
}

#[test]
fn image_rects_are_pixel_snapped_like_blink() {
    let image = ImageData {
        width: 1,
        height: 1,
        rgba: vec![255, 0, 0, 255].into(),
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
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0,
            255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ]
        .into(),
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
        rgba: vec![255, 0, 0, 255, 0, 255, 0, 255].into(),
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
        rgba: vec![255, 0, 0, 255, 0, 255, 0, 0].into(),
    };

    let sampled = sample_image_bilinear(&image, 0.5, 0.0);

    assert_eq!(sampled, [254, 0, 0, 128]);
}

#[test]
fn image_opacity_is_applied_during_composite() {
    let image = ImageData {
        width: 1,
        height: 1,
        rgba: vec![255, 0, 0, 255].into(),
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
        rgba: vec![255, 0, 0, 255].into(),
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
fn block_after_full_width_inline_table_starts_after_inline_row() {
    let layout = layout_for_test(
        r#"
        <table width="100%">
          <tr>
            <td style="display:block">
              <table align="left" width="100%" style="display:inline-block">
                <tr><td><img width="50" height="50" alt=""></td></tr>
              </table>
              <table width="100%">
                <tr><td><img width="200" height="100" alt=""></td></tr>
              </table>
            </td>
          </tr>
        </table>
        "#,
        300,
    );
    let images = collect_layouts(&layout, &|child| {
        matches!(child.kind, LayoutKind::Image(_)) && child.rect.width > 1.0
    });
    assert_eq!(images.len(), 2);
    assert!(
        images[1].rect.y >= images[0].rect.y + images[0].rect.height - 0.1,
        "block image should start after inline table image: first={:?}, second={:?}",
        images[0].rect,
        images[1].rect
    );
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
    let cell = find_layout(&layout, |child| matches!(child.kind, LayoutKind::Cell)).expect("cell");

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
    assert!(image.debug.is_none());
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
fn anonymous_text_boxes_do_not_repaint_parent_borders() {
    let html = build_document(
        r#"<table width="120" cellpadding="0" cellspacing="0"><tr><td style="padding:12px;border-top:2px dashed #ff0000;border-bottom:2px dashed #ff0000;font-size:16px;line-height:24px;color:#000">Total</td></tr></table>"#,
        None,
        None,
        120,
    );
    let request = RenderRequest::defaults_for_html(html, 120, 80, 1.0);
    let mut renderer = MailCanvasRenderer::new(120, 80, 1.0).unwrap();
    let image = renderer.render_png(request).unwrap();
    let decoded = image::load_from_memory(&image.png).unwrap().to_rgba8();

    let red_rows = (0..decoded.height())
        .filter(|&y| {
            (0..decoded.width())
                .filter(|&x| {
                    let pixel = decoded.get_pixel(x, y).0;
                    pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40
                })
                .count()
                > 8
        })
        .count();

    assert_eq!(red_rows, 4);
}

#[test]
fn debug_snapshot_is_opt_in() {
    let html = build_document(
        r#"<table><tr><td><p>Hello debug</p></td></tr></table>"#,
        None,
        None,
        320,
    );
    let mut request = RenderRequest::defaults_for_html(html, 320, 240, 1.0);
    request.debug = RenderDebugOptions::layout_dump();
    let mut renderer = MailCanvasRenderer::new(320, 240, 1.0).unwrap();
    let image = renderer.render_png(request).unwrap();
    let debug = image.debug.expect("debug snapshot");
    assert!(debug.layout.is_some());
    assert!(!debug.text_rects.is_empty());
}

#[test]
fn layout_debug_metadata_is_opt_in() {
    let html = build_document(
        r#"<p id="hero" class="title">Hello debug</p>"#,
        None,
        None,
        320,
    );
    let html = inline_css(&html, 320, 240).unwrap();
    let document = kuchiki::parse_html().one(html);
    let mut font_system = FontSystem::new();
    let mut engine = LayoutEngine::new(
        &mut font_system,
        resource_policy_for_test(),
        FontFamilyIndex::default(),
        Vec::new(),
        RenderLimits::default(),
        false,
    );
    let layout = engine.layout_document(&document, 320).unwrap();
    let text = find_text_layout(&layout).expect("text layout");

    assert_eq!(layout.debug, LayoutDebugMeta::default());
    assert_eq!(text.debug, LayoutDebugMeta::default());
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
        Some(r#"@IMPORT url("https://example.com/email.css");"#),
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
