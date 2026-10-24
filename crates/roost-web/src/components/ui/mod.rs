//! Roost-specific attachment, transcript and composer compositions.
//! Product callers import generic controls and hooks directly from Libero.
//! These modules retain only content semantics and product interaction policies.

pub mod attachment;
pub mod input_prompt;
pub mod markdown;
pub mod message;

pub use attachment::{
    Attachment, AttachmentActions, AttachmentContent, AttachmentDescription, AttachmentGroup,
    AttachmentMedia, AttachmentMediaVariant, AttachmentOrientation, AttachmentSize,
    AttachmentState, AttachmentTitle,
};
pub use input_prompt::{
    InputPrompt, InputPromptFooter, InputPromptSubmit, InputPromptTextarea, InputPromptTools,
};
pub use markdown::Markdown;
pub use message::{Message, MessageAlign, MessageContent, MessageFooter, MessageHeader};
