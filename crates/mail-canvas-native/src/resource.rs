use std::fs;
use std::io::Cursor;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use data_url::DataUrl;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use mail_canvas_core::{
    AssetKind, AssetReport, AssetSource, AssetStatus, ImageData, RenderRequest, ResourceProvider,
    ResourceProviderFactory,
};
use url::Url;

const MAX_ASSET_REPORTS: usize = 512;

const BLINK_RESOURCE_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "HeadlessChrome/147.0.7727.15 Safari/537.36"
);

#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    pub(crate) base_url: Option<Url>,
    pub(crate) policy: mail_canvas_core::ResourcePolicy,
    pub(crate) total_bytes: Arc<Mutex<usize>>,
    pub(crate) resource_count: Arc<Mutex<usize>>,
    pub(crate) asset_reports: Arc<Mutex<Vec<AssetReport>>>,
}

impl ResourcePolicy {
    pub(crate) fn from_request(request: &RenderRequest, document_base_url: Option<Url>) -> Self {
        Self {
            base_url: request.base_url.clone().or(document_base_url),
            policy: request.resource_policy.clone(),
            total_bytes: Arc::new(Mutex::new(0)),
            resource_count: Arc::new(Mutex::new(0)),
            asset_reports: Arc::new(Mutex::new(Vec::new())),
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

    fn record_resource_usage(&self, bytes: usize) -> Result<()> {
        let mut count = self
            .resource_count
            .lock()
            .expect("resource count mutex poisoned");
        *count = count.saturating_add(1);
        if *count > self.policy.max_resource_count {
            bail!(
                "resource count exceeds max-resource-count: {} > {}",
                *count,
                self.policy.max_resource_count
            );
        }

        let mut total = self
            .total_bytes
            .lock()
            .expect("resource bytes mutex poisoned");
        *total = total.saturating_add(bytes);
        if *total > self.policy.max_total_resource_bytes {
            bail!(
                "resource bytes exceed max-total-resource-bytes: {} > {}",
                *total,
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

    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Vec<u8>> {
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
    bytes: Vec<u8>,
}

pub(crate) fn load_image(
    src: &str,
    policy: &ResourcePolicy,
    initiator: &'static str,
) -> Result<ImageData> {
    let loaded = load_resource_bytes_inner(src, policy, AssetKind::Image, initiator, false)?;
    match decode_image_bytes(&loaded.bytes, policy) {
        Ok(image) => {
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

pub(crate) fn load_resource_bytes(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
) -> Result<Vec<u8>> {
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
            bytes,
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

    match url.scheme() {
        "file" => match load_file_url(&url, policy) {
            Ok(bytes) => {
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
    if let Some(base) = &policy.base_url {
        if base.scheme() == "file" {
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
        }
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes)?;
    policy.record_resource_usage(bytes.len())?;
    Ok(bytes)
}

fn load_remote_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if !policy.policy.allow_remote {
        bail!("remote resources are disabled");
    }
    if policy.policy.https_only && url.scheme() != "https" {
        bail!("non-HTTPS remote resource rejected");
    }
    if policy.policy.deny_private_networks {
        reject_private_host(url)?;
    }

    let mut last_error = None;
    for _ in 0..3 {
        match load_remote_url_once(url, policy) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("failed to fetch {url}")))
}

fn load_remote_url_once(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .https_only(policy.policy.https_only)
        .max_redirects(3)
        .timeout_global(Some(effective_timeout(policy)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url.as_str())
        .header("User-Agent", BLINK_RESOURCE_USER_AGENT)
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(policy.policy.max_resource_bytes as u64)
        .read_to_vec()
        .with_context(|| format!("failed to read response body from {url}"))?;
    ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes)?;
    policy.record_resource_usage(bytes.len())?;
    Ok(bytes)
}

fn effective_timeout(policy: &ResourcePolicy) -> Duration {
    if policy.policy.timeout.is_zero() {
        Duration::from_secs(8)
    } else {
        policy.policy.timeout
    }
}

fn reject_private_host(url: &Url) -> Result<()> {
    let Some(host) = url.host_str() else {
        bail!("remote resource missing host");
    };
    if host.eq_ignore_ascii_case("localhost") {
        bail!("localhost resource rejected");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        let rejected = match ip {
            IpAddr::V4(ip) => {
                ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
            }
            IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_unique_local()
                    || ip.is_unicast_link_local()
            }
        };
        if rejected {
            bail!("private or local remote resource rejected");
        }
    }
    Ok(())
}

fn ensure_resource_size(len: usize, max_len: usize) -> Result<()> {
    if len > max_len {
        bail!("resource is too large: {len} bytes > {max_len} bytes");
    }
    Ok(())
}

fn decode_image_bytes(bytes: &[u8], policy: &ResourcePolicy) -> Result<ImageData> {
    ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes)?;
    let max_side = u32::try_from(policy.policy.max_decoded_pixels.min(u64::from(u32::MAX)))
        .expect("bounded decoded pixel limit");
    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(policy.policy.max_decoded_pixels.saturating_mul(4));
    reader.limits(limits);
    let mut decoder = reader
        .with_guessed_format()
        .context("failed to guess image format")?
        .into_decoder()
        .context("failed to create image decoder")?;
    let orientation = decoder
        .orientation()
        .context("failed to read image orientation")?;
    let mut image = DynamicImage::from_decoder(decoder).context("failed to decode image")?;
    image.apply_orientation(orientation);
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > policy.policy.max_decoded_pixels {
        bail!(
            "decoded image is too large: {pixels} pixels > {} pixels",
            policy.policy.max_decoded_pixels
        );
    }
    Ok(ImageData {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use image::{ColorType, codecs::jpeg::JpegEncoder};

    use super::*;

    fn test_policy() -> ResourcePolicy {
        ResourcePolicy {
            base_url: None,
            policy: mail_canvas_core::ResourcePolicy {
                allow_remote: false,
                https_only: true,
                deny_private_networks: true,
                timeout: Duration::from_secs(1),
                max_resource_bytes: 1024 * 1024,
                max_total_resource_bytes: 2 * 1024 * 1024,
                max_decoded_pixels: 1024,
                max_resource_count: 8,
            },
            total_bytes: Arc::new(Mutex::new(0)),
            resource_count: Arc::new(Mutex::new(0)),
            asset_reports: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[test]
    fn decode_applies_exif_orientation_like_blink() {
        let mut jpeg = Vec::new();
        JpegEncoder::new(&mut jpeg)
            .encode(&[255, 0, 0, 0, 255, 0], 1, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");
        let oriented = jpeg_with_exif_orientation(jpeg, 6);

        let image = decode_image_bytes(&oriented, &test_policy()).expect("decode");

        assert_eq!((image.width, image.height), (2, 1));
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

    fn jpeg_with_exif_orientation(jpeg: Vec<u8>, orientation: u16) -> Vec<u8> {
        assert_eq!(&jpeg[0..2], &[0xff, 0xd8]);
        let mut exif = Vec::new();
        exif.extend_from_slice(b"Exif\0\0");
        exif.extend_from_slice(b"MM");
        exif.extend_from_slice(&42u16.to_be_bytes());
        exif.extend_from_slice(&8u32.to_be_bytes());
        exif.extend_from_slice(&1u16.to_be_bytes());
        exif.extend_from_slice(&0x0112u16.to_be_bytes());
        exif.extend_from_slice(&3u16.to_be_bytes());
        exif.extend_from_slice(&1u32.to_be_bytes());
        exif.extend_from_slice(&orientation.to_be_bytes());
        exif.extend_from_slice(&0u16.to_be_bytes());
        exif.extend_from_slice(&0u32.to_be_bytes());

        let segment_len = u16::try_from(exif.len() + 2).expect("segment length");
        let mut out = Vec::new();
        out.extend_from_slice(&jpeg[0..2]);
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&segment_len.to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }
}
