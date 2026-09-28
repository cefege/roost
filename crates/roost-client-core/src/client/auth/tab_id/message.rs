//! The claim channel's one message shape, `{ type, id, nonce }` (v2
//! `apps/web/src/client/auth/tab-id.ts` `postOccupied`/`handleClaimMessage`).
//! Split from `tab_id` so the decision table reads apart from the wire shape;
//! read and written by `roost-web`'s `platform::tab_id`.

/// One message on the claim channel (v2 `{ type, id, nonce }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimMessage {
    /// "Does any document own `id`?"
    Probe { id: String, nonce: String },
    /// "I own `id`", answering the probe that carried `nonce`.
    Occupied { id: String, nonce: String },
}

impl ClaimMessage {
    /// The message whose `type`, `id` and `nonce` properties read as given; a
    /// missing or non-string property, or an unknown `type`, is not a message.
    pub fn parse(kind: Option<&str>, id: Option<String>, nonce: Option<String>) -> Option<Self> {
        let (id, nonce) = (id?, nonce?);
        match kind? {
            "probe" => Some(Self::Probe { id, nonce }),
            "occupied" => Some(Self::Occupied { id, nonce }),
            _ => None,
        }
    }

    /// The `type` property.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Probe { .. } => "probe",
            Self::Occupied { .. } => "occupied",
        }
    }

    /// The `id` property.
    pub fn id(&self) -> &str {
        match self {
            Self::Probe { id, .. } | Self::Occupied { id, .. } => id,
        }
    }

    /// The `nonce` property.
    pub fn nonce(&self) -> &str {
        match self {
            Self::Probe { nonce, .. } | Self::Occupied { nonce, .. } => nonce,
        }
    }
}
