//! Bounded leases over real SDK event-stream responses.
//!
//! A next operation reads one actual upstream chunk. No status polling, event
//! synthesis or unbounded queue sits between SDK and browser backpressure.

use std::fmt::Write as _;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use roost_protocol::native_dashboard::{
    HttpHeader, HttpResponse, MAX_RESPONSE_BODY_BYTES, StreamResponse, token_is_well_formed,
};
use tokio::sync::watch;

use super::resources::{OwnedResource, StreamSlot};
use super::{Refusal, header_str, refused, unavailable};

const MAX_STREAMS: usize = 2;
const STREAM_IDLE: Duration = Duration::from_secs(45);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn open(
    resource: &Arc<OwnedResource>,
    response: reqwest::Response,
    status: u16,
    headers: Vec<HttpHeader>,
) -> Result<HttpResponse, Refusal> {
    if status != 200
        || !header_str(response.headers(), "content-type")
            .is_some_and(|value| value.starts_with("text/event-stream"))
    {
        return Err(refused(
            "the native dashboard did not return its event stream",
        ));
    }
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|_| refused("the dashboard stream token could not be generated"))?;
    let mut id = String::with_capacity(64);
    for byte in nonce {
        write!(&mut id, "{byte:02x}")
            .map_err(|_| refused("the dashboard stream token could not be encoded"))?;
    }
    let mut streams = resource
        .streams
        .lock()
        .map_err(|_| refused("the dashboard stream table is poisoned"))?;
    streams.retain(|_, stream| stream.touched.elapsed() < STREAM_IDLE);
    if streams.len() >= MAX_STREAMS || *resource.closed.borrow() {
        return Err(refused(
            "the OMP stats resource cannot admit another event stream",
        ));
    }
    let (cancelled, _) = watch::channel(false);
    let expiry = expire_idle(
        Arc::downgrade(resource),
        id.clone(),
        resource.closed.subscribe(),
    );
    streams.insert(
        id.clone(),
        StreamSlot {
            response: Some(response),
            touched: Instant::now(),
            cancelled,
            expiry,
        },
    );
    tracing::debug!(conversation_id = %resource.key.conversation_id, "OMP stats upstream stream opened");
    Ok(HttpResponse {
        status,
        headers,
        body_base64: String::new(),
        stream: Some(StreamResponse {
            id,
            finished: false,
        }),
    })
}

pub(super) async fn next(resource: &OwnedResource, id: &str) -> Result<HttpResponse, Refusal> {
    if !token_is_well_formed(id) {
        return Err(refused("the dashboard stream token is invalid"));
    }
    let (mut response, mut cancelled) = {
        let mut streams = resource
            .streams
            .lock()
            .map_err(|_| refused("the dashboard stream table is poisoned"))?;
        let slot = streams
            .get_mut(id)
            .ok_or_else(|| refused("the dashboard stream no longer belongs to this resource"))?;
        if slot.touched.elapsed() >= STREAM_IDLE {
            streams.remove(id);
            return Err(refused("the dashboard stream lease expired"));
        }
        (
            slot.response
                .take()
                .ok_or_else(|| refused("the dashboard stream already has an outstanding read"))?,
            slot.cancelled.subscribe(),
        )
    };
    let mut reading = ReadingStream {
        resource,
        id,
        keep: false,
    };
    let chunk = tokio::select! {
        biased;
        _ = cancelled.changed() => return Err(refused("the dashboard stream was closed")),
        result = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk()) => result
            .map_err(|_| refused("the native dashboard event stream timed out"))?
            .map_err(|error| unavailable("the native dashboard event stream failed", &error))?,
    };
    let finished = chunk.is_none();
    let body_base64 = match chunk {
        Some(chunk) if chunk.len() <= MAX_RESPONSE_BODY_BYTES => STANDARD.encode(chunk),
        Some(_) => {
            return Err(refused(
                "the dashboard event stream chunk exceeds its bound",
            ));
        }
        None => String::new(),
    };
    if !finished {
        let mut streams = resource
            .streams
            .lock()
            .map_err(|_| refused("the dashboard stream table is poisoned"))?;
        let slot = streams
            .get_mut(id)
            .ok_or_else(|| refused("the dashboard stream was closed during its read"))?;
        slot.response = Some(response);
        slot.touched = Instant::now();
        reading.keep = true;
    }
    Ok(HttpResponse {
        status: 200,
        headers: Vec::new(),
        body_base64,
        stream: Some(StreamResponse {
            id: id.to_owned(),
            finished,
        }),
    })
}

pub(super) fn close(resource: &OwnedResource, id: &str) -> Result<HttpResponse, Refusal> {
    if !token_is_well_formed(id) {
        return Err(refused("the dashboard stream token is invalid"));
    }
    resource
        .streams
        .lock()
        .map_err(|_| refused("the dashboard stream table is poisoned"))?
        .remove(id);
    tracing::debug!(conversation_id = %resource.key.conversation_id, "OMP stats upstream stream closed");
    Ok(HttpResponse {
        status: 204,
        headers: Vec::new(),
        body_base64: String::new(),
        stream: Some(StreamResponse {
            id: id.to_owned(),
            finished: true,
        }),
    })
}

struct ReadingStream<'a> {
    resource: &'a OwnedResource,
    id: &'a str,
    keep: bool,
}

impl Drop for ReadingStream<'_> {
    fn drop(&mut self) {
        if !self.keep
            && let Ok(mut streams) = self.resource.streams.lock()
        {
            streams.remove(self.id);
        }
    }
}

fn expire_idle(
    resource: Weak<OwnedResource>,
    id: String,
    mut closed: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let remaining = {
                let Some(resource) = resource.upgrade() else {
                    return;
                };
                let Ok(mut streams) = resource.streams.lock() else {
                    return;
                };
                let Some(slot) = streams.get(&id) else {
                    return;
                };
                let remaining = STREAM_IDLE.saturating_sub(slot.touched.elapsed());
                if remaining.is_zero() {
                    streams.remove(&id);
                    return;
                }
                remaining
            };
            tokio::select! {
                _ = closed.changed() => return,
                _ = tokio::time::sleep(remaining) => {}
            }
        }
    })
}
