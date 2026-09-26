//! Per-domain hydration: the retained snapshot, the subscription, and the
//! `domain_ready` barrier that closes the gap between them.
//!
//! A domain's retained snapshot must precede its live frames, and only
//! `domain_ready` admits live application traffic for it. The terminal domain
//! additionally requires the one-time `SessionsList` snapshot token, because
//! terminal hydration without it would apply live frames on top of a baseline
//! the client never received.
//!
//! Contract: `protocol/spec/sync.md:28,50`. v2: `apps/web/src/store/sync-domain-state.ts`
//! and `apps/web/src/store/sync-inbound.ts:91-190`.

use crate::sync::inbound::SyncFrame;
use crate::sync::link::{SyncDomain, SyncState};

/// One domain's generation and hydration state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainToken {
    /// The coordinator's generation for this domain on this socket.
    pub generation: u64,
    /// The client asked for this domain, or the coordinator announced it as
    /// already subscribed.
    pub subscribed: bool,
    /// `domain_ready` arrived, so the snapshot/live gap is closed and live
    /// application traffic for this domain may be applied.
    pub ready: bool,
    /// The one-time snapshot token, when the domain issued one.
    pub snapshot_token: Option<String>,
}

impl SyncState {
    /// Record the coordinator's per-domain generations from `SyncSubscribed`.
    pub fn install_subscribed(
        &mut self,
        generation: u64,
        entries: &[(SyncDomain, u64, bool)],
    ) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        for (domain, domain_generation, subscribed) in entries {
            self.domains.insert(
                *domain,
                DomainToken {
                    generation: *domain_generation,
                    subscribed: *subscribed,
                    ready: false,
                    snapshot_token: None,
                },
            );
        }
        true
    }

    /// Record a domain reset, or the first announcement for a domain this
    /// socket has not seen.
    ///
    /// A reset drops readiness and the snapshot token: the retained snapshot is
    /// gone, so live traffic for that domain is not admissible yet. That is the
    /// difference between a reset and a re-subscribe, and getting it wrong is
    /// how a client paints a grid on top of a session list it never received.
    pub fn reset_domain(
        &mut self,
        generation: u64,
        domain: SyncDomain,
        domain_generation: u64,
    ) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.domains.insert(
            domain,
            DomainToken {
                generation: domain_generation,
                subscribed: true,
                ready: false,
                snapshot_token: None,
            },
        );
        true
    }

    /// The one-time snapshot token for a domain, issued by a bootstrap RPC.
    ///
    /// Recorded against the domain's CURRENT generation. A token issued for an
    /// older generation is not a token for this one, so it is refused rather
    /// than stored and later matched against a `domain_ready` that belongs to a
    /// different snapshot.
    pub fn issue_snapshot_token(
        &mut self,
        generation: u64,
        domain: SyncDomain,
        token: impl Into<String>,
    ) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        let Some(entry) = self.domains.get_mut(&domain) else {
            return false;
        };
        entry.snapshot_token = Some(token.into());
        true
    }

    /// Whether a domain may accept live application traffic.
    pub fn domain_is_ready(&self, domain: SyncDomain) -> bool {
        self.domains.get(&domain).is_some_and(|entry| entry.ready)
    }

    /// The domain's current generation, for correlating a result.
    pub fn domain_generation(&self, domain: SyncDomain) -> Option<u64> {
        self.domains.get(&domain).map(|entry| entry.generation)
    }

    /// Close a domain's snapshot/live gap, or refuse to.
    ///
    /// The terminal domain needs its one-time snapshot token; a `domain_ready`
    /// without one means live traffic would be applied on top of a snapshot the
    /// client never received, so the domain is reset instead with the
    /// coordinator's own reason string, `snapshot_token_invalid`
    /// (`protocol/spec/sync.md:50`).
    pub fn mark_domain_ready(
        &mut self,
        generation: u64,
        domain: SyncDomain,
        snapshot_token: Option<&str>,
    ) -> Result<(), &'static str> {
        if !self.accepts(generation) {
            return Err("socket generation is not current");
        }
        let Some(entry) = self.domains.get_mut(&domain) else {
            return Err("domain was never announced");
        };
        if domain == SyncDomain::Terminal
            && !matches!(
                (entry.snapshot_token.as_deref(), snapshot_token),
                (Some(installed), Some(presented)) if installed == presented
            )
        {
            return Err("snapshot_token_invalid");
        }
        entry.ready = true;
        Ok(())
    }

    /// Whether a frame may be applied to a store that is not hydrated yet.
    ///
    /// v2's `_consumeSyncFrame` refuses an application frame that arrives before
    /// `SyncSubscribed` (`apps/web/src/store/sync-inbound.ts:54-59`). This is
    /// that gate: a live-traffic frame with no subscription behind it is a
    /// protocol violation, not a hydration race, and applying it would fold an
    /// event into a store that has no sessions to fold it onto.
    pub fn may_apply(&self, frame: &SyncFrame) -> bool {
        match frame {
            // The announcement and a reset are what CREATE the subscription
            // state, so they are the two frames that are always admissible.
            SyncFrame::Subscribed { .. } | SyncFrame::DomainReset { .. } => true,
            SyncFrame::DomainReady { domain, .. } => self.domain_generation(*domain).is_some(),
            _ => self
                .domains
                .values()
                .any(|entry| entry.subscribed && entry.ready),
        }
    }

    /// The domain a frame belongs to. Every non-control frame names exactly one.
    pub fn frame_domain(&self, frame: &SyncFrame) -> SyncDomain {
        match frame {
            SyncFrame::DomainReady { domain, .. } | SyncFrame::DomainReset { domain, .. } => {
                *domain
            }
            SyncFrame::CellGrid { .. } | SyncFrame::CellGridChunk { .. } => SyncDomain::Terminal,
            // Session, workspace, task and audit traffic all ride the domains
            // the coordinator announced; the specific one is the host's
            // concern, and only the terminal domain has a fence here.
            _ => SyncDomain::Workers,
        }
    }
}
