use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use url::Url;

pub(crate) const MAX_CONSOLE_MESSAGES: usize = 50;
pub(crate) const MAX_CONSOLE_MESSAGE_LEN: usize = 2048;
pub(crate) const MAX_RENDER_WARNINGS: usize = 100;
pub(crate) const DEFAULT_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;
pub(crate) const DEFAULT_MAX_DOM_NODES: usize = 100_000;
pub(crate) const DEFAULT_MAX_LAYOUT_DEPTH: usize = 64;
pub(crate) const DEFAULT_MAX_TABLE_CELLS: usize = 100_000;

#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub html: String,
    pub width: u32,
    pub viewport_height: u32,
    pub min_height: u32,
    pub scale: f32,
    pub timeout: Duration,
    pub settle: Duration,
    pub base_url: Option<Url>,
    pub max_height: Option<u32>,
    pub allow_remote: bool,
    pub https_only: bool,
    pub max_image_bytes: usize,
    pub max_decoded_pixels: u64,
    pub max_dom_nodes: usize,
    pub max_layout_depth: usize,
    pub max_table_cells: usize,
}

impl RenderRequest {
    pub fn defaults_for_html(html: String, width: u32, viewport_height: u32, scale: f32) -> Self {
        Self {
            html,
            width,
            viewport_height,
            min_height: 1,
            scale,
            timeout: Duration::from_secs(30),
            settle: Duration::ZERO,
            base_url: None,
            max_height: None,
            allow_remote: false,
            https_only: true,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
            max_dom_nodes: DEFAULT_MAX_DOM_NODES,
            max_layout_depth: DEFAULT_MAX_LAYOUT_DEPTH,
            max_table_cells: DEFAULT_MAX_TABLE_CELLS,
        }
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
