use std::fs;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use data_url::DataUrl;
use mail_canvas_core::{AssetKind, AssetSource, AssetStatus};
use url::Url;

use crate::remote::load_remote_url;
use crate::resource::{ResourcePolicy, asset_report, asset_source_for_url, resource_error_status};

pub(crate) struct LoadedResourceBytes {
    pub(crate) resolved_url: Option<String>,
    pub(crate) source: AssetSource,
    pub(crate) bytes: Arc<[u8]>,
}

impl LoadedResourceBytes {
    pub(crate) fn cacheable(&self) -> bool {
        matches!(self.source, AssetSource::File | AssetSource::Remote)
    }
}

pub(crate) struct ResolvedResourceUrl {
    pub(crate) url: Url,
    pub(crate) resolved_url: String,
    pub(crate) source: AssetSource,
}

impl ResolvedResourceUrl {
    pub(crate) fn cacheable(&self) -> bool {
        matches!(self.url.scheme(), "file" | "https" | "http")
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

pub(crate) fn load_resource_bytes_inner(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
    record_loaded: bool,
) -> Result<LoadedResourceBytes> {
    if src.trim_start().starts_with("data:") {
        return load_data_url(src, policy, kind, initiator, record_loaded);
    }

    let resolved = resolve_resource_url(src, policy, kind, initiator)?;
    load_resolved_resource_bytes_inner(src, policy, kind, initiator, record_loaded, resolved)
}

pub(crate) fn resolve_resource_url(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
) -> Result<ResolvedResourceUrl> {
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
    let resolved_url = url.as_str().to_owned();
    let source = asset_source_for_url(&url);
    Ok(ResolvedResourceUrl {
        url,
        resolved_url,
        source,
    })
}

pub(crate) fn load_resolved_resource_bytes_inner(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
    record_loaded: bool,
    resolved: ResolvedResourceUrl,
) -> Result<LoadedResourceBytes> {
    if kind != AssetKind::Image {
        if let Some(bytes) = policy
            .byte_cache
            .lock()
            .expect("byte cache mutex poisoned")
            .get(resolved.resolved_url.as_str())
            .cloned()
        {
            if record_loaded {
                policy.push_asset_report(
                    asset_report(kind, AssetStatus::Loaded, src)
                        .with_source(resolved.source)
                        .with_initiator(initiator)
                        .with_bytes(bytes.len())
                        .with_optional_resolved_url(Some(resolved.resolved_url.clone())),
                );
            }
            return Ok(LoadedResourceBytes {
                resolved_url: Some(resolved.resolved_url),
                source: resolved.source,
                bytes,
            });
        }
    }

    match resolved.url.scheme() {
        "file" => match load_file_url(&resolved.url, policy) {
            Ok(bytes) => Ok(loaded_url_bytes(LoadedUrlBytes {
                src,
                kind,
                initiator,
                policy,
                url: &resolved.url,
                resolved_url: Some(resolved.resolved_url),
                source: AssetSource::File,
                bytes,
                record_loaded,
            })),
            Err(error) => {
                policy.push_asset_report(
                    asset_report(kind, resource_error_status(&error), src)
                        .with_source(AssetSource::File)
                        .with_initiator(initiator)
                        .with_detail(error.to_string())
                        .with_optional_resolved_url(Some(resolved.resolved_url)),
                );
                Err(error)
            }
        },
        "https" | "http" => match load_remote_url(&resolved.url, policy) {
            Ok(bytes) => Ok(loaded_url_bytes(LoadedUrlBytes {
                src,
                kind,
                initiator,
                policy,
                url: &resolved.url,
                resolved_url: Some(resolved.resolved_url),
                source: AssetSource::Remote,
                bytes,
                record_loaded,
            })),
            Err(error) => {
                policy.push_asset_report(
                    asset_report(kind, resource_error_status(&error), src)
                        .with_source(AssetSource::Remote)
                        .with_initiator(initiator)
                        .with_detail(error.to_string())
                        .with_optional_resolved_url(Some(resolved.resolved_url)),
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
                    .with_optional_resolved_url(Some(resolved.resolved_url)),
            );
            Err(error)
        }
    }
}

fn load_data_url(
    src: &str,
    policy: &ResourcePolicy,
    kind: AssetKind,
    initiator: &'static str,
    record_loaded: bool,
) -> Result<LoadedResourceBytes> {
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
    Ok(loaded)
}

struct LoadedUrlBytes<'a> {
    src: &'a str,
    kind: AssetKind,
    initiator: &'static str,
    policy: &'a ResourcePolicy,
    url: &'a Url,
    resolved_url: Option<String>,
    source: AssetSource,
    bytes: Vec<u8>,
    record_loaded: bool,
}

fn loaded_url_bytes(params: LoadedUrlBytes<'_>) -> LoadedResourceBytes {
    let bytes = Arc::<[u8]>::from(params.bytes);
    cache_resource_bytes(params.url, params.kind, params.policy, &bytes);
    if params.record_loaded {
        params.policy.push_asset_report(
            asset_report(params.kind, AssetStatus::Loaded, params.src)
                .with_source(params.source)
                .with_initiator(params.initiator)
                .with_bytes(bytes.len())
                .with_optional_resolved_url(params.resolved_url.clone()),
        );
    }
    LoadedResourceBytes {
        resolved_url: params.resolved_url,
        source: params.source,
        bytes,
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
        .insert(url.as_str().to_owned(), Arc::clone(bytes));
}

pub(crate) fn load_file_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
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
