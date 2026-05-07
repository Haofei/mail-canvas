use url::Url;

#[derive(Debug, Clone)]
pub struct PreparedDocument {
    pub html: String,
    pub base_url: Option<Url>,
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

pub(crate) fn inject_head_markup(source_html: &str, head: &str) -> String {
    let lower = source_html.to_ascii_lowercase();

    if let Some(open_head_index) = lower.find("<head") {
        if let Some(close_offset) = source_html[open_head_index..].find('>') {
            let insert_at = open_head_index + close_offset + 1;
            let mut out = String::with_capacity(source_html.len() + head.len());
            out.push_str(&source_html[..insert_at]);
            out.push_str(head);
            out.push_str(&source_html[insert_at..]);
            return out;
        }
    }

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
