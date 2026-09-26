//! The request-time middleware: the Host/Origin admission layer, the security
//! and CORS headers, the caller's origin profile, the rate limiter, and the
//! per-request audit hook.
//!
//! One field on `CoordServices` for the limiter, because a bucket set that is
//! per-listener rather than per-process is a limit every socket can spend
//! again on the next connection.
//!
//! The four request-time modules are declared here rather than by the slices
//! that fill them, so the middleware stack and the audit hook can be built in
//! parallel without either editing this file.

pub mod admission_layer;
pub mod audit;
pub mod caller_origin;
pub mod rate_limit;
pub mod security;
