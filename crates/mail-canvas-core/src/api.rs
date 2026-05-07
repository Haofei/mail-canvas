use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use url::Url;

use crate::debug::RenderDebugSnapshot;

pub(crate) const MAX_CONSOLE_MESSAGES: usize = 50;
pub(crate) const MAX_CONSOLE_MESSAGE_LEN: usize = 2048;
pub(crate) const MAX_RENDER_WARNINGS: usize = 100;
pub(crate) const DEFAULT_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_TOTAL_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;
pub(crate) const DEFAULT_MAX_RESOURCE_COUNT: usize = 128;
pub(crate) const DEFAULT_MAX_DOM_NODES: usize = 100_000;
pub(crate) const DEFAULT_MAX_LAYOUT_DEPTH: usize = 64;
pub(crate) const DEFAULT_MAX_TABLE_CELLS: usize = 100_000;

#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    pub allow_remote: bool,
    pub https_only: bool,
    pub deny_private_networks: bool,
    pub timeout: Duration,
    pub max_resource_bytes: usize,
    pub max_total_resource_bytes: usize,
    pub max_decoded_pixels: u64,
    pub max_resource_count: usize,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            allow_remote: false,
            https_only: true,
            deny_private_networks: true,
            timeout: Duration::from_secs(30),
            max_resource_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_total_resource_bytes: DEFAULT_MAX_TOTAL_RESOURCE_BYTES,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
            max_resource_count: DEFAULT_MAX_RESOURCE_COUNT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub html: String,
    pub width: u32,
    pub viewport_height: u32,
    pub min_height: u32,
    pub scale: f32,
    pub base_url: Option<Url>,
    pub max_height: Option<u32>,
    pub resource_policy: ResourcePolicy,
    pub max_dom_nodes: usize,
    pub max_layout_depth: usize,
    pub max_table_cells: usize,
    pub debug: RenderDebugOptions,
}

impl RenderRequest {
    pub fn defaults_for_html(html: String, width: u32, viewport_height: u32, scale: f32) -> Self {
        Self {
            html,
            width,
            viewport_height,
            min_height: 1,
            scale,
            base_url: None,
            max_height: None,
            resource_policy: ResourcePolicy::default(),
            max_dom_nodes: DEFAULT_MAX_DOM_NODES,
            max_layout_depth: DEFAULT_MAX_LAYOUT_DEPTH,
            max_table_cells: DEFAULT_MAX_TABLE_CELLS,
            debug: RenderDebugOptions::default(),
        }
    }

    pub fn with_base_url(mut self, base_url: Option<Url>) -> Self {
        self.base_url = base_url;
        self
    }

    pub fn with_resource_policy(mut self, resource_policy: ResourcePolicy) -> Self {
        self.resource_policy = resource_policy;
        self
    }

    pub fn with_min_height(mut self, min_height: u32) -> Self {
        self.min_height = min_height;
        self
    }

    pub fn with_max_height(mut self, max_height: Option<u32>) -> Self {
        self.max_height = max_height;
        self
    }

    pub fn with_debug(mut self, debug: RenderDebugOptions) -> Self {
        self.debug = debug;
        self
    }

    pub fn with_limits(
        mut self,
        max_dom_nodes: usize,
        max_layout_depth: usize,
        max_table_cells: usize,
    ) -> Self {
        self.max_dom_nodes = max_dom_nodes;
        self.max_layout_depth = max_layout_depth;
        self.max_table_cells = max_table_cells;
        self
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderDebugOptions {
    pub layout: bool,
    pub text_rects: bool,
    pub image_diagnostics: bool,
}

impl RenderDebugOptions {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn layout_dump() -> Self {
        Self {
            layout: true,
            text_rects: true,
            image_diagnostics: true,
        }
    }

    pub(crate) fn any(self) -> bool {
        self.layout || self.text_rects || self.image_diagnostics
    }
}

#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub png: Vec<u8>,
    pub css_width: u32,
    pub css_height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: f32,
    pub content_css_width: u32,
    pub console_messages: Vec<ConsoleMessage>,
    pub warnings: Vec<RenderWarning>,
    pub assets: Vec<AssetReport>,
    pub debug: Option<RenderDebugSnapshot>,
}

#[derive(Debug, Clone)]
pub struct RenderedPdf {
    pub pdf: Vec<u8>,
    pub css_width: u32,
    pub css_height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale: f32,
    pub console_messages: Vec<ConsoleMessage>,
    pub warnings: Vec<RenderWarning>,
    pub assets: Vec<AssetReport>,
    pub debug: Option<RenderDebugSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderDiagnosticsReport {
    pub warnings: Vec<RenderWarning>,
    pub assets: Vec<AssetReport>,
    pub console_messages: Vec<ConsoleMessage>,
}

impl RenderedImage {
    pub fn diagnostics(&self) -> RenderDiagnosticsReport {
        RenderDiagnosticsReport {
            warnings: self.warnings.clone(),
            assets: self.assets.clone(),
            console_messages: self.console_messages.clone(),
        }
    }
}

impl RenderedPdf {
    pub fn diagnostics(&self) -> RenderDiagnosticsReport {
        RenderDiagnosticsReport {
            warnings: self.warnings.clone(),
            assets: self.assets.clone(),
            console_messages: self.console_messages.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_request_builder_methods_update_optional_fields() {
        let request = RenderRequest::defaults_for_html("<p>Hello</p>".to_string(), 600, 800, 1.0)
            .with_min_height(320)
            .with_max_height(Some(1200))
            .with_limits(1000, 32, 500)
            .with_debug(RenderDebugOptions::layout_dump());

        assert_eq!(request.min_height, 320);
        assert_eq!(request.max_height, Some(1200));
        assert_eq!(request.max_dom_nodes, 1000);
        assert_eq!(request.max_layout_depth, 32);
        assert_eq!(request.max_table_cells, 500);
        assert!(request.debug.layout);
        assert!(request.debug.text_rects);
        assert!(request.debug.image_diagnostics);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsoleMessage {
    pub level: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderWarningCode {
    ImageLoadFailed,
    LayoutLimitReached,
    StylesheetLoadFailed,
    UnsupportedCssDeclaration,
    WebFontLimitReached,
    WebFontLoadFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Image,
    Stylesheet,
    WebFont,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSource {
    DataUrl,
    File,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Loaded,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetReport {
    pub kind: AssetKind,
    pub status: AssetStatus,
    pub request_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<AssetSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub attempts: u32,
}

impl AssetReport {
    pub fn new(kind: AssetKind, status: AssetStatus, request_url: impl Into<String>) -> Self {
        Self {
            kind,
            status,
            request_url: request_url.into(),
            resolved_url: None,
            source: None,
            initiator: None,
            bytes: None,
            detail: None,
            attempts: 1,
        }
    }

    pub fn with_resolved_url(mut self, resolved_url: impl Into<String>) -> Self {
        self.resolved_url = Some(resolved_url.into());
        self
    }

    pub fn with_optional_resolved_url(mut self, resolved_url: Option<String>) -> Self {
        self.resolved_url = resolved_url;
        self
    }

    pub fn with_source(mut self, source: AssetSource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn with_initiator(mut self, initiator: impl Into<String>) -> Self {
        self.initiator = Some(initiator.into());
        self
    }

    pub fn with_bytes(mut self, bytes: usize) -> Self {
        self.bytes = Some(bytes);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn merge_from(&mut self, newer: Self) {
        self.attempts = self.attempts.saturating_add(newer.attempts);
        self.status = newer.status;
        self.bytes = newer.bytes.or(self.bytes);
        self.detail = newer.detail.or(self.detail.clone());
        self.source = newer.source.or(self.source);
        self.resolved_url = newer.resolved_url.or(self.resolved_url.clone());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderWarning {
    pub code: RenderWarningCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl RenderWarning {
    pub fn new(code: RenderWarningCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            node: None,
            property: None,
            value: None,
            url: None,
        }
    }

    pub fn with_node(mut self, node: impl Into<String>) -> Self {
        self.node = Some(node.into());
        self
    }

    pub fn with_property(mut self, property: impl Into<String>, value: impl Into<String>) -> Self {
        self.property = Some(property.into());
        self.value = Some(value.into());
        self
    }

    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }
}

#[derive(Debug, Default)]
pub(crate) struct RenderDiagnostics {
    pub(crate) console_messages: Vec<ConsoleMessage>,
    pub(crate) warnings: Vec<RenderWarning>,
}

impl RenderDiagnostics {
    pub(crate) fn push_warning(&mut self, warning: RenderWarning) {
        push_console_message(&mut self.console_messages, "warn", &warning.message);
        if self.warnings.len() >= MAX_RENDER_WARNINGS {
            return;
        }
        self.warnings.push(warning);
    }
}

pub(crate) fn push_console_message(
    messages: &mut Vec<ConsoleMessage>,
    level: &'static str,
    message: &str,
) {
    if messages.len() >= MAX_CONSOLE_MESSAGES {
        return;
    }

    let mut message = message.to_string();
    if message.len() > MAX_CONSOLE_MESSAGE_LEN {
        message.truncate(MAX_CONSOLE_MESSAGE_LEN);
        message.push_str("... (truncated)");
    }
    messages.push(ConsoleMessage { level, message });
}

pub trait EmailRenderer {
    fn render_png(&mut self, request: RenderRequest) -> Result<RenderedImage>;
    fn render_pdf(&mut self, request: RenderRequest) -> Result<RenderedPdf>;
}
