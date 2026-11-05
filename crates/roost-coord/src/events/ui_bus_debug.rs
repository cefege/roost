//! Redacted formatting for volatile UI bus traffic.
//! Bus diagnostics identify the recipient and receipt without logging values.
//! Kept beside the bus declarations; no transport or persistence lives here.

use super::UiBusMsg;
impl std::fmt::Debug for UiBusMsg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::State { fp, tab_id, .. } => formatter
                .debug_struct("UiBusMsg::State")
                .field("fp", fp)
                .field("tab_id", tab_id)
                .finish(),
            Self::Command { target_tab_id, .. } => formatter
                .debug_struct("UiBusMsg::Command")
                .field("target_tab_id", target_tab_id)
                .finish(),
            Self::Apply {
                target_tab_id,
                target_socket_id,
                correlation_id,
                ..
            } => formatter
                .debug_struct("UiBusMsg::Apply")
                .field("target_tab_id", target_tab_id)
                .field("target_socket_id", target_socket_id)
                .field("correlation_id", correlation_id)
                .finish(),
            Self::OmpHostService {
                fingerprint,
                tab_id,
                frame,
            } => formatter
                .debug_struct("UiBusMsg::OmpHostService")
                .field("fingerprint", fingerprint)
                .field("tab_id", tab_id)
                .field("service_id", &frame.service_id)
                .field("origin_request_id", &frame.origin_request_id)
                .finish(),
            Self::OmpUiActionReceipt {
                fingerprint,
                tab_id,
                frame,
            } => formatter
                .debug_struct("UiBusMsg::OmpUiActionReceipt")
                .field("fingerprint", fingerprint)
                .field("tab_id", tab_id)
                .field("origin_request_id", &frame.origin_request_id)
                .field("after_seq", &frame.after_seq)
                .field("failed", &!frame.error.is_empty())
                .finish(),
        }
    }
}
