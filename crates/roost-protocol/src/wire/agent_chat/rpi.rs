//! Typed RPI commands and retained outcomes shared by coordinator and worker.
//! The worker envelope carries JSON only after kind and payload validation.
//! Fingerprints are canonical semantic JSON, independent of delivery epochs.

use roost_proto::RpiCommandKind;
use serde::{Deserialize, Serialize};

use super::AgentChatAttachment;
use crate::{ProtocolError, ProtocolResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RpiCommandPayload {
    Open {
        cwd: String,
        model_provider: String,
        model_id: String,
        thinking_level: Option<String>,
        resume_pending: bool,
    },
    Submit {
        text: String,
        #[serde(deserialize_with = "deserialize_attachments")]
        attachments: Vec<AgentChatAttachment>,
        when_busy: String,
    },
    Abort {
        background: Option<bool>,
    },
    Withdraw {
        submission_id: String,
    },
    Configure {
        model_provider: Option<String>,
        model_id: Option<String>,
        thinking_level: Option<String>,
        title: Option<String>,
    },
    Close {},
}

impl RpiCommandPayload {
    pub fn kind(&self) -> RpiCommandKind {
        match self {
            Self::Open { .. } => RpiCommandKind::Open,
            Self::Submit { .. } => RpiCommandKind::Submit,
            Self::Abort { .. } => RpiCommandKind::Abort,
            Self::Withdraw { .. } => RpiCommandKind::Withdraw,
            Self::Configure { .. } => RpiCommandKind::Configure,
            Self::Close {} => RpiCommandKind::Close,
        }
    }

    pub fn parse(kind: RpiCommandKind, payload_json: &str) -> ProtocolResult<Self> {
        let payload: Self = serde_json::from_str(payload_json)
            .map_err(|_| ProtocolError::new("rpi.payload", "invalid command payload"))?;
        payload.validate_kind(kind)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate_kind(&self, kind: RpiCommandKind) -> ProtocolResult<()> {
        if self.kind() != kind {
            return Err(ProtocolError::new(
                "rpi.kind",
                "command kind does not match payload",
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        match self {
            Self::Open {
                cwd,
                model_provider,
                model_id,
                ..
            } => {
                require_nonblank("rpi.cwd", cwd)?;
                require_nonblank("rpi.model_provider", model_provider)?;
                require_nonblank("rpi.model_id", model_id)?;
            }
            Self::Submit { when_busy, .. } => {
                if !matches!(when_busy.as_str(), "" | "follow_up" | "reject") {
                    return Err(ProtocolError::new(
                        "rpi.when_busy",
                        "unsupported busy policy",
                    ));
                }
            }
            Self::Withdraw { submission_id } => {
                require_nonblank("rpi.submission_id", submission_id)?;
            }
            Self::Configure {
                model_provider,
                model_id,
                ..
            } => match (model_provider, model_id) {
                (Some(provider), Some(model)) => {
                    require_nonblank("rpi.model_provider", provider)?;
                    require_nonblank("rpi.model_id", model)?;
                }
                (None, None) => {}
                _ => return Err(ProtocolError::new("rpi.model", "model pair is required")),
            },
            Self::Abort { .. } | Self::Close {} => {}
        }
        Ok(())
    }

    /// Store this canonical value for equality; it is not a cryptographic digest.
    /// Delivery epoch, command sequence and connection identity are excluded.
    pub fn semantic_fingerprint(&self) -> ProtocolResult<String> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|_| ProtocolError::new("rpi.payload", "cannot encode command payload"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RpiCommandResult {
    Open {
        actor_id: String,
        interrupted_submission_ids: Vec<String>,
        needs_resume: bool,
    },
    Submitted {},
    Aborted {},
    Withdrawn {
        withdrawn: bool,
    },
    Configured {},
    Closed {},
    Error {
        code: RpiErrorCode,
    },
}

/// Worker failures carry stable codes, never raw provider errors or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpiErrorCode {
    InvalidArgument,
    NotFound,
    Busy,
    WorkerUnavailable,
    EpochMismatch,
    AuthenticationRequired,
    ModelUnavailable,
    ConfigurationInvalid,
    StorageFailure,
    RuntimeFailure,
    ToolOutcomeUnknown,
}

impl RpiErrorCode {
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid rpi command",
            Self::NotFound => "conversation not found",
            Self::Busy => "conversation is busy",
            Self::WorkerUnavailable => "worker unavailable",
            Self::EpochMismatch => "worker epoch changed",
            Self::AuthenticationRequired => "worker-local provider login required",
            Self::ModelUnavailable => "selected model unavailable",
            Self::ConfigurationInvalid => "worker-local provider configuration invalid",
            Self::StorageFailure => "rpi storage failure",
            Self::RuntimeFailure => "rpi runtime failure",
            Self::ToolOutcomeUnknown => "tool outcome unknown after worker restart",
        }
    }
}

fn require_nonblank(field: &str, value: &str) -> ProtocolResult<()> {
    if value.trim().is_empty() {
        return Err(ProtocolError::new(field, "must not be blank"));
    }
    Ok(())
}

fn deserialize_attachments<'de, D>(deserializer: D) -> Result<Vec<AgentChatAttachment>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Attachment {
        filename: String,
        mime_type: String,
        #[serde(default)]
        data: Vec<u8>,
        #[serde(default)]
        path: String,
    }
    Vec::<Attachment>::deserialize(deserializer).map(|attachments| {
        attachments
            .into_iter()
            .map(|attachment| AgentChatAttachment {
                filename: attachment.filename,
                mime_type: attachment.mime_type,
                data: attachment.data,
                path: attachment.path,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests;
