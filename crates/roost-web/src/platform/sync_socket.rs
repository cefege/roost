//! The Sync socket over the browser's `WebSocket`.
//!
//! Owned by `platform`, opened in response to the client core's
//! `Effect::DialSync`, and depends on `web-sys` plus the handshake contract the
//! core states (`SyncDial`) — never on a store rule.
//!
//! The core owns the generation and the fencing decision; this owns the wire.
//! Three behaviours from v2 (`apps/web/src/store/sync.ts`) are load-bearing and
//! are reproduced here rather than left to the caller:
//!
//! - the credential rides a SUBPROTOCOL, never the query string, so it does not
//!   land in an access log;
//! - `accepting` goes false BEFORE `close()` on every intentional close, so a
//!   frame already in the browser's queue cannot be applied to a generation that
//!   has been retired;
//! - a `send` that throws is reported as `false`, never propagated: a write that
//!   did not happen is a fact the state machine settles, not an exception in the
//!   middle of an input handler.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

use js_sys::{Array, Uint8Array};
use roost_client_core::SyncDial;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{BinaryType, CloseEvent, ErrorEvent, MessageEvent, WebSocket};

/// The most frames held for the host to drain before the oldest is dropped.
///
/// A browser socket delivers into a callback, not a stream, so something has to
/// be a queue. This one is bounded because an unbounded queue turns a coordinator
/// that has stopped reading into unbounded memory growth in the tab, and a
/// dropped application frame is recoverable by a resync while the tab dying is
/// not.
pub const SYNC_SOCKET_INBOX_MAX: usize = 512;

/// One thing the socket observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSocketMessage {
    /// The handshake completed. NOT hydration readiness: the domains are not
    /// backed by a snapshot until `SyncSubscribed` and `DomainReady` arrive.
    Open,
    /// One binary frame, undecoded.
    Binary(Vec<u8>),
    /// The socket closed. `code` is the WebSocket close code, and `1006` means
    /// the peer vanished without a close frame.
    Closed {
        /// The close code.
        code: u16,
        /// The close reason, empty when the peer sent none.
        reason: String,
    },
}

/// Why a socket could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSocketError {
    /// The dial could not be rendered as a URL.
    MalformedUrl {
        /// The URL that would not parse.
        url: String,
    },
    /// There is no `window`, so there is no `WebSocket` constructor to reach.
    NoWindow,
    /// The browser refused to open it.
    Rejected {
        /// What the browser reported.
        message: String,
    },
}

impl fmt::Display for SyncSocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedUrl { url } => write!(formatter, "the sync URL does not parse: {url}"),
            Self::NoWindow => write!(
                formatter,
                "no window: this build is not running in a document"
            ),
            Self::Rejected { message } => {
                write!(formatter, "the browser refused the socket: {message}")
            }
        }
    }
}

impl std::error::Error for SyncSocketError {}

/// A transport that opens one Sync socket from a `SyncDial`.
pub trait SyncSocket {
    /// Open the socket the dial describes, with `bearer` as the credential
    /// subprotocol. A `None` bearer opens the socket unauthenticated, which the
    /// coordinator refuses — that is a real answer, not an error to hide.
    fn open(
        &self,
        dial: &SyncDial,
        bearer: Option<String>,
    ) -> Result<SyncSocketHandle, SyncSocketError>;
}

/// Renders a `SyncDial` as the URL the coordinator upgrades for.
pub fn sync_socket_url(base_url: &str, dial: &SyncDial) -> String {
    let scheme = if base_url.starts_with("https://") {
        "wss://"
    } else if base_url.starts_with("http://") {
        "ws://"
    } else {
        // A relative base means the app is served from its own origin, and the
        // page's own scheme is the only honest answer — a coordinator behind a
        // TLS-terminating proxy is reached as `wss` even when the override says
        // `http`.
        return format!(
            "{}?since={}&tab={}&flow={}&sync_v={}",
            dial.path,
            dial.since,
            encode_component(&dial.tab_id),
            dial.flow,
            dial.sync_v
        );
    };
    format!(
        "{scheme}{}{}?since={}&tab={}&flow={}&sync_v={}",
        base_url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(base_url)
            .trim_end_matches('/'),
        dial.path,
        dial.since,
        encode_component(&dial.tab_id),
        dial.flow,
        dial.sync_v
    )
}

/// The percent-encoding a query value needs, without pulling a URL crate in.
///
/// Only the tab id can carry anything outside the unreserved set, and a tab id
/// that reaches a coordinator unencoded is a coordinator that has to guess where
/// the value ended.
fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// An open socket, and the queue its callbacks fill.
#[derive(Debug)]
pub struct SyncSocketHandle {
    socket: WebSocket,
    inbox: Rc<RefCell<VecDeque<SyncSocketMessage>>>,
    accepting: Rc<Cell<bool>>,
    /// Kept alive because dropping a `Closure` unhooks the listener it was
    /// registered with, which would leave a live socket with no callbacks.
    _listeners: Rc<SyncSocketListeners>,
}

#[derive(Debug)]
struct SyncSocketListeners {
    _on_open: Closure<dyn FnMut(JsValue)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(ErrorEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl SyncSocketHandle {
    /// The frames and lifecycle events observed since the last drain.
    pub fn drain(&self) -> Vec<SyncSocketMessage> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    /// Whether the browser has the socket open.
    pub fn is_open(&self) -> bool {
        self.socket.ready_state() == WebSocket::OPEN
    }

    /// Whether frames from this socket may still be applied.
    ///
    /// Set false by `close` before the close frame is sent, which is what stops a
    /// frame already queued by the browser from being folded into a generation
    /// the caller has already retired.
    pub fn is_accepting(&self) -> bool {
        self.accepting.get()
    }

    /// Write one encoded frame. `false` means it did not go out.
    pub fn send(&self, frame: &[u8]) -> bool {
        if !self.is_accepting() || !self.is_open() {
            return false;
        }
        self.socket.send_with_u8_array(frame).is_ok()
    }

    /// Stop accepting, then close.
    ///
    /// The order is the contract: an intentional close is not a peer close, and
    /// a frame delivered between the two states would otherwise be applied to a
    /// socket the caller has already replaced.
    pub fn close(&self, code: u16, reason: &str) {
        self.accepting.set(false);
        let _ = self.socket.close_with_code_and_reason(code, reason);
    }
}

impl Drop for SyncSocketHandle {
    fn drop(&mut self) {
        // A handle that goes out of scope must not leave a socket open with
        // callbacks pointing at a queue nothing will ever drain.
        self.accepting.set(false);
        let _ = self.socket.close_with_code_and_reason(
            roost_client_core::SYNC_GENERATION_RETIRED_CLOSE_CODE,
            "generation retired",
        );
    }
}

/// The browser's `WebSocket`, wrapped as the Sync transport.
#[derive(Debug, Clone)]
pub struct WebSocketSyncSocket {
    base_url: String,
}

impl WebSocketSyncSocket {
    /// A transport against `base_url`'s origin.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl SyncSocket for WebSocketSyncSocket {
    fn open(
        &self,
        dial: &SyncDial,
        bearer: Option<String>,
    ) -> Result<SyncSocketHandle, SyncSocketError> {
        let url = sync_socket_url(&self.base_url, dial);
        // `WebSocket::new_with_str_sequence` panics on an unparseable URL, and a
        // panic inside an effect is a dead tab, so the URL is parsed first.
        if web_sys::Url::new(&url).is_err() {
            return Err(SyncSocketError::MalformedUrl { url });
        }
        if web_sys::window().is_none() {
            return Err(SyncSocketError::NoWindow);
        }

        let protocols = Array::new();
        protocols.push(&JsValue::from_str(dial.subprotocol));
        if let Some(bearer) = bearer {
            protocols.push(&JsValue::from_str(&bearer));
        }
        let socket = WebSocket::new_with_str_sequence(&url, &protocols).map_err(|error| {
            SyncSocketError::Rejected {
                message: error.as_string().unwrap_or_else(|| format!("{error:?}")),
            }
        })?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let inbox: Rc<RefCell<VecDeque<SyncSocketMessage>>> =
            Rc::new(RefCell::new(VecDeque::new()));
        let accepting = Rc::new(Cell::new(true));

        let on_open = {
            let inbox = Rc::clone(&inbox);
            Closure::wrap(Box::new(move |_event: JsValue| {
                push(&inbox, SyncSocketMessage::Open);
            }) as Box<dyn FnMut(JsValue)>)
        };
        let on_message = {
            let inbox = Rc::clone(&inbox);
            Closure::wrap(Box::new(move |event: MessageEvent| {
                let Ok(data) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    // A text frame on a binary socket is a coordinator that
                    // negotiated something else. It is dropped rather than
                    // guessed at, and the close that follows is what the host
                    // reacts to.
                    return;
                };
                push(
                    &inbox,
                    SyncSocketMessage::Binary(Uint8Array::new(&data).to_vec()),
                );
            }) as Box<dyn FnMut(MessageEvent)>)
        };
        let on_error = {
            // v2 logs and changes nothing: `onclose` always follows, and acting
            // here as well is how a socket gets retired twice.
            Closure::wrap(Box::new(move |_event: ErrorEvent| {}) as Box<dyn FnMut(ErrorEvent)>)
        };
        let on_close = {
            let inbox = Rc::clone(&inbox);
            Closure::wrap(Box::new(move |event: CloseEvent| {
                push(
                    &inbox,
                    SyncSocketMessage::Closed {
                        code: event.code(),
                        reason: event.reason(),
                    },
                );
            }) as Box<dyn FnMut(CloseEvent)>)
        };

        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(SyncSocketHandle {
            socket,
            inbox,
            accepting,
            _listeners: Rc::new(SyncSocketListeners {
                _on_open: on_open,
                _on_message: on_message,
                _on_error: on_error,
                _on_close: on_close,
            }),
        })
    }
}

/// Append one observation, dropping the oldest past the bound.
fn push(inbox: &Rc<RefCell<VecDeque<SyncSocketMessage>>>, message: SyncSocketMessage) {
    let mut queue = inbox.borrow_mut();
    while queue.len() >= SYNC_SOCKET_INBOX_MAX {
        queue.pop_front();
    }
    queue.push_back(message);
}
