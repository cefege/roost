//! Coordinator-owned web access used by harness tools.
//! Fetches validate every resolved address and every redirect hop.
//! Search uses the Exa account pool and its configured endpoint.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use roost_llm::{Endpoints, LlmError};

use crate::{FetchedPage, SearchHit};

const MAX_FETCH_BYTES: usize = 10 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 10;
const EXA_DEFAULT: &str = "https://api.exa.ai";

pub(crate) async fn fetch(url: &str) -> Result<FetchedPage, LlmError> {
    tokio::time::timeout(FETCH_TIMEOUT, fetch_inner(url))
        .await
        .map_err(|_| LlmError::Network("web fetch timed out after 30 seconds".into()))?
}

async fn fetch_inner(url: &str) -> Result<FetchedPage, LlmError> {
    let mut current = parse_http_url(url)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        let addresses = validate_destination(&current).await?;
        let host = current
            .host_str()
            .ok_or_else(|| LlmError::Decode("URL has no host".into()))?;
        let pinned_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()?;
        let response = pinned_client
            .get(current.clone())
            .timeout(FETCH_TIMEOUT)
            .send()
            .await?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(LlmError::Decode("too many redirects".into()));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| LlmError::Decode("redirect has no valid Location".into()))?;
            current = current
                .join(location)
                .map_err(|error| LlmError::Decode(error.to_string()))?;
            current = parse_http_url(current.as_str())?;
            continue;
        }
        if !response.status().is_success() {
            return Err(LlmError::Http {
                status: response.status().as_u16(),
                body: "web fetch failed".into(),
            });
        }
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = read_bounded(response).await?;
        let text = if content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            html2text::from_read(bytes.as_slice(), 100)
                .map_err(|error| LlmError::Decode(error.to_string()))?
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        return Ok(FetchedPage {
            final_url,
            content_type,
            text,
        });
    }
    Err(LlmError::Decode("too many redirects".into()))
}

async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, LlmError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_FETCH_BYTES {
            return Err(LlmError::Decode("web page exceeds 10 MiB limit".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse_http_url(value: &str) -> Result<reqwest::Url, LlmError> {
    let parsed = reqwest::Url::parse(value).map_err(|error| LlmError::Decode(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(LlmError::Decode(
            "only http and https URLs are allowed".into(),
        ));
    }
    Ok(parsed)
}

async fn validate_destination(url: &reqwest::Url) -> Result<Vec<SocketAddr>, LlmError> {
    let port = url
        .port_or_known_default()
        .ok_or_else(|| LlmError::Decode("URL has no port".into()))?;
    let host = url
        .host_str()
        .ok_or_else(|| LlmError::Decode("URL has no host".into()))?;
    let addresses: Vec<SocketAddr> = if let Ok(address) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(address, port)]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| LlmError::Network(error.to_string()))?
            .collect()
    };
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !is_public_address(address.ip()))
    {
        return Err(LlmError::Decode(
            "web fetch destination resolves to a prohibited address".into(),
        ));
    }
    Ok(addresses)
}

pub(crate) fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || octets[0] == 0
                || octets[0] >= 224
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 169 && octets[1] == 254))
        }
        IpAddr::V6(ip) => {
            let octets = ip.octets();
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (octets[0] & 0xfe) == 0xfc
                || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80)
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| !is_public_address(IpAddr::V4(mapped))))
        }
    }
}

pub(crate) async fn search(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    api_key: &str,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>, LlmError> {
    let url = format!("{}/search", endpoints.base("exa", EXA_DEFAULT));
    let response = http
        .post(url)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({
            "query": query,
            "numResults": limit.clamp(1, 20),
            "contents": { "text": { "maxCharacters": 500 } }
        }))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(LlmError::Http {
            status: response.status().as_u16(),
            body: "Exa search failed".into(),
        });
    }
    let payload: serde_json::Value = response.json().await?;
    let results = payload
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| LlmError::Decode("Exa response has no results".into()))?;
    Ok(results
        .iter()
        .take(limit.clamp(1, 20) as usize)
        .filter_map(|result| {
            Some(SearchHit {
                title: result.get("title")?.as_str()?.to_owned(),
                url: result.get("url")?.as_str()?.to_owned(),
                snippet: result
                    .get("text")
                    .or_else(|| result.get("highlights").and_then(|value| value.get(0)))?
                    .as_str()?
                    .to_owned(),
                published: result
                    .get("publishedDate")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::is_public_address;
    use std::net::IpAddr;

    #[test]
    fn blocks_private_loopback_link_local_and_unique_local_addresses() {
        for address in ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fd00::1"] {
            let parsed = address.parse::<IpAddr>();
            assert!(parsed.is_ok(), "{address} is a valid test IP");
            if let Ok(parsed) = parsed {
                assert!(!is_public_address(parsed), "{parsed} must be blocked");
            }
        }
    }

    #[test]
    fn permits_public_addresses() {
        for address in ["8.8.8.8", "2606:4700:4700::1111"] {
            let parsed = address.parse::<IpAddr>();
            assert!(parsed.is_ok(), "{address} is a valid test IP");
            if let Ok(parsed) = parsed {
                assert!(is_public_address(parsed), "{parsed} must be allowed");
            }
        }
    }
}
