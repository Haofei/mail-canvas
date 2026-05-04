use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow};
use url::Url;

#[derive(Debug, Clone)]
pub struct PreparedDocument {
    pub html: String,
    pub base_url: Option<Url>,
}

pub fn build_document_from_files(
    html_path: &Path,
    css_path: Option<&Path>,
    base_url: Option<&str>,
    width: u32,
) -> Result<PreparedDocument> {
    let html = fs::read_to_string(html_path)
        .with_context(|| format!("failed to read {}", html_path.display()))?;
    let css = match css_path {
        Some(path) => Some(
            fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        ),
        None => None,
    };

    let base = match base_url {
        Some(raw) => Some(Url::parse(raw).with_context(|| format!("invalid --base-url {raw}"))?),
        None => {
            let dir = html_path.parent().unwrap_or_else(|| Path::new("."));
            let dir = dir.canonicalize().with_context(|| {
                format!("failed to resolve HTML parent directory: {}", dir.display())
            })?;
            Some(Url::from_directory_path(&dir).map_err(|()| {
                anyhow!(
                    "failed to convert HTML parent directory to base URL: {}",
                    dir.display()
                )
            })?)
        }
    };

    Ok(PreparedDocument {
        html: build_document(&html, css.as_deref(), base.as_ref(), width),
        base_url: base,
    })
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
