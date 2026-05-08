use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use std::sync::Arc;
use url::Url;

use crate::{AssetKind, AssetReport};

#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

pub fn repair_png_chunk_crcs(bytes: &[u8]) -> Option<Vec<u8>> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < PNG_SIGNATURE.len() || &bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return None;
    }

    let mut repaired = None;
    let mut offset = PNG_SIGNATURE.len();
    while offset.checked_add(12)? <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type_start = offset + 4;
        let chunk_data_start = offset + 8;
        let chunk_crc_start = chunk_data_start.checked_add(length)?;
        let next = chunk_crc_start.checked_add(4)?;
        if next > bytes.len() {
            return None;
        }

        let expected = crc32fast::hash(&bytes[chunk_type_start..chunk_crc_start]);
        let actual = u32::from_be_bytes(bytes[chunk_crc_start..next].try_into().ok()?);
        if expected != actual {
            let repaired = repaired.get_or_insert_with(|| bytes.to_vec());
            repaired[chunk_crc_start..next].copy_from_slice(&expected.to_be_bytes());
        }

        if &bytes[chunk_type_start..chunk_data_start] == b"IEND" {
            return repaired;
        }
        offset = next;
    }
    None
}

pub trait ResourceProvider: Clone {
    fn load_image(&self, src: &str, initiator: &'static str) -> Result<ImageData>;
    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Arc<[u8]>>;
    fn take_asset_reports(&self) -> Vec<AssetReport>;
    fn record_asset_report(&self, report: AssetReport);
}

pub trait ResourceProviderFactory {
    type Provider: ResourceProvider;

    fn create(
        &self,
        request: &crate::RenderRequest,
        document_base_url: Option<Url>,
    ) -> Self::Provider;
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct TestResourceProvider {
    policy: crate::ResourcePolicy,
    usage: std::sync::Arc<std::sync::Mutex<TestResourceUsage>>,
    asset_reports: std::sync::Arc<std::sync::Mutex<Vec<AssetReport>>>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestResourceUsage {
    total_bytes: usize,
    count: usize,
}

#[cfg(test)]
impl TestResourceProvider {
    pub(crate) fn from_request(request: &crate::RenderRequest) -> Self {
        Self {
            policy: request.resource_policy.clone(),
            usage: std::sync::Arc::new(std::sync::Mutex::new(TestResourceUsage::default())),
            asset_reports: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn push_asset_report(&self, report: AssetReport) {
        let mut reports = self
            .asset_reports
            .lock()
            .expect("asset report mutex poisoned");
        reports.push(report);
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TestResourceProviderFactory;

#[cfg(test)]
impl ResourceProviderFactory for TestResourceProviderFactory {
    type Provider = TestResourceProvider;

    fn create(
        &self,
        request: &crate::RenderRequest,
        _document_base_url: Option<Url>,
    ) -> Self::Provider {
        TestResourceProvider::from_request(request)
    }
}

#[cfg(test)]
impl ResourceProvider for TestResourceProvider {
    fn load_image(&self, src: &str, initiator: &'static str) -> Result<ImageData> {
        use anyhow::{anyhow, bail};
        use data_url::DataUrl;
        use image::{DynamicImage, ImageDecoder, ImageReader, Limits};

        if !src.trim_start().starts_with("data:") {
            if (src.starts_with("http://") || src.starts_with("https://"))
                && (!self.policy.allow_remote
                    || (self.policy.https_only && src.starts_with("http://")))
            {
                self.push_asset_report(
                    crate::AssetReport::new(
                        crate::AssetKind::Image,
                        crate::AssetStatus::Blocked,
                        src.to_string(),
                    )
                    .with_source(crate::AssetSource::Remote)
                    .with_initiator(initiator),
                );
                bail!("remote resources are disabled");
            }
            bail!("image not available in core test provider");
        }

        let data_url =
            DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
        let (bytes, _) = data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
        if bytes.len() > self.policy.max_resource_bytes {
            bail!("image resource exceeds max-image-bytes");
        }
        self.record_resource_usage(bytes.len())?;
        let mut reader = ImageReader::new(std::io::Cursor::new(&bytes));
        let mut limits = Limits::default();
        limits.max_image_width =
            Some(self.policy.max_decoded_pixels.min(u64::from(u32::MAX)) as u32);
        limits.max_image_height =
            Some(self.policy.max_decoded_pixels.min(u64::from(u32::MAX)) as u32);
        limits.max_alloc = Some(self.policy.max_decoded_pixels.saturating_mul(4));
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
        if u64::from(width) * u64::from(height) > self.policy.max_decoded_pixels {
            bail!("decoded image exceeds max-decoded-pixels");
        }
        self.push_asset_report(
            crate::AssetReport::new(
                crate::AssetKind::Image,
                crate::AssetStatus::Loaded,
                src.to_string(),
            )
            .with_source(crate::AssetSource::DataUrl)
            .with_initiator(initiator)
            .with_bytes(bytes.len()),
        );
        Ok(ImageData {
            width,
            height,
            rgba: rgba.into_raw().into(),
        })
    }

    fn load_bytes(&self, src: &str, kind: AssetKind, initiator: &'static str) -> Result<Arc<[u8]>> {
        use anyhow::{anyhow, bail};
        use data_url::DataUrl;

        if src.trim_start().starts_with("data:") {
            let data_url =
                DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
            let (bytes, _) = data_url
                .decode_to_vec()
                .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
            self.record_resource_usage(bytes.len())?;
            self.push_asset_report(
                crate::AssetReport::new(kind, crate::AssetStatus::Loaded, src.to_string())
                    .with_source(crate::AssetSource::DataUrl)
                    .with_initiator(initiator)
                    .with_bytes(bytes.len()),
            );
            return Ok(Arc::from(bytes));
        }

        if (src.starts_with("http://") || src.starts_with("https://"))
            && (!self.policy.allow_remote || (self.policy.https_only && src.starts_with("http://")))
        {
            self.push_asset_report(
                crate::AssetReport::new(kind, crate::AssetStatus::Blocked, src.to_string())
                    .with_source(crate::AssetSource::Remote)
                    .with_initiator(initiator),
            );
            bail!("remote resources are disabled");
        }

        bail!("resource not available in core test provider")
    }

    fn take_asset_reports(&self) -> Vec<AssetReport> {
        let mut reports = self
            .asset_reports
            .lock()
            .expect("asset report mutex poisoned");
        std::mem::take(&mut *reports)
    }

    fn record_asset_report(&self, report: AssetReport) {
        self.push_asset_report(report);
    }
}

#[cfg(test)]
impl TestResourceProvider {
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
