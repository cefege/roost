//! Talking to a coordinator as the harness: a device key seeded straight into
//! its database (`seed`), the JWT that key signs (`jwt`), and the handful of
//! RPCs a run needs (`rpc`). Works unchanged against the v2 and v3 coordinators.

mod jwt;
mod rpc;
mod seed;

pub use jwt::{BenchDevice, now_ms};
pub use rpc::CoordClient;
pub use seed::seed_device;
