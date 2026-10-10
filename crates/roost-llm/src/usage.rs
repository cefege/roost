//! Per-credential usage snapshots and rate-limit headers from provider responses.
//! The account pool uses these snapshots for ranking and rotation; API failures
//! are represented on each report instead of failing chat requests.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::{
    credentials::{CredentialKind, CredentialStore},
    endpoints::Endpoints,
    pool::now_ms,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub name: String,
    pub used_fraction: f64,
    pub resets_at_ms: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub credential_id: i64,
    pub provider: String,
    pub label: String,
    pub windows: Vec<UsageWindow>,
    pub note: Option<String>,
    pub fetched_ms: i64,
}

#[derive(Debug, Default)]
pub(crate) struct UsageCache {
    reports: Mutex<HashMap<i64, UsageReport>>,
}
impl UsageCache {
    pub(crate) fn reports(&self) -> Vec<UsageReport> {
        self.reports
            .lock()
            .map(|reports| reports.values().cloned().collect())
            .unwrap_or_default()
    }
    pub(crate) fn get(&self, credential_id: i64) -> Option<UsageReport> {
        self.reports.lock().ok()?.get(&credential_id).cloned()
    }
    pub(crate) fn insert(&self, report: UsageReport) {
        if let Ok(mut reports) = self.reports.lock() {
            reports.insert(report.credential_id, report);
        }
    }
}

pub(crate) fn header_observer(
    cache: Arc<UsageCache>,
    credential_id: i64,
    provider: String,
    label: String,
) -> crate::providers::HeaderObserver {
    Arc::new(move |headers: &HeaderMap| {
        if provider != "anthropic" {
            return;
        }
        let mut report = cache.get(credential_id).unwrap_or(UsageReport {
            credential_id,
            provider: provider.clone(),
            label: label.clone(),
            windows: Vec::new(),
            note: None,
            fetched_ms: now_ms(),
        });
        for name in ["5h", "7d"] {
            let prefix = format!("anthropic-ratelimit-unified-{name}-");
            let utilization = headers
                .iter()
                .find(|(header, _)| header.as_str() == format!("{prefix}utilization"))
                .and_then(|(_, value)| value.to_str().ok())
                .and_then(|value| value.parse::<f64>().ok());
            let reset = headers
                .iter()
                .find(|(header, _)| header.as_str() == format!("{prefix}reset"))
                .and_then(|(_, value)| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok());
            if utilization.is_none() && reset.is_none() {
                continue;
            }
            let window = UsageWindow {
                name: name.into(),
                used_fraction: utilization.unwrap_or_else(|| {
                    report
                        .windows
                        .iter()
                        .find(|window| window.name == name)
                        .map_or(0.0, |window| window.used_fraction)
                }),
                resets_at_ms: reset
                    .map(|seconds| seconds.saturating_mul(1000))
                    .or_else(|| {
                        report
                            .windows
                            .iter()
                            .find(|window| window.name == name)
                            .and_then(|window| window.resets_at_ms)
                    }),
            };
            if let Some(existing) = report.windows.iter_mut().find(|window| window.name == name) {
                *existing = window;
            } else {
                report.windows.push(window);
            }
        }
        if !report.windows.is_empty() {
            report.provider = provider.clone();
            report.label = label.clone();
            report.note = None;
            report.fetched_ms = now_ms();
            cache.insert(report);
        }
    })
}

pub async fn fetch_usage(
    store: &dyn CredentialStore,
    http: &reqwest::Client,
    endpoints: &Endpoints,
    credential_id: i64,
    provider: &str,
) -> Option<UsageReport> {
    let row = store
        .list(provider)
        .await
        .into_iter()
        .find(|row| row.id == credential_id)?;
    let result = match (&row.kind, provider) {
        (CredentialKind::OAuth { access, .. }, "anthropic") => {
            fetch_anthropic(http, endpoints, access).await
        }
        (
            CredentialKind::OAuth {
                access, account_id, ..
            },
            "openai-codex",
        ) => fetch_codex(http, endpoints, access, account_id.as_deref()).await,
        (CredentialKind::ApiKey { key }, "openrouter") => {
            fetch_openrouter(http, endpoints, key).await
        }
        _ => Ok((Vec::new(), Some("usage not reported".into()))),
    };
    let (windows, note) = result.unwrap_or_else(|error| (Vec::new(), Some(error)));
    Some(UsageReport {
        credential_id,
        provider: provider.into(),
        label: row.label,
        windows,
        note,
        fetched_ms: now_ms(),
    })
}

const ANTHROPIC_USAGE_BETAS: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20,effort-2025-11-24,extended-cache-ttl-2025-04-11";
async fn fetch_anthropic(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    access: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), String> {
    let base = endpoints.base("anthropic-console", "https://api.anthropic.com");
    let response = http
        .get(format!("{base}/api/oauth/usage"))
        .bearer_auth(access)
        .header("anthropic-beta", ANTHROPIC_USAGE_BETAS)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Ok((Vec::new(), Some(status.to_string())));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let mut windows = Vec::new();
    for (name, key) in [("5h", "five_hour"), ("7d", "seven_day")] {
        if let Some(bucket) = body.get(key) {
            let fraction = bucket
                .get("utilization")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
                / 100.0;
            let resets_at_ms = bucket
                .get("resets_at")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_timestamp);
            windows.push(UsageWindow {
                name: name.into(),
                used_fraction: fraction,
                resets_at_ms,
            });
        }
    }
    Ok((windows, None))
}
async fn fetch_codex(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    access: &str,
    account_id: Option<&str>,
) -> Result<(Vec<UsageWindow>, Option<String>), String> {
    let base = endpoints.base("openai-codex", "https://chatgpt.com/backend-api");
    let mut request = http.get(format!("{base}/wham/usage")).bearer_auth(access);
    if let Some(account_id) = account_id {
        request = request.header("chatgpt-account-id", account_id);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Ok((Vec::new(), Some(status.to_string())));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let mut windows = Vec::new();
    for (name, key) in [
        ("primary", "primary_window"),
        ("secondary", "secondary_window"),
    ] {
        if let Some(window) = body.get("rate_limit").and_then(|rate| rate.get(key)) {
            let fraction = window
                .get("used_percent")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
                / 100.0;
            let reset = window
                .get("reset_at")
                .and_then(serde_json::Value::as_i64)
                .map(|seconds| seconds.saturating_mul(1000));
            windows.push(UsageWindow {
                name: name.into(),
                used_fraction: fraction,
                resets_at_ms: reset,
            });
        }
    }
    Ok((windows, None))
}
async fn fetch_openrouter(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    key: &str,
) -> Result<(Vec<UsageWindow>, Option<String>), String> {
    let base = endpoints.base("openrouter", "https://openrouter.ai/api/v1");
    let response = http
        .get(format!("{base}/key"))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Ok((Vec::new(), Some(status.to_string())));
    }
    let body: serde_json::Value = response.json().await.map_err(|error| error.to_string())?;
    let data = body.get("data").unwrap_or(&body);
    let used = data.get("usage").and_then(serde_json::Value::as_f64);
    let limit = data.get("limit").and_then(serde_json::Value::as_f64);
    let note = match (used, limit) {
        (Some(used), Some(limit)) => Some(format!("credits used {used:.2} / limit {limit:.2}")),
        _ => Some("usage unavailable".into()),
    };
    Ok((Vec::new(), note))
}
fn parse_timestamp(value: &str) -> Option<i64> {
    let (date, time) = value.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let year = date.next()??;
    let month = date.next()??;
    let day = date.next()??;
    let (clock, offset) = time.split_at(
        time.find(['Z', '+', '-'])
            .filter(|position| *position >= 8)
            .unwrap_or(time.len()),
    );
    let mut clock_parts = clock.split(':');
    let hour = clock_parts.next()?.parse::<i64>().ok()?;
    let minute = clock_parts.next()?.parse::<i64>().ok()?;
    let seconds_part = clock_parts.next()?;
    let seconds = seconds_part.split('.').next()?.parse::<i64>().ok()?;
    let millis = seconds_part
        .split_once('.')
        .and_then(|(_, fraction)| fraction.get(..3))
        .and_then(|fraction| fraction.parse::<i64>().ok())
        .unwrap_or(0);
    let days = days_from_civil(year, month, day);
    let mut timestamp = (((days * 24 + hour) * 60 + minute) * 60 + seconds) * 1000 + millis;
    if !offset.is_empty() && offset != "Z" {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits = offset.get(1..6)?.replace(':', "");
        let hours = digits.get(..2)?.parse::<i64>().ok()?;
        let minutes = digits.get(2..4)?.parse::<i64>().ok()?;
        timestamp -= sign * (hours * 60 + minutes) * 60_000;
    }
    Some(timestamp)
}
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
