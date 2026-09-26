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
//! document makes afterwards as a `Referer`.

pub mod carrier;
pub mod clock;
pub mod fragment_credential;
pub mod rpc;
pub mod storage;
pub mod sync_socket;

pub use carrier::{CarrierIdentity, mint_connection_id};
pub use clock::BrowserClock;
pub use fragment_credential::{FragmentCredential, capture_and_scrub, parse_fragment_credential};
pub use rpc::{ConnectTransport, FetchConnectTransport, UnaryRequest, UnaryResponse};
pub use storage::{LocalStorageKeyValueStore, SessionStorageKeyValueStore};
pub use sync_socket::{SyncSocket, SyncSocketHandle, SyncSocketMessage, WebSocketSyncSocket};
