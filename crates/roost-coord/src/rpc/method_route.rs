//! Every `CoordinatorService` method, the domain that owns it, and what it requires
//! to be called.
//!
//! The 78 rows live in `method_route_rows`, one table per domain, so
//! this file is the vocabulary and the accessors rather than 700 lines of data.
//! `all_method_routes` concatenates them in the proto's declaration order, which is
//! the order `tests/method_route_coverage.rs` asserts.
//!
//! Owned by the coordinator's RPC layer. This table is the coverage guard for
//! the service block, and it is data rather than code so a test can assert it
//! against the proto's own service block.
//!
//! WHY A TABLE AND NOT A CONVENTION. In Rust a method the service impl omits is
//! a **compile error**, because the generated trait requires every method -- so
//! the guard that is still needed is different: it must catch a method in the
//! proto that no table row names, and a table row naming a method the proto
//! does not declare. Both directions are covered by
//! `tests/method_route_coverage.rs`, which parses
//! `protocol/proto/roost/v1/coordinator.proto`.
//!
//! WHY AUTH IS A COLUMN AND NOT A FUNCTION. The requirement is a per-method
//! fact that a reviewer reads in one place, and a function would hide it behind
//! a call. `Public` is not a weaker `Device`; it is a different answer, and
//! `MiscHealth` being public is what lets a load balancer probe it.

/// Who may call a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// No credential. Seven methods are in this class, and most are
    /// gated by a secret inside the request body instead
    /// (`docs/phase3-coord-contract.md` §4.5).
    Public,
    /// A browser or legacy browser key.
    Device,
    /// A browser key **and** a request that arrived on the host.
    ///
    /// **THE REQUIREMENT IS REAL AND THE GATE DOES NOT ENFORCE IT.**
    /// `principal_satisfies` answers `is_browser` for this variant and for
    /// [`AuthRequirement::Device`] alike, so a row carrying it records a
    /// restriction nothing checks. That is not a reason to stop using it — it is
    /// the reason the table records what a method requires and the gate is a
    /// separate question. **A row that under-claims is the same defect as one
    /// that over-claims**: narrowing `MiscDbExportUrl` to `Device` would have
    /// made the table agree with the gate by recording less than the method
    /// enforces.
    ///
    /// On-host is asserted in two shapes which must not be conflated — only the
    /// first is what this variant means:
    ///
    /// - **A credentialed device AND on-host.** One site, the DB export route
    ///   (`http/db_export.rs`). This is `DeviceOnHost`.
    /// - **On-host as an UNCREDENTIALED FALLBACK**, admitting a caller holding
    ///   no key at all: the pairing approver check and device revocation
    ///   (`auth/rpc_pairing.rs`, `auth/rpc_devices.rs`). This is NOT this
    ///   variant and the table has no way to say it; those rows record what
    ///   their handlers enforce, and the credential-less path is currently
    ///   unreachable because `AuthGate::admit` only inserts a `Caller` for a
    ///   request that carried one.
    ///
    /// Whoever implements the gate's half has to pick which shape each row
    /// means. `exactly_one_method_requires_on_host_and_the_gate_does_not_yet_
    /// enforce_it` in `tests/method_route_coverage.rs` holds both halves: the
    /// requirement set, and an assertion against the real gate that FAILS THE
    /// DAY the locality check is written.
    DeviceOnHost,
    /// A machine key.
    Worker,
    /// A browser key, and a worker restricted to its own open recovery.
    DeviceOrOwnWorkerRecovery,
    /// A browser key plus a per-tab or socket-generation fence.
    DevicePlusFence,
}

impl AuthRequirement {
    /// Whether a browser key satisfies this requirement, and so whether a
    /// caller who presented none should be told to present one.
    ///
    /// NOT the same question `service::permission_denied_for` asks, and the
    /// difference is the whole of it. There the principal is KNOWN and did not
    /// qualify, so the marker names the layer that refused. Here nothing
    /// resolved, so the marker names the credential that would have worked --
    /// and for `DeviceOrOwnWorkerRecovery` a browser key is one of the two, so
    /// it is a true answer, while `Worker` is excluded because telling a
    /// machine to present a device key names a credential it may not hold.
    ///
    /// The same answer reaches the caller: a browser that had never been paired
    /// and a worker whose key the coordinator does not know both receive
    /// `x-roost-auth-layer: device`.
    #[must_use]
    pub const fn admits_browser_key(self) -> bool {
        matches!(
            self,
            Self::Device
                | Self::DeviceOnHost
                | Self::DeviceOrOwnWorkerRecovery
                | Self::DevicePlusFence
        )
    }
}

/// One row: a method, its owning domain, and its requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodRoute {
    /// The proto method name, spelled exactly as the service block spells it.
    pub method: &'static str,
    /// The domain folder that owns it.
    pub domain: &'static str,
    /// What a caller must present.
    pub auth: AuthRequirement,
}

/// Every method in the `service CoordinatorService` block: 78 rows, in the
/// proto's declaration order.
///
/// A FUNCTION, not a `const`: Rust cannot concatenate slices in a `const`
/// context, and the two alternatives are both worse. A macro that emitted the
/// concatenation is the metaprogramming this repo's rules exist to prevent, and a
/// `static` initialised to an empty slice would make the coverage guard pass
/// vacuously -- the exact failure the guard exists to catch. The `OnceLock` is
/// the only lazily-built state in the crate and one function reaches it.
#[must_use]
pub fn all_method_routes() -> &'static [MethodRoute] {
    use std::sync::OnceLock;
    static JOINED: OnceLock<Vec<MethodRoute>> = OnceLock::new();
    JOINED.get_or_init(|| {
        super::method_route_rows::ALL_TABLES
            .iter()
            .flat_map(|table| table.iter().copied())
            .collect()
    })
}

/// The auth requirement recorded for a method, if the table has a row.
#[must_use]
pub fn auth_requirement(method: &str) -> Option<AuthRequirement> {
    all_method_routes()
        .iter()
        .find(|route| route.method == method)
        .map(|route| route.auth)
}
