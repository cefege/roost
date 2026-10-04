//! The loop OVER dials: mint a credential, open a socket, serve it, and decide
//! what the ending means. Owned by [`LinkLoop::run`], which is the only caller.
//! Depends on [`crate::backoff`]'s ladder, [`crate::link_dial`]'s socket, and
//! [`crate::runtime::link_serve`] for one dial's life. Ports the dial/cleanup
//! half of v2 `apps/worker/src/transport/coord-link.ts` (the connection fence
//! and `onDetach`) and `coord-link-outbox.ts` `detachSocket`.
//!
//! It is its own file because the rule that decides everything here is the one
//! at the bottom of the loop: **a dropped link is a reconnect, never a stop.**
//! The keeper holds this machine's PTYs and outlives this process on purpose, so
//! a worker that gave up on a coordinator would take the local door — and every
//! browser on this machine — down with it. A reader looking for "what ends a
//! worker" should find that in one place rather than interleaved with the
//! barrier's rules.

use std::time::Instant;

use crate::link_dial::dial;
use crate::outbox::Lane;

use super::{DIAL_TIMEOUT, LinkLoop, report_escalation};
use crate::runtime::stop::{LinkEndOutcome, StopReason, StopSignal, verdict_for_link_end};

impl LinkLoop {
    /// Run until the process is asked to stop.
    ///
    /// Returns only for a stop. Every other ending of a link is a reconnect.
    pub async fn run(mut self, mut stop: StopSignal) -> StopReason {
        if !self.snapshot.is_active() {
            tracing::error!(
                "no snapshot provider is installed, so the link's barrier will never be released \
                 past the snapshot stage and this worker will carry no live traffic"
            );
        }
        loop {
            if let Some(reason) = stop.reason() {
                return self.dispose_replay(reason);
            }
            let attempt = self.policy.begin_dial();
            tracing::info!(attempt, coordinator = %self.endpoint.url(), "dialling the coordinator");
            let credential = match self.credential.mint() {
                Ok(credential) => credential,
                Err(error) => {
                    self.report_dial_failure(attempt, &error.to_string());
                    self.wait_before_next_dial(&mut stop).await;
                    continue;
                }
            };
            let link = match dial(&self.endpoint, &credential, DIAL_TIMEOUT).await {
                Ok(link) => link,
                Err(error) => {
                    self.report_dial_failure(attempt, &error.to_string());
                    // v2's cleanup runs `onDetach` for a socket that never
                    // opened, too; a mint failure never built one.
                    self.notify_detach();
                    self.wait_before_next_dial(&mut stop).await;
                    continue;
                }
            };
            self.policy.note_link_opened(Instant::now());
            self.links_opened += 1;
            // A new connection generation: every fence taken on the previous
            // socket is stale from here on.
            let generation = self.uplink.advance();
            tracing::info!(
                attempt,
                links_opened = self.links_opened,
                generation,
                "the coordinator link is open"
            );
            let end = crate::runtime::link_serve::serve(&mut self, link, &mut stop).await;
            if let Some(escalation) = self.policy.note_link_dropped() {
                report_escalation(escalation);
            }
            self.detach_link();
            match verdict_for_link_end(&end) {
                LinkEndOutcome::Redial => {
                    self.redisials += 1;
                    tracing::info!(
                        reason = ?end,
                        redisials = self.redisials,
                        "the coordinator link ended; redialling"
                    );
                }
                LinkEndOutcome::Stop(reason) => {
                    tracing::info!(reason = %reason, "the coordinator link closed for a stop");
                    return self.dispose_replay(reason);
                }
            }
        }
    }

    /// v2 `detachSocket` + `onDetach`. Application state resets and the
    /// socket's lanes go with it: the pong, every control frame (a reply to a
    /// request of the gone connection, pending terminal metadata the replay
    /// re-asserts), terminal frames the next connection re-baselines, and raw
    /// metadata. A pending agent status survives in the agent-status outbox,
    /// whose possibly-lost retirements replay first on the next link. The durable mirror
    /// does not reset: a durable row that vanished on reconnect would be a hole
    /// in the coordinator's record of what happened.
    pub(super) fn detach_link(&mut self) {
        let generation = self.uplink.advance();
        self.pump.on_disconnect();
        self.authorised = None;
        self.snapshot_since = None;
        self.snapshot_wanted = false;
        // v2 `disconnect`: the replay is pending again until the next link
        // has replayed and reached its snapshot.
        if let Some(delivery) = &self.durable_delivery {
            delivery.mark_pending();
        }
        self.terminal_metadata_negotiated = false;
        let liveness = self.outbox.discard(Lane::Liveness);
        let control = self.outbox.discard(Lane::Control);
        let terminal = self.outbox.discard(Lane::Terminal);
        let raw = self.outbox.discard(Lane::RawMetadata);
        self.forget_terminal_metadata();
        self.agent_statuses.disconnect();
        tracing::info!(
            generation,
            liveness,
            control,
            terminal,
            raw,
            "the coordinator link detached; its pong, control, terminal and raw frames were dropped"
        );
        self.notify_detach();
    }

    /// v2 `clear`: the link is gone for good, so a reconcile waiting for its
    /// durable replay is refused rather than left waiting for ever.
    fn dispose_replay(&self, reason: StopReason) -> StopReason {
        if let Some(delivery) = &self.durable_delivery {
            delivery.dispose();
        }
        reason
    }

    fn notify_detach(&self) {
        if let Some(owners) = self.dispatcher.owners() {
            owners.lifecycle.on_detach();
        }
    }

    fn report_dial_failure(&mut self, attempt: u32, reason: &str) {
        if let Some(escalation) = self.policy.note_dial_failed() {
            report_escalation(escalation);
        }
        tracing::warn!(
            attempt,
            reason,
            "the coordinator link did not open; redialling"
        );
    }

    async fn wait_before_next_dial(&self, stop: &mut StopSignal) {
        let delay = self.policy.next_delay();
        tracing::info!(
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "waiting before the next dial"
        );
        tokio::select! {
            biased;
            reason = stop.requested() => {
                tracing::info!(reason = %reason, "stopping while waiting to dial");
            }
            () = tokio::time::sleep(delay) => {}
        }
    }
}
