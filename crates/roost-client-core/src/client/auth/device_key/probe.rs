//! Asking the coordinator what it thinks of a key: the probe itself, the
//! recovery that probe drives, and the one precondition a reset turns on.
//!
//! Split from `device_key` because these three are the only part of the
//! lifecycle that needs an answer from OUTSIDE the store. An inherent `impl`
//! spans files through a child module, and a child is also the only place that
//! may read the parent's private store fields without loosening them for the
//! whole crate — the seam `global_search/page.rs` already uses in this crate.

use crate::client::auth::keystore::{DeviceKey, KeyAdmission, KeyStoreError, ROTATION_STAGE_SLOT};
use crate::client::auth::key_rotation::{RotationRecovery, recover_rotation};

use super::DeviceKeyManager;

impl<'host> DeviceKeyManager<'host> {
    /// Whether the coordinator has rejected this device, which is the only
    /// thing that admits a reset.
    pub fn is_reset_eligible(&self) -> Result<bool, KeyStoreError> {
        let Some(current) = self.store.read_current()? else {
            return Ok(false);
        };
        Ok(self.probe_key(current)? == KeyAdmission::DeviceRejected)
    }

    /// Resolve a leftover rotation stage by probing, then promote or discard.
    pub(super) fn recover_stage(&self) -> Result<RotationRecovery, KeyStoreError> {
        let Some(stage) = self.store.read_rotation_stage()? else {
            return Ok(RotationRecovery::None);
        };
        let staged = self.probe_key(stage.key)?;
        let current = if staged == KeyAdmission::Authorized {
            None
        } else {
            match self.store.read_current()? {
                Some(current) => Some(self.probe_key(current)?),
                None => None,
            }
        };
        match recover_rotation(staged, current) {
            RotationRecovery::Promoted => {
                self.store.promote_rotation_stage(&stage)?;
                self.cached.borrow_mut().take();
                tracing::info!(
                    target: "auth",
                    operation = %stage.operation_id,
                    "auth.rotation_recovered_promoted"
                );
                Ok(RotationRecovery::Promoted)
            }
            RotationRecovery::Discarded => {
                self.store.delete_rotation_stage(&stage.operation_id)?;
                tracing::info!(
                    target: "auth",
                    operation = %stage.operation_id,
                    "auth.rotation_recovered_discarded"
                );
                Ok(RotationRecovery::Discarded)
            }
            ambiguous => {
                tracing::warn!(
                    target: "auth",
                    operation = %stage.operation_id,
                    "auth.rotation_ambiguous"
                );
                Ok(ambiguous)
            }
        }
    }

    /// Ask the coordinator about one specific key.
    pub(super) fn probe_key(&self, key: DeviceKey) -> Result<KeyAdmission, KeyStoreError> {
        let token = self.sign_with(key, ROTATION_STAGE_SLOT, self.clock.now_ms())?;
        Ok(self.probe.probe_bearer(token.token()))
    }
}
