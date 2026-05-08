//! Fallback normalization for common email-safe font families.
//!
//! Keep this catalog limited to broadly used system/email-safe font names and
//! generic-family mapping. Do not add template-specific font hacks here; if one
//! template looks off, fix the font selection or metrics algorithm instead of
//! adding a one-off family rule.

#[cfg(test)]
pub(crate) const SANS_FALLBACK_CANDIDATES: &[&str] = &[
    "Arial",
    "Helvetica",
    "Helvetica Neue",
    "Avenir",
    "Segoe UI",
    "Tahoma",
    "Trebuchet MS",
    "Verdana",
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

const SANS_GENERIC_NAMES: &[&str] = &["sans-serif", "ui-sans-serif", "system-ui", "-apple-system"];
const SERIF_GENERIC_NAMES: &[&str] = &["serif", "ui-serif"];
const MONO_GENERIC_NAMES: &[&str] = &["monospace", "ui-monospace"];

const SAFE_SANS_SYSTEM_FONTS: &[&str] = &[
    "arial",
    "arial nova",
    "avenir",
    "avenir next",
    "avenir next lt pro",
    "helvetica",
    "helvetica neue",
    "nimbus sans",
    "segoe ui",
    "tahoma",
    "trebuchet ms",
    "verdana",
    "lucida grande",
    "lucida sans",
    "lucida sans unicode",
    "corbel",
];
const SAFE_SERIF_SYSTEM_FONTS: &[&str] = &["georgia", "times", "times new roman", "cambria"];
const SAFE_MONO_SYSTEM_FONTS: &[&str] = &["courier", "courier new"];

pub(crate) fn normalized_font_family(font_family: Option<&str>) -> Option<String> {
    font_family.map(|family| family.trim().trim_matches(['"', '\'']).to_ascii_lowercase())
}

pub(crate) fn generic_font_family(value: &str) -> Option<&'static str> {
    if eq_ignore_ascii_case_any(value, SANS_GENERIC_NAMES) {
        Some("sans-serif")
    } else if eq_ignore_ascii_case_any(value, SERIF_GENERIC_NAMES) {
        Some("serif")
    } else if eq_ignore_ascii_case_any(value, MONO_GENERIC_NAMES) {
        Some("monospace")
    } else {
        None
    }
}

pub(crate) fn is_safe_system_font(value: &str) -> bool {
    safe_system_font_generic(value).is_some()
}

pub(crate) fn safe_system_font_generic(value: &str) -> Option<&'static str> {
    if eq_ignore_ascii_case_any(value, SAFE_SANS_SYSTEM_FONTS) {
        Some("sans-serif")
    } else if eq_ignore_ascii_case_any(value, SAFE_SERIF_SYSTEM_FONTS) {
        Some("serif")
    } else if eq_ignore_ascii_case_any(value, SAFE_MONO_SYSTEM_FONTS) {
        Some("monospace")
    } else {
        None
    }
}

fn eq_ignore_ascii_case_any(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_font_family_is_case_insensitive_without_normalizing() {
        assert_eq!(generic_font_family("SYSTEM-UI"), Some("sans-serif"));
        assert_eq!(generic_font_family("Ui-Serif"), Some("serif"));
        assert_eq!(generic_font_family("ui-MONOSPACE"), Some("monospace"));
    }

    #[test]
    fn safe_system_font_generic_is_case_insensitive_without_normalizing() {
        assert_eq!(
            safe_system_font_generic("Helvetica Neue"),
            Some("sans-serif")
        );
        assert_eq!(safe_system_font_generic("TIMES NEW ROMAN"), Some("serif"));
        assert_eq!(safe_system_font_generic("Courier New"), Some("monospace"));
        assert!(is_safe_system_font("ARIAL"));
    }
}
