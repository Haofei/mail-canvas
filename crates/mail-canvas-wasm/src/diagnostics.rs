use mail_canvas_core::{AssetReport, ConsoleMessage, RenderWarning};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DiagnosticsSnapshot {
    warnings: Vec<RenderWarning>,
    assets: Vec<AssetReport>,
    console_messages: Vec<ConsoleMessage>,
}

#[derive(Serialize)]
struct DiagnosticsSnapshotRef<'a> {
    warnings: &'a [RenderWarning],
    assets: &'a [AssetReport],
    console_messages: &'a [ConsoleMessage],
}

pub(crate) fn diagnostics_json(snapshot: &DiagnosticsSnapshot) -> String {
    serde_json::to_string(snapshot)
        .unwrap_or_else(|_| "{\"warnings\":[],\"assets\":[],\"console_messages\":[]}".to_string())
}

pub(crate) fn diagnostics_json_from_parts(
    warnings: &[RenderWarning],
    assets: &[AssetReport],
    console_messages: &[ConsoleMessage],
) -> String {
    serde_json::to_string(&DiagnosticsSnapshotRef {
        warnings,
        assets,
        console_messages,
    })
    .unwrap_or_else(|_| "{\"warnings\":[],\"assets\":[],\"console_messages\":[]}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_json_contains_render_sections() {
        let json = diagnostics_json(&DiagnosticsSnapshot::default());
        assert!(json.contains("\"warnings\""));
        assert!(json.contains("\"assets\""));
        assert!(json.contains("\"console_messages\""));
    }
}
