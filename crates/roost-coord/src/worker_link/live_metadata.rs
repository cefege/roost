//! Terminal metadata observations admitted through the live worker link.
//! Resolves sessions from channel ownership before publishing title, clipboard,
//! notification and activity transitions to coordinator services.

use super::*;
use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::coord_worker::TerminalMetadata;

impl WorkerFrameDispatcher {
    /// One semantic metadata record: a title, an activity stamp, or both.
    ///
    /// v2's `acceptTerminalMetadata`. The session is resolved through the byte
    /// hub rather than trusted from the frame, and an unmapped channel is a drop
    /// with a reason rather than a title attributed to a session that may not
    /// own it.
    pub(super) fn publish_metadata(
        &self,
        worker: &WorkerFp,
        metadata: &TerminalMetadata,
    ) -> DispatchOutcome {
        if !self
            .handle
            .capabilities
            .contains(CAPABILITY_TERMINAL_METADATA_V1)
        {
            tracing::debug!(
                worker_fp = %worker,
                "a semantic metadata frame arrived without the negotiated capability"
            );
            return DispatchOutcome::Refused;
        }
        let services = &self.core.services;
        let Some(session_id) = services.byte_hub.resolve(worker, metadata.channel_id) else {
            tracing::debug!(
                worker_fp = %worker,
                channel_id = metadata.channel_id.as_u32(),
                "a metadata frame arrived on an unbound channel"
            );
            return DispatchOutcome::Refused;
        };
        if metadata.title_changed {
            services
                .titles
                .observe_title(&services.buses, session_id.as_str(), &metadata.title);
        }
        if metadata.clipboard_changed {
            services.buses.clipboard_bus.publish(
                crate::events::bus_messages::SessionClipboardWrite {
                    session_id: session_id.as_str().to_owned(),
                    text: metadata.clipboard.clone(),
                },
            );
            // The length only: the text is the operator's clipboard.
            tracing::debug!(
                worker_fp = %worker,
                clipboard_bytes = metadata.clipboard.len(),
                "terminal emitted an OSC 52 clipboard write"
            );
            let core = self.core.clone();
            let source_session_id = session_id.as_str().to_owned();
            let source_worker_fp = worker.as_str().to_owned();
            let text = metadata.clipboard.clone();
            tokio::spawn(async move {
                if let Err(error) = crate::clipboard::capture_osc52(
                    &core,
                    &source_session_id,
                    &source_worker_fp,
                    &text,
                    crate::serve::now_ms(),
                )
                .await
                {
                    tracing::error!(error = %error, clipboard_bytes = text.len(), "OSC 52 history persistence failed");
                }
            });
        }
        if metadata.command_finished {
            services.buses.command_finished_bus.publish(
                crate::events::bus_messages::SessionCommandFinished {
                    session_id: session_id.as_str().to_owned(),
                    exit_code: metadata.command_exit_code,
                    duration_ms: metadata.command_duration_ms,
                },
            );
            tracing::debug!(
                worker_fp = %worker,
                exit_code = ?metadata.command_exit_code,
                duration_ms = metadata.command_duration_ms,
                "a long shell command finished"
            );
        }
        if metadata.bell {
            services
                .buses
                .bell_bus
                .publish(crate::events::bus_messages::SessionBell {
                    session_id: session_id.as_str().to_owned(),
                });
            tracing::debug!(worker_fp = %worker, session_id = %session_id, "terminal bell received");
        }
        if metadata.progress.is_some()
            || metadata.user_vars_changed
            || !metadata.notifications.is_empty()
        {
            // A coalesced record can carry several notifications; each
            // reaches the browser, the retained facts ride the first.
            let mut progress = metadata.progress;
            let mut user_vars = metadata
                .user_vars_changed
                .then(|| metadata.user_vars.clone());
            let mut notifications = metadata
                .notifications
                .iter()
                .cloned()
                .map(Some)
                .collect::<Vec<_>>();
            if notifications.is_empty() {
                notifications.push(None);
            }
            for notification in notifications {
                services.terminal_signals.observe(
                    &services.buses,
                    session_id.as_str(),
                    progress.take(),
                    user_vars.take(),
                    notification,
                );
            }
        }
        if let Some(records) = &metadata.program_status {
            services.terminal_signals.observe_program_status(
                &services.buses,
                session_id.as_str(),
                records.clone(),
            );
        }
        if metadata.activity_changed
            && let Ok(observed_at_ms) = i64::try_from(metadata.activity_ts_ms)
        {
            services.feed.last_activity().observe_and_publish(
                &services.buses,
                session_id.as_str(),
                observed_at_ms,
            );
        }
        DispatchOutcome::Handled
    }
}
