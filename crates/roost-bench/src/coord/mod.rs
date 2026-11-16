//! Talking to a coordinator as the harness: a device key seeded straight into
//! its database (`seed`), the JWT that key signs (`jwt`), and the handful of
//! RPCs a run needs (`rpc`).

mod jwt;
mod rpc;
mod seed;

pub use jwt::{BenchDevice, now_ms};
pub use rpc::CoordClient;
pub use seed::seed_device;
