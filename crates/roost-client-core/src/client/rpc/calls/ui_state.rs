//! `UiReportState`: this browser tab's route and portable layout, reported to
//! the coordinator so a CLI or agent can read and drive it.
//!
//! Called by the UI bridge (roost-web SHELL) through `CoordRpc::call` with a
//! report from `client::ui_command::build_ui_state_report`. v2's call site is
//! `coordClient.uiReportState(...)` in `apps/web/src/lib/uiStateReport.ts`.

use roost_proto::{UiReportStateRequest, UiReportStateResponse};
use roost_protocol::proto_adapters::layout_document_proto::layout_document_to_proto;

use crate::client::rpc::codec::{RpcCodecError, decode_message, encode_message};
use crate::client::rpc::unary::UnaryMethod;
use crate::client::ui_command::UiStateReport;

/// `UiReportState`: publish this tab's state. The answer is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct UiReportState {
    /// The report.
    pub report: UiStateReport,
}

impl UnaryMethod for UiReportState {
    const METHOD: &'static str = "UiReportState";
    type Response = ();

    fn encode_request(&self) -> Result<Vec<u8>, RpcCodecError> {
        let layout_document = match &self.report.layout_document {
            Some(document) => {
                roost_proto::buffa::MessageField::some(layout_document_to_proto(document).map_err(
                    |error| RpcCodecError::UnencodableRequest {
                        method: Self::METHOD,
                        detail: error.to_string(),
                    },
                )?)
            }
            None => roost_proto::buffa::MessageField::none(),
        };
        encode_message(
            Self::METHOD,
            &UiReportStateRequest {
                tab_id: self.report.tab_id.clone(),
                active_path: self.report.active_path.clone(),
                folder_key: self.report.folder_key.clone(),
                layout_document,
                ..Default::default()
            },
        )
    }

    fn decode_response(body: &[u8]) -> Result<(), RpcCodecError> {
        decode_message::<UiReportStateResponse>(Self::METHOD, body).map(|_| ())
    }
}
