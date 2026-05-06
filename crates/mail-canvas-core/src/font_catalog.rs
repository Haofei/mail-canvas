#[cfg(test)]
pub(crate) const SANS_FALLBACK_CANDIDATES: &[&str] = &[
    "Arial",
    "Helvetica",
    "Helvetica Neue",
    "Avenir",
    "Segoe UI",
    "Roboto",
    "Open Sans",
    "DejaVu Sans",
    "Arimo",
    "Noto Sans",
];

#[cfg(test)]
pub(crate) const SERIF_FALLBACK_CANDIDATES: &[&str] = &[
    "Times",
    "Times New Roman",
    "Georgia",
    "Palatino",
    "Palatino Linotype",
    "Iowan Old Style",
    "DejaVu Serif",
    "Tinos",
    "Noto Serif",
];

#[cfg(test)]
pub(crate) const MONO_FALLBACK_CANDIDATES: &[&str] = &[
    "Courier New",
    "Menlo",
    "Monaco",
    "Consolas",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
];

pub(crate) fn normalized_font_family(font_family: Option<&str>) -> Option<String> {
    font_family.map(|family| family.trim().trim_matches(['"', '\'']).to_ascii_lowercase())
}

pub(crate) fn generic_font_family(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "sans-serif" | "ui-sans-serif" | "system-ui" | "-apple-system" => Some("sans-serif"),
        "serif" | "ui-serif" => Some("serif"),
        "monospace" | "ui-monospace" => Some("monospace"),
        _ => None,
    }
}

pub(crate) fn is_safe_system_font(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "arial"
            | "arial nova"
            | "avenir"
            | "avenir next"
            | "avenir next lt pro"
            | "helvetica"
            | "helvetica neue"
            | "nimbus sans"
            | "segoe ui"
            | "corbel"
            | "georgia"
            | "times"
            | "times new roman"
            | "cambria"
            | "courier"
            | "courier new"
    )
}

pub(crate) fn safe_system_font_generic(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "arial" | "arial nova" | "avenir" | "avenir next" | "avenir next lt pro" | "helvetica"
        | "helvetica neue" | "nimbus sans" | "segoe ui" | "corbel" => Some("sans-serif"),
        "georgia" | "times" | "times new roman" | "cambria" => Some("serif"),
        "courier" | "courier new" => Some("monospace"),
        _ => None,
    }
}
