//! The coordinator's credential-encryption key, with redacted formatting.
//! Config values are logged at startup, so key bytes must never be formatted.

#[derive(Clone, PartialEq, Eq)]
pub struct CoordinatorSecretKey([u8; 32]);

impl CoordinatorSecretKey {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for CoordinatorSecretKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CoordinatorSecretKey([REDACTED])")
    }
}
