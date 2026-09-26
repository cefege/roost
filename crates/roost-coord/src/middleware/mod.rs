//! The request-time middleware: the Host/Origin admission layer, the security
//! and CORS headers, the caller's origin profile, the rate limiter, and the
//! per-request audit hook.
//!
//! One field on `CoordServices` for the limiter, because a bucket set that is
//! per-listener rather than per-process is a limit every socket can spend
//! again on the next connection.
//!
//! The request-time modules are declared here rather than by the slices that
//! fill them, so the middleware stack and the audit hook can be built in
//! parallel without either editing this file. `audit` owns the durable write
//! and `audit_layer` owns the mount that calls it; `rate_limit` owns the
//! bucket policy and `rate_limit_layer` owns the mount that calls it. The
//! pairs are separate because a policy is callable from anywhere and a mount is
//! specific to one surface: folding them together would put a durable row
//! write, or a budget spend, inside the CORS layer.

pub mod admission_layer;
pub mod audit;
pub mod audit_layer;
pub mod caller_origin;
pub mod rate_limit;
pub mod rate_limit_layer;
pub mod security;
