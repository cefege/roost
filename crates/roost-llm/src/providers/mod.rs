//! Provider HTTP clients for the supported Anthropic, Codex Responses and
//! OpenAI-compatible wire APIs. The account pool calls `stream_chat`; each
//! client owns request serialization and SSE event translation.

mod anthropic;
mod codex;
mod completions;
mod sse;

use std::sync::Arc;

use futures::{StreamExt, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::ResolvedAuth,
    catalog::WireApi,
    endpoints::Endpoints,
    error::LlmError,
    message::{ChatRequest, StreamEvent},
};

pub type HeaderObserver = Arc<dyn Fn(&HeaderMap) + Send + Sync>;

type ProviderStream = futures::stream::BoxStream<'static, Result<StreamEvent, LlmError>>;

pub fn stream_chat(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    request: ChatRequest,
    auth: ResolvedAuth,
    cancel: CancellationToken,
    observer: Option<HeaderObserver>,
) -> futures::stream::BoxStream<'static, Result<StreamEvent, LlmError>> {
    match request.model.api {
        WireApi::AnthropicMessages => {
            anthropic::stream(http, endpoints, request, auth, cancel, observer)
        }
        WireApi::OpenAiCodexResponses => {
            codex::stream(http, endpoints, request, auth, cancel, observer)
        }
        WireApi::OpenAiCompletions => {
            completions::stream(http, endpoints, request, auth, cancel, observer)
        }
        WireApi::TypesafeSystemOne => Box::pin(stream::once(async {
            Err(LlmError::Decode(
                "classifier models do not support chat completions".into(),
            ))
        })),
    }
}

fn auth_headers(
    auth: &ResolvedAuth,
    api_key_name: &str,
    bearer: bool,
) -> Result<HeaderMap, LlmError> {
    let mut headers = HeaderMap::new();
    let use_bearer = bearer || auth.is_oauth();
    let name = if use_bearer {
        AUTHORIZATION
    } else {
        reqwest::header::HeaderName::from_bytes(api_key_name.as_bytes())
            .map_err(|error| LlmError::Decode(error.to_string()))?
    };
    let value = if use_bearer {
        format!("Bearer {}", auth.secret())
    } else {
        auth.secret().to_owned()
    };
    headers.insert(
        name,
        HeaderValue::from_str(&value).map_err(|error| LlmError::Auth(error.to_string()))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    Ok(headers)
}

async fn post_sse(
    http: &reqwest::Client,
    url: String,
    headers: HeaderMap,
    body: serde_json::Value,
    cancel: &CancellationToken,
    observer: &Option<HeaderObserver>,
) -> Result<reqwest::Response, LlmError> {
    let request = http
        .post(url)
        .headers(headers)
        .json(&body)
        .build()
        .map_err(LlmError::from)?;
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        result = http.execute(request) => result.map_err(LlmError::from)?,
    };
    if let Some(observer) = observer {
        observer(response.headers());
    }
    if !response.status().is_success() {
        let status = response.status();
        let headers = response.headers().clone();
        let body = tokio::select! {
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
            result = response.text() => result.unwrap_or_default(),
        };
        return Err(http_error(status.as_u16(), &headers, body));
    }
    Ok(response)
}

fn http_error(status: u16, headers: &HeaderMap, body: String) -> LlmError {
    if status == 429 {
        let retry_after_ms = headers
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<f64>().ok())
            .map(|seconds| (seconds * 1000.0) as u64);
        let reset_at_ms = headers.iter().find_map(|(name, value)| {
            let name = name.as_str();
            (name.starts_with("anthropic-ratelimit-unified-") && name.ends_with("-reset"))
                .then(|| value.to_str().ok().and_then(parse_reset_ms))
                .flatten()
        });
        LlmError::RateLimited {
            retry_after_ms,
            reset_at_ms,
        }
    } else if status == 401 || status == 403 {
        LlmError::Auth(body)
    } else {
        LlmError::Http { status, body }
    }
}

fn parse_reset_ms(value: &str) -> Option<i64> {
    if let Ok(timestamp) = value.parse::<i64>() {
        return Some(if timestamp < 10_000_000_000 {
            timestamp.saturating_mul(1000)
        } else {
            timestamp
        });
    }
    if let Ok(timestamp) = value.parse::<f64>() {
        return Some(if timestamp < 10_000_000_000.0 {
            (timestamp * 1000.0) as i64
        } else {
            timestamp as i64
        });
    }
    parse_iso_reset_ms(value).or_else(|| chrono_like_time(value))
}
fn parse_iso_reset_ms(value: &str) -> Option<i64> {
    let (date, time) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time, offset_seconds) = if let Some(time) = time.strip_suffix('Z') {
        (time, 0)
    } else if let Some(position) = time
        .char_indices()
        .rev()
        .find_map(|(index, character)| matches!(character, '+' | '-').then_some(index))
    {
        let (time, offset) = time.split_at(position);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let mut fields = offset[1..].split(':');
        let hours = fields.next()?.parse::<i64>().ok()?;
        let minutes = fields.next()?.parse::<i64>().ok()?;
        (time, sign * (hours * 3600 + minutes * 60))
    } else {
        (time, 0)
    };
    let (time, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut time_parts = time.split(':');
    let hours = time_parts.next()?.parse::<i64>().ok()?;
    let minutes = time_parts.next()?.parse::<i64>().ok()?;
    let seconds = time_parts.next()?.parse::<i64>().ok()?;
    if time_parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    let fraction_ms = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.chars().take(3).collect::<String>();
        let parsed = digits.parse::<i64>().ok()?;
        parsed
            * match digits.len() {
                1 => 100,
                2 => 10,
                _ => 1,
            }
    };
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let timestamp_seconds = days * 86_400 + hours * 3600 + minutes * 60 + seconds - offset_seconds;
    Some(
        timestamp_seconds
            .saturating_mul(1000)
            .saturating_add(fraction_ms),
    )
}

fn chrono_like_time(value: &str) -> Option<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let (days, remainder) = if let Some((days, rest)) = whole.split_once('d') {
        (days.parse::<i64>().ok()?, rest)
    } else {
        (0, whole)
    };
    let mut fields = remainder.split(':');
    let hours = fields.next()?.parse::<i64>().ok()?;
    let minutes = fields.next()?.parse::<i64>().ok()?;
    let seconds = fields.next()?.parse::<i64>().ok()?;
    let fraction_ms = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 100,
        2 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction
            .chars()
            .take(3)
            .collect::<String>()
            .parse::<i64>()
            .ok()?,
    };
    let millis = seconds.saturating_mul(1000).saturating_add(fraction_ms);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(now_ms.saturating_add((((days * 24 + hours) * 60 + minutes) * 60) * 1000 + millis))
}

fn response_events(
    response: reqwest::Response,
    cancel: CancellationToken,
) -> futures::stream::BoxStream<'static, Result<String, LlmError>> {
    Box::pin(async_stream::try_stream! {
        let mut body = response.bytes_stream();
        let mut buffer = String::new();
        let mut data = Vec::new();
        let mut completed = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => None,
                chunk = body.next() => chunk,
            };
            let Some(chunk) = chunk else {
                if cancel.is_cancelled() {
                    Err(LlmError::Cancelled)?;
                }
                break;
            };
            let chunk = chunk.map_err(LlmError::from)?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(newline) = buffer.find('\n') {
                let line = buffer.drain(..=newline).collect::<String>();
                sse::consume_line(line.trim_end_matches(['\n', '\r']), &mut data, &mut completed);
                for event in completed.drain(..) { yield event; }
            }
        }
        if !buffer.is_empty() {
            sse::consume_line(buffer.trim_end_matches(['\n', '\r']), &mut data, &mut completed);
        }
        sse::consume_line("", &mut data, &mut completed);
        for event in completed { yield event; }
    })
}

fn decode_json(data: &str) -> Result<serde_json::Value, LlmError> {
    serde_json::from_str(data).map_err(|error| LlmError::Decode(format!("{error}: {data}")))
}

#[cfg(test)]
mod tests {
    use super::parse_reset_ms;

    #[test]
    fn rate_limit_resets_accept_seconds_offsets_and_durations() {
        assert_eq!(parse_reset_ms("10"), Some(10_000));
        assert_eq!(parse_reset_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_reset_ms("1970-01-01T01:00:00+01:00"), Some(0));
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(
            parse_reset_ms("0:00:05.500")
                .is_some_and(|reset| reset >= before + 5_000 && reset <= before + 6_000)
        );
    }
}
