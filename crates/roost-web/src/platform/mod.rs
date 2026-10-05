//! The browser platform: the capabilities the client core cannot own.
//!
//! `roost_client_core` names no DOM type and takes no async runtime, so every
//! browser capability reaches it one of two ways — as a typed `Effect` the host
//! performs (`rpc`, `sync_socket`, `carrier`) or as one of the two
//! synchronous traits it calls into (`Clock`, `KeyValueStore`). This module is
//! the host side of that seam and nothing else: no store rule, no session
//! projection, no rendering.
//!
//! Each submodule is a whole capability rather than a wrapper over one call, so
//! a later slice that needs a fourth behaviour in one of them extends the
//! capability rather than wrapping it again. `carrier` owns the identity half of
//! a direct carrier — the connection id, and the grant a `DirectCarrier` is
//! assembled from — while the transports that OPEN one are the loopback and
//! WebRTC carriers' own slices. `fragment_credential` runs before the router
//! mounts, because a bearer left in the address bar reaches every request the
//! document makes afterwards as a `Referer`. `tab_id` claims the document's
//! tab id before the pump's first transport. `secrets` is the strict half of
//! `tab_id`'s randomness: a document with no `crypto.getRandomValues` refuses a
//! ceremony that needs a secret rather than filling one from `Math.random`,
//! which is guessable from four earlier draws.

#[cfg(target_arch = "wasm32")]
pub mod attachments;
pub mod browser;
pub mod browser_platform;
pub mod carrier;
pub mod carriers;
pub mod clock;
pub mod connect;
pub mod device_key;
pub mod door_probe;
pub mod file_save;
pub mod fragment_credential;
pub mod location;
#[cfg(target_arch = "wasm32")]
pub mod loopback;
pub mod network;
pub mod peer;
pub mod rpc;
pub mod secrets;
pub mod self_label;
pub mod storage;
pub mod sync_socket;
#[cfg(target_arch = "wasm32")]
pub mod tab_id;
pub mod terminal_view_id;
pub mod visibility;
pub mod worker_paths;

pub use carrier::{CarrierIdentity, mint_connection_id};
pub use clock::BrowserClock;
pub use fragment_credential::{FragmentCredential, capture_and_scrub, parse_fragment_credential};
pub use rpc::{ConnectTransport, FetchConnectTransport, UnaryRequest, UnaryResponse};
pub use storage::{LocalStorageKeyValueStore, SessionStorageKeyValueStore};
pub use sync_socket::{SyncSocket, SyncSocketHandle, SyncSocketMessage, WebSocketSyncSocket};
pub use worker_paths::BrowserWorkerPaths;
