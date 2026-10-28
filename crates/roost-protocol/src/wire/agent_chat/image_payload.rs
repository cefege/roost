//! Shared inline-image budget for unary agent-chat submissions.
//! The budget reserves transport-envelope headroom below the coordinator's body cap.
//! Validation includes encoded text, attachment metadata, and protobuf field overhead.

use thiserror::Error;

use super::AgentChatAttachment;

/// Maximum accounted agent-chat content, leaving 1 MiB for the RPC envelope.
pub const MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES: u64 = 15 * 1024 * 1024;

/// Why an inline-image submission exceeds the shared wire budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("agent chat image payload is {actual_bytes} bytes; the limit is {limit_bytes} bytes")]
pub struct AgentChatImagePayloadTooLarge {
    /// Accounted text, metadata, protobuf overhead, and inline image bytes.
    pub actual_bytes: u64,
    /// Maximum accounted payload size.
    pub limit_bytes: u64,
}

/// Checks a submission's accounted wire size when it contains an inline image.
///
/// Text-only submissions are unchanged. The estimate counts every serialized
/// attachment field and the nested attachment's length prefix, leaving the
/// constant envelope reserve for the remaining request fields and framing.
///
/// # Errors
/// Returns an error when an image-bearing request exceeds the shared budget.
pub fn validate_agent_chat_image_payload(
    text: &str,
    attachments: &[AgentChatAttachment],
) -> Result<(), AgentChatImagePayloadTooLarge> {
    if !attachments
        .iter()
        .any(|attachment| attachment.mime_type.starts_with("image/"))
    {
        return Ok(());
    }

    let mut accounted_bytes = encoded_field_size(text.len() as u64);
    for attachment in attachments {
        let nested_bytes = encoded_field_size(attachment.filename.len() as u64)
            .saturating_add(encoded_field_size(attachment.mime_type.len() as u64))
            .saturating_add(encoded_field_size(attachment.data.len() as u64))
            .saturating_add(encoded_field_size(attachment.path.len() as u64));
        accounted_bytes = accounted_bytes.saturating_add(encoded_field_size(nested_bytes));
        if accounted_bytes > MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES {
            return Err(AgentChatImagePayloadTooLarge {
                actual_bytes: accounted_bytes,
                limit_bytes: MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES,
            });
        }
    }
    Ok(())
}

fn encoded_field_size(value_bytes: u64) -> u64 {
    1 + varint_size(value_bytes) + value_bytes
}

fn varint_size(mut value: u64) -> u64 {
    let mut size = 1;
    while value >= 0x80 {
        value >>= 7;
        size += 1;
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_with_data_size(data_size: usize) -> AgentChatAttachment {
        AgentChatAttachment {
            filename: "image.png".to_owned(),
            mime_type: "image/png".to_owned(),
            data: vec![0; data_size],
            path: String::new(),
        }
    }

    fn encoded_submit_size(data_size: usize) -> u64 {
        use roost_proto::buffa::Message;

        let request = roost_proto::AgentChatSubmitRequest {
            attachments: vec![roost_proto::AgentChatAttachment {
                filename: "image.png".to_owned(),
                mime_type: "image/png".to_owned(),
                data: vec![0; data_size],
                ..Default::default()
            }],
            ..Default::default()
        };
        u64::from(request.encoded_len())
    }

    #[test]
    fn accepts_known_wire_budget_boundary_and_rejects_one_more_byte() {
        // 22 metadata bytes, 5 data-field bytes, 5 repeated-message bytes,
        // and 4 conservative bytes for empty text and path fields.
        let data_budget = MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES as usize - 36;
        assert_eq!(
            encoded_submit_size(data_budget),
            MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES - 4
        );
        assert_eq!(
            encoded_submit_size(data_budget + 1),
            MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES - 3
        );
        assert!(
            validate_agent_chat_image_payload("", &[image_with_data_size(data_budget)]).is_ok()
        );
        assert!(
            validate_agent_chat_image_payload("", &[image_with_data_size(data_budget + 1)])
                .is_err(),
            "the next byte crosses the accounted wire budget"
        );
    }

    #[test]
    fn combined_images_and_metadata_share_one_budget_but_text_only_is_unchanged() {
        let half = (MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES / 2) as usize;
        let first = image_with_data_size(half);
        let second = image_with_data_size(half);
        assert!(validate_agent_chat_image_payload("", &[first, second]).is_err());
        assert!(validate_agent_chat_image_payload(&"x".repeat(20 * 1024 * 1024), &[]).is_ok());
    }
}
