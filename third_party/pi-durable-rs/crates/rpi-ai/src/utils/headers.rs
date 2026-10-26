//! Port of `packages/ai/src/utils/headers.ts` @ pi 0.82.1 (2efa728), plus
//! `mergeHeaders` from `packages/ai/src/models.ts` (kept here per design §3.6:
//! case-insensitive merge with `null`-value deletion).

use std::collections::HashMap;

use crate::types::ProviderHeaders;

/// `headersToRecord`: flattens a reqwest header map into a plain record.
/// Header names are lowercased (HTTP header names are case-insensitive and
/// reqwest normalizes to lowercase), matching the JS `Headers.entries()`
/// behavior.
pub fn headers_to_record(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// `providerHeadersToRecord`: drops `None` (suppress) entries; returns `None`
/// when nothing remains.
pub fn provider_headers_to_record(
    headers: Option<&ProviderHeaders>,
) -> Option<HashMap<String, String>> {
    let headers = headers?;
    let result: HashMap<String, String> = headers
        .iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone())))
        .collect();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Converts provider headers to a reqwest header map, dropping `None`
/// suppression markers (the HTTP boundary).
pub fn provider_headers_to_header_map(
    headers: &ProviderHeaders,
) -> Result<reqwest::header::HeaderMap, String> {
    let mut map = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let Some(value) = value else { continue };
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("Invalid header name {name:?}: {error}"))?;
        let header_value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|error| format!("Invalid value for header {name:?}: {error}"))?;
        map.append(header_name, header_value);
    }
    Ok(map)
}

/// `Model.headers` (plain string map) as [`ProviderHeaders`].
pub fn model_headers(model: &crate::types::Model) -> Option<ProviderHeaders> {
    model.headers.as_ref().map(|headers| {
        headers
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect()
    })
}

/// `mergeHeaders` (models.ts): case-insensitive override merge. An override
/// entry replaces any base entry with the same lowercased name; a `None`
/// override value deletes the base header (suppression).
pub fn merge_headers(
    base: Option<&ProviderHeaders>,
    overrides: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    let mut merged: ProviderHeaders = base.cloned().unwrap_or_default();
    for (name, value) in overrides.into_iter().flatten() {
        let lower = name.to_lowercase();
        merged.retain(|existing, _| existing.to_lowercase() != lower);
        merged.insert(name.clone(), value.clone());
    }
    Some(merged)
}

/// Folds several header sources into one with [`merge_headers`] semantics.
/// Mirrors the Anthropic/OpenAI SDK `buildHeaders` net behavior: sources are
/// applied in order, later ones win case-insensitively, `None` suppresses.
/// `87af49dec` (rpi-user-agent.ts (upstream pi-user-agent.ts)): the default pi `User-Agent` as the first
/// merge source — later sources (`model.headers`, request headers) override
/// it case-insensitively, and a `None` value suppresses it.
pub fn rpi_user_agent_headers() -> Option<ProviderHeaders> {
    Some(
        [(
            "User-Agent".to_owned(),
            Some(crate::utils::rpi_user_agent::get_rpi_user_agent()),
        )]
        .into(),
    )
}

pub fn merge_headers_chain(sources: &[Option<ProviderHeaders>]) -> ProviderHeaders {
    let mut merged: Option<ProviderHeaders> = None;
    for source in sources {
        merged = merge_headers(merged.as_ref(), source.as_ref());
    }
    merged.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_headers_case_insensitive_override() {
        let base: ProviderHeaders = [("X-API-Key".to_owned(), Some("a".to_owned()))].into();
        let overrides: ProviderHeaders = [("x-api-key".to_owned(), Some("b".to_owned()))].into();
        let merged = merge_headers(Some(&base), Some(&overrides)).expect("merged");
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get("x-api-key"), Some(&Some("b".to_owned())));
    }

    #[test]
    fn test_merge_headers_null_deletes() {
        let base: ProviderHeaders = [("Authorization".to_owned(), Some("a".to_owned()))].into();
        let overrides: ProviderHeaders = [("authorization".to_owned(), None)].into();
        let merged = merge_headers(Some(&base), Some(&overrides)).expect("merged");
        // The suppression marker itself is kept (upstream `merged[name] = null`);
        // `provider_headers_to_record` drops it at the HTTP boundary.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged.get("authorization"), Some(&None));
    }

    #[test]
    fn test_provider_headers_to_record() {
        let headers: ProviderHeaders = [
            ("a".to_owned(), Some("1".to_owned())),
            ("b".to_owned(), None),
        ]
        .into();
        let record = provider_headers_to_record(Some(&headers)).expect("record");
        assert_eq!(record, HashMap::from([("a".to_owned(), "1".to_owned())]));
        assert!(provider_headers_to_record(Some(&ProviderHeaders::new())).is_none());
        assert!(provider_headers_to_record(None).is_none());
    }
}
