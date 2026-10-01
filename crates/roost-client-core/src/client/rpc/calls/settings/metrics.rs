//! The coordinator's live telemetry counters.
//!
//! Called by roost-web's Settings metrics pane. v2 call site:
//! `apps/web/src/components/Settings/MetricsPane.tsx:39-59` (`coordClient.miscMetrics`,
//! polled every five seconds). The per-route maps become sorted vectors here so
//! the pane does not carry a `BTreeMap` whose iteration order it has to re-sort
//! on every poll anyway.

use roost_proto::{MiscMetricsRequest, MiscMetricsResponse};

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;

/// One route's counters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouteCounters {
    /// The route path.
    pub path: String,
    /// How many requests it served.
    pub requests: u64,
    /// How many of those were errors.
    pub errors: u64,
}

/// The coordinator's counters since it started.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricsSnapshot {
    /// Milliseconds since the coordinator booted.
    pub uptime_ms: u64,
    /// Per-route counters, busiest route first.
    pub routes: Vec<RouteCounters>,
    /// Every request served.
    pub total_requests: u64,
    /// Every 4xx and 5xx answered.
    pub total_errors: u64,
}

/// `MiscMetrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GetMetrics;

impl UnaryMethod for GetMetrics {
    const METHOD: &'static str = "MiscMetrics";
    type Response = MetricsSnapshot;

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        encode_message(Self::METHOD, &MiscMetricsRequest::default())
    }

    fn decode_response(body: &[u8]) -> Result<MetricsSnapshot, RpcCodecError> {
        let response: MiscMetricsResponse = decode_message(Self::METHOD, body)?;
        let error_counts = response.errors;
        let mut routes: Vec<RouteCounters> = response
            .requests
            .into_iter()
            .map(|(path, requests)| RouteCounters {
                errors: error_counts.get(&path).copied().unwrap_or_default(),
                path,
                requests,
            })
            .collect();
        routes.sort_by(|left, right| {
            right
                .requests
                .cmp(&left.requests)
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(MetricsSnapshot {
            uptime_ms: response.uptime_ms,
            routes,
            total_requests: response.total_requests,
            total_errors: response.total_errors,
        })
    }
}
