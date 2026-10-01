//! The loopback direct carrier's socket: the browser's `WebSocket`, opened
//! against a worker on this machine's own loopback interface.
//!
//! Owned by `platform`, opened by the direct-carrier host once a coordinator
//! grant has been minted and a door has been discovered. Depends on the client
//! core's door contract (`client::local::door` — the path, the subprotocol, and
//! `admit_ready`, which judges what the worker sends back) and on nothing else:
//! this file moves bytes and reports what it saw, and the frame vocabulary above
//! it is `roost_client_core::client::carriers::wire`.
//!
//! Three behaviours are load-bearing and are reproduced here rather than left to
//! the caller:
//!
//! - the `Hello` goes out on OPEN, before anything else can be written, because
//!   `local_terminal.proto:18` makes it the first frame and a worker that reads
//!   a `Resync` first has already been told to refuse;
//! - `accepting` goes false BEFORE `close()`, so a frame the browser had
//!   already queued cannot be applied to a carrier the caller has replaced;
//! - a `send` that throws is reported as `false`, never propagated: a write that
//!   did not happen is a fact the state machine settles, not an exception raised
//!   in the middle of a keystroke.
//!
//! Ported from `apps/web/src/store/transport/local-terminal.ts:126-141` (`start`,
//! `sendFrame`, `finish`).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

use js_sys::{Array, Uint8Array};
use roost_client_core::client::local::door::{LOCAL_TERMINAL_SUBPROTOCOL, local_terminal_url};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{BinaryType, CloseEvent, ErrorEvent, MessageEvent, WebSocket};

/// The most frames held for the host to drain before the oldest is dropped.
///
/// Bounded for the reason `SYNC_SOCKET_INBOX_MAX` is: a browser socket delivers
/// into a callback rather than a stream, and an unbounded queue turns a worker
/// that has stopped reading into unbounded growth in the tab. A dropped cell
/// frame is recoverable by a resync; a tab that ran out of memory is not.
pub const LOOPBACK_SOCKET_INBOX_MAX: usize = 512;

/// One thing the loopback socket observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackMessage {
    /// The socket opened. The `Hello` has been written by the time this is
    /// queued, so a host may treat it as "the credential is spent".
    Open,
    /// One `LocalTerminalServerFrame`, undecoded.
    Binary(Vec<u8>),
    /// The socket closed. `1006` means the worker vanished without a close
    /// frame, which is a redial rather than a retirement.
    Closed {
        /// The close code.
        code: u16,
        /// The close reason, empty when the worker sent none.
        reason: String,
    },
}

/// Why a loopback socket could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopbackError {
    /// The discovered origin is not one this client will dial.
    UnusableOrigin {
        /// The origin that was refused.
        origin: String,
    },
    /// There is no `window`, so there is no `WebSocket` constructor to reach.
    NoWindow,
    /// The browser refused to open it.
    Rejected {
        /// What the browser reported.
        message: String,
    },
}

impl fmt::Display for LoopbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnusableOrigin { origin } => write!(
                formatter,
                "the worker door origin is not one this client dials: {origin}"
            ),
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

impl std::error::Error for LoopbackError {}

/// An open loopback socket, and the queue its callbacks fill.
#[derive(Debug)]
pub struct LoopbackHandle {
    connection_id: String,
    socket: WebSocket,
    inbox: Rc<RefCell<VecDeque<LoopbackMessage>>>,
    accepting: Rc<Cell<bool>>,
    /// Kept alive because dropping a `Closure` unhooks the listener it was
    /// registered with, which would leave a live socket with no callbacks.
    _listeners: Rc<LoopbackListeners>,
}

#[derive(Debug)]
struct LoopbackListeners {
    _on_open: Closure<dyn FnMut(JsValue)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(ErrorEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl LoopbackHandle {
    /// The host's own id for this connection.
    ///
    /// Minted at open rather than supplied, because a ROLLING worker has no
    /// identity of its own and the per-connection namespace is the only thing
    /// that can fence a redial's frames from the generation they replaced
    /// (`client::local::door::admit_ready`).
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// The frames and lifecycle events observed since the last drain.
    pub fn drain(&self) -> Vec<LoopbackMessage> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    /// Whether the browser has the socket open.
    pub fn is_open(&self) -> bool {
        self.socket.ready_state() == WebSocket::OPEN
    }

    /// Whether frames from this socket may still be applied.
    pub fn is_accepting(&self) -> bool {
        self.accepting.get()
    }

    /// Write one already-encoded `LocalTerminalClientFrame`. `false` means it
    /// did not go out, which is a fact the caller settles and not an error.
    pub fn send(&self, frame: &[u8]) -> bool {
        if !self.is_accepting() || !self.is_open() {
            return false;
        }
        self.socket.send_with_u8_array(frame).is_ok()
    }

    /// Stop accepting, then close.
    pub fn close(&self, code: u16, reason: &str) {
        self.accepting.set(false);
        let _ = self.socket.close_with_code_and_reason(code, reason);
    }
}

impl Drop for LoopbackHandle {
    fn drop(&mut self) {
        // A handle that goes out of scope must not leave a socket open with
        // callbacks pointing at a queue nothing will ever drain.
        self.accepting.set(false);
        let _ = self.socket.close_with_code_and_reason(
            roost_client_core::SYNC_GENERATION_RETIRED_CLOSE_CODE,
            "carrier retired",
        );
    }
}

/// Open the worker's local door for a terminal carrier.
///
/// `connection_id` is the CALLER's, not minted here, and that is the whole
/// reason this function is not self-contained: the `notify` callback fires
/// BEFORE this returns, and it has to find the socket it is reporting for. An id
/// minted at the end would leave the first callback with nothing to name.
///
/// `hello` is written on open and nowhere else: the credential is spent exactly
/// once per socket, and a second `Hello` on a live carrier is the thing
/// `client::local::door::SecretUseLedger` exists to prevent.
pub fn open_loopback_socket(
    connection_id: String,
    origin: &str,
    hello: &[u8],
    notify: Rc<dyn Fn()>,
) -> Result<LoopbackHandle, LoopbackError> {
    let url = local_terminal_url(origin).map_err(|_| LoopbackError::UnusableOrigin {
        origin: origin.to_owned(),
    })?;
    if web_sys::Url::new(&url).is_err() {
        return Err(LoopbackError::UnusableOrigin {
            origin: origin.to_owned(),
        });
    }
    if web_sys::window().is_none() {
        return Err(LoopbackError::NoWindow);
    }

    let protocols = Array::new();
    protocols.push(&JsValue::from_str(LOCAL_TERMINAL_SUBPROTOCOL));
    let socket = WebSocket::new_with_str_sequence(&url, &protocols).map_err(|error| {
        LoopbackError::Rejected {
            message: error.as_string().unwrap_or_else(|| format!("{error:?}")),
        }
    })?;
    socket.set_binary_type(BinaryType::Arraybuffer);

    let inbox: Rc<RefCell<VecDeque<LoopbackMessage>>> = Rc::new(RefCell::new(VecDeque::new()));
    let accepting = Rc::new(Cell::new(true));

    let on_open = {
        let inbox = Rc::clone(&inbox);
        let socket = socket.clone();
        let notify = Rc::clone(&notify);
        // OWNED, because the listener is a `'static` closure and the caller's
        // slice does not outlive this function. The credential is written once,
        // so the copy is bounded by one frame.
        let hello = Rc::new(hello.to_vec());
        Closure::wrap(Box::new(move |_event: JsValue| {
            // The credential is spent here and only here. A worker that reads
            // any other frame first has already been told to refuse, so the
            // order is the handshake rather than a convenience.
            let _ = socket.send_with_u8_array(hello.as_slice());
            push(&inbox, LoopbackMessage::Open);
            notify();
        }) as Box<dyn FnMut(JsValue)>)
    };
    let on_message = {
        let inbox = Rc::clone(&inbox);
        let notify = Rc::clone(&notify);
        Closure::wrap(Box::new(move |event: MessageEvent| {
            let Ok(data) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                // A text frame on a binary carrier is a worker that negotiated
                // something else. It is dropped rather than guessed at, and the
                // close that follows is what the host reacts to.
                return;
            };
            push(
                &inbox,
                LoopbackMessage::Binary(Uint8Array::new(&data).to_vec()),
            );
            notify();
        }) as Box<dyn FnMut(MessageEvent)>)
    };
    let on_error = {
        // Logs nothing and changes nothing: `onclose` always follows, and
        // reacting here as well is how a carrier gets retired twice.
        Closure::wrap(Box::new(move |_event: ErrorEvent| {}) as Box<dyn FnMut(ErrorEvent)>)
    };
    let on_close = {
        let inbox = Rc::clone(&inbox);
        Closure::wrap(Box::new(move |event: CloseEvent| {
            push(
                &inbox,
                LoopbackMessage::Closed {
                    code: event.code(),
                    reason: event.reason(),
                },
            );
            notify();
        }) as Box<dyn FnMut(CloseEvent)>)
    };

    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

    Ok(LoopbackHandle {
        connection_id,
        socket,
        inbox,
        accepting,
        _listeners: Rc::new(LoopbackListeners {
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        }),
    })
}

/// Append one observation, dropping the oldest past the bound.
fn push(inbox: &Rc<RefCell<VecDeque<LoopbackMessage>>>, message: LoopbackMessage) {
    let mut queue = inbox.borrow_mut();
    while queue.len() >= LOOPBACK_SOCKET_INBOX_MAX {
        queue.pop_front();
    }
    queue.push_back(message);
}
