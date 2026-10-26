//! Port of `packages/ai/src/utils/rpi-user-agent.ts (upstream pi-user-agent.ts)` @ pi `9841914`
//! (v0.85.0+, `87af49dec`: pi UA as the first header-merge source for the
//! API adapters).
//!
//! Intentional differences: the upstream `"pi (browser)"` branch (no Node
//! builtins) has no rpi counterpart — rpi is always a native binary with
//! `uname` access — so [`get_rpi_user_agent`] always renders the
//! platform/release/arch form. The platform/release/arch reads consolidate
//! the local implementation previously inlined in
//! `api/openai_codex_responses.rs` (`96317e50b` origin).

/// `getPiUserAgent()` (upstream) — rpi renders `rpi (<platform> <release>; <arch>)`.
///
/// `platform` mirrors Node's `os.platform()` values via `std::env::consts::OS`
/// (`linux`/`macos`/`windows`; Node calls macOS `darwin`, but rpi emits its
/// own UA namespace — the codex precedent, unchanged by this consolidation).
pub fn get_rpi_user_agent() -> String {
    format!(
        "rpi ({} {}; {})",
        std::env::consts::OS,
        os_release(),
        std::env::consts::ARCH
    )
}

#[cfg(unix)]
fn os_release() -> String {
    // SAFETY: `utsname` is a plain C struct; zeroed is a valid initial state
    // and `uname` writes NUL-terminated arrays on success.
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            return std::ffi::CStr::from_ptr(uts.release.as_ptr())
                .to_string_lossy()
                .into_owned();
        }
        String::new()
    }
}

#[cfg(not(unix))]
fn os_release() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rpi (<platform> <release>; <arch>)` — three-part shape with the
    /// literal `pi` prefix (upstream rpi-user-agent.ts (upstream pi-user-agent.ts):18).
    #[test]
    fn user_agent_shape() {
        let ua = get_rpi_user_agent();
        assert!(ua.starts_with("rpi ("), "prefix: {ua}");
        assert!(ua.ends_with(')'));
        let inner = ua.trim_start_matches("rpi (").trim_end_matches(')');
        let parts: Vec<&str> = inner.split(';').map(str::trim).collect();
        assert_eq!(parts.len(), 2, "platform-release; arch: {ua}");
        assert!(!parts[1].is_empty(), "arch: {ua}");
    }
}
