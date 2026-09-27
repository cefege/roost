//! What this worker can say about WHICH machine it is on.
//!
//! The identity is normalised by the protocol's own normaliser, so a value that
//! passes here is a value the coordinator will accept, and three nulls are no
//! identity at all. The sources are fixtures rather than this machine's, because
//! the only way an `os-release` and a `sysctl` are both readable on a host that
//! has neither is to supply them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_host::HostPlatform;
use roost_worker::host::identity::{IdentitySources, collect_host_identity, os_release_value};

/// The identity is normalised by the PROTOCOL's own normaliser, so a value that
/// passes here is a value the coordinator will accept, and an identity of three
/// nulls is no identity at all.
#[test]
fn a_host_identity_is_collected_from_this_hosts_sources_or_is_none() {
    let sources = FixtureSources::new(
        "PRETTY_NAME=\"Fedora Linux 42 (Workstation Edition)\"\nNAME=Fedora\n".to_string(),
    );
    let linux = collect_host_identity(HostPlatform::Linux, &sources)
        .expect("a distribution is an identity");
    assert_eq!(
        linux.linux_distribution.as_deref(),
        Some("Fedora Linux 42 (Workstation Edition)")
    );
    assert!(linux.hardware_model.is_none());
    let apple = collect_host_identity(
        HostPlatform::MacOs,
        &FixtureSources {
            distribution: String::new(),
            chip: "Apple M3 Max".to_string(),
            model: "Mac16,6".to_string(),
        },
    )
    .expect("a chip is an identity");
    assert_eq!(apple.chip.as_deref(), Some("Apple M3 Max"));
    assert_eq!(apple.hardware_model.as_deref(), Some("Mac16,6"));
    // An Intel string in the brand field is not a chip name, and a value that
    // is nothing but whitespace is nothing at all.
    assert!(
        collect_host_identity(
            HostPlatform::MacOs,
            &FixtureSources {
                distribution: String::new(),
                chip: "Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz".to_string(),
                model: "   ".to_string(),
            },
        )
        .is_none(),
        "an unclassifiable identity was reported as one"
    );
    assert!(collect_host_identity(HostPlatform::Windows, &sources).is_none());
}

/// `os-release` quoting and escaping, which is where a distribution name picks
/// up a stray quote if it is read naively.
#[test]
fn an_os_release_value_is_unquoted_and_unescaped() {
    let source = "NAME=\"Fedora Linux\"\nPRETTY_NAME=\"Fedora Linux 42 (Workstation Edition)\"\nQUOTED=\"a\\\\b\\\"c\\$d\"\n";
    let value = |key: &str| os_release_value(source, key).as_deref();
    assert_eq!(
        value("PRETTY_NAME"),
        Some("Fedora Linux 42 (Workstation Edition)")
    );
    assert_eq!(value("QUOTED"), Some("a\\b\"c$d"));
    assert_eq!(value("MISSING"), None);
    // A key that is a PREFIX of another key must not match it.
    assert_eq!(value("NAME"), Some("Fedora Linux"));
}

/// Sources that answer from a fixture rather than from this machine, which is
/// the only way an `os-release` and a `sysctl` are readable on a host that has
/// neither.
struct FixtureSources {
    distribution: String,
    chip: String,
    model: String,
}

impl FixtureSources {
    fn new(distribution: String) -> Self {
        Self {
            distribution,
            chip: String::new(),
            model: String::new(),
        }
    }
}

impl IdentitySources for FixtureSources {
    fn sysctl(&self, name: &str) -> Option<String> {
        match name {
            "machdep.cpu.brand_string" if !self.chip.is_empty() => Some(self.chip.clone()),
            "hw.model" if !self.model.trim().is_empty() => Some(self.model.clone()),
            _ => None,
        }
    }

    fn os_release(&self) -> Option<String> {
        (!self.distribution.is_empty()).then(|| self.distribution.clone())
    }
}
