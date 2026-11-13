//! Process-owned stats resources and byte-stream operations.
//!
//! ACP admits resources from its live child; control frames carry that exact
//! conversation/transport binding, never an independently privileged URL.

use serde::{Deserialize, Serialize};

use super::{DashboardTarget, parse_dashboard_origin, token_is_well_formed};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceKey {
    pub conversation_id: String,
    pub transport_id: String,
    pub resource_id: String,
}

impl ResourceKey {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.conversation_id.is_empty()
            && self.conversation_id.len() <= 256
            && !self.transport_id.is_empty()
            && self.transport_id.len() <= 256
            && token_is_well_formed(&self.resource_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardOpen {
    pub resource_id: String,
    pub origin: String,
    pub fragment: String,
}

impl DashboardOpen {
    #[must_use]
    pub fn parse(value: &str) -> Option<(Self, DashboardTarget)> {
        let open: Self = serde_json::from_str(value).ok()?;
        if !token_is_well_formed(&open.resource_id) || !fragment_is_safe(&open.fragment) {
            return None;
        }
        let target = parse_dashboard_origin(&open.origin)?;
        Some((open, target))
    }
}

#[must_use]
pub fn fragment_is_safe(fragment: &str) -> bool {
    fragment.is_empty()
        || fragment.strip_prefix("#/traces?s=").is_some_and(|session| {
            !session.is_empty()
                && session.len() <= super::MAX_REQUEST_PATH_BYTES
                && session
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'#'))
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamRequest {
    Open,
    Next { id: String },
    Close { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamResponse {
    pub id: String,
    pub finished: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owned_dashboard_routes_are_admitted() {
        let resource = "a".repeat(64);
        let value = serde_json::json!({"resourceId": resource, "origin": "http://127.0.0.1:3847", "fragment": "#/traces?s=%2Ftmp%2Fsession.jsonl"});
        assert!(DashboardOpen::parse(&value.to_string()).is_some());
        assert!(!fragment_is_safe(
            "#/traces?s=x&redirect=https://example.com"
        ));
        assert!(!fragment_is_safe("https://example.com"));
        assert!(DashboardOpen::parse("http://127.0.0.1:3847").is_none());
    }
}
