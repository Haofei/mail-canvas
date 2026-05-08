use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use data_url::DataUrl;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ImageData, RenderRequest, ResourcePolicy,
    ResourceProvider, ResourceProviderFactory, repair_png_chunk_crcs,
};
use url::Url;

use crate::{
    DEFAULT_MAX_RESOURCE_BYTES, DEFAULT_MAX_RESOURCE_COUNT, DEFAULT_MAX_TOTAL_RESOURCE_BYTES,
};

const MAX_ASSET_REPORTS: usize = 512;

#[derive(Debug, Clone, Default)]
pub(crate) struct AssetRegistry {
    inner: Arc<Mutex<AssetRegistryInner>>,
}

#[derive(Debug, Default)]
struct AssetRegistryInner {
    entries: HashMap<String, Arc<[u8]>>,
    total_bytes: usize,
}

impl AssetRegistry {
    pub(crate) fn register(&self, url: &str, bytes: &[u8]) -> Result<()> {
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

    pub(crate) fn clear(&self) {
        let mut inner = self.inner.lock().expect("asset registry mutex poisoned");
        inner.entries.clear();
        inner.total_bytes = 0;
    }

    pub(crate) fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("asset registry mutex poisoned")
            .entries
            .len()
    }

    fn get(&self, src: &str, base_url: Option<&Url>) -> Option<RegisteredAsset> {
        {
            let inner = self.inner.lock().expect("asset registry mutex poisoned");
            if let Some(bytes) = inner.entries.get(src) {
                return Some(RegisteredAsset {
                    request_url: src.to_string(),
                    resolved_url: None,
                    bytes: Arc::clone(bytes),
                });
            }
        }

        let resolved = resolve_asset_url(src, base_url)?;
        let inner = self.inner.lock().expect("asset registry mutex poisoned");
        let bytes = Arc::clone(inner.entries.get(resolved.as_str())?);
        Some(RegisteredAsset {
            request_url: src.to_string(),
            resolved_url: Some(resolved.as_str().to_owned()),
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

    fn cache_key(&self) -> &str {
        self.resolved_url.as_deref().unwrap_or(&self.request_url)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WasmResourceProvider {
    assets: AssetRegistry,
    base_url: Option<Url>,
    policy: ResourcePolicy,
    usage: Rc<RefCell<ResourceUsage>>,
    asset_reports: Rc<RefCell<Vec<AssetReport>>>,
    image_cache: Rc<RefCell<HashMap<String, ImageData>>>,
}

#[derive(Debug, Default)]
struct ResourceUsage {
    total_bytes: usize,
    count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct WasmResourceProviderFactory {
    pub(crate) assets: AssetRegistry,
}

impl ResourceProviderFactory for WasmResourceProviderFactory {
    type Provider = WasmResourceProvider;

    fn create(&self, request: &RenderRequest, document_base_url: Option<Url>) -> Self::Provider {
        WasmResourceProvider {
            assets: self.assets.clone(),
            base_url: request.base_url.clone().or(document_base_url),
            policy: request.resource_policy.clone(),
            usage: Rc::new(RefCell::new(ResourceUsage::default())),
            asset_reports: Rc::new(RefCell::new(Vec::new())),
            image_cache: Rc::new(RefCell::new(HashMap::new())),
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
            let cache_key = asset.cache_key().to_owned();
            let source = asset.source();
            if let Some(image) = self.image_cache.borrow().get(cache_key.as_str()).cloned() {
                self.record_asset_report(
                    AssetReport::new(AssetKind::Image, AssetStatus::Loaded, asset.request_url)
                        .with_optional_resolved_url(asset.resolved_url)
                        .with_source(source)
                        .with_initiator(initiator),
                );
                return Ok(image);
            }

            ensure_resource_size_with_limit(asset.bytes.len(), self.policy.max_resource_bytes)?;
            self.record_resource_usage(asset.bytes.len())?;
            let image = decode_registered_image(
                &asset.bytes,
                self.policy.max_resource_bytes,
                self.policy.max_decoded_pixels,
            )?;
            self.image_cache
                .borrow_mut()
                .insert(cache_key, image.clone());
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
        std::mem::take(&mut *self.asset_reports.borrow_mut())
    }

    fn record_asset_report(&self, report: AssetReport) {
        let mut reports = self.asset_reports.borrow_mut();
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
        let mut usage = self.usage.borrow_mut();
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
        return Ok(parsed.as_str().to_owned());
    }
    Ok(url.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};

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
    fn provider_reuses_decoded_registered_images_within_render() {
        let registry = AssetRegistry::default();
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, ColorType::Rgba8.into())
            .expect("encode png");
        registry
            .register("https://cdn.example.com/logo.png", &png)
            .expect("register image");

        let provider = WasmResourceProvider {
            assets: registry,
            base_url: None,
            policy: ResourcePolicy::default(),
            usage: Rc::new(RefCell::new(ResourceUsage::default())),
            asset_reports: Rc::new(RefCell::new(Vec::new())),
            image_cache: Rc::new(RefCell::new(HashMap::new())),
        };

        let first = provider
            .load_image("https://cdn.example.com/logo.png", "img")
            .expect("first image");
        let second = provider
            .load_image("https://cdn.example.com/logo.png", "img")
            .expect("second image");

        assert!(Arc::ptr_eq(&first.rgba, &second.rgba));
        assert_eq!(provider.usage.borrow().count, 1);
    }
}
