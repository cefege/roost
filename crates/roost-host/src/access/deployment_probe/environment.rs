//! Literal access-setting and environment-source validation for Flux evidence.
//! Called by the read-only deployment probe before presenting a proposal.
//! Rejects ambiguous values and indirect overrides of access settings.

use serde_json::Value;

use super::{coordinator_container, error, kubectl_output};
use crate::ProtocolResult;

pub(super) fn access_settings() -> [&'static str; 7] {
    [
        crate::ENV_COORDINATOR_BIND,
        crate::ENV_TRUST_PROXY,
        crate::ENV_TRUSTED_PROXY_CIDRS,
        crate::ENV_WEB_PUBLIC_URL,
        crate::ENV_COORDINATOR_PUBLIC_URL,
        crate::ENV_CORS_ALLOWED_ORIGINS,
        crate::ENV_RETAINED_FRONTDOOR_ORIGINS,
    ]
}

pub(super) async fn verify_environment_sources(
    deployment: &Value,
    namespace: &str,
) -> ProtocolResult<()> {
    let coordinator = coordinator_container(deployment)?;
    let Some(sources) = coordinator.get("envFrom") else {
        return Ok(());
    };
    let sources = sources
        .as_array()
        .ok_or_else(|| error("environment sources are ambiguous"))?;
    for source in sources {
        let prefix = source
            .get("prefix")
            .map_or(Some(""), Value::as_str)
            .ok_or_else(|| error("environment source prefix is invalid"))?;
        let (kind, reference, template) = match (
            source.get("secretRef"),
            source.get("configMapRef"),
        ) {
            (Some(reference), None) => (
                "secret",
                reference,
                "go-template={{range $key, $_ := .data}}{{println $key}}{{end}}",
            ),
            (None, Some(reference)) => (
                "configmap",
                reference,
                "go-template={{range $key, $_ := .data}}{{println $key}}{{end}}{{range $key, $_ := .binaryData}}{{println $key}}{{end}}",
            ),
            _ => return Err(error("environment source is ambiguous")),
        };
        let name = reference
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| safe_resource_name(name))
            .ok_or_else(|| error("environment source name is invalid"))?;
        let bytes = kubectl_output(&["get", kind, name, "-n", namespace, "-o", template]).await?;
        let keys = std::str::from_utf8(&bytes)
            .map_err(|_| error("environment source keys are invalid"))?;
        validate_environment_keys(prefix, keys)?;
    }
    Ok(())
}

pub(super) fn safe_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        })
        && !name.starts_with('-')
        && !name.starts_with('.')
        && !name.ends_with('-')
        && !name.ends_with('.')
}

pub(super) fn validate_environment_keys(prefix: &str, keys: &str) -> ProtocolResult<()> {
    for key in keys.lines() {
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || access_settings()
                .iter()
                .any(|setting| *setting == format!("{prefix}{key}"))
        {
            return Err(error(
                "environment source contains ambiguous access settings or keys",
            ));
        }
    }
    Ok(())
}
