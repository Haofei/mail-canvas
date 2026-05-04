use std::fs;
use std::io::Cursor;
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use data_url::DataUrl;
use image::{ImageReader, Limits};
use url::Url;

use crate::RenderRequest;

#[derive(Debug, Clone)]
pub(crate) struct ResourcePolicy {
    pub(crate) base_url: Option<Url>,
    pub(crate) allow_remote: bool,
    pub(crate) https_only: bool,
    pub(crate) timeout: Duration,
    pub(crate) max_image_bytes: usize,
    pub(crate) max_decoded_pixels: u64,
}

impl ResourcePolicy {
    pub(crate) fn from_request(request: &RenderRequest, document_base_url: Option<Url>) -> Self {
        Self {
            base_url: request.base_url.clone().or(document_base_url),
            allow_remote: request.allow_remote,
            https_only: request.https_only,
            timeout: if request.timeout.is_zero() {
                Duration::from_secs(8)
            } else {
                request.timeout
            },
            max_image_bytes: request.max_image_bytes.max(1),
            max_decoded_pixels: request.max_decoded_pixels.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ImageData {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) fn load_image(src: &str, policy: &ResourcePolicy) -> Result<ImageData> {
    let bytes = load_resource_bytes(src, policy)?;
    decode_image_bytes(&bytes, policy)
}

pub(crate) fn load_resource_bytes(src: &str, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if src.trim_start().starts_with("data:") {
        let data_url =
            DataUrl::process(src).map_err(|error| anyhow!("invalid data URL: {error}"))?;
        let (bytes, _) = data_url
            .decode_to_vec()
            .map_err(|error| anyhow!("invalid data URL body: {error:?}"))?;
        ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
        return Ok(bytes);
    }

    let url = Url::parse(src)
        .or_else(|_| {
            policy
                .base_url
                .as_ref()
                .ok_or(url::ParseError::RelativeUrlWithoutBase)
                .and_then(|base| base.join(src))
        })
        .with_context(|| format!("failed to resolve resource URL {src}"))?;

    match url.scheme() {
        "file" => load_file_url(&url, policy),
        "https" | "http" => load_remote_url(&url, policy),
        scheme => bail!("unsupported resource URL scheme: {scheme}"),
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
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    Ok(bytes)
}

fn load_remote_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if !policy.allow_remote {
        bail!("remote resources are disabled");
    }
    if policy.https_only && url.scheme() != "https" {
        bail!("non-HTTPS remote resource rejected");
    }
    reject_private_host(url)?;

    let agent = ureq::Agent::config_builder()
        .https_only(policy.https_only)
        .max_redirects(3)
        .timeout_global(Some(policy.timeout))
        .build()
        .new_agent();
    let mut response = agent
        .get(url.as_str())
        .header(
            "User-Agent",
            "Mozilla/5.0 AppleWebKit/537.36 Chrome/120 Safari/537.36",
        )
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(policy.max_image_bytes as u64)
        .read_to_vec()
        .with_context(|| format!("failed to read response body from {url}"))?;
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    Ok(bytes)
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
    ensure_resource_size(bytes.len(), policy.max_image_bytes)?;
    let max_side = policy.max_decoded_pixels.min(u64::from(u32::MAX)) as u32;
    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(policy.max_decoded_pixels.saturating_mul(4));
    reader.limits(limits);
    let image = reader
        .with_guessed_format()
        .context("failed to guess image format")?
        .decode()
        .context("failed to decode image")?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > policy.max_decoded_pixels {
        bail!(
            "decoded image is too large: {pixels} pixels > {} pixels",
            policy.max_decoded_pixels
        );
    }
    Ok(ImageData {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}
