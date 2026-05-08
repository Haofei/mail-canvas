use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use cosmic_text::FontSystem;
use data_url::DataUrl;
use fontdb::Database;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use js_sys::Uint8Array;
use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ConsoleMessage, MailCanvasFontFallback,
    RenderOutputBackend, RenderRequest, RenderWarning, RenderedImage, RendererCore, ResourcePolicy,
    ResourceProvider, ResourceProviderFactory, repair_png_chunk_crcs,
};
use serde::Serialize;
use tiny_skia::Pixmap;
use url::Url;
use wasm_bindgen::prelude::*;

const DEFAULT_MAX_RESOURCE_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_DECODED_PIXELS: u64 = 16_000_000;
const DEFAULT_MAX_TOTAL_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_RESOURCE_COUNT: usize = 128;
const MAX_ASSET_REPORTS: usize = 512;

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

#[derive(Default)]
struct WasmOutputBackend;

impl RenderOutputBackend for WasmOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, _rendered: &mail_canvas_core::RenderedRgba) -> Result<Vec<u8>> {
        bail!("PDF is not supported in wasm")
    }
}

#[derive(Debug, Clone, Default)]
struct AssetRegistry {
    inner: Arc<Mutex<AssetRegistryInner>>,
}

#[derive(Debug, Default)]
struct AssetRegistryInner {
    entries: HashMap<String, Arc<[u8]>>,
    total_bytes: usize,
}

impl AssetRegistry {
    fn register(&self, url: &str, bytes: &[u8]) -> Result<()> {
        ensure_resource_size_with_limit(bytes.len(), DEFAULT_MAX_RESOURCE_BYTES)?;
        let key = normalize_registry_key(url)?;
        let mut inner = self.inner.lock().expect("asset registry mutex poisoned");
        let existing_len = inner.entries.get(&key).map_or(0, |bytes| bytes.len());
        if existing_len == 0 && inner.entries.len() >= DEFAULT_MAX_RESOURCE_COUNT {
            bail!(
                "asset count exceeds max-resource-count: {} > {}",
                inner.entries.len() + 1,
                DEFAULT_MAX_RESOURCE_COUNT
            );
        }
        let next_total = inner
            .total_bytes
            .saturating_sub(existing_len)
            .saturating_add(bytes.len());
        if next_total > DEFAULT_MAX_TOTAL_RESOURCE_BYTES {
            bail!(
                "asset bytes exceed max-total-resource-bytes: {} > {}",
                next_total,
                DEFAULT_MAX_TOTAL_RESOURCE_BYTES
            );
        }
        inner.entries.insert(key, Arc::<[u8]>::from(bytes));
        inner.total_bytes = next_total;
        Ok(())
    }

    fn clear(&self) {
        let mut inner = self.inner.lock().expect("asset registry mutex poisoned");
        inner.entries.clear();
        inner.total_bytes = 0;
    }

    fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("asset registry mutex poisoned")
            .entries
            .len()
    }

    fn get(&self, src: &str, base_url: Option<&Url>) -> Option<RegisteredAsset> {
        let inner = self.inner.lock().expect("asset registry mutex poisoned");
        if let Some(bytes) = inner.entries.get(src) {
            return Some(RegisteredAsset {
                request_url: src.to_string(),
                resolved_url: None,
                bytes: Arc::clone(bytes),
            });
        }
        let resolved = resolve_asset_url(src, base_url)?;
        let bytes = Arc::clone(inner.entries.get(resolved.as_str())?);
        Some(RegisteredAsset {
            request_url: src.to_string(),
            resolved_url: Some(resolved.to_string()),
            bytes,
        })
    }
}

#[derive(Debug, Clone)]
struct RegisteredAsset {
    request_url: String,
    resolved_url: Option<String>,
    bytes: Arc<[u8]>,
}

impl RegisteredAsset {
    fn source(&self) -> AssetSource {
        self.resolved_url.as_deref().map_or_else(
            || asset_source_for_url(&self.request_url),
            asset_source_for_url,
        )
    }
}

#[derive(Debug, Clone)]
struct WasmResourceProvider {
    assets: AssetRegistry,
    base_url: Option<Url>,
    policy: ResourcePolicy,
    usage: Arc<Mutex<ResourceUsage>>,
    asset_reports: Arc<Mutex<Vec<AssetReport>>>,
}

#[derive(Debug, Default)]
struct ResourceUsage {
    total_bytes: usize,
    count: usize,
}

#[derive(Debug, Clone)]
struct WasmResourceProviderFactory {
    assets: AssetRegistry,
}

impl ResourceProviderFactory for WasmResourceProviderFactory {
    type Provider = WasmResourceProvider;

    fn create(&self, request: &RenderRequest, document_base_url: Option<Url>) -> Self::Provider {
        WasmResourceProvider {
            assets: self.assets.clone(),
            base_url: request.base_url.clone().or(document_base_url),
            policy: request.resource_policy.clone(),
            usage: Arc::new(Mutex::new(ResourceUsage::default())),
            asset_reports: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ResourceProvider for WasmResourceProvider {
    fn load_image(
        &self,
        src: &str,
        initiator: &'static str,
    ) -> Result<mail_canvas_core::ImageData> {
        if let Some(asset) = self.assets.get(src, self.base_url.as_ref()) {
            ensure_resource_size_with_limit(asset.bytes.len(), self.policy.max_resource_bytes)?;
            self.record_resource_usage(asset.bytes.len())?;
            let image = decode_registered_image(
                &asset.bytes,
                self.policy.max_resource_bytes,
                self.policy.max_decoded_pixels,
            )?;
            let source = asset.source();
            self.record_asset_report(
                AssetReport::new(AssetKind::Image, AssetStatus::Loaded, asset.request_url)
                    .with_optional_resolved_url(asset.resolved_url)
                    .with_source(source)
                    .with_initiator(initiator)
                    .with_bytes(asset.bytes.len()),
            );
            return Ok(image);
        }

        if src.trim_start().starts_with("data:") {
            let (bytes, image) = load_data_image(
                src,
                self.policy.max_resource_bytes,
                self.policy.max_decoded_pixels,
            )?;
            self.record_resource_usage(bytes.len())?;
            self.record_asset_report(
                AssetReport::new(AssetKind::Image, AssetStatus::Loaded, src.to_string())
                    .with_source(AssetSource::DataUrl)
                    .with_initiator(initiator)
                    .with_bytes(bytes.len()),
            );
            return Ok(image);
        }

        self.record_asset_report(blocked_asset_report(
            AssetKind::Image,
            src,
            self.base_url.as_ref(),
            initiator,
        ));
        bail!("resource is not registered in wasm asset cache")
    }

    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Arc<[u8]>> {
        if let Some(asset) = self.assets.get(src, self.base_url.as_ref()) {
            ensure_resource_size_with_limit(asset.bytes.len(), self.policy.max_resource_bytes)?;
            self.record_resource_usage(asset.bytes.len())?;
            let source = asset.source();
            self.record_asset_report(
                AssetReport::new(kind, AssetStatus::Loaded, asset.request_url)
                    .with_optional_resolved_url(asset.resolved_url)
                    .with_source(source)
                    .with_initiator(initiator)
                    .with_bytes(asset.bytes.len()),
            );
            return Ok(Arc::clone(&asset.bytes));
        }

        if src.trim_start().starts_with("data:") {
            let data_url =
                DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
            let (bytes, _) = data_url
                .decode_to_vec()
                .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
            ensure_resource_size_with_limit(bytes.len(), self.policy.max_resource_bytes)?;
            self.record_resource_usage(bytes.len())?;
            self.record_asset_report(
                AssetReport::new(kind, AssetStatus::Loaded, src.to_string())
                    .with_source(AssetSource::DataUrl)
                    .with_initiator(initiator)
                    .with_bytes(bytes.len()),
            );
            return Ok(Arc::from(bytes));
        }

        self.record_asset_report(blocked_asset_report(
            kind,
            src,
            self.base_url.as_ref(),
            initiator,
        ));
        bail!("resource is not registered in wasm asset cache")
    }

    fn take_asset_reports(&self) -> Vec<AssetReport> {
        let mut reports = self
            .asset_reports
            .lock()
            .expect("asset report mutex poisoned");
        std::mem::take(&mut *reports)
    }

    fn record_asset_report(&self, report: AssetReport) {
        let mut reports = self
            .asset_reports
            .lock()
            .expect("asset report mutex poisoned");
        if let Some(existing) = reports.iter_mut().find(|existing| {
            existing.kind == report.kind
                && existing.request_url == report.request_url
                && existing.initiator == report.initiator
        }) {
            existing.merge_from(report);
            return;
        }
        if reports.len() < MAX_ASSET_REPORTS {
            reports.push(report);
        }
    }
}

impl WasmResourceProvider {
    fn record_resource_usage(&self, bytes: usize) -> Result<()> {
        let mut usage = self.usage.lock().expect("resource usage mutex poisoned");
        usage.count = usage.count.saturating_add(1);
        if usage.count > self.policy.max_resource_count {
            bail!(
                "resource count exceeds max-resource-count: {} > {}",
                usage.count,
                self.policy.max_resource_count
            );
        }

        usage.total_bytes = usage.total_bytes.saturating_add(bytes);
        if usage.total_bytes > self.policy.max_total_resource_bytes {
            bail!(
                "resource bytes exceed max-total-resource-bytes: {} > {}",
                usage.total_bytes,
                self.policy.max_total_resource_bytes
            );
        }
        Ok(())
    }
}

fn blocked_asset_report(
    kind: AssetKind,
    src: &str,
    base_url: Option<&Url>,
    initiator: &'static str,
) -> AssetReport {
    AssetReport::new(kind, AssetStatus::Blocked, src.to_string())
        .with_source(asset_source_for_request(src, base_url))
        .with_initiator(initiator)
        .with_detail("resource is not registered in wasm asset cache")
}

fn load_data_image(
    src: &str,
    max_resource_bytes: usize,
    max_decoded_pixels: u64,
) -> Result<(Vec<u8>, mail_canvas_core::ImageData)> {
    let data_url = DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
    let (bytes, _) = data_url
        .decode_to_vec()
        .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
    ensure_resource_size_with_limit(bytes.len(), max_resource_bytes)?;
    let image = decode_registered_image(&bytes, max_resource_bytes, max_decoded_pixels)?;
    Ok((bytes, image))
}

fn decode_registered_image(
    bytes: &[u8],
    max_resource_bytes: usize,
    max_decoded_pixels: u64,
) -> Result<mail_canvas_core::ImageData> {
    ensure_resource_size_with_limit(bytes.len(), max_resource_bytes)?;
    match decode_registered_image_strict(bytes, max_decoded_pixels) {
        Ok(image) => Ok(image),
        Err(error) => {
            let Some(repaired) = repair_png_chunk_crcs(bytes) else {
                return Err(error);
            };
            decode_registered_image_strict(&repaired, max_decoded_pixels)
                .with_context(|| format!("failed to decode image after PNG CRC repair: {error}"))
        }
    }
}

fn decode_registered_image_strict(
    bytes: &[u8],
    max_decoded_pixels: u64,
) -> Result<mail_canvas_core::ImageData> {
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(
        u32::try_from(max_decoded_pixels.min(u64::from(u32::MAX)))
            .expect("bounded decoded pixel limit"),
    );
    limits.max_image_height = Some(
        u32::try_from(max_decoded_pixels.min(u64::from(u32::MAX)))
            .expect("bounded decoded pixel limit"),
    );
    limits.max_alloc = Some(max_decoded_pixels.saturating_mul(4));
    reader.limits(limits);
    let mut decoder = reader
        .with_guessed_format()?
        .into_decoder()
        .map_err(|error| anyhow!("failed to create image decoder: {error}"))?;
    let orientation = decoder
        .orientation()
        .map_err(|error| anyhow!("failed to read image orientation: {error}"))?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let rgba = image.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    if u64::from(width) * u64::from(height) > max_decoded_pixels {
        bail!("decoded image exceeds max-decoded-pixels");
    }
    Ok(mail_canvas_core::ImageData {
        width,
        height,
        rgba: rgba.into_raw().into(),
    })
}

fn resolve_asset_url(src: &str, base_url: Option<&Url>) -> Option<Url> {
    Url::parse(src)
        .ok()
        .or_else(|| base_url.and_then(|base| base.join(src).ok()))
}

fn normalize_registry_key(url: &str) -> Result<String> {
    if url.trim().is_empty() {
        bail!("asset URL must not be empty");
    }
    if let Ok(parsed) = Url::parse(url) {
        return Ok(parsed.to_string());
    }
    Ok(url.to_string())
}

fn asset_source_for_url(src: &str) -> AssetSource {
    if src.trim_start().starts_with("data:") {
        AssetSource::DataUrl
    } else if src.starts_with("http://") || src.starts_with("https://") {
        AssetSource::Remote
    } else {
        AssetSource::File
    }
}

fn asset_source_for_request(src: &str, base_url: Option<&Url>) -> AssetSource {
    resolve_asset_url(src, base_url).map_or_else(
        || asset_source_for_url(src),
        |url| asset_source_for_url(url.as_str()),
    )
}

fn ensure_resource_size_with_limit(bytes: usize, max_bytes: usize) -> Result<()> {
    if bytes > max_bytes {
        bail!("resource bytes exceed max-resource-bytes: {bytes} > {max_bytes}");
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
struct DiagnosticsSnapshot {
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

fn diagnostics_json(snapshot: &DiagnosticsSnapshot) -> String {
    serde_json::to_string(snapshot)
        .unwrap_or_else(|_| "{\"warnings\":[],\"assets\":[],\"console_messages\":[]}".to_string())
}

fn diagnostics_json_from_parts(
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

fn js_error(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_relative_urls_against_base() {
        let registry = AssetRegistry::default();
        registry
            .register("https://cdn.example.com/assets/logo.png", &[1, 2, 3])
            .unwrap();

        let asset = registry
            .get(
                "./logo.png",
                Some(&Url::parse("https://cdn.example.com/assets/email.html").unwrap()),
            )
            .expect("resolved asset");

        assert_eq!(asset.request_url, "./logo.png");
        assert_eq!(
            asset.resolved_url.as_deref(),
            Some("https://cdn.example.com/assets/logo.png")
        );
        assert_eq!(asset.bytes.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn registry_enforces_asset_count_limit() {
        let registry = AssetRegistry::default();
        for index in 0..DEFAULT_MAX_RESOURCE_COUNT {
            registry
                .register(&format!("https://cdn.example.com/{index}.css"), &[1])
                .unwrap();
        }

        let error = registry
            .register("https://cdn.example.com/overflow.css", &[1])
            .unwrap_err();

        assert!(error.to_string().contains("max-resource-count"));
    }

    #[test]
    fn asset_source_uses_resolved_url() {
        let registry = AssetRegistry::default();
        registry
            .register("https://cdn.example.com/assets/logo.png", &[1, 2, 3])
            .unwrap();
        let asset = registry
            .get(
                "./logo.png",
                Some(&Url::parse("https://cdn.example.com/assets/email.html").unwrap()),
            )
            .expect("resolved asset");

        assert_eq!(asset.source(), AssetSource::Remote);
        assert_eq!(
            asset_source_for_request(
                "./missing.png",
                Some(&Url::parse("https://cdn.example.com/assets/email.html").unwrap()),
            ),
            AssetSource::Remote
        );
    }

    #[test]
    fn diagnostics_json_contains_render_sections() {
        let json = diagnostics_json(&DiagnosticsSnapshot::default());
        assert!(json.contains("\"warnings\""));
        assert!(json.contains("\"assets\""));
        assert!(json.contains("\"console_messages\""));
    }

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
