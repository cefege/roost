//! Converts wire chat attachments into durable agent-runtime attachments.
//! This keeps request-shape mapping out of the coordinator's chat handlers.

use roost_protocol::wire::agent_chat::AgentChatAttachment;

pub(super) fn to_domain_attachments(
    attachments: Vec<roost_proto::AgentChatAttachment>,
) -> Vec<AgentChatAttachment> {
    attachments
        .into_iter()
        .map(|attachment| AgentChatAttachment {
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            data: attachment.data,
            path: attachment.path,
        })
        .collect()
}
