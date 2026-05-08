use anyhow::{Context as _, Result};
use css_inline::CSSInliner;
use lightningcss::declaration::DeclarationBlock;
use lightningcss::media_query::{
    MediaCondition, MediaFeature, MediaFeatureComparison, MediaFeatureId, MediaFeatureName,
    MediaFeatureValue, MediaList, MediaQuery, MediaType, Operator, Qualifier,
};
use lightningcss::rules::font_face::FontFaceProperty;
use lightningcss::rules::{CssRule, CssRuleList};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;

#[cfg(test)]
pub(crate) fn inline_css(html: &str, viewport_width: u32, viewport_height: u32) -> Result<String> {
    let html = strip_hidden_conditional_comments(html);
    inline_css_from_stripped_html(&html, viewport_width, viewport_height)
}

pub(crate) fn inline_css_from_stripped_html(
    html: &str,
    viewport_width: u32,
    viewport_height: u32,
) -> Result<String> {
    let html = sanitize_html_for_css_inliner(html);
    let viewport = CssViewport {
        width: viewport_width as f32,
        height: viewport_height as f32,
    };
    let html = expand_active_media_rules_in_style_blocks(&html, viewport);
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

fn sanitize_html_for_css_inliner(html: &str) -> String {
    let html = strip_mso_declaration_attributes(html);
    sanitize_style_attributes(&html)
}

fn strip_mso_declaration_attributes(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut index = 0usize;
    let mut last = 0usize;

    while index < html.len() {
        if bytes[index].is_ascii_whitespace()
            && html[index + 1..]
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("mso-"))
        {
            let mut cursor = index + 1;
            while cursor < html.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'>'
            {
                cursor += 1;
            }
            let candidate = &html[index + 1..cursor];
            if candidate.contains(':') && candidate.ends_with(';') {
                out.push_str(&html[last..index]);
                index = cursor;
                last = index;
                continue;
            }
            index = cursor;
            continue;
        }

        index += 1;
    }

    out.push_str(&html[last..]);
    out
}

fn sanitize_style_attributes(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut offset = 0usize;

    while let Some(style_start) = find_ascii_case_insensitive_from(html, "style=", offset) {
        let value_start = style_start + "style=".len();
        if !is_attribute_name_boundary(html, style_start) || value_start >= html.len() {
            out.push_str(&html[offset..value_start]);
            offset = value_start;
            continue;
        }

        out.push_str(&html[offset..style_start]);
        let quote = html.as_bytes()[value_start];
        if quote == b'"' || quote == b'\'' {
            let content_start = value_start + 1;
            let Some(end_rel) = html[content_start..].find(quote as char) else {
                out.push_str(&html[style_start..]);
                return out;
            };
            let content_end = content_start + end_rel;
            out.push_str("style=\"");
            out.push_str(&escape_style_attr(&sanitize_style_attribute_value(
                &html[content_start..content_end],
            )));
            out.push('"');
            offset = content_end + 1;
        } else {
            let mut content_end = value_start;
            while content_end < html.len() {
                let byte = html.as_bytes()[content_end];
                if byte.is_ascii_whitespace() || byte == b'>' {
                    break;
                }
                content_end += 1;
            }
            out.push_str("style=\"");
            out.push_str(&escape_style_attr(&sanitize_style_attribute_value(
                &html[value_start..content_end],
            )));
            out.push('"');
            offset = content_end;
        }
    }

    out.push_str(&html[offset..]);
    out
}

pub(crate) fn find_ascii_case_insensitive_from(
    haystack: &str,
    needle: &str,
    offset: usize,
) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Some(offset.min(haystack.len()));
    }
    if offset > haystack.len().saturating_sub(needle.len()) {
        return None;
    }
    haystack.as_bytes()[offset..]
        .windows(needle.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(needle))
        .map(|position| offset + position)
}

fn is_attribute_name_boundary(html: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let previous = html.as_bytes()[start - 1];
    previous.is_ascii_whitespace() || previous == b'<'
}

fn sanitize_style_attribute_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (name, value) in css_declarations(value) {
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&name);
        out.push_str(": ");
        out.push_str(&value);
        out.push(';');
    }
    out
}

fn escape_style_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

pub(crate) fn strip_hidden_conditional_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut offset = 0usize;

    while let Some(start) = find_ascii_case_insensitive_from(html, "<!--[if", offset) {
        out.push_str(&html[offset..start]);

        if let Some(content_start) = downlevel_revealed_content_start(html, start) {
            let Some((content_end, close_end)) =
                downlevel_revealed_content_end(html, content_start)
            else {
                out.push_str(&html[start..]);
                return out;
            };
            out.push_str(&strip_hidden_conditional_comments(
                &html[content_start..content_end],
            ));
            offset = close_end;
            continue;
        }

        let Some(end) = find_ascii_case_insensitive_from(html, "<![endif]-->", start) else {
            out.push_str(&html[start..]);
            return out;
        };
        offset = end + "<![endif]-->".len();
    }

    out.push_str(&html[offset..]);
    out
}

fn downlevel_revealed_content_end(html: &str, content_start: usize) -> Option<(usize, usize)> {
    let mut offset = content_start;
    let mut depth = 0usize;
    let endif_start = loop {
        let next_if = find_ascii_case_insensitive_from(html, "<!--[if", offset);
        let next_endif = find_ascii_case_insensitive_from(html, "<![endif]-->", offset)?;
        if let Some(next_if) = next_if {
            if next_if < next_endif {
                depth += 1;
                offset = next_if + "<!--[if".len();
                continue;
            }
        }
        if depth > 0 {
            depth -= 1;
            offset = next_endif + "<![endif]-->".len();
            continue;
        }
        break next_endif;
    };
    let close_end = endif_start + "<![endif]-->".len();
    let before_endif = &html[content_start..endif_start];
    let comment_start = before_endif.rfind("<!--")?;
    let candidate_end = content_start + comment_start;
    let trailing = &html[candidate_end + "<!--".len()..endif_start];
    if trailing.trim().is_empty() {
        Some((candidate_end, close_end))
    } else {
        Some((endif_start, close_end))
    }
}

fn downlevel_revealed_content_start(html: &str, start: usize) -> Option<usize> {
    let condition_end = start + html[start..].find("]>")? + "]>".len();
    let marker_prefix_whitespace_len = html[condition_end..]
        .bytes()
        .take_while(u8::is_ascii_whitespace)
        .count();
    let marker_start = condition_end + marker_prefix_whitespace_len;
    if let Some(marker) = html[marker_start..].strip_prefix("<!--") {
        let whitespace_len = marker.bytes().take_while(u8::is_ascii_whitespace).count();
        let marker_after_whitespace = &marker[whitespace_len..];
        if marker_after_whitespace.starts_with("-->") {
            return Some(marker_start + "<!--".len() + whitespace_len + "-->".len());
        }
        if marker_after_whitespace.starts_with('>') {
            return Some(marker_start + "<!--".len() + whitespace_len + ">".len());
        }
    }
    if let Some(marker) = html[marker_start..].strip_prefix("<!") {
        let whitespace_len = marker.bytes().take_while(u8::is_ascii_whitespace).count();
        if marker[whitespace_len..].starts_with("-->") {
            return Some(marker_start + "<!".len() + whitespace_len + "-->".len());
        }
    }
    None
}

#[derive(Clone, Copy)]
struct CssViewport {
    width: f32,
    height: f32,
}

fn expand_active_media_rules_in_style_blocks(html: &str, viewport: CssViewport) -> String {
    let mut out = String::with_capacity(html.len());
    let mut offset = 0;

    while let Some(start) = find_ascii_case_insensitive_from(html, "<style", offset) {
        let Some(open_rel) = html[start..].find('>') else {
            break;
        };
        let content_start = start + open_rel + 1;
        let Some(content_end) = find_ascii_case_insensitive_from(html, "</style>", content_start)
        else {
            break;
        };

        out.push_str(&html[offset..content_start]);
        let css = &html[content_start..content_end];
        out.push_str(&expand_active_media_rules(css, viewport));
        offset = content_end;
    }

    out.push_str(&html[offset..]);
    out
}

fn expand_active_media_rules(css: &str, viewport: CssViewport) -> String {
    if let Some(expanded) = expand_active_media_rules_with_lightningcss(css, viewport) {
        return expanded;
    }

    let mut out = strip_media_rules_fallback(css);
    append_active_media_css_fallback(css, viewport.width as u32, &mut out);
    out
}

fn expand_active_media_rules_with_lightningcss(css: &str, viewport: CssViewport) -> Option<String> {
    let options = ParserOptions {
        error_recovery: true,
        ..Default::default()
    };
    let stylesheet = StyleSheet::parse(css, options).ok()?;
    let mut out = String::new();
    append_active_rules(&stylesheet.rules, viewport, &mut out)?;
    Some(out)
}

fn append_active_rules<R: ToCss>(
    rules: &CssRuleList<'_, R>,
    viewport: CssViewport,
    out: &mut String,
) -> Option<()> {
    for rule in &rules.0 {
        match rule {
            CssRule::Media(media) => {
                if media_list_matches(&media.query, viewport) {
                    append_active_rules(&media.rules, viewport, out)?;
                }
            }
            _ => append_serialized_rule(rule, out)?,
        }
    }
    Some(())
}

fn append_serialized_rule<R: ToCss>(rule: &CssRule<'_, R>, out: &mut String) -> Option<()> {
    let css = rule.to_css_string(PrinterOptions::default()).ok()?;
    if !css.trim().is_empty() {
        out.push_str(&css);
        out.push('\n');
    }
    Some(())
}

pub(crate) fn style_blocks(html: &str) -> impl Iterator<Item = &str> + '_ {
    StyleBlocks { html, offset: 0 }
}

struct StyleBlocks<'a> {
    html: &'a str,
    offset: usize,
}

impl<'a> Iterator for StyleBlocks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let start = find_ascii_case_insensitive_from(self.html, "<style", self.offset)?;
        let Some(open_rel) = self.html[start..].find('>') else {
            self.offset = self.html.len();
            return None;
        };
        let content_start = start + open_rel + 1;
        let Some(content_end) =
            find_ascii_case_insensitive_from(self.html, "</style>", content_start)
        else {
            self.offset = self.html.len();
            return None;
        };
        self.offset = content_end + "</style>".len();
        Some(&self.html[content_start..content_end])
    }
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
    let declarations = split_css_top_level(block, ';');
    let mut out = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        out.push((
            name.trim().to_ascii_lowercase(),
            strip_css_important(value.trim()).trim().to_string(),
        ));
    }
    out
}

fn font_face_blocks(css: &str) -> Vec<&str> {
    let mut faces = Vec::new();
    let mut offset = 0usize;

    while let Some(face_start) = find_ascii_case_insensitive_from(css, "@font-face", offset) {
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
    let format_start = find_ascii_case_insensitive_from(segment, "format(", 0)?;
    css_function_value(segment, format_start).map(|(value, _)| unquote_css_value(&value))
}

pub(crate) fn first_css_url(value: &str) -> Option<String> {
    let url_start = find_ascii_case_insensitive_from(value, "url(", 0)?;
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
                let part = source[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }

    if start <= source.len() {
        let part = source[start..].trim();
        if !part.is_empty() {
            parts.push(part);
        }
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
    if let Some(stripped) = strip_ascii_case_insensitive_suffix(trimmed, "!important") {
        stripped.trim_end()
    } else {
        value
    }
}

fn strip_ascii_case_insensitive_suffix<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    if value.len() < suffix.len() {
        return None;
    }
    let suffix_start = value.len() - suffix.len();
    value[suffix_start..]
        .eq_ignore_ascii_case(suffix)
        .then_some(&value[..suffix_start])
}

pub(crate) fn unquote_css_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn strip_media_rules_fallback(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut offset = 0;

    while let Some(media_start) = find_ascii_case_insensitive_from(css, "@media", offset) {
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

fn append_active_media_css_fallback(css: &str, viewport_width: u32, out: &mut String) {
    let mut offset = 0;

    while let Some(media_start) = find_ascii_case_insensitive_from(css, "@media", offset) {
        let condition_start = media_start + "@media".len();
        let Some(open_rel) = css[condition_start..].find('{') else {
            break;
        };
        let open = condition_start + open_rel;
        let Some(close) = find_matching_brace(css, open) else {
            break;
        };

        if media_condition_matches_fallback(&css[condition_start..open], viewport_width) {
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

fn media_condition_matches_fallback(condition: &str, viewport_width: u32) -> bool {
    condition
        .split(',')
        .any(|query| single_media_query_matches_fallback(query, viewport_width))
}

fn single_media_query_matches_fallback(query: &str, viewport_width: u32) -> bool {
    let query = query.trim();
    if query.is_empty()
        || contains_ascii_case_insensitive(query, "not screen")
        || contains_ascii_case_insensitive(query, "prefers-color-scheme")
        || (contains_ascii_case_insensitive(query, "print")
            && !contains_ascii_case_insensitive(query, "screen")
            && !contains_ascii_case_insensitive(query, "all"))
    {
        return false;
    }

    let width = viewport_width as f32;
    if !media_width_constraints_satisfy(query, "max-width", |max_width| width <= max_width) {
        return false;
    }
    if !media_width_constraints_satisfy(query, "min-width", |min_width| width >= min_width) {
        return false;
    }
    true
}

fn contains_ascii_case_insensitive(value: &str, needle: &str) -> bool {
    find_ascii_case_insensitive_from(value, needle, 0).is_some()
}

fn media_list_matches(list: &MediaList<'_>, viewport: CssViewport) -> bool {
    list.media_queries.is_empty()
        || list
            .media_queries
            .iter()
            .any(|query| media_query_matches(query, viewport))
}

fn media_query_matches(query: &MediaQuery<'_>, viewport: CssViewport) -> bool {
    let mut matches = media_type_matches(&query.media_type)
        && query
            .condition
            .as_ref()
            .is_none_or(|condition| media_condition_matches(condition, viewport));

    if query.qualifier == Some(Qualifier::Not) {
        matches = !matches;
    }
    matches
}

fn media_type_matches(media_type: &MediaType<'_>) -> bool {
    matches!(media_type, MediaType::All | MediaType::Screen)
}

fn media_condition_matches(condition: &MediaCondition<'_>, viewport: CssViewport) -> bool {
    match condition {
        MediaCondition::Feature(feature) => media_feature_matches(feature, viewport),
        MediaCondition::Not(condition) => !media_condition_matches(condition, viewport),
        MediaCondition::Operation {
            operator,
            conditions,
        } => match operator {
            Operator::And => conditions
                .iter()
                .all(|condition| media_condition_matches(condition, viewport)),
            Operator::Or => conditions
                .iter()
                .any(|condition| media_condition_matches(condition, viewport)),
        },
        MediaCondition::Unknown(_) => false,
    }
}

fn media_feature_matches(feature: &MediaFeature<'_>, viewport: CssViewport) -> bool {
    match feature {
        MediaFeature::Boolean { name } => media_feature_boolean_value(name, viewport),
        MediaFeature::Plain { name, value } => media_feature_plain_matches(name, value, viewport),
        MediaFeature::Range {
            name,
            operator,
            value,
        } => {
            let Some(left) = media_feature_numeric_value(name, viewport) else {
                return false;
            };
            let Some(right) = media_feature_value_to_number(value) else {
                return false;
            };
            compare_media_numbers(left, *operator, right)
        }
        MediaFeature::Interval {
            name,
            start,
            start_operator,
            end,
            end_operator,
        } => {
            let Some(value) = media_feature_numeric_value(name, viewport) else {
                return false;
            };
            let Some(start) = media_feature_value_to_number(start) else {
                return false;
            };
            let Some(end) = media_feature_value_to_number(end) else {
                return false;
            };
            compare_media_numbers(start, *start_operator, value)
                && compare_media_numbers(value, *end_operator, end)
        }
    }
}

fn media_feature_plain_matches(
    name: &MediaFeatureName<'_, MediaFeatureId>,
    value: &MediaFeatureValue<'_>,
    viewport: CssViewport,
) -> bool {
    if let Some(left) = media_feature_numeric_value(name, viewport) {
        return media_feature_value_to_number(value)
            .is_some_and(|right| (left - right).abs() < f32::EPSILON);
    }

    match (standard_media_feature_id(name), value) {
        (Some(MediaFeatureId::Orientation), MediaFeatureValue::Ident(value)) => {
            value.0.eq_ignore_ascii_case(viewport_orientation(viewport))
        }
        _ => false,
    }
}

fn media_feature_boolean_value(
    name: &MediaFeatureName<'_, MediaFeatureId>,
    viewport: CssViewport,
) -> bool {
    media_feature_numeric_value(name, viewport).is_some_and(|value| value > 0.0)
}

fn media_feature_numeric_value(
    name: &MediaFeatureName<'_, MediaFeatureId>,
    viewport: CssViewport,
) -> Option<f32> {
    match standard_media_feature_id(name)? {
        MediaFeatureId::Width | MediaFeatureId::DeviceWidth => Some(viewport.width),
        MediaFeatureId::Height | MediaFeatureId::DeviceHeight => Some(viewport.height),
        MediaFeatureId::AspectRatio | MediaFeatureId::DeviceAspectRatio => {
            (viewport.height > 0.0).then_some(viewport.width / viewport.height)
        }
        MediaFeatureId::Color => Some(24.0),
        MediaFeatureId::ColorIndex | MediaFeatureId::Monochrome | MediaFeatureId::Grid => Some(0.0),
        _ => None,
    }
}

fn standard_media_feature_id(
    name: &MediaFeatureName<'_, MediaFeatureId>,
) -> Option<MediaFeatureId> {
    match name {
        MediaFeatureName::Standard(id) => Some(*id),
        MediaFeatureName::Custom(_) | MediaFeatureName::Unknown(_) => None,
    }
}

fn media_feature_value_to_number(value: &MediaFeatureValue<'_>) -> Option<f32> {
    match value {
        MediaFeatureValue::Length(length) => length.to_px(),
        MediaFeatureValue::Number(value) => Some(*value),
        MediaFeatureValue::Integer(value) => Some(*value as f32),
        MediaFeatureValue::Boolean(value) => Some(if *value { 1.0 } else { 0.0 }),
        MediaFeatureValue::Ratio(ratio) => (ratio.1 != 0.0).then_some(ratio.0 / ratio.1),
        MediaFeatureValue::Resolution(_)
        | MediaFeatureValue::Ident(_)
        | MediaFeatureValue::Env(_) => None,
    }
}

fn compare_media_numbers(left: f32, operator: MediaFeatureComparison, right: f32) -> bool {
    match operator {
        MediaFeatureComparison::Equal => (left - right).abs() < f32::EPSILON,
        MediaFeatureComparison::GreaterThan => left > right,
        MediaFeatureComparison::GreaterThanEqual => left >= right,
        MediaFeatureComparison::LessThan => left < right,
        MediaFeatureComparison::LessThanEqual => left <= right,
    }
}

fn viewport_orientation(viewport: CssViewport) -> &'static str {
    if viewport.height >= viewport.width {
        "portrait"
    } else {
        "landscape"
    }
}

fn media_width_constraints_satisfy(
    query: &str,
    name: &str,
    predicate: impl Fn(f32) -> bool,
) -> bool {
    let mut offset = 0;
    while let Some(name_start) = find_ascii_case_insensitive_from(query, name, offset) {
        let index = name_start + name.len();
        if let Some(colon_rel) = query[index..].find(':') {
            let value_start = index + colon_rel + 1;
            if let Some(value) = parse_leading_css_number(&query[value_start..]) {
                if !predicate(value) {
                    return false;
                }
            }
            offset = value_start;
        } else {
            break;
        }
    }
    true
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
        let html = r#"<!--[IF !MSO]><!-- -->
            <link href="fonts.css" rel="stylesheet">
            <!--<![ENDIF]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert!(!stripped.contains("[if"));
        assert!(!stripped.contains("[IF"));
        assert!(!stripped.contains("[endif]"));
        assert!(!stripped.contains("[ENDIF]"));
        assert!(stripped.contains(r#"<link href="fonts.css" rel="stylesheet">"#));
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content_with_spaced_closing_marker() {
        let html = r#"<!--[if !mso]>
            <!-- -->
            <link href="fonts.css" rel="stylesheet">
            <!--
                <![endif]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert!(!stripped.contains("[if"));
        assert!(!stripped.contains("[endif]"));
        assert!(stripped.contains(r#"<link href="fonts.css" rel="stylesheet">"#));
        assert!(!stripped.contains("<!--"));
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content_with_compact_marker() {
        let html = r#"<!--[if !mso]><!--><style>.x { color: red; }</style><!--<![endif]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert_eq!(
            style_blocks(&stripped).collect::<Vec<_>>(),
            vec![".x { color: red; }"]
        );
    }

    #[test]
    fn keeps_downlevel_revealed_conditional_content_with_bang_marker() {
        let html = r#"<!--[if !true]><! --><div class="modern">Modern</div><!-- <![endif]-->"#;

        let stripped = strip_hidden_conditional_comments(html);

        assert!(!stripped.contains("[if"));
        assert!(!stripped.contains("[endif]"));
        assert_eq!(stripped.trim(), r#"<div class="modern">Modern</div>"#);
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

        let inlined = inline_css(html, 800, 800).unwrap();

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

        let inlined = inline_css(html, 600, 800).unwrap();

        assert!(inlined.contains("width: 320px"));
        assert!(!inlined.contains("@media"));
    }

    #[test]
    fn expands_media_rules_inside_uppercase_style_tags_without_lowercase_copy() {
        let html = r#"
            <html><head><STYLE>
              .stack { width: 280px; }
              @media (max-width:720px) { .stack { width: 320px !important; } }
            </STYLE></head>
            <body><table class="stack"><tr><td>Stacked</td></tr></table></body></html>
        "#;

        let inlined = inline_css(html, 600, 800).unwrap();

        assert!(inlined.contains("width: 320px"));
        assert!(!inlined.contains("@media"));
    }

    #[test]
    fn matches_compound_media_queries_with_lightningcss() {
        let html = r#"
            <html><head><style>
              .stack { padding: 4px; }
              @media screen and (min-width:400px) and (max-width:700px) {
                .stack { padding: 8px; }
              }
            </style></head>
            <body><div class="stack">Stacked</div></body></html>
        "#;

        let active = inline_css(html, 600, 800).unwrap();
        let inactive = inline_css(html, 800, 800).unwrap();

        assert!(active.contains("padding: 8px"));
        assert!(inactive.contains("padding: 4px"));
        assert!(!inactive.contains("padding: 8px"));
    }

    #[test]
    fn fallback_media_query_matching_is_case_insensitive_without_normalizing() {
        assert!(single_media_query_matches_fallback(
            "SCREEN and (MAX-WIDTH: 720px)",
            600
        ));
        assert!(!single_media_query_matches_fallback(
            "SCREEN and (MIN-WIDTH: 720px)",
            600
        ));
        assert!(!single_media_query_matches_fallback(
            "PRINT and (MAX-WIDTH: 720px)",
            600
        ));
    }

    #[test]
    fn preserves_media_rule_cascade_position() {
        let html = r#"
            <html><head><style>
              .stack { padding: 4px; }
              @media (max-width:700px) { .stack { padding: 8px; } }
              .stack { padding: 12px; }
            </style></head>
            <body><div class="stack">Stacked</div></body></html>
        "#;

        let inlined = inline_css(html, 600, 800).unwrap();

        assert!(inlined.contains("padding: 12px"));
        assert!(!inlined.contains("padding: 8px"));
    }

    #[test]
    fn matches_orientation_media_queries() {
        let html = r#"
            <html><head><style>
              .stack { padding: 4px; }
              @media (orientation: portrait) { .stack { padding: 8px; } }
            </style></head>
            <body><div class="stack">Stacked</div></body></html>
        "#;

        let portrait = inline_css(html, 600, 800).unwrap();
        let landscape = inline_css(html, 800, 600).unwrap();

        assert!(portrait.contains("padding: 8px"));
        assert!(landscape.contains("padding: 4px"));
        assert!(!landscape.contains("padding: 8px"));
    }

    #[test]
    fn tolerates_unquoted_inline_style_attributes() {
        let html = r#"
            <html><body>
              <table class="card" style=width:504px;><tr><td>Card</td></tr></table>
            </body></html>
        "#;

        let inlined = inline_css(html, 800, 800).unwrap();

        assert!(inlined.contains("width: 504px"));
    }

    #[test]
    fn sanitizes_uppercase_inline_style_attributes_without_lowercase_copy() {
        let html = r#"
            <html><body>
              <table class="card" STYLE="width:504px;"><tr><td>Card</td></tr></table>
            </body></html>
        "#;

        let inlined = inline_css(html, 800, 800).unwrap();

        assert!(inlined.contains("width: 504px"));
    }

    #[test]
    fn strips_bare_mso_declaration_attributes_before_inlining() {
        let html = r#"
            <html><body>
              <table class="card" mso-table-lspace:0; mso-table-rspace:0; style="width:504px;"><tr><td>Card</td></tr></table>
            </body></html>
        "#;

        let inlined = inline_css(html, 800, 800).unwrap();

        assert!(inlined.contains("width: 504px"));
        assert!(!inlined.contains("mso-table-lspace:0"));
    }

    #[test]
    fn inlines_important_display_rules_over_existing_child_inline_display() {
        let html = r#"
            <html><head><style>.mobile { display: none !important; }</style></head>
            <body><table><tr><td class="mobile"><div style="display: table;">Mobile</div></td></tr></table></body></html>
        "#;

        let inlined = inline_css(html, 800, 800).unwrap();

        assert!(inlined.contains("display: none"));
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
