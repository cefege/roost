//! The Deepgram socket: one recording's lifecycle over one WebSocket.
//!
//! The coordinator stores the key and hands it to an authenticated browser; the
//! browser then speaks to Deepgram directly, so everything that can go wrong
//! here is the credential, the socket or silence — and each has its own caption
//! in `super::handshake`. The decision half of this protocol (the URL, the close
//! codes, the failure vocabulary) is pure and tested there; this is the half that
//! opens sockets and keeps timers.
//! Ports `apps/web/src/voice/deepgramDictation.ts`.

use std::cell::{RefCell, RefMut};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};

use js_sys::{Array, ArrayBuffer, Uint8Array};
use wasm_bindgen::JsValue;
use web_sys::{BinaryType, WebSocket};

use super::audio_capture::{ChunkSink, OpenListener};
use super::deepgram_frames::Frame;
use super::deepgram_session::Session;
use super::engine_events::bind_socket;

pub use super::engine_events::{EngineEvent, EngineSink};
use super::handshake::{captions, credential_rejected};
use super::keyterms::Keyterm;
use super::state::RunFence;

/// How long a keepalive frame waits between sends. Deepgram closes an idle
/// stream well before a minute.
pub const KEEPALIVE_MS: i32 = 5_000;

/// How long a reconnect waits: long enough not to hammer a failing endpoint,
/// short enough that the operator does not notice the gap.
pub const RECONNECT_DELAY_MS: i32 = 500;

/// How long a stopped stream is given to deliver its final result.
pub const FINALIZE_WAIT_MS: i32 = 3_000;

/// How long a graph may deliver nothing before it counts as silent.
pub const SILENCE_GRACE_MS: i32 = 2_500;

/// How long the device has to attach before the tap counts as stalled.
pub const START_GRACE_MS: i32 = 9_000;

/// The keyterms one socket open will carry. Asked once per connection, because
/// Deepgram fixes the list when the socket opens.
pub type KeytermSource = Rc<dyn Fn() -> Vec<Keyterm>>;

/// One credential request. A Rust future rather than a JS promise: the call is
/// the client core's own, and the browser is not in that path.
pub type GrantCall = Pin<Box<dyn Future<Output = Result<String, String>>>>;

/// The stored key, asked for at the start of every connection.
pub type GrantSource = Rc<dyn Fn() -> GrantCall>;

/// One recording's Deepgram engine.
pub struct Deepgram {
    language: String,
    grant: GrantSource,
    keyterms: KeytermSource,
    pub(super) sink: EngineSink,
    pub(super) session: RefCell<Session>,
    pub(super) keepalive: RefCell<Option<i32>>,
    /// Which recording this engine is on, and whether that recording has ended.
    ///
    /// Every continuation this engine arms — a socket callback, a deadline, a
    /// key handoff still in flight — outlives the tap that armed it, so the
    /// fence is the one thing that decides whether it may still speak. It lives
    /// outside [`Session`] because that is wiped per recording, and a wiped
    /// counter would let the first run answer for the second.
    fence: RefCell<RunFence>,
    /// The timers and socket callbacks below outlive the call that armed them,
    /// so each one re-enters the engine through this rather than through a
    /// borrow it cannot hold across an await.
    pub(super) owner: RefCell<Option<Weak<Deepgram>>>,
    /// What the capture host answers when its device open settles. Built here
    /// so every open this engine asks for — the first and a repair — reports to
    /// the same place.
    open_listener: OpenListener,
}

impl std::fmt::Debug for Deepgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Deepgram")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl Deepgram {
    /// An engine that asks `grant` for the stored key each time it connects.
    ///
    /// The `Rc` is what the component holds; the engine re-enters itself through
    /// the weak handle set here, so an unmounted recording settles nothing.
    #[must_use]
    pub fn shared(
        language: String,
        grant: GrantSource,
        keyterms: KeytermSource,
        sink: EngineSink,
    ) -> Rc<Self> {
        let engine = Rc::new_cyclic(|weak| {
            let owner = weak.clone();
            Self {
                language,
                grant,
                keyterms,
                sink,
                session: RefCell::new(Session::default()),
                keepalive: RefCell::new(None),
                fence: RefCell::new(RunFence::default()),
                owner: RefCell::new(Some(owner.clone())),
                open_listener: Rc::new(move |failure: Option<String>| {
                    let Some(engine) = owner.upgrade() else {
                        return;
                    };
                    match failure {
                        None => engine.mic_attached(),
                        Some(caption) => engine.fail(&caption),
                    }
                }),
            }
        });
        engine
    }

    /// The verdict handler, cloned per open so a repair does not need the
    /// engine to be reachable from the capture host.
    pub(super) fn open_listener(&self) -> OpenListener {
        Rc::clone(&self.open_listener)
    }

    pub(super) fn weak(&self) -> Option<Weak<Deepgram>> {
        self.owner.borrow().clone()
    }

    pub(super) fn session(&self) -> RefMut<'_, Session> {
        self.session.borrow_mut()
    }

    /// The recording this engine is on, which a socket callback must match to
    /// be allowed to speak for it.
    pub(super) fn current_run(&self) -> u64 {
        self.fence.borrow().current()
    }

    /// Whether a continuation issued for `run` still belongs to this recording.
    pub(super) fn admits(&self, run: u64) -> bool {
        self.fence.borrow().admits(run)
    }

    /// Claim the one settle this recording gets, so the finalize answer and the
    /// deadline that waits for it cannot both report the same words.
    pub(super) fn claim_settle(&self) -> bool {
        self.fence.borrow_mut().claim_settle()
    }

    /// Open a recording, handing back the token every continuation it arms
    /// carries. What the previous recording heard is dropped here: it belongs
    /// to a draft that has already been read.
    pub(super) fn begin_recording(&self) -> u64 {
        let run = self.fence.borrow_mut().begin();
        *self.session() = Session::default();
        run
    }

    /// Open the device, then the socket.
    pub fn start(self: &Rc<Self>) {
        let run = self.begin_recording();
        super::audio_capture::start_capture(self.chunk_sink(), self.open_listener());
        self.connect(run);
        self.arm_start_grace(run);
        self.arm_silence_watch(run);
    }

    /// Queue linear16 for the socket, or for the buffer the socket will flush.
    pub(super) fn chunk_sink(self: &Rc<Self>) -> ChunkSink {
        let weak = Rc::downgrade(self);
        Rc::new(move |chunk: Vec<u8>| {
            if let Some(engine) = weak.upgrade() {
                engine.queue(chunk);
            }
        })
    }

    fn queue(&self, chunk: Vec<u8>) {
        let socket = {
            let mut session = self.session();
            let socket = session.socket.clone();
            let open = socket
                .as_ref()
                .is_some_and(|socket| socket.ready_state() == WebSocket::OPEN);
            if !open {
                session.prebuffer.push(chunk);
                return;
            }
            socket
        };
        if let Some(socket) = socket {
            let _ = socket.send_with_array_buffer(&audio_buffer(&chunk));
        }
    }

    /// Forget the transcript without touching the socket.
    pub fn reset(&self) {
        {
            let mut session = self.session();
            session.segments.clear();
            session.interim.clear();
        }
        (self.sink)(EngineEvent::Transcript {
            settled: String::new(),
            hypothesis: String::new(),
        });
    }

    /// Ask the coordinator for the key, then open the socket with it.
    ///
    /// The run is the one this connect was ISSUED for: the key handoff is a
    /// round trip, and the recording that asked for the key is usually over by
    /// the time it answers.
    pub(super) fn connect(self: &Rc<Self>, run: u64) {
        let engine = Rc::clone(self);
        wasm_bindgen_futures::spawn_local(async move {
            engine.open_socket(run).await;
        });
    }

    async fn open_socket(self: Rc<Self>, run: u64) {
        let token = match (self.grant)().await {
            Ok(token) => token,
            Err(error) => {
                if !self.admits(run) {
                    return;
                }
                tracing::warn!(target: "voice", stage = "grant", detail = %error, "voice.ws_failed");
                self.fail(captions::SERVICE_UNAVAILABLE);
                return;
            }
        };
        // A key that arrives after its recording ended opens nothing: the
        // socket it installed would replace the one the NEXT recording opened,
        // and every frame it carried would be read as this engine's.
        if !self.admits(run) {
            tracing::debug!(target = "voice", stage = "grant", "voice.connect_dropped");
            return;
        }
        if token.is_empty() {
            self.fail(captions::SERVICE_UNAVAILABLE);
            return;
        }
        let pairs: Vec<(String, String)> = (self.keyterms)()
            .into_iter()
            .map(|keyterm| (keyterm.term, keyterm.variant))
            .collect();
        let url = super::handshake::build_url(&self.language, &pairs);
        let protocols = Array::new();
        protocols.push(&JsValue::from_str("token"));
        protocols.push(&JsValue::from_str(&token));
        let Ok(socket) = WebSocket::new_with_str_sequence(&url, &protocols) else {
            self.fail(captions::SERVICE_UNAVAILABLE);
            return;
        };
        socket.set_binary_type(BinaryType::Arraybuffer);
        bind_socket(&socket, &self, run);
        self.session().socket = Some(socket);
    }

    /// One frame from Deepgram, read by the pure reader in
    /// `super::deepgram_frames` and folded into the recording it answers to.
    pub(super) fn inbound_frame(&self, run: u64, data: JsValue) {
        let Some(text) = data.as_string() else {
            tracing::warn!(target = "voice", "voice.frame_parse_failed");
            return;
        };
        self.fold(run, super::deepgram_frames::read(&text));
    }

    /// Fold one frame, if the socket it arrived on still speaks for this engine.
    ///
    /// A socket outlives the tap that opened it and the browser keeps
    /// delivering to it, so the last frame of one recording routinely lands
    /// after the next one has started. Dropping it is what keeps a word from
    /// the PREVIOUS conversation out of a draft the operator is reading.
    pub(super) fn fold(&self, run: u64, frame: Frame) {
        if !self.admits(run) {
            tracing::debug!(target = "voice", stage = "stale", "voice.frame_dropped");
            return;
        }
        match frame {
            Frame::Ignored => {}
            Frame::Rejected(detail) => {
                tracing::warn!(target = "voice", stage = "msg", detail, "voice.ws_failed");
                self.fail(&credential_rejected(&detail));
            }
            Frame::Transcript {
                transcript,
                is_final,
                from_finalize,
            } => {
                {
                    let mut session = self.session();
                    session.results += 1;
                    if is_final {
                        session.segments.push(transcript);
                        session.interim.clear();
                    } else {
                        session.interim = transcript;
                    }
                }
                {
                    let session = self.session.borrow();
                    (self.sink)(EngineEvent::Transcript {
                        settled: session.settled(),
                        hypothesis: session.interim.clone(),
                    });
                }
                if from_finalize {
                    self.settle();
                }
            }
        }
    }

    pub(super) fn opened(&self) {
        let buffered = std::mem::take(&mut self.session().prebuffer);
        self.session().socket_open = true;
        let socket = self.session.borrow().socket.clone();
        if let Some(socket) = socket {
            for chunk in buffered {
                let _ = socket.send_with_array_buffer(&audio_buffer(&chunk));
            }
        }
        self.arm_keepalive();
        self.announce_live();
    }

    /// The recording is genuinely live only once the device attached AND the
    /// socket opened. Neither alone promotes the UI out of `starting`.
    fn announce_live(&self) {
        let fire = {
            let mut session = self.session();
            if session.mic_attached && session.socket_open && !session.announced_live {
                session.announced_live = true;
                true
            } else {
                false
            }
        };
        if fire {
            (self.sink)(EngineEvent::Live);
        }
    }

    /// The device attached, which the capture host reports once per open.
    fn mic_attached(&self) {
        self.session().mic_attached = true;
        self.announce_live();
    }
}

/// One chunk of audio as the array buffer the socket is sent.
///
/// `WebSocket::send` also takes a typed-array view, and the bytes that cross
/// the wire are identical — but the streaming protocol names its audio
/// messages as an array buffer, and a view is a different message to a peer
/// that reads what it was handed rather than a slice of it.
fn audio_buffer(chunk: &[u8]) -> ArrayBuffer {
    Uint8Array::from(chunk).buffer()
}
