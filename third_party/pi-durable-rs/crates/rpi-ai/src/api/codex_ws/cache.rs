//! Codex WebSocket session cache: 5min/55min dual-TTL connection reuse,
//! per-session permanent SSE fallback, debug stats, and session resource
//! cleanup (port of the cache half of
//! `packages/ai/src/api/openai-codex-responses.ts` @ pi 4181f66).
//!
//! Connections are pooled per `sessionId → accountId` (cfe6b6a05, #7284):
//! rotating accounts must not reuse a socket authenticated by another
//! account. Debug stats and the SSE-fallback set stay keyed by session id
//! only, matching upstream.
//!
//! State-machine notes live in [`crate::api::codex_ws`].

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, Once, RwLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use super::{CodexError, WsStream, close_silently};
use crate::session_resources::register_session_resource_cleanup;

/// `SESSION_WEBSOCKET_CACHE_TTL_MS` (idle TTL).
pub const SESSION_WEBSOCKET_CACHE_TTL_MS: u64 = 5 * 60 * 1000;
/// `SESSION_WEBSOCKET_MAX_AGE_MS` (hard connection age limit).
pub const SESSION_WEBSOCKET_MAX_AGE_MS: u64 = 55 * 60 * 1000;

// ---------------------------------------------------------------------------
// TTL configuration (test seam)
// ---------------------------------------------------------------------------

/// The effective TTLs: production defaults, overridable in tests
/// ([`set_codex_websocket_ttls_for_tests`]).
static TTL_OVERRIDE: RwLock<Option<(Duration, Duration)>> = RwLock::new(None);

fn ttls() -> (Duration, Duration) {
    TTL_OVERRIDE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or((
            Duration::from_millis(SESSION_WEBSOCKET_CACHE_TTL_MS),
            Duration::from_millis(SESSION_WEBSOCKET_MAX_AGE_MS),
        ))
}

/// Test seam: overrides (cache TTL, max age) and returns the previous values
/// for restoration. Production code never calls this; the defaults are the
/// upstream constants.
pub fn set_codex_websocket_ttls_for_tests(
    cache_ttl: Duration,
    max_age: Duration,
) -> (Duration, Duration) {
    let previous = ttls();
    *TTL_OVERRIDE.write().unwrap_or_else(|e| e.into_inner()) = Some((cache_ttl, max_age));
    previous
}

// ---------------------------------------------------------------------------
// Cache entry
// ---------------------------------------------------------------------------

/// `CachedWebSocketContinuationState`.
#[derive(Debug, Clone)]
pub struct CachedWebSocketContinuationState {
    pub last_request_body: Value,
    pub last_response_id: String,
    pub last_response_items: Vec<Value>,
}

/// `CachedWebSocketConnection`. The socket is `None` while `busy` (it is owned
/// by the in-flight request); `pending` holds a message consumed by the
/// liveness probe on acquire.
struct CacheEntry {
    socket: Option<WsStream>,
    pending: Option<Message>,
    busy: bool,
    created_at: Instant,
    /// Bumped on every acquire/release; an idle-expiry task only fires when
    /// its captured generation still matches (upstream `clearTimeout`).
    idle_generation: u64,
    continuation: Option<CachedWebSocketContinuationState>,
}

static SESSION_CACHE: LazyLock<Mutex<HashMap<String, HashMap<String, CacheEntry>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache() -> std::sync::MutexGuard<'static, HashMap<String, HashMap<String, CacheEntry>>> {
    ensure_cleanup_registered();
    SESSION_CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Upstream `accountEntries.delete(accountId)` plus
/// `if (accountEntries?.size === 0) websocketSessionCache.delete(sessionId)`.
fn remove_account_entry(
    cache: &mut HashMap<String, HashMap<String, CacheEntry>>,
    session_id: &str,
    account_id: &str,
) -> Option<CacheEntry> {
    let entry = cache
        .get_mut(session_id)
        .and_then(|account_entries| account_entries.remove(account_id));
    if cache
        .get(session_id)
        .is_some_and(|account_entries| account_entries.is_empty())
    {
        cache.remove(session_id);
    }
    entry
}

/// `isWebSocketSessionExpired`.
fn is_session_expired(entry: &CacheEntry) -> bool {
    entry.created_at.elapsed() >= ttls().1
}

// ---------------------------------------------------------------------------
// Acquire / release
// ---------------------------------------------------------------------------

/// Result of [`acquire`]: the connection plus its cache disposition.
pub struct AcquiredWebSocket {
    pub socket: WsStream,
    /// Message consumed by the liveness probe; deliver before reading.
    pub pending: Option<Message>,
    pub reused: bool,
    /// The `(session_id, account_id)` this connection is (or will be) cached
    /// under. `None` for one-shot connections (no session id, or the cached
    /// entry was busy) — [`release`] closes those.
    pub cache_key: Option<(String, String)>,
}

/// Non-blocking reusability probe, standing in for upstream's `readyState`
/// check (tungstenite exposes no ready state): `Pending` means alive; a close
/// frame, EOF, or error means dead. A data message that arrived unexpectedly
/// is stashed into the entry so no event is lost.
fn probe(socket: &mut WsStream) -> ProbeResult {
    use futures::FutureExt;
    std::future::poll_fn(|cx| socket.poll_next_unpin(cx))
        .now_or_never()
        .into()
}

enum ProbeResult {
    Alive,
    AliveWithMessage(Message),
    Dead,
}

impl From<Option<Option<Result<Message, tokio_tungstenite::tungstenite::Error>>>> for ProbeResult {
    fn from(poll: Option<Option<Result<Message, tokio_tungstenite::tungstenite::Error>>>) -> Self {
        match poll {
            None => ProbeResult::Alive,
            Some(Some(Ok(message @ (Message::Text(_) | Message::Binary(_))))) => {
                ProbeResult::AliveWithMessage(message)
            }
            // Ping/Pong: the connection is alive; the frame is disposable
            // (tungstenite already queued the pong for the ping).
            Some(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => {
                ProbeResult::Alive
            }
            Some(Some(Ok(Message::Close(_)))) | Some(Some(Err(_))) | Some(None) => {
                ProbeResult::Dead
            }
        }
    }
}

/// `acquireWebSocket`.
pub async fn acquire(
    session_id: Option<&str>,
    account_id: &str,
    url: &str,
    headers: &reqwest::header::HeaderMap,
    connect_timeout_ms: Option<u64>,
    signal: Option<&tokio_util::sync::CancellationToken>,
) -> Result<AcquiredWebSocket, CodexError> {
    let Some(session_id) = session_id else {
        // One-shot: connect and always close on release.
        let socket = super::connect(url, headers, connect_timeout_ms, signal).await?;
        return Ok(AcquiredWebSocket {
            socket,
            pending: None,
            reused: false,
            cache_key: None,
        });
    };

    enum CachedTake {
        Reuse {
            socket: WsStream,
            pending: Option<Message>,
        },
        Expired {
            socket: Option<WsStream>,
        },
        Busy,
        Fresh,
    }

    let take = {
        let mut cache = cache();
        let cached = cache
            .get(session_id)
            .is_some_and(|account_entries| account_entries.contains_key(account_id));
        if !cached {
            CachedTake::Fresh
        } else {
            // invariant: contains_key checked immediately above
            let entry = cache
                .get_mut(session_id)
                .and_then(|account_entries| account_entries.get_mut(account_id))
                .unwrap();
            // Invalidate any pending idle-expiry task (upstream clearTimeout).
            entry.idle_generation += 1;
            if !entry.busy && is_session_expired(entry) {
                let socket = entry.socket.take();
                remove_account_entry(&mut cache, session_id, account_id);
                CachedTake::Expired { socket }
            } else if !entry.busy && entry.socket.is_some() {
                entry.busy = true;
                CachedTake::Reuse {
                    // invariant: checked is_some() above
                    socket: entry.socket.take().unwrap(),
                    pending: entry.pending.take(),
                }
            } else if entry.busy {
                CachedTake::Busy
            } else {
                // Idle entry without a socket cannot happen (socket is only
                // None while busy); treat as dead and drop the entry.
                remove_account_entry(&mut cache, session_id, account_id);
                CachedTake::Fresh
            }
        }
    };

    match take {
        CachedTake::Reuse {
            mut socket,
            pending,
        } => {
            if let Some(message) = pending {
                // A stashed probe message means the socket was already probed.
                return Ok(AcquiredWebSocket {
                    socket,
                    pending: Some(message),
                    reused: true,
                    cache_key: Some((session_id.to_owned(), account_id.to_owned())),
                });
            }
            match probe(&mut socket) {
                ProbeResult::Alive => Ok(AcquiredWebSocket {
                    socket,
                    pending: None,
                    reused: true,
                    cache_key: Some((session_id.to_owned(), account_id.to_owned())),
                }),
                ProbeResult::AliveWithMessage(message) => Ok(AcquiredWebSocket {
                    socket,
                    pending: Some(message),
                    reused: true,
                    cache_key: Some((session_id.to_owned(), account_id.to_owned())),
                }),
                ProbeResult::Dead => {
                    close_silently(&mut socket, 1000, "done").await;
                    remove_account_entry(&mut cache(), session_id, account_id);
                    // Fall through to a fresh cached connection.
                    Box::pin(acquire(
                        Some(session_id),
                        account_id,
                        url,
                        headers,
                        connect_timeout_ms,
                        signal,
                    ))
                    .await
                }
            }
        }
        CachedTake::Expired { socket } => {
            if let Some(mut socket) = socket {
                close_silently(&mut socket, 1000, "connection_age_limit").await;
            }
            Box::pin(acquire(
                Some(session_id),
                account_id,
                url,
                headers,
                connect_timeout_ms,
                signal,
            ))
            .await
        }
        CachedTake::Busy => {
            // One-shot alongside the busy cached connection (upstream connects
            // a fresh, uncached socket).
            let socket = super::connect(url, headers, connect_timeout_ms, signal).await?;
            Ok(AcquiredWebSocket {
                socket,
                pending: None,
                reused: false,
                cache_key: None,
            })
        }
        CachedTake::Fresh => {
            let socket = super::connect(url, headers, connect_timeout_ms, signal).await?;
            cache().entry(session_id.to_owned()).or_default().insert(
                account_id.to_owned(),
                CacheEntry {
                    socket: None,
                    pending: None,
                    busy: true,
                    created_at: Instant::now(),
                    idle_generation: 0,
                    continuation: None,
                },
            );
            Ok(AcquiredWebSocket {
                socket,
                pending: None,
                reused: false,
                cache_key: Some((session_id.to_owned(), account_id.to_owned())),
            })
        }
    }
}

/// `release({ keep })`: returns the socket to the cache (scheduling the idle
/// expiry) or closes and evicts it.
pub async fn release(
    cache_key: Option<(String, String)>,
    socket: WsStream,
    pending: Option<Message>,
    keep: bool,
) {
    let Some((session_id, account_id)) = cache_key else {
        let mut socket = socket;
        close_silently(&mut socket, 1000, "done").await;
        return;
    };

    if keep {
        let mut close_socket = None;
        let generation = {
            let mut cache = cache();
            match cache
                .get_mut(&session_id)
                .and_then(|account_entries| account_entries.get_mut(&account_id))
            {
                Some(entry) if entry.busy => {
                    entry.socket = Some(socket);
                    entry.pending = pending;
                    entry.busy = false;
                    entry.idle_generation += 1;
                    Some(entry.idle_generation)
                }
                // Entry vanished (close_sessions raced us): close the socket.
                _ => {
                    close_socket = Some(socket);
                    None
                }
            }
        };
        if let Some(generation) = generation {
            schedule_idle_expiry(session_id, account_id, generation);
        }
        if let Some(mut socket) = close_socket {
            close_silently(&mut socket, 1000, "done").await;
        }
        return;
    }

    // Not kept: evict the entry (if still ours) and close.
    remove_account_entry(&mut cache(), &session_id, &account_id);
    let mut socket = socket;
    close_silently(&mut socket, 1000, "done").await;
}

/// `scheduleSessionWebSocketExpiry`: spawned timer task; fires only when the
/// entry is still idle at the same generation (upstream re-checks `busy`).
fn schedule_idle_expiry(session_id: String, account_id: String, generation: u64) {
    let ttl = ttls().0;
    tokio::spawn(async move {
        tokio::time::sleep(ttl).await;
        let socket = {
            let mut cache = cache();
            // Upstream re-checks `entry.busy` when the timer fires.
            let evict = matches!(
                cache.get(&session_id).and_then(|account_entries| account_entries.get(&account_id)),
                Some(entry) if !entry.busy && entry.idle_generation == generation
            );
            if evict {
                remove_account_entry(&mut cache, &session_id, &account_id)
                    .and_then(|entry| entry.socket)
            } else {
                None
            }
        };
        if let Some(mut socket) = socket {
            close_silently(&mut socket, 1000, "idle_timeout").await;
        }
    });
}

// ---------------------------------------------------------------------------
// Continuation state (cache-resumption bookkeeping)
// ---------------------------------------------------------------------------

/// Reads a clone of the entry's continuation state.
pub fn continuation_for(
    session_id: &str,
    account_id: &str,
) -> Option<CachedWebSocketContinuationState> {
    cache()
        .get(session_id)
        .and_then(|account_entries| account_entries.get(account_id))
        .and_then(|entry| entry.continuation.clone())
}

/// Sets or clears the entry's continuation state (no-op when the entry is
/// gone, e.g. after `close_sessions`).
pub fn set_continuation(
    session_id: &str,
    account_id: &str,
    value: Option<CachedWebSocketContinuationState>,
) {
    if let Some(entry) = cache()
        .get_mut(session_id)
        .and_then(|account_entries| account_entries.get_mut(account_id))
    {
        entry.continuation = value;
    }
}

// ---------------------------------------------------------------------------
// Debug stats + SSE fallback
// ---------------------------------------------------------------------------

/// `OpenAICodexWebSocketDebugStats` (field names mirror upstream camelCase via
/// serde for any future RPC exposure).
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiCodexWebSocketDebugStats {
    pub requests: u64,
    pub connections_created: u64,
    pub connections_reused: u64,
    pub cached_context_requests: u64,
    pub store_true_requests: u64,
    pub full_context_requests: u64,
    pub delta_requests: u64,
    pub last_input_items: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_delta_input_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_previous_response_id: Option<String>,
    pub websocket_failures: u64,
    pub sse_fallbacks: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_fallback_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_web_socket_error: Option<String>,
}

static DEBUG_STATS: LazyLock<Mutex<HashMap<String, OpenAiCodexWebSocketDebugStats>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SSE_FALLBACK_SESSIONS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn stats_map() -> std::sync::MutexGuard<'static, HashMap<String, OpenAiCodexWebSocketDebugStats>> {
    DEBUG_STATS.lock().unwrap_or_else(|e| e.into_inner())
}

fn fallback_set() -> std::sync::MutexGuard<'static, HashSet<String>> {
    SSE_FALLBACK_SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Mutates the session's stats entry, creating it first
/// (`getOrCreateWebSocketDebugStats`).
pub fn with_debug_stats(
    session_id: &str,
    update: impl FnOnce(&mut OpenAiCodexWebSocketDebugStats),
) {
    let mut stats = stats_map();
    update(stats.entry(session_id.to_owned()).or_default());
}

/// `getOpenAICodexWebSocketDebugStats`.
pub fn get_openai_codex_websocket_debug_stats(
    session_id: &str,
) -> Option<OpenAiCodexWebSocketDebugStats> {
    stats_map().get(session_id).cloned()
}

/// `resetOpenAICodexWebSocketDebugStats`: clears stats and the SSE fallback
/// set (one session or all).
pub fn reset_openai_codex_websocket_debug_stats(session_id: Option<&str>) {
    match session_id {
        Some(session_id) => {
            stats_map().remove(session_id);
            fallback_set().remove(session_id);
        }
        None => {
            stats_map().clear();
            fallback_set().clear();
        }
    }
}

/// `isWebSocketSseFallbackActive`.
pub fn is_sse_fallback_active(session_id: Option<&str>) -> bool {
    session_id.is_some_and(|session_id| fallback_set().contains(session_id))
}

/// `recordWebSocketSseFallback`.
pub fn record_websocket_sse_fallback(session_id: Option<&str>) {
    let Some(session_id) = session_id else { return };
    let active = is_sse_fallback_active(Some(session_id));
    with_debug_stats(session_id, |stats| {
        stats.sse_fallbacks += 1;
        stats.websocket_fallback_active = Some(active);
    });
}

/// `recordWebSocketFailure`: marks the session as permanently fallen back to
/// SSE and records the failure.
pub fn record_websocket_failure(session_id: Option<&str>, error: &CodexError) {
    let Some(session_id) = session_id else { return };
    fallback_set().insert(session_id.to_owned());
    let message = error.message();
    with_debug_stats(session_id, |stats| {
        stats.websocket_failures += 1;
        stats.last_web_socket_error = Some(message);
        stats.websocket_fallback_active = Some(true);
    });
}

// ---------------------------------------------------------------------------
// Session resource cleanup
// ---------------------------------------------------------------------------

/// `closeOpenAICodexWebSocketSessions` (async form): evicts cached
/// connections and closes their sockets with code 1000 (`debug_close`).
pub async fn close_openai_codex_web_socket_sessions(session_id: Option<&str>) {
    let sockets: Vec<WsStream> = {
        let mut cache = cache();
        match session_id {
            Some(session_id) => cache
                .remove(session_id)
                .into_iter()
                .flat_map(|account_entries| account_entries.into_values())
                .filter_map(|entry| entry.socket)
                .collect(),
            None => cache
                .drain()
                .flat_map(|(_, account_entries)| account_entries.into_values())
                .filter_map(|entry| entry.socket)
                .collect(),
        }
    };
    for mut socket in sockets {
        close_silently(&mut socket, 1000, "debug_close").await;
    }
}

fn close_sessions_sync(session_id: Option<&str>) {
    // Best-effort eviction; sockets whose close frame cannot be sent without a
    // runtime are dropped (closing the TCP connection) — acceptable for a
    // cleanup path.
    let sockets: Vec<WsStream> = {
        let mut cache = cache();
        match session_id {
            Some(session_id) => cache
                .remove(session_id)
                .into_iter()
                .flat_map(|account_entries| account_entries.into_values())
                .filter_map(|entry| entry.socket)
                .collect(),
            None => cache
                .drain()
                .flat_map(|(_, account_entries)| account_entries.into_values())
                .filter_map(|entry| entry.socket)
                .collect(),
        }
    };
    if sockets.is_empty() {
        return;
    }
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        for mut socket in sockets {
            handle.spawn(async move {
                close_silently(&mut socket, 1000, "debug_close").await;
            });
        }
    }
}

fn ensure_cleanup_registered() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        register_session_resource_cleanup(close_sessions_sync);
    });
}
