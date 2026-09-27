//! A direct terminal carrier: the election between loopback and a WebRTC peer,
//! the staged candidate, and the eight preconditions a promotion must satisfy.
//!
//! Ported from v2's `apps/web/src/store/transport/terminal-peer*.ts` and
//! `apps/web/src/client/carriers/`. The carrier's IDENTITY — the connection id
//! and the grant a `DirectCarrier` is assembled from — is the browser host's,
//! in `roost-web/src/platform/carrier.rs`; everything that decides which carrier
//! a session should ride is here.
//!
//! Depends on `roost_protocol` for the peer packet framing and on
//! `crate::terminal` for the replica and the route registry. It performs no I/O:
//! a transport is an effect, and its result arrives as a `ClientEvent`.
