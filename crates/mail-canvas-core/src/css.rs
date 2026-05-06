use anyhow::{Context as _, Result};
use css_inline::CSSInliner;
use lightningcss::declaration::DeclarationBlock;
use lightningcss::rules::font_face::FontFaceProperty;
use lightningcss::rules::{CssRule, CssRuleList};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;

use crate::document::inject_head_markup;

pub(crate) fn inline_css(html: &str, viewport_width: u32) -> Result<String> {
    let html = strip_hidden_conditional_comments(html);
    let html = inject_active_media_styles(&html, viewport_width);
    let html = strip_media_rules_from_style_blocks(&html);
    CSSInliner::options()
        .load_remote_stylesheets(false)
        .keep_style_tags(false)
        .keep_link_tags(false)
        .apply_width_attributes(true)
        .apply_height_attributes(true)
        .build()
        .inline(&html)
        .context("failed to inline CSS")
}

pub(crate) fn strip_hidden_conditional_comments(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut offset = 0usize;

    while let Some(start_rel) = lower[offset..].find("<!--[if") {
        let start = offset + start_rel;
        out.push_str(&html[offset..start]);

        if let Some(content_start) = downlevel_revealed_content_start(&lower, start) {
            let Some(close_rel) = lower[content_start..].find("<!--<![endif]-->") else {
                out.push_str(&html[start..]);
                return out;
            };
            let content_end = content_start + close_rel;
            out.push_str(&strip_hidden_conditional_comments(
                &html[content_start..content_end],
            ));
            offset = content_end + "<!--<![endif]-->".len();
            continue;
        }

        let Some(end_rel) = lower[start..].find("<![endif]-->") else {
            out.push_str(&html[start..]);
            return out;
        };
        let end = start + end_rel;
        offset = end + "<![endif]-->".len();
    }

    out.push_str(&html[offset..]);
    out
}

fn downlevel_revealed_content_start(lower: &str, start: usize) -> Option<usize> {
    let condition_end = start + lower[start..].find("]>")? + "]>".len();
    let marker = lower[condition_end..].strip_prefix("<!--")?;
    let whitespace_len = marker.bytes().take_while(u8::is_ascii_whitespace).count();
    let marker_after_whitespace = &marker[whitespace_len..];
    if marker_after_whitespace.starts_with("-->") {
        return Some(condition_end + "<!--".len() + whitespace_len + "-->".len());
    }
    if marker_after_whitespace.starts_with('>') {
        return Some(condition_end + "<!--".len() + whitespace_len + ">".len());
    }
    None
}

fn inject_active_media_styles(html: &str, viewport_width: u32) -> String {
    let css = active_media_css(html, viewport_width);
    if css.trim().is_empty() {
        return html.to_string();
    }

    let style = format!("\n<style id=\"email-render-active-media\">\n{css}\n</style>\n");
    inject_head_markup(html, &style)
}

fn strip_media_rules_from_style_blocks(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut offset = 0;

    while let Some(start_rel) = lower[offset..].find("<style") {
        let start = offset + start_rel;
        let Some(open_rel) = lower[start..].find('>') else {
            break;
        };
        let content_start = start + open_rel + 1;
        let Some(end_rel) = lower[content_start..].find("</style>") else {
            break;
        };
        let content_end = content_start + end_rel;

        out.push_str(&html[offset..content_start]);
        out.push_str(&strip_media_rules(&html[content_start..content_end]));
        offset = content_end;
    }

    out.push_str(&html[offset..]);
    out
}

fn strip_media_rules(css: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let mut out = String::with_capacity(css.len());
    let mut offset = 0;

    while let Some(media_rel) = lower[offset..].find("@media") {
        let media_start = offset + media_rel;
        let condition_start = media_start + "@media".len();
        let Some(open_rel) = css[condition_start..].find('{') else {
            break;
        };
        let open = condition_start + open_rel;
        let Some(close) = find_matching_brace(css, open) else {
            break;
        };

        out.push_str(&css[offset..media_start]);
        offset = close + 1;
    }

    out.push_str(&css[offset..]);
    out
}

fn active_media_css(html: &str, viewport_width: u32) -> String {
    let mut out = String::new();
    for style in style_blocks(html) {
        append_active_media_css(style, viewport_width, &mut out);
    }
    out
}

pub(crate) fn style_blocks(html: &str) -> Vec<&str> {
    let lower = html.to_ascii_lowercase();
    let mut blocks = Vec::new();
    let mut offset = 0;

    while let Some(start_rel) = lower[offset..].find("<style") {
        let start = offset + start_rel;
        let Some(open_rel) = lower[start..].find('>') else {
            break;
        };
        let content_start = start + open_rel + 1;
        let Some(end_rel) = lower[content_start..].find("</style>") else {
            break;
        };
        let content_end = content_start + end_rel;
        blocks.push(&html[content_start..content_end]);
        offset = content_end + "</style>".len();
    }

    blocks
}

pub(crate) fn css_declarations(block: &str) -> Vec<(String, String)> {
    if let Some(declarations) = lightningcss_declarations(block) {
        return declarations;
    }

    fallback_css_declarations(block)
}

pub(crate) fn font_face_declarations(css: &str) -> Vec<Vec<(String, String)>> {
    let options = ParserOptions {
        error_recovery: true,
        ..Default::default()
    };

    if let Ok(stylesheet) = StyleSheet::parse(css, options) {
        let mut faces = Vec::new();
        collect_font_face_declarations(&stylesheet.rules, &mut faces);
        if !faces.is_empty() {
            return faces;
        }
    }

    font_face_blocks(css)
        .into_iter()
        .map(css_declarations)
        .collect()
}

fn lightningcss_declarations(block: &str) -> Option<Vec<(String, String)>> {
    let options = ParserOptions {
        error_recovery: true,
        ..Default::default()
    };
    let block = DeclarationBlock::parse_string(block, options).ok()?;
    let mut declarations = Vec::with_capacity(block.len());
    for declaration in &block.declarations {
        push_lightningcss_declaration(&mut declarations, declaration, false);
    }
    for declaration in &block.important_declarations {
        push_lightningcss_declaration(&mut declarations, declaration, false);
    }
    Some(declarations)
}

fn push_lightningcss_declaration(
    declarations: &mut Vec<(String, String)>,
    declaration: &lightningcss::properties::Property<'_>,
    important: bool,
) {
    let Ok(serialized) = declaration.to_css_string(important, PrinterOptions::default()) else {
        return;
    };
    let Some((name, value)) = serialized.split_once(':') else {
        return;
    };
    declarations.push((
        name.trim().to_ascii_lowercase(),
        strip_css_important(value.trim()).trim().to_string(),
    ));
}

fn collect_font_face_declarations<R>(
    rules: &CssRuleList<'_, R>,
    faces: &mut Vec<Vec<(String, String)>>,
) {
    for rule in &rules.0 {
        match rule {
            CssRule::FontFace(face) => {
                let declarations = face
                    .properties
                    .iter()
                    .filter_map(font_face_property_to_declaration)
                    .collect();
                faces.push(declarations);
            }
            CssRule::Media(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::Style(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::Supports(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::MozDocument(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::Nesting(rule) => collect_font_face_declarations(&rule.style.rules, faces),
            CssRule::LayerBlock(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::Container(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::Scope(rule) => collect_font_face_declarations(&rule.rules, faces),
            CssRule::StartingStyle(rule) => collect_font_face_declarations(&rule.rules, faces),
            _ => {}
        }
    }
}

fn font_face_property_to_declaration(property: &FontFaceProperty<'_>) -> Option<(String, String)> {
    let serialized = property.to_css_string(PrinterOptions::default()).ok()?;
    let (name, value) = serialized.split_once(':')?;
    Some((
        name.trim().to_ascii_lowercase(),
        strip_css_important(value.trim()).trim().to_string(),
    ))
}

fn fallback_css_declarations(block: &str) -> Vec<(String, String)> {
    split_css_top_level(block, ';')
        .into_iter()
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            Some((
                name.trim().to_ascii_lowercase(),
                strip_css_important(value.trim()).trim().to_string(),
            ))
        })
        .collect()
}

fn font_face_blocks(css: &str) -> Vec<&str> {
    let lower = css.to_ascii_lowercase();
    let mut faces = Vec::new();
    let mut offset = 0usize;

    while let Some(face_rel) = lower[offset..].find("@font-face") {
        let face_start = offset + face_rel;
        let Some(open_rel) = css[face_start..].find('{') else {
            break;
        };
        let open = face_start + open_rel;
        let Some(close) = find_matching_brace(css, open) else {
            break;
        };
        faces.push(&css[open + 1..close]);
        offset = close + 1;
    }

    faces
}

pub(crate) fn css_format_hint(segment: &str) -> Option<String> {
    let lower = segment.to_ascii_lowercase();
    let format_start = lower.find("format(")?;
    css_function_value(segment, format_start).map(|(value, _)| unquote_css_value(&value))
}

pub(crate) fn first_css_url(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let url_start = lower.find("url(")?;
    css_function_value(value, url_start).map(|(url, _)| url)
}

pub(crate) fn first_quoted_css_string(value: &str) -> Option<String> {
    let trimmed = value.trim_start();
    let quote = trimmed.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }

    let mut index = 1usize;
    let bytes = trimmed.as_bytes();
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return Some(trimmed[1..index].trim().to_string()),
            _ => index += 1,
        }
    }
    None
}

pub(crate) fn css_function_value(source: &str, function_start: usize) -> Option<(String, usize)> {
    let open = source[function_start..].find('(')? + function_start;
    let bytes = source.as_bytes();
    let mut index = open + 1;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }

    let quote = match bytes.get(index).copied() {
        Some(b'\'') | Some(b'"') => {
            let quote = bytes[index];
            index += 1;
            Some(quote)
        }
        _ => None,
    };
    let value_start = index;

    if let Some(quote) = quote {
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => index += 2,
                byte if byte == quote => {
                    let value = source[value_start..index].trim().to_string();
                    index += 1;
                    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                        index += 1;
                    }
                    if bytes.get(index) == Some(&b')') {
                        return Some((value, index + 1));
                    }
                    return None;
                }
                _ => index += 1,
            }
        }
        return None;
    }

    while index < bytes.len() && bytes[index] != b')' {
        index += 1;
    }
    if bytes.get(index) == Some(&b')') {
        return Some((source[value_start..index].trim().to_string(), index + 1));
    }
    None
}

fn split_css_top_level(source: &str, separator: char) -> Vec<&str> {
    let bytes = source.as_bytes();
    let separator = separator as u8;
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let mut quote = None;
    let mut paren_depth = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(current_quote) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == current_quote {
                quote = None;
            }
            index += 1;
            continue;
        }

        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            byte if byte == separator && paren_depth == 0 => {
                parts.push(source[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }

    if start <= source.len() {
        parts.push(source[start..].trim());
    }
    parts
}

pub(crate) fn next_css_segment_end(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut quote = None;
    let mut paren_depth = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(current_quote) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == current_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b',' if paren_depth == 0 => return index,
            _ => {}
        }
        index += 1;
    }

    source.len()
}

fn strip_css_important(value: &str) -> &str {
    let trimmed = value.trim_end();
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with("!important") {
        trimmed[..trimmed.len() - "!important".len()].trim_end()
    } else {
        value
    }
}

pub(crate) fn unquote_css_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn append_active_media_css(css: &str, viewport_width: u32, out: &mut String) {
    let lower = css.to_ascii_lowercase();
    let mut offset = 0;

    while let Some(media_rel) = lower[offset..].find("@media") {
        let media_start = offset + media_rel;
        let condition_start = media_start + "@media".len();
        let Some(open_rel) = css[condition_start..].find('{') else {
            break;
        };
        let open = condition_start + open_rel;
        let Some(close) = find_matching_brace(css, open) else {
            break;
        };

        if media_condition_matches(&css[condition_start..open], viewport_width) {
            let body = css[open + 1..close].trim();
            if !body.is_empty() {
                out.push_str(body);
                out.push('\n');
            }
        }
        offset = close + 1;
    }
}

pub(crate) fn find_matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }

    let mut depth = 0usize;
    let mut index = open;
    let mut quote = None;
    let mut in_comment = false;

    while index < bytes.len() {
        let byte = bytes[index];
        if in_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                in_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

        if let Some(current_quote) = quote {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == current_quote {
                quote = None;
            }
            index += 1;
            continue;
        }

        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            in_comment = true;
            index += 2;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }

    None
}

fn media_condition_matches(condition: &str, viewport_width: u32) -> bool {
    condition
        .split(',')
        .any(|query| single_media_query_matches(query, viewport_width))
}

fn single_media_query_matches(query: &str, viewport_width: u32) -> bool {
    let query = query.to_ascii_lowercase();
    let query = query.trim();
    if query.is_empty()
        || query.contains("not screen")
        || query.contains("prefers-color-scheme")
        || (query.contains("print") && !query.contains("screen") && !query.contains("all"))
    {
        return false;
    }

    let width = viewport_width as f32;
    for max_width in media_width_constraints(query, "max-width") {
        if width > max_width {
            return false;
        }
    }
    for min_width in media_width_constraints(query, "min-width") {
        if width < min_width {
            return false;
        }
    }
    true
}

fn media_width_constraints(query: &str, name: &str) -> Vec<f32> {
    let mut values = Vec::new();
    let mut offset = 0;
    while let Some(index_rel) = query[offset..].find(name) {
        let index = offset + index_rel + name.len();
        if let Some(colon_rel) = query[index..].find(':') {
            let value_start = index + colon_rel + 1;
            if let Some(value) = parse_leading_css_number(&query[value_start..]) {
                values.push(value);
            }
            offset = value_start;
        } else {
            break;
        }
    }
    values
}

fn parse_leading_css_number(value: &str) -> Option<f32> {
    let value = value.trim_start();
    let end = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-'))
        .map(|(index, ch)| index + ch.len_utf8())
        .last()?;
    value[..end]
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration_value<'a>(declarations: &'a [(String, String)], name: &str) -> Option<&'a str> {
        declarations
            .iter()
            .find(|(declaration_name, _)| declaration_name == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn parses_declarations_with_lightningcss() {
        let declarations = css_declarations(
            r#"color: red !important; background-image: url("https://example.test/a;b.png");"#,
        );

        assert_eq!(declaration_value(&declarations, "color"), Some("red"));
        assert!(
            declaration_value(&declarations, "background-image")
                .is_some_and(|value| value.contains("a;b.png"))
        );
    }

    #[test]
    fn extracts_font_faces_with_lightningcss() {
        let faces = font_face_declarations(
            r#"@media screen {
                @font-face {
                    font-family: "Inter";
                    src: url("fonts/inter.woff2") format("woff2");
                    unicode-range: U+0000-00FF;
                }
            }"#,
        );

        assert_eq!(faces.len(), 1);
        assert_eq!(declaration_value(&faces[0], "font-family"), Some("Inter"));
        assert!(
            declaration_value(&faces[0], "src")
                .is_some_and(|value| value.contains("inter.woff2") && value.contains("woff2"))
        );
        assert_eq!(declaration_value(&faces[0], "unicode-range"), Some("U+??"));
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content_with_spaced_marker() {
        let html = r#"<!--[if !mso]><!-- -->
            <link href="fonts.css" rel="stylesheet">
            <!--<![endif]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert!(!stripped.contains("[if"));
        assert!(!stripped.contains("[endif]"));
        assert!(stripped.contains(r#"<link href="fonts.css" rel="stylesheet">"#));
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content_with_compact_marker() {
        let html = r#"<!--[if !mso]><!--><style>.x { color: red; }</style><!--<![endif]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert_eq!(style_blocks(&stripped), vec![".x { color: red; }"]);
    }

    #[test]
    fn strips_media_rules_after_extracting_active_viewport_css() {
        let html = r#"
            <html><head><style>
              .desktop_hide { display: none; }
              @media (max-width:720px) { .desktop_hide { display: table !important; } }
            </style></head>
            <body><table class="desktop_hide"><tr><td>Hidden on desktop</td></tr></table></body></html>
        "#;

        let inlined = inline_css(html, 800).unwrap();

        assert!(inlined.contains("display: none"));
        assert!(!inlined.contains("display: table"));
    }

    #[test]
    fn keeps_matching_media_rules_as_active_inline_css() {
        let html = r#"
            <html><head><style>
              .stack { width: 280px; }
              @media (max-width:720px) { .stack { width: 320px !important; } }
            </style></head>
            <body><table class="stack"><tr><td>Stacked</td></tr></table></body></html>
        "#;

        let inlined = inline_css(html, 600).unwrap();

        assert!(inlined.contains("width: 320px"));
        assert!(!inlined.contains("@media"));
    }

    #[test]
    fn downlevel_revealed_content_can_contain_nested_mso_conditionals() {
        let html = r#"<!--[if !mso]><!-->
            <table class="desktop_hide"><tr><td><a><!--[if mso]><v:roundrect><![endif]--><span>Mobile</span></a></td></tr></table>
            <!--<![endif]-->
            <table class="row-8"><tr><td>Desktop row</td></tr></table>"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert!(stripped.contains(r#"<table class="desktop_hide">"#));
        assert!(stripped.contains(r#"<span>Mobile</span>"#));
        assert!(stripped.contains(r#"<table class="row-8">"#));
        assert!(!stripped.contains("v:roundrect"));
        assert!(!stripped.contains("[endif]"));
    }
}
