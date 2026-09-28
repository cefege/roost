//! Every `CoordinatorService` method, the domain that owns it, what it requires
//! to be called, and whether this crate answers it yet.
//!
//! The 103 rows live in `method_route_rows`, one table per v2 domain folder, so
//! this file is the vocabulary and the accessors rather than 700 lines of data.
//! `all_method_routes` concatenates them in the proto's declaration order, which is
//! the order `tests/method_route_coverage.rs` asserts.
//!
//! Owned by the coordinator's RPC layer. This table is the coverage guard that
//! replaces v2's "a second `router.service()` call shadows the rest" hazard, and
//! it is data rather than code so a test can assert it against the proto's own
//! service block.
//!
//! WHY A TABLE AND NOT A CONVENTION. v2's guard was two tests that built the
//! real router and asserted each `requestPath` appeared exactly once in
//! `router.handlers` (`apps/coord/tests/agents/agent-status-handlers.test.ts:169-185`,
//! `apps/coord/tests/attachments/attachment-direct-handlers.test.ts:198-210`).
//! In Rust the shadowing failure is a **compile error**, because the generated
//! trait requires every method -- so the guard that is still needed is
//! different: it must catch a method in the proto that no table row names, and a
//! table row naming a method the proto does not declare. Both directions are
//! covered by `tests/method_route_coverage.rs`, which parses
//! `protocol/proto/roost/v1/coordinator.proto`.
//!
//! WHY AUTH IS A COLUMN AND NOT A FUNCTION. The requirement is a per-method
//! fact that a reviewer reads in one place, and a function would hide it behind
//! a call. `Public` is not a weaker `Device`; it is a different answer, and
//! `MiscHealth` being public is what lets a load balancer probe it.

/// Who may call a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// No credential. Six methods are in this class, and five of the six are
    /// gated by a secret inside the request body instead
    /// (`docs/phase3-coord-contract.md` §4.5).
    Public,
    /// A browser or legacy browser key.
    Device,
    /// A browser key **and** a request that arrived on the host.
    ///
    /// **THE REQUIREMENT IS REAL IN v2 AND THE GATE DOES NOT ENFORCE IT.**
    /// `principal_satisfies` answers `is_browser` for this variant and for
    /// [`AuthRequirement::Device`] alike, so a row carrying it records a
    /// restriction nothing checks. That is not a reason to stop using it — it is
    /// the reason the table records what v2 requires and the gate is a separate
    /// question. **A row that under-claims is the same defect as one that
    /// over-claims**: narrowing `MiscDbExportUrl` to `Device` would have made
    /// the table agree with the gate by recording less than v2 enforces.
    ///
    /// v2 asserts on-host in FIVE places, and they are TWO SHAPES which must
    /// not be conflated — only the first is what this variant means:
    ///
    /// - **UNGUARDED `assertOnHost(...)`** — a credentialed device AND
    ///   on-host. One site, `handlers-system.ts:115`. This is `DeviceOnHost`.
    /// - **`if (!caller) assertOnHost(...)`** — on-host as an UNCREDENTIALED
    ///   FALLBACK, admitting a caller holding no key at all. Four sites:
    ///   pairing ×3 and `handlers-devices.ts:72`. This is NOT this variant and
    ///   the table has no way to say it; those rows record what their handlers
    ///   enforce, and the credential-less path is currently unreachable because
    ///   `AuthGate::admit` only inserts a `Caller` for a request that carried
    ///   one.
    ///
    /// Whoever implements the gate's half has to pick which shape each row
    /// means. `the_on_host_requirement_matches_v2_and_the_gate_does_not_yet_
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
    /// No handler exists, so no requirement is enforced and none is claimed.
    ///
    /// Distinct from `Public`, which is a DELIBERATE absence of a credential.
    /// Recording a requirement for a method nothing answers would assert a
    /// guarantee the code does not provide -- and a reader auditing the table
    /// would reasonably believe one is enforced.
    Unwired,
}

/// What this crate does with a method today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStatus {
    /// Answered in this slice, with real behaviour.
    Implemented,
    /// The v2 coordinator answers it, and this crate's handler for it has not
    /// been ported. The reply is `Unimplemented` with the domain named, so the
    /// failure says who owns it rather than pretending the method is retired.
    ///
    /// A fact about ONE METHOD, not about its domain folder: five of the six
    /// folders that carry an awaiting row also carry implemented ones
    /// (`sessions` is 10 and 17), so a status keyed on the folder cannot exist
    /// and this comment once implied one. `tests/method_route_arm_pairing.rs`
    /// is what keeps the two hand-maintained statements of this — this status
    /// and `service_impl.rs`'s `delegated_*` arm — from drifting apart.
    AwaitingDomainPort,
    /// v2 does not answer it either: it is declared in the proto and routed to
    /// Connect's own unimplemented stub. Sixteen methods are in this class.
    UnwiredInV2,
}

/// One row: a method, its owner, its requirement, and its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodRoute {
    /// The proto method name, spelled exactly as the service block spells it.
    pub method: &'static str,
    /// The v2 domain folder that owns it, and the `make<Domain>Handlers` factory
    /// that spread it into the single service literal.
    pub domain: &'static str,
    /// What a caller must present.
    pub auth: AuthRequirement,
    /// What this crate answers today.
    pub status: PortStatus,
}

/// Every method in the `service CoordinatorService` block, 103 in total.
/// Every method in the `service CoordinatorService` block: 103 rows, in the
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

/// The domain that owns a method, if the table has a row.
#[must_use]
pub fn owning_domain(method: &str) -> Option<&'static str> {
    all_method_routes()
        .iter()
        .find(|route| route.method == method)
        .map(|route| route.domain)
}
