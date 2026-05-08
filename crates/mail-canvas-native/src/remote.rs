use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use url::Url;

use crate::resource::ResourcePolicy;

const BLINK_RESOURCE_USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) ",
    "AppleWebKit/537.36 (KHTML, like Gecko) ",
    "HeadlessChrome/147.0.7727.15 Safari/537.36"
);

pub(crate) fn load_remote_url(url: &Url, policy: &ResourcePolicy) -> Result<Vec<u8>> {
    if !policy.policy.allow_remote {
        bail!("remote resources are disabled");
    }
    let mut current = url.clone();
    let mut last_error = None;
    for redirect_count in 0..=3 {
        validate_remote_url(&current, policy)?;
        match load_remote_url_once(&current, policy) {
            Ok(RemoteFetch::Bytes(bytes)) => return Ok(bytes),
            Ok(RemoteFetch::Redirect(next)) => {
                if redirect_count == 3 {
                    bail!("too many redirects while fetching {url}");
                }
                current = next;
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("failed to fetch {url}")))
}

enum RemoteFetch {
    Bytes(Vec<u8>),
    Redirect(Url),
}

fn load_remote_url_once(url: &Url, policy: &ResourcePolicy) -> Result<RemoteFetch> {
    let mut response = policy
        .agent
        .get(url.as_str())
        .header("User-Agent", BLINK_RESOURCE_USER_AGENT)
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;
    if response.status().is_redirection() {
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| anyhow!("redirect response missing Location header"))?;
        let next = url
            .join(location)
            .with_context(|| format!("invalid redirect Location from {url}: {location}"))?;
        return Ok(RemoteFetch::Redirect(next));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(policy.policy.max_resource_bytes as u64)
        .read_to_vec()
        .with_context(|| format!("failed to read response body from {url}"))?;
    super::resource::ensure_resource_size(bytes.len(), policy.policy.max_resource_bytes)?;
    policy.record_resource_usage(bytes.len())?;
    Ok(RemoteFetch::Bytes(bytes))
}

pub(crate) fn resource_agent(policy: &mail_canvas_core::ResourcePolicy) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(policy.https_only)
        .max_redirects(0)
        .timeout_global(Some(effective_timeout(policy)))
        .build()
        .new_agent()
}

fn effective_timeout(policy: &mail_canvas_core::ResourcePolicy) -> Duration {
    if policy.timeout.is_zero() {
        Duration::from_secs(8)
    } else {
        policy.timeout
    }
}

pub(crate) fn validate_remote_url(url: &Url, policy: &ResourcePolicy) -> Result<()> {
    if policy.policy.https_only && url.scheme() != "https" {
        bail!("non-HTTPS remote resource rejected");
    }
    match url.scheme() {
        "https" | "http" => {}
        scheme => bail!("unsupported remote resource URL scheme: {scheme}"),
    }
    if policy.policy.deny_private_networks {
        reject_private_host(url)?;
    }
    Ok(())
}

pub(crate) fn reject_private_host(url: &Url) -> Result<()> {
    let Some(host) = url.host_str() else {
        bail!("remote resource missing host");
    };
    if host.eq_ignore_ascii_case("localhost") {
        bail!("localhost resource rejected");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_or_local_ip(ip) {
            bail!("private or local remote resource rejected");
        }
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve remote resource host: {host}"))?;
    for address in addresses {
        if is_private_or_local_ip(address.ip()) {
            bail!("private or local remote resource rejected");
        }
    }
    Ok(())
}

fn is_private_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.octets()[0] == 169 && ip.octets()[1] == 254
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}
