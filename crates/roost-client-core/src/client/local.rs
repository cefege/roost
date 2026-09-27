//! Local-first transport: the loopback door a browser on the same machine opens,
//! and the outbound Sync the client writes when no direct carrier is ready.
//!
//! Ported from v2's `apps/web/src/client/carriers/attachment-loopback.ts`,
//! `localBootstrap.ts`, `localWorkerDiscovery.ts` and
//! `apps/web/src/store/transport/{local-terminal,local-terminal-grants,sync-outbound}.ts`.
//!
//! "Local-first" is an ORDER, not a preference: a session with live view demand
//! on a machine this browser can reach over loopback takes that door before the
//! coordinator's Sync carries its cells. Depends on `roost_protocol` and on
//! `crate::terminal`; it opens nothing, because a socket is an effect.
