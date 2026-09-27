//! File attachments over a direct carrier: the chunked upload and download
//! loops, the SHA-256 dedup probe, and the peer negotiation that a transfer
//! needs before a byte moves.
//!
//! Ported from v2's `apps/web/src/client/attachments/`. The chunking PLAN and its
//! caps belong to `roost_protocol`; this owns when to ask for the next chunk,
//! what to do with a refused one, and the transfer's own progress.
//!
//! Depends on `roost_protocol` and on `crate::terminal::RouteRegistry`. It moves
//! no bytes: a chunk is an effect, and a transferred byte arrives as an event.
