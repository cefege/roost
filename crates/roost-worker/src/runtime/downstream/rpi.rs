//! Routes typed Rpi requests to the process-owned runtime.
//! Replies are fenced to the current worker process epoch; an absent owner
//! rejects commands and catalogs rather than acknowledging an execution.

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use super::{Dispatcher, DownstreamLink};

impl Dispatcher {
    pub(super) fn rpi_command(
        &self,
        request: roost_proto::DRpiCommand,
        link: &mut dyn DownstreamLink,
    ) {
        let owner = self.owners.as_ref().and_then(|owners| owners.rpi.as_ref());
        if request.expected_worker_epoch == self.process_epoch
            && let Some(owner) = owner
        {
            owner.command(request);
            return;
        }
        let result = if request.expected_worker_epoch != self.process_epoch {
            r#"{"kind":"error","code":"epoch_mismatch"}"#
        } else {
            r#"{"kind":"error","code":"worker_unavailable"}"#
        };
        link.reply(CoordWorkerUpstream::RpiCommandAck(
            roost_proto::WRpiCommandAck {
                conversation_id: request.conversation_id,
                request_id: request.request_id,
                command_seq: request.command_seq,
                worker_epoch: self.process_epoch.clone(),
                status: roost_proto::RpiCommandAckStatus::Rejected.into(),
                result_json: result.into(),
                ..Default::default()
            },
        ));
    }

    pub(super) fn rpi_events_ack(&self, request: roost_proto::DRpiEventsAck) {
        if request.worker_epoch != self.process_epoch {
            return;
        }
        if let Some(owner) = self.owners.as_ref().and_then(|owners| owners.rpi.as_ref()) {
            owner.ack(request);
        }
    }

    pub(super) fn rpi_models_list(
        &self,
        request: roost_proto::DRpiModelsList,
        link: &mut dyn DownstreamLink,
    ) {
        let owner = self.owners.as_ref().and_then(|owners| owners.rpi.as_ref());
        if request.expected_worker_epoch == self.process_epoch
            && let Some(owner) = owner
        {
            owner.models(request);
            return;
        }
        link.reply(CoordWorkerUpstream::RpiModelsListResult(
            roost_proto::WRpiModelsListResult {
                request_id: request.request_id,
                worker_epoch: self.process_epoch.clone(),
                error_code: "unavailable".into(),
                error_message: "RPI runtime unavailable on this machine".into(),
                ..Default::default()
            },
        ));
    }
}
