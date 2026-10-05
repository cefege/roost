//! Free loopback ports for a bench stack. The installed workers hold the
//! default ports on a fleet machine, so a bench stack never uses them.

use std::net::TcpListener;

use crate::error::BenchError;

/// A port the kernel just handed out on 127.0.0.1. The listener is dropped
/// before the child binds; the window is the same one the deleted smoke
/// harness accepted.
pub fn free_loopback_port() -> Result<u16, BenchError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| BenchError::io("reserving a loopback port", error))?;
    let port = listener
        .local_addr()
        .map_err(|error| BenchError::io("reading a reserved port", error))?
        .port();
    Ok(port)
}
