//! The loopback door a browser on the worker's own machine talks to, and its
//! origin allowlist.
//!
//! No imports, deliberately: the browser bundle links this crate, and a
//! component that pulled in path or validation code to learn a port number
//! would ship that code to every device.

/// The bind an unset `ROOST_WORKER_LOCAL_UI_BIND` resolves to.
///
/// Loopback only. This door upgrades terminal sockets for the PTYs on the
/// worker's own machine, so any non-loopback interface would hand them to the
/// network. v3 uses 4114 so a v2 worker on 4104 and a v3 worker can both run
/// on one machine during the port.
pub const DEFAULT_WORKER_LOCAL_UI_BIND: &str = "127.0.0.1:4114";

/// The origin a browser reaches the door at.
pub const DEFAULT_WORKER_LOCAL_UI_ORIGIN: &str = "http://127.0.0.1:4114";

/// The environment variable that overrides the bind.
pub const WORKER_LOCAL_UI_BIND_ENV: &str = "ROOST_WORKER_LOCAL_UI_BIND";

/// True when `origin` is the default door origin, which the coordinator
/// pre-allowlists for CORS, the Sync WebSocket, and the SPA's `connect-src`.
///
/// An operator who moves the port must both allowlist the new origin on the
/// coordinator and set the browser-side override the SPA reads, so this check
/// names only the default rather than trying to parse and trust any origin.
pub fn is_default_door_origin(origin: &str) -> bool {
    origin == DEFAULT_WORKER_LOCAL_UI_ORIGIN
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_WORKER_LOCAL_UI_BIND, DEFAULT_WORKER_LOCAL_UI_ORIGIN, is_default_door_origin,
    };

    #[test]
    fn the_door_is_loopback_only() {
        assert!(DEFAULT_WORKER_LOCAL_UI_BIND.starts_with("127.0.0.1:"));
        assert!(DEFAULT_WORKER_LOCAL_UI_ORIGIN.starts_with("http://127.0.0.1:"));
    }

    #[test]
    fn only_the_default_origin_is_allowlisted() {
        assert!(is_default_door_origin(DEFAULT_WORKER_LOCAL_UI_ORIGIN));
        assert!(!is_default_door_origin("http://127.0.0.1:4104"));
        assert!(!is_default_door_origin("https://evil.example"));
    }
}
