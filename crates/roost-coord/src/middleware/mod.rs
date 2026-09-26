//! The request-time middleware: the Host/Origin admission layer, the security
//! and CORS headers, the caller's origin profile, the rate limiter, and the
//! per-request audit hook.
//!
//! One field on `CoordServices` for the limiter, because a bucket set that is
//! per-listener rather than per-process is a limit every socket can spend
//! again on the next connection.

pub mod rate_limit;
