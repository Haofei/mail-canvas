use std::sync::Arc;

use anyhow::Result;
use cosmic_text::FontSystem;
use fontdb::Database;
use js_sys::Uint8Array;
use mail_canvas_core::{
    MailCanvasFontFallback, RenderRequest, RenderedImage, RendererCore, ResourcePolicy,
};
use url::Url;
use wasm_bindgen::prelude::*;

mod diagnostics;
mod output;
mod resource;

use diagnostics::{DiagnosticsSnapshot, diagnostics_json, diagnostics_json_from_parts};
use output::WasmOutputBackend;
use resource::{AssetRegistry, WasmResourceProviderFactory};

pub(crate) const DEFAULT_MAX_RESOURCE_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;
pub(crate) const DEFAULT_MAX_TOTAL_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_RESOURCE_COUNT: usize = 128;

#[wasm_bindgen]
pub struct WasmRenderer {
    inner: RendererCore,
    output: WasmOutputBackend,
    assets: AssetRegistry,
    last_diagnostics_json: String,
}

#[wasm_bindgen]
pub struct RenderedRgba {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

#[wasm_bindgen]
impl RenderedRgba {
    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn data(&self) -> Uint8Array {
        Uint8Array::from(self.data.as_slice())
    }
}

#[wasm_bindgen]
impl WasmRenderer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WasmRenderer, JsValue> {
        let db = Database::new();
        let font_system = FontSystem::new_with_locale_and_db_and_fallback(
            "en-US".to_string(),
            db,
            MailCanvasFontFallback,
        );
        Ok(Self {
            inner: RendererCore::new(font_system),
            output: WasmOutputBackend,
            assets: AssetRegistry::default(),
            last_diagnostics_json: diagnostics_json(&DiagnosticsSnapshot::default()),
        })
    }

    pub fn register_font(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let db = self.inner.font_system_mut().db_mut();
        let ids = db.load_font_source(fontdb::Source::Binary(Arc::new(bytes.to_vec())));
        if ids.is_empty() {
            return Err(JsValue::from_str(
                "font bytes did not contain a loadable face",
            ));
        }
        Ok(())
    }

    pub fn register_asset(&mut self, url: &str, bytes: &[u8]) -> Result<(), JsValue> {
        self.assets.register(url, bytes).map_err(js_error)
    }

    pub fn clear_assets(&mut self) {
        self.assets.clear();
    }

    pub fn asset_count(&self) -> u32 {
        u32::try_from(self.assets.len()).unwrap_or(u32::MAX)
    }

    pub fn render_png(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
    ) -> Result<Uint8Array, JsValue> {
        self.render_png_with_base_url(html, width, viewport_height, scale, "")
    }

    pub fn render_png_with_base_url(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
        base_url: &str,
    ) -> Result<Uint8Array, JsValue> {
        self.render_png_with_base_url_and_max_height(
            html,
            width,
            viewport_height,
            scale,
            base_url,
            0,
        )
    }

    pub fn render_png_with_base_url_and_max_height(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
        base_url: &str,
        max_height: u32,
    ) -> Result<Uint8Array, JsValue> {
        let mut request = build_request(
            html,
            width,
            viewport_height,
            scale,
            parse_optional_url(base_url)?,
        );
        if max_height > 0 {
            request.max_height = Some(max_height);
        }
        let rendered = self.render_with_request(request).map_err(js_error)?;
        Ok(Uint8Array::from(rendered.png.as_slice()))
    }

    pub fn render_rgba(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
    ) -> Result<RenderedRgba, JsValue> {
        self.render_rgba_with_base_url(html, width, viewport_height, scale, "")
    }

    pub fn render_rgba_with_base_url(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
        base_url: &str,
    ) -> Result<RenderedRgba, JsValue> {
        let rendered = self
            .render_rgba_with_request(build_request(
                html,
                width,
                viewport_height,
                scale,
                parse_optional_url(base_url)?,
            ))
            .map_err(js_error)?;
        Ok(RenderedRgba {
            width: rendered.pixel_width,
            height: rendered.pixel_height,
            data: rendered.rgba,
        })
    }

    pub fn diagnostics_json(&self) -> String {
        self.last_diagnostics_json.clone()
    }
}

impl WasmRenderer {
    fn render_with_request(&mut self, request: RenderRequest) -> Result<RenderedImage> {
        let rendered = self.inner.render_png_with(
            request,
            &WasmResourceProviderFactory {
                assets: self.assets.clone(),
            },
            &self.output,
        )?;
        self.last_diagnostics_json = diagnostics_json_from_parts(
            &rendered.warnings,
            &rendered.assets,
            &rendered.console_messages,
        );
        Ok(rendered)
    }

    fn render_rgba_with_request(
        &mut self,
        request: RenderRequest,
    ) -> Result<mail_canvas_core::RenderedRgba> {
        let rendered = self.inner.render_rgba_with(
            request,
            &WasmResourceProviderFactory {
                assets: self.assets.clone(),
            },
        )?;
        self.last_diagnostics_json = diagnostics_json_from_parts(
            &rendered.warnings,
            &rendered.assets,
            &rendered.console_messages,
        );
        Ok(rendered)
    }
}

fn build_request(
    html: &str,
    width: u32,
    viewport_height: u32,
    scale: f32,
    base_url: Option<Url>,
) -> RenderRequest {
    let mut request =
        RenderRequest::defaults_for_html(html.to_string(), width, viewport_height, scale);
    request.base_url = base_url;
    request.resource_policy = ResourcePolicy {
        allow_remote: false,
        https_only: true,
        deny_private_networks: true,
        timeout: std::time::Duration::from_secs(30),
        max_resource_bytes: DEFAULT_MAX_RESOURCE_BYTES,
        max_total_resource_bytes: DEFAULT_MAX_TOTAL_RESOURCE_BYTES,
        max_decoded_pixels: DEFAULT_MAX_DECODED_PIXELS,
        max_resource_count: DEFAULT_MAX_RESOURCE_COUNT,
    };
    request
}

fn parse_optional_url(raw: &str) -> Result<Option<Url>, JsValue> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Url::parse(trimmed)
        .map(Some)
        .map_err(|error| JsValue::from_str(&format!("invalid base URL: {error}")))
}

fn js_error(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_canvas_core::{AssetSource, AssetStatus};

    #[test]
    fn renderer_uses_registered_assets_for_relative_urls() {
        let mut renderer = WasmRenderer::new().expect("renderer");
        renderer
            .register_asset(
                "https://cdn.example.com/assets/logo.gif",
                &[
                    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00,
                    0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02,
                    0x44, 0x01, 0x00, 0x3b,
                ],
            )
            .expect("register asset");

        let rendered = renderer
            .render_with_request(build_request(
                "<img src=\"./logo.gif\" width=\"1\" height=\"1\" alt=\"\">",
                16,
                16,
                1.0,
                Some(Url::parse("https://cdn.example.com/assets/email.html").unwrap()),
            ))
            .expect("render");

        assert_eq!(rendered.assets.len(), 1);
        assert_eq!(rendered.assets[0].status, AssetStatus::Loaded);
        assert_eq!(
            rendered.assets[0].resolved_url.as_deref(),
            Some("https://cdn.example.com/assets/logo.gif")
        );
        assert_eq!(rendered.assets[0].source, Some(AssetSource::Remote));

        let diagnostics: serde_json::Value =
            serde_json::from_str(&renderer.diagnostics_json()).expect("diagnostics json");
        assert_eq!(diagnostics["assets"][0]["status"], "loaded");
    }

    #[test]
    fn render_rgba_returns_direct_pixel_buffer() {
        let mut renderer = WasmRenderer::new().expect("renderer");
        let rendered = renderer
            .render_rgba_with_request(build_request(
                "<div style=\"width:10px;height:8px;background:#336699\"></div>",
                20,
                20,
                2.0,
                None,
            ))
            .expect("render rgba");

        assert_eq!(rendered.pixel_width, 40);
        assert_eq!(rendered.pixel_height, 16);
        assert_eq!(
            rendered.rgba.len(),
            (rendered.pixel_width * rendered.pixel_height * 4) as usize
        );
        let diagnostics: serde_json::Value =
            serde_json::from_str(&renderer.diagnostics_json()).expect("diagnostics json");
        assert_eq!(diagnostics["warnings"].as_array().unwrap().len(), 0);
    }
}
