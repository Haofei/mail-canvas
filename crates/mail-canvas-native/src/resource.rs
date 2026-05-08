use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use data_url::DataUrl;
use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ImageData, RenderRequest, ResourceProvider,
    ResourceProviderFactory,
};
use url::Url;

use crate::image::decode_image_bytes;
use crate::remote::{load_remote_url, resource_agent};

const MAX_ASSET_REPORTS: usize = 512;

#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    pub(crate) base_url: Option<Url>,
    pub(crate) policy: mail_canvas_core::ResourcePolicy,
    pub(crate) usage: Arc<Mutex<ResourceUsage>>,
    pub(crate) asset_reports: Arc<Mutex<Vec<AssetReport>>>,
    image_cache: Arc<Mutex<HashMap<String, ImageData>>>,
    byte_cache: Arc<Mutex<HashMap<String, Arc<[u8]>>>>,
    pub(crate) agent: ureq::Agent,
}

#[derive(Debug, Default)]
pub(crate) struct ResourceUsage {
    pub(crate) total_bytes: usize,
    pub(crate) count: usize,
}

impl ResourcePolicy {
    pub(crate) fn from_request(request: &RenderRequest, document_base_url: Option<Url>) -> Self {
        let agent = resource_agent(&request.resource_policy);
        Self {
            base_url: request.base_url.clone().or(document_base_url),
            policy: request.resource_policy.clone(),
            usage: Arc::new(Mutex::new(ResourceUsage::default())),
            asset_reports: Arc::new(Mutex::new(Vec::new())),
            image_cache: Arc::new(Mutex::new(HashMap::new())),
            byte_cache: Arc::new(Mutex::new(HashMap::new())),
            agent,
        }
    }

    pub(crate) fn take_asset_reports(&self) -> Vec<AssetReport> {
        let mut reports = self
            .asset_reports
            .lock()
            .expect("asset report mutex poisoned");
        std::mem::take(&mut *reports)
    }

    pub(crate) fn record_asset_report(&self, report: AssetReport) {
        self.push_asset_report(report);
    }

    fn push_asset_report(&self, report: AssetReport) {
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
        if reports.len() >= MAX_ASSET_REPORTS {
            return;
        }
        reports.push(report);
    }

    pub(crate) fn record_resource_usage(&self, bytes: usize) -> Result<()> {
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

impl ResourceProvider for ResourcePolicy {
    fn load_image(&self, src: &str, initiator: &'static str) -> Result<ImageData> {
        load_image(src, self, initiator)
    }

    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Arc<[u8]>> {
        load_resource_bytes(src, self, kind, initiator)
    }

    fn take_asset_reports(&self) -> Vec<AssetReport> {
        Self::take_asset_reports(self)
    }

    fn record_asset_report(&self, report: AssetReport) {
        Self::record_asset_report(self, report);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NativeResourceProviderFactory;

impl ResourceProviderFactory for NativeResourceProviderFactory {
    type Provider = ResourcePolicy;

    fn create(&self, request: &RenderRequest, document_base_url: Option<Url>) -> Self::Provider {
        ResourcePolicy::from_request(request, document_base_url)
    }
}

struct LoadedResourceBytes {
    resolved_url: Option<String>,
    source: AssetSource,
    bytes: Arc<[u8]>,
}

pub(crate) fn load_image(
    src: &str,
    policy: &ResourcePolicy,
    initiator: &'static str,
) -> Result<ImageData> {
    let cache_key = cacheable_image_key(src, policy);
    if let Some(key) = cache_key.as_deref() {
        if let Some(image) = policy
            .image_cache
            .lock()
            .expect("image cache mutex poisoned")
            .get(key)
            .cloned()
        {
            policy.push_asset_report(
                asset_report(AssetKind::Image, AssetStatus::Loaded, src)
                    .with_source(asset_source_for_cache_key(key))
                    .with_initiator(initiator)
                    .with_optional_resolved_url(Some(key.to_string())),
            );
            return Ok(image);
        }
    }

    let loaded = load_resource_bytes_inner(src, policy, AssetKind::Image, initiator, false)?;
    match decode_image_bytes(&loaded.bytes, &policy.policy) {
        Ok(image) => {
            if let Some(key) = cache_key {
                policy
                    .image_cache
                    .lock()
                    .expect("image cache mutex poisoned")
                    .insert(key, image.clone());
            }
            policy.push_asset_report(
                asset_report(AssetKind::Image, AssetStatus::Loaded, src)
                    .with_source(loaded.source)
                    .with_initiator(initiator)
                    .with_bytes(loaded.bytes.len())
                    .with_optional_resolved_url(loaded.resolved_url),
            );
            Ok(image)
        }
        Err(error) => {
            policy.push_asset_report(
                asset_report(AssetKind::Image, AssetStatus::Failed, src)
                    .with_source(loaded.source)
                    .with_initiator(initiator)
                    .with_bytes(loaded.bytes.len())
                    .with_detail(error.to_string())
                    .with_optional_resolved_url(loaded.resolved_url),
            );
            Err(error)
        }
    }
}

fn cacheable_image_key(src: &str, policy: &ResourcePolicy) -> Option<String> {
    if src.trim_start().starts_with("data:") {
        return None;
    }
    let url = Url::parse(src).or_else(|_| {
        policy
            .base_url
            .as_ref()
            .ok_or(url::ParseError::RelativeUrlWithoutBase)
            .and_then(|base| base.join(src))
    });
    let url = url.ok()?;
    matches!(url.scheme(), "file" | "https" | "http").then(|| url.to_string())
}

fn asset_source_for_cache_key(key: &str) -> AssetSource {
    match Url::parse(key).ok() {
        Some(url) if url.scheme() == "file" => AssetSource::File,
        _ => AssetSource::Remote,
    }
}

pub(crate) fn load_resource_bytes(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
) -> Result<Arc<[u8]>> {
    let loaded = load_resource_bytes_inner(src, policy, kind, initiator, true)?;
    Ok(loaded.bytes)
}

fn load_resource_bytes_inner(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
    record_loaded: bool,
) -> Result<LoadedResourceBytes> {
    if src.trim_start().starts_with("data:") {
        let data_url = match DataUrl::process(src) {
            Ok(data_url) => data_url,
            Err(error) => {
                let error = anyhow!("invalid data URL: {error}");
                policy.push_asset_report(
                    asset_report(kind, AssetStatus::Failed, src)
                        .with_source(AssetSource::DataUrl)
                        .with_initiator(initiator)
                        .with_detail(error.to_string()),
                );
                return Err(error);
            }
        };
        let (bytes, _) = match data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))
        {
            Ok(decoded) => decoded,
            Err(error) => {
                policy.push_asset_report(
                    asset_report(kind, AssetStatus::Failed, src)
                        .with_source(AssetSource::DataUrl)
                        .with_initiator(initiator)
                        .with_detail(error.to_string()),
                );
                return Err(error);
            }
        };
        if let Err(error) = ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes) {
            policy.push_asset_report(
                asset_report(kind, AssetStatus::Failed, src)
                    .with_source(AssetSource::DataUrl)
                    .with_initiator(initiator)
                    .with_bytes(bytes.len())
                    .with_detail(error.to_string()),
            );
            return Err(error);
        }
        policy.record_resource_usage(bytes.len())?;
        let loaded = LoadedResourceBytes {
            resolved_url: None,
            source: AssetSource::DataUrl,
            bytes: Arc::from(bytes),
        };
        if record_loaded {
            policy.push_asset_report(
                asset_report(kind, AssetStatus::Loaded, src)
                    .with_source(AssetSource::DataUrl)
                    .with_initiator(initiator)
                    .with_bytes(loaded.bytes.len()),
            );
        }
        return Ok(loaded);
    }

    let url = Url::parse(src)
        .or_else(|_| {
            policy
                .base_url
                .as_ref()
                .ok_or(url::ParseError::RelativeUrlWithoutBase)
                .and_then(|base| base.join(src))
        })
        .with_context(|| format!("failed to resolve resource URL {src}"));
    let url = match url {
        Ok(url) => url,
        Err(error) => {
            policy.push_asset_report(
                asset_report(kind, AssetStatus::Failed, src)
                    .with_initiator(initiator)
                    .with_detail(error.to_string()),
            );
            return Err(error);
        }
    };
    let resolved_url = Some(url.to_string());
    let source = asset_source_for_url(&url);

    if kind != AssetKind::Image {
        if let Some(bytes) = policy
            .byte_cache
            .lock()
            .expect("byte cache mutex poisoned")
            .get(url.as_str())
            .cloned()
        {
            if record_loaded {
                policy.push_asset_report(
                    asset_report(kind, AssetStatus::Loaded, src)
                        .with_source(source)
                        .with_initiator(initiator)
                        .with_bytes(bytes.len())
                        .with_optional_resolved_url(resolved_url.clone()),
                );
            }
            return Ok(LoadedResourceBytes {
                resolved_url,
                source,
                bytes,
            });
        }
    }

    match url.scheme() {
        "file" => match load_file_url(&url, policy) {
            Ok(bytes) => {
                let bytes = Arc::<[u8]>::from(bytes);
                cache_resource_bytes(&url, kind, policy, &bytes);
                if record_loaded {
                    policy.push_asset_report(
                        asset_report(kind, AssetStatus::Loaded, src)
                            .with_source(AssetSource::File)
                            .with_initiator(initiator)
                            .with_bytes(bytes.len())
                            .with_optional_resolved_url(resolved_url.clone()),
                    );
                }
                Ok(LoadedResourceBytes {
                    resolved_url,
                    source: AssetSource::File,
                    bytes,
                })
            }
            Err(error) => {
                policy.push_asset_report(
                    asset_report(kind, resource_error_status(&error), src)
                        .with_source(AssetSource::File)
                        .with_initiator(initiator)
                        .with_detail(error.to_string())
                        .with_optional_resolved_url(resolved_url),
                );
                Err(error)
            }
        },
        "https" | "http" => match load_remote_url(&url, policy) {
            Ok(bytes) => {
                let bytes = Arc::<[u8]>::from(bytes);
                cache_resource_bytes(&url, kind, policy, &bytes);
                if record_loaded {
                    policy.push_asset_report(
                        asset_report(kind, AssetStatus::Loaded, src)
                            .with_source(AssetSource::Remote)
                            .with_initiator(initiator)
                            .with_bytes(bytes.len())
                            .with_optional_resolved_url(resolved_url.clone()),
                    );
                }
                Ok(LoadedResourceBytes {
                    resolved_url,
                    source: AssetSource::Remote,
                    bytes,
                })
            }
            Err(error) => {
                policy.push_asset_report(
                    asset_report(kind, resource_error_status(&error), src)
                        .with_source(AssetSource::Remote)
                        .with_initiator(initiator)
                        .with_detail(error.to_string())
                        .with_optional_resolved_url(resolved_url),
                );
                Err(error)
            }
        },
        scheme => {
            let error = anyhow!("unsupported resource URL scheme: {scheme}");
            policy.push_asset_report(
                asset_report(kind, AssetStatus::Failed, src)
                    .with_initiator(initiator)
                    .with_detail(error.to_string())
                    .with_optional_resolved_url(resolved_url),
            );
            Err(error)
        }
    }
}

fn cache_resource_bytes(url: &Url, kind: AssetKind, policy: &ResourcePolicy, bytes: &Arc<[u8]>) {
    if kind == AssetKind::Image {
        return;
    }
    policy
        .byte_cache
        .lock()
        .expect("byte cache mutex poisoned")
        .insert(url.to_string(), Arc::clone(bytes));
}

fn asset_source_for_url(url: &Url) -> AssetSource {
    if url.scheme() == "file" {
        AssetSource::File
    } else {
        AssetSource::Remote
    }
}

fn asset_report(kind: AssetKind, status: AssetStatus, request_url: &str) -> AssetReport {
    AssetReport::new(kind, status, request_url.to_string())
}

fn resource_error_status(error: &anyhow::Error) -> AssetStatus {
    let message = error.to_string();
    if message.contains("disabled")
        || message.contains("rejected")
        || message.contains("outside the base directory")
    {
        AssetStatus::Blocked
    } else {
        AssetStatus::Failed
    }
}

fn load_file_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    let path = url
        .to_file_path()
        .map_err(|()| anyhow!("invalid file URL: {url}"))?;
    let Some(base) = &policy.base_url else {
        bail!("file resources require a file base URL");
    };
    if base.scheme() != "file" {
        bail!("file resources require a file base URL");
    }
    if let Ok(root) = base.to_file_path() {
        let root = root.canonicalize().unwrap_or(root);
        let target = path.canonicalize().unwrap_or(path.clone());
        if !target.starts_with(&root) {
            bail!(
                "file resource is outside the base directory: {}",
                target.display()
            );
        }
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes)?;
    policy.record_resource_usage(bytes.len())?;
    Ok(bytes)
}

pub(crate) fn ensure_resource_size(len: usize, max_len: usize) -> Result<()> {
    if len > max_len {
        bail!("resource is too large: {len} bytes > {max_len} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use image::{ColorType, ImageEncoder, codecs::png::PngEncoder};

    use super::*;

    fn test_policy() -> ResourcePolicy {
        let policy = mail_canvas_core::ResourcePolicy {
            allow_remote: false,
            https_only: true,
            deny_private_networks: true,
            timeout: Duration::from_secs(1),
            max_resource_bytes: 1024 * 1024,
            max_total_resource_bytes: 2 * 1024 * 1024,
            max_decoded_pixels: 1024,
            max_resource_count: 8,
        };
        let agent = resource_agent(&policy);
        ResourcePolicy {
            base_url: None,
            policy,
            usage: Arc::new(Mutex::new(ResourceUsage::default())),
            asset_reports: Arc::new(Mutex::new(Vec::new())),
            image_cache: Arc::new(Mutex::new(HashMap::new())),
            byte_cache: Arc::new(Mutex::new(HashMap::new())),
            agent,
        }
    }

    #[test]
    fn load_image_reuses_decoded_file_images_within_render() {
        let dir =
            std::env::temp_dir().join(format!("mail-canvas-image-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let image_path = dir.join("pixel.png");
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, ColorType::Rgba8.into())
            .expect("encode png");
        fs::write(&image_path, png).expect("write png");

        let mut policy = test_policy();
        policy.base_url = Some(Url::from_directory_path(&dir).expect("file base url"));

        let first = load_image("pixel.png", &policy, "img").expect("first load");
        let second = load_image("pixel.png", &policy, "img").expect("second load");

        assert!(Arc::ptr_eq(&first.rgba, &second.rgba));
        assert_eq!(policy.usage.lock().expect("resource usage mutex").count, 1);

        let _ = fs::remove_file(image_path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn load_bytes_reuses_file_stylesheets_within_render() {
        let dir =
            std::env::temp_dir().join(format!("mail-canvas-byte-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let css_path = dir.join("style.css");
        fs::write(&css_path, b".hero { color: red }").expect("write css");

        let mut policy = test_policy();
        policy.base_url = Some(Url::from_directory_path(&dir).expect("file base url"));

        let first = policy
            .load_bytes("style.css", AssetKind::Stylesheet, "link")
            .expect("first load");
        let second = policy
            .load_bytes("style.css", AssetKind::Stylesheet, "link")
            .expect("second load");

        assert_eq!(first, second);
        assert!(Arc::ptr_eq(&first, &second));
        let usage = policy.usage.lock().expect("resource usage mutex");
        assert_eq!(usage.count, 1);
        assert_eq!(usage.total_bytes, first.len());

        let _ = fs::remove_file(css_path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn file_resources_require_file_base_url() {
        let dir =
            std::env::temp_dir().join(format!("mail-canvas-file-policy-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let file_path = dir.join("asset.txt");
        fs::write(&file_path, b"asset").expect("write asset");
        let file_url = Url::from_file_path(&file_path).expect("file url");
        let policy = test_policy();

        let error = load_file_url(&file_url, &policy).expect_err("file base is required");

        assert!(error.to_string().contains("file base URL"));
        let _ = fs::remove_file(file_path);
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn file_resources_stay_under_file_base_url() {
        let root =
            std::env::temp_dir().join(format!("mail-canvas-file-root-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!(
            "mail-canvas-file-outside-{}.txt",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create root");
        fs::write(&outside, b"outside").expect("write outside");
        let mut policy = test_policy();
        policy.base_url = Some(Url::from_directory_path(&root).expect("file base url"));
        let outside_url = Url::from_file_path(&outside).expect("outside file url");

        let error = load_file_url(&outside_url, &policy).expect_err("outside should be rejected");

        assert!(error.to_string().contains("outside the base directory"));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir(root);
    }

    #[test]
    fn resource_policy_enforces_total_resource_bytes() {
        let policy = test_policy();
        policy
            .record_resource_usage(1024 * 1024)
            .expect("first megabyte");
        let error = policy
            .record_resource_usage(1024 * 1024 + 1)
            .expect_err("should exceed aggregate bytes");
        assert!(
            error.to_string().contains("max-total-resource-bytes"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resource_policy_enforces_resource_count() {
        let policy = test_policy();
        for _ in 0..8 {
            policy
                .record_resource_usage(1)
                .expect("within count budget");
        }
        let error = policy
            .record_resource_usage(1)
            .expect_err("should exceed aggregate count");
        assert!(
            error.to_string().contains("max-resource-count"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn private_host_rejection_covers_literal_and_resolved_hosts() {
        let localhost = Url::parse("https://localhost/image.png").unwrap();
        assert!(crate::remote::reject_private_host(&localhost).is_err());

        let loopback = Url::parse("https://127.0.0.1/image.png").unwrap();
        assert!(crate::remote::reject_private_host(&loopback).is_err());
    }

    #[test]
    fn redirect_targets_are_revalidated() {
        let policy = test_policy();
        let target = Url::parse("http://example.com/image.png").unwrap();

        let error = crate::remote::validate_remote_url(&target, &policy)
            .expect_err("http should be rejected");

        assert!(error.to_string().contains("non-HTTPS"));
    }
}
