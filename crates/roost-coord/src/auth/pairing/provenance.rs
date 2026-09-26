//! Server-observed metadata about a pairing request, captured once at the
//! request boundary and never taken from the request body.
//!
//! Owned by the pairing slice. Ported from
//! `apps/coord/src/auth/pair-request-provenance.ts`, which exists because an
//! unowned pair request is a standing credential: the operator who approves it
//! is shown a device label, a browser, an operating system, a city and a
//! country, and every one of those has to be something the COORDINATOR observed
//! rather than something the requester asserted. There is no `PairCreate` field
//! a browser can fill in to influence what the approver reads.
//!
//! WHAT IS CAPTURED, AND WHAT IS NOT. A user agent, three client hints, and
//! the edge's geo headers -- bounded, control-stripped, and truncated on a
//! UTF-8 boundary. Not the public key, not the requester token, not the
//! verification code: those are secrets or authority, and this struct is the
//! one thing a `pairBus` frame carries, so a secret that reached it would be a
//! secret on every connected browser.
//!
//! THE GEO HEADERS ARE ONLY READ UNDER A TRUSTED PROXY. `cf-ipcountry` and its
//! siblings are client-settable, so honouring them on a direct listener would
//! let anybody claim to be in any country. The trust profile is chosen at boot
//! from config and never sniffed from the request
//! (`apps/coord/src/middleware/caller-origin.ts:1-6`).

use axum::http::HeaderMap;

use super::agent_table::first_case_insensitive;
pub use super::agent_table::{UserAgentDescription, describe_user_agent};

use crate::coord_core::ListenerTrust;
use crate::middleware::caller_origin::CallerOrigin;
use crate::events::persistence_input::truncate_persisted_utf8;

/// The bound on any single user-agent or client-hint value written to a row.
pub const MAX_PROVENANCE_UTF8_BYTES: usize = 512;

/// The bound on each edge geo value. Short, because a country code is two
/// characters and a region is a name: anything longer is not geography.
pub const MAX_GEO_UTF8_BYTES: usize = 64;

/// The device class an approver reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientDeviceType {
    /// A browser reporting no mobile signal, or a known non-mobile user agent.
    Desktop,
    /// A phone, by client hint or user agent.
    Mobile,
    /// A tablet, which is checked before mobile because an iPad reports
    /// `sec-ch-ua-mobile: ?1`.
    Tablet,
}

impl ClientDeviceType {
    /// The value `pair_requests.client_device_type` stores.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
            Self::Tablet => "tablet",
        }
    }
}

/// Everything about a pairing request the approver is shown, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PairRequestProvenance {
    /// The requester's user agent, as reported.
    pub user_agent: Option<String>,
    /// Browser name parsed from it.
    pub client_browser: Option<String>,
    /// Operating system parsed from it.
    pub client_os: Option<String>,
    /// Device class parsed from it.
    pub client_device_type: Option<ClientDeviceType>,
    /// The address the request arrived from.
    pub source_ip: String,
    /// Edge geo country, when the edge supplied one.
    pub country_code: Option<String>,
    /// Edge geo region, when the edge supplied one.
    pub region: Option<String>,
    /// Edge geo city, when the edge supplied one.
    pub city: Option<String>,
}

impl PairRequestProvenance {
    /// The source address, which is the one field that is never absent.
    #[must_use]
    pub fn source_ip(&self) -> &str {
        &self.source_ip
    }
}

/// Capture the server-observed metadata of one pairing request.
#[must_use]
pub fn capture_pair_request_provenance(
    headers: &HeaderMap,
    origin: &CallerOrigin,
) -> PairRequestProvenance {
    let user_agent = read_bounded(headers, "user-agent", MAX_PROVENANCE_UTF8_BYTES);
    let agent = user_agent.as_deref().unwrap_or("");
    let fallback = describe_user_agent(agent);

    PairRequestProvenance {
        client_browser: read_browser_hint(headers).or_else(|| fallback.browser.map(String::from)),
        client_os: read_platform_hint(headers).or_else(|| fallback.os.map(String::from)),
        client_device_type: read_mobile_hint(headers).or(fallback.device_type),
        user_agent,
        source_ip: normalize(&origin.client_ip, MAX_PROVENANCE_UTF8_BYTES),
        country_code: read_geo_country(headers, origin.listener),
        region: read_geo(headers, "cf-region", origin.listener),
        city: read_geo(headers, "cf-ipcity", origin.listener),
    }
}

/// The browser named by `sec-ch-ua`, or `None` when it names no real one.
///
/// `sec-ch-ua` is GREASE-bearing, so entries beginning `Not` are planted
/// decoys and are dropped before the table is consulted
/// (`pair-request-provenance.ts:126`). An entry the table does not know is
/// still reported, truncated: "a browser we have never heard of" tells an
/// operator more than a blank field.
fn read_browser_hint(headers: &HeaderMap) -> Option<String> {
    let header = non_empty(headers, "sec-ch-ua")?;
    let quoted = quoted_brands(&header);
    let candidates: Vec<String> = if quoted.is_empty() {
        header
            .split(',')
            .map(|brand| brand.split(';').next().unwrap_or("").trim().to_string())
            .collect()
    } else {
        quoted
    };
    let usable: Vec<&str> = candidates
        .iter()
        .map(String::as_str)
        .filter(|brand| !brand.is_empty() && !is_grease_brand(brand))
        .collect();
    let first = *usable.first()?;
    Some(
        first_case_insensitive(
            &usable.join(","),
            [
                ("Microsoft Edge", "Edge"),
                ("Edge", "Edge"),
                ("Opera", "Opera"),
                ("Google Chrome", "Chrome"),
                ("Chrome", "Chrome"),
                ("Chromium", "Chrome"),
                ("Firefox", "Firefox"),
                ("Safari", "Safari"),
            ],
        )
        .map_or_else(
            || normalize(first, MAX_PROVENANCE_UTF8_BYTES),
            str::to_string,
        ),
    )
}

/// The platform named by `sec-ch-ua-platform`, unquoted and bounded.
fn read_platform_hint(headers: &HeaderMap) -> Option<String> {
    let raw = non_empty(headers, "sec-ch-ua-platform")?;
    let unquoted = raw.trim().trim_matches('"');
    if unquoted.is_empty() {
        return None;
    }
    Some(
        first_case_insensitive(
            unquoted,
            [
                ("Windows", "Windows"),
                ("macOS", "macOS"),
                ("Mac OS X", "macOS"),
                ("Android", "Android"),
                ("iOS", "iOS"),
                ("iPhone", "iOS"),
                ("iPad", "iOS"),
                ("Linux", "Linux"),
            ],
        )
        .map_or_else(
            || normalize(unquoted, MAX_PROVENANCE_UTF8_BYTES),
            str::to_string,
        ),
    )
}

/// The device class `sec-ch-ua-mobile` claims, and nothing else.
///
/// A hint the browser did not send, or sent as anything other than `?1`/`?0`,
/// is `None` rather than a guess: this header is the only signal that
/// distinguishes a phone from a small window.
fn read_mobile_hint(headers: &HeaderMap) -> Option<ClientDeviceType> {
    match non_empty(headers, "sec-ch-ua-mobile")?.trim() {
        "?1" => Some(ClientDeviceType::Mobile),
        "?0" => Some(ClientDeviceType::Desktop),
        _ => None,
    }
}

/// The edge's country: only under a trusted proxy, and only two letters.
///
/// Uppercased on the way in, because `cf-ipcountry` is documented uppercase and
/// a lowercase value is normalized rather than dropped -- a proxy that
/// lowercases it should not lose the field an operator uses to spot an
/// unexpected country.
fn read_geo_country(headers: &HeaderMap, listener: ListenerTrust) -> Option<String> {
    let value = read_geo(headers, "cf-ipcountry", listener)?.to_uppercase();
    (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase())).then_some(value)
}

/// One edge geo value, and only from a listener that vouches for the address.
fn read_geo(headers: &HeaderMap, name: &str, listener: ListenerTrust) -> Option<String> {
    if listener != ListenerTrust::Forwarded {
        return None;
    }
    read_bounded(headers, name, MAX_GEO_UTF8_BYTES)
}

/// A `sec-ch-ua` brand is a decoy when it starts with `Not` plus a separator.
fn is_grease_brand(brand: &str) -> bool {
    let rest = brand
        .strip_prefix("Not")
        .or_else(|| brand.strip_prefix("not"))
        .unwrap_or("");
    matches!(rest.as_bytes().first(), Some(b' ' | b'_' | b';' | b'='))
}

/// The quoted brand names inside a `sec-ch-ua` value.
fn quoted_brands(header: &str) -> Vec<String> {
    let mut brands = Vec::new();
    let mut rest = header;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else {
            break;
        };
        brands.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    brands
}

/// A header's value, when it is present and not empty.
fn non_empty(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    (!value.is_empty()).then(|| value.to_string())
}

/// A header's value, control-stripped and truncated to its byte bound.
fn read_bounded(headers: &HeaderMap, name: &str, max_bytes: usize) -> Option<String> {
    non_empty(headers, name).map(|value| normalize(&value, max_bytes))
}

/// Strip control characters, then truncate on a UTF-8 boundary.
///
/// C0, DEL and C1 are removed rather than replaced: these values are rendered
/// in an operator's browser and stored in an audit-visible column, and a
/// stripped value cannot carry a newline into a log line or an escape into a
/// table cell.
fn normalize(value: &str, max_bytes: usize) -> String {
    let stripped: String = value
        .chars()
        .filter(|character| {
            let code = u32::from(*character);
            !(code <= 0x1f || (0x7f..=0x9f).contains(&code))
        })
        .collect();
    truncate_persisted_utf8(&stripped, max_bytes).to_string()
}

