//! Exercises the real macOS memory probe under a launch-service PATH.
//! A child test process isolates its environment from the test runner.
//! The OS byte count must survive even when sysctl is absent from PATH.

use std::process::Command;

use super::{HostPlatform, host_total_memory_bytes};

#[test]
fn macos_memory_probe_works_with_a_minimal_service_path() -> Result<(), Box<dyn std::error::Error>>
{
    const EXPECTED_MEMORY: &str = "ROOST_TEST_EXPECTED_MACOS_HOST_MEMORY";
    if let Ok(expected) = std::env::var(EXPECTED_MEMORY) {
        assert_eq!(
            host_total_memory_bytes(HostPlatform::MacOs),
            expected.parse::<u64>()?
        );
        return Ok(());
    }

    let probe = Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()?;
    if !probe.status.success() {
        return Err(std::io::Error::other(format!("sysctl failed: {}", probe.status)).into());
    }
    let expected = String::from_utf8(probe.stdout)?.trim().parse::<u64>()?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "host_memory::macos_tests::macos_memory_probe_works_with_a_minimal_service_path",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env(EXPECTED_MEMORY, expected.to_string())
        .output()?;
    assert!(
        child.status.success(),
        "minimal-PATH memory probe failed:\n{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr),
    );
    Ok(())
}
