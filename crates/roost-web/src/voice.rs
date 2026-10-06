//! Dictation: the browser half of voice input for the terminal composer.
//!
//! Ports `apps/web/src/voice/**` and `apps/web/src/components/MobileVoiceInput.tsx`.
//! The browser opens Deepgram's own socket with a key the coordinator stores, or
//! falls back to the browser's speech recognizer; there is no server-side
//! transcription, so every piece here is either a pure decision or a host.
//!
//! The split follows what each piece can be tested against. `engine`, `state`,
//! `send_gate`, `ownership`, `handshake`, `keyterms`, `keyterm_stopwords`, `pcm`
//! and `transcript` are pure: they decide the engine name in `data-engine`, the
//! value in `data-state`, what Send does mid-recording, who owns the
//! microphone, the handshake URL and every error caption, the vocabulary that
//! rides it, the linear16 bytes and how a provisional transcript is painted —
//! all without a microphone, a coordinator or a browser. `capabilities`,
//! `audio_capture`, `deepgram_engine`, `grant` and `web_speech` are the hosts
//! that ask the browser for those facts, open the sockets, and perform the
//! capture.

pub mod engine;
pub mod handshake;
pub mod keyterm_forms;
pub mod keyterm_lexicon;
pub mod keyterm_stopwords;
pub mod keyterms;
#[cfg(target_arch = "wasm32")]
pub mod lexicon_store;
pub mod ownership;
pub mod pcm;
pub mod send_gate;
pub mod shell_controls;
pub mod state;
pub mod transcript;

#[cfg(target_arch = "wasm32")]
pub mod audio_capture;
#[cfg(target_arch = "wasm32")]
pub mod audio_graph;
#[cfg(target_arch = "wasm32")]
pub mod capabilities;
#[cfg(target_arch = "wasm32")]
mod capture_facts;
#[cfg(target_arch = "wasm32")]
mod capture_open;
#[cfg(target_arch = "wasm32")]
mod deepgram_ending;
#[cfg(target_arch = "wasm32")]
pub mod deepgram_engine;
pub mod deepgram_frames;
#[cfg(target_arch = "wasm32")]
mod deepgram_session;
#[cfg(target_arch = "wasm32")]
pub mod engine_events;
#[cfg(target_arch = "wasm32")]
mod grant;
#[cfg(any(target_arch = "wasm32", test))]
pub mod open_device;
#[cfg(target_arch = "wasm32")]
pub mod web_speech;

#[cfg(target_arch = "wasm32")]
pub use audio_capture::warm_mic;
#[cfg(target_arch = "wasm32")]
pub use capabilities::probe_mic_permission;
#[cfg(target_arch = "wasm32")]
pub use deepgram_engine::{Deepgram, FINALIZE_WAIT_MS, GrantSource, KeytermSource};
#[cfg(target_arch = "wasm32")]
pub use engine_events::{EngineEvent, EngineSink};
#[cfg(target_arch = "wasm32")]
pub use grant::{grant_source, keyterm_source};
#[cfg(target_arch = "wasm32")]
pub use web_speech::WebSpeech;
