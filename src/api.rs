use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use url::Url;

pub(crate) const MAX_CONSOLE_MESSAGES: usize = 50;
pub(crate) const MAX_CONSOLE_MESSAGE_LEN: usize = 2048;
pub(crate) const MAX_RENDER_WARNINGS: usize = 100;
pub(crate) const DEFAULT_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;

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
