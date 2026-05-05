use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use cosmic_text::FontSystem;
use data_url::DataUrl;
use fontdb::Database;
use image::{ImageReader, Limits};
use js_sys::Uint8Array;
use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, RenderOutputBackend, RenderRequest,
    RenderedImage, RendererCore, ResourceProvider, ResourceProviderFactory,
};
use tiny_skia::Pixmap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmRenderer {
    inner: RendererCore,
    output: WasmOutputBackend,
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
            cosmic_text::PlatformFallback,
        );
        Ok(Self {
            inner: RendererCore::new(font_system),
            output: WasmOutputBackend::default(),
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

    pub fn render_png(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
    ) -> Result<Uint8Array, JsValue> {
        let request =
            RenderRequest::defaults_for_html(html.to_string(), width, viewport_height, scale);
        let rendered = self
            .inner
            .render_png_with(request, &WasmResourceProviderFactory, &self.output)
            .map_err(js_error)?;
        Ok(Uint8Array::from(rendered.png.as_slice()))
    }

    pub fn render_rgba(
        &mut self,
        html: &str,
        width: u32,
        viewport_height: u32,
        scale: f32,
    ) -> Result<RenderedRgba, JsValue> {
        let request =
            RenderRequest::defaults_for_html(html.to_string(), width, viewport_height, scale);
        self.inner
            .render_png_with(request, &WasmResourceProviderFactory, &self.output)
            .map_err(js_error)?;
        let snapshot = self
            .output
            .take_snapshot()
            .ok_or_else(|| JsValue::from_str("missing RGBA snapshot"))?;
        Ok(RenderedRgba {
            width: snapshot.width,
            height: snapshot.height,
            data: snapshot.rgba,
        })
    }
}

#[derive(Default)]
struct WasmOutputBackend {
    snapshot: RefCell<Option<RgbaSnapshot>>,
}

#[derive(Clone)]
struct RgbaSnapshot {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl WasmOutputBackend {
    fn take_snapshot(&self) -> Option<RgbaSnapshot> {
        self.snapshot.borrow_mut().take()
    }
}

impl RenderOutputBackend for WasmOutputBackend {
    fn encode_png(&self, pixmap: &Pixmap) -> Result<Vec<u8>> {
        self.snapshot.replace(Some(RgbaSnapshot {
            width: pixmap.width(),
            height: pixmap.height(),
            rgba: pixmap.data().to_vec(),
        }));
        pixmap.encode_png().map_err(Into::into)
    }

    fn encode_pdf(&self, _rendered: &RenderedImage) -> Result<Vec<u8>> {
        bail!("PDF is not supported in wasm")
    }
}

#[derive(Debug, Clone)]
struct WasmResourceProvider {
    asset_reports: Arc<Mutex<Vec<AssetReport>>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct WasmResourceProviderFactory;

impl ResourceProviderFactory for WasmResourceProviderFactory {
    type Provider = WasmResourceProvider;

    fn create(
        &self,
        _request: &RenderRequest,
        _document_base_url: Option<url::Url>,
    ) -> Self::Provider {
        WasmResourceProvider {
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
        if !src.trim_start().starts_with("data:") {
            self.record_asset_report(
                AssetReport::new(AssetKind::Image, AssetStatus::Blocked, src.to_string())
                    .with_initiator(initiator),
            );
            bail!("only data URLs are supported in wasm");
        }
        let data_url =
            DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
        let (bytes, _) = data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
        let mut reader = ImageReader::new(std::io::Cursor::new(&bytes));
        reader = reader.with_guessed_format()?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        reader.limits(limits);
        let rgba = reader.decode()?.to_rgba8();
        self.record_asset_report(
            AssetReport::new(AssetKind::Image, AssetStatus::Loaded, src.to_string())
                .with_source(AssetSource::DataUrl)
                .with_initiator(initiator)
                .with_bytes(bytes.len()),
        );
        Ok(mail_canvas_core::ImageData {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    }

    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Vec<u8>> {
        if !src.trim_start().starts_with("data:") {
            self.record_asset_report(
                AssetReport::new(kind, AssetStatus::Blocked, src.to_string())
                    .with_initiator(initiator),
            );
            bail!("only data URLs are supported in wasm");
        }
        let data_url =
            DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
        let (bytes, _) = data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
        self.record_asset_report(
            AssetReport::new(kind, AssetStatus::Loaded, src.to_string())
                .with_source(AssetSource::DataUrl)
                .with_initiator(initiator)
                .with_bytes(bytes.len()),
        );
        Ok(bytes)
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
        reports.push(report);
    }
}

fn js_error(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&error.to_string())
}
