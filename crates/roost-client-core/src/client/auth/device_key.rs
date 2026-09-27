//! The device key's lifecycle: load or generate, sign, rotate, reset.
//!
//! This is the port of `apps/web/src/client/auth/web-key.ts`, and the three
//! decisions in it are the ones with no visible symptom when they are wrong:
//!
//! 1. **A first boot that loses the race adopts the winner.** Two browsers
//!    opening the ceremony at once both read an empty slot, both generate, and
//!    both `add`; the loser's `add` is refused and it re-reads. Anything else —
//!    a retry, a `put`, a second `load_or_generate` that mints again — leaves
//!    one device silently replaced by another.
//! 2. **A rotation is a stage, not a replacement.** The replacement key exists
//!    locally before the coordinator is asked and is promoted only after it
//!    answers, so a crash in between leaves a question the next boot can answer
//!    by probing rather than a device that cannot authenticate. The vocabulary
//!    and the decision table are in `key_rotation`.
//! 3. **A reset needs the coordinator's explicit rejection.** Anything less —
//!    an unreachable coordinator, a probe that timed out — is not consent to
//!    delete an identity.
//!
//! Credentials are reused for [`JWT_CACHE_TTL_MS`] and re-signed in place after
//! that; a rotation or a reset drops the cache, because a cached credential
//! names the key that was just retired.
//!
//! Ported from `apps/web/src/client/auth/web-key.ts`; the contract is
//! `protocol/spec/auth-and-pairing.md:24-25`.

use std::cell::RefCell;

use crate::client::auth::keystore::{
    DEVICE_KEY_SLOT, DeviceKey, KEY_MINTED_FLAG, KeyAdmission, KeyStoreError, ROTATION_STAGE_SLOT,
    RotationStage, SecureKeyStore,
};
use crate::client::auth::key_rotation::{
    DeviceKeyProbe, DeviceKeyRotator, ResetOutcome, RotationError, RotationOutcome,
    RotationRecovery, RotationRequest, recover_rotation,
};
use crate::platform::{Clock, KeyValueStore};

/// What this client currently is, to the coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentKeyInfo {
    /// The `kid`/`sub` of the credential: 64 lowercase hex characters.
    pub fingerprint: String,
    /// Always `false`.
    ///
    /// A field rather than nothing because the device settings pane shows it,
    /// and a pane that can only ever print `false` cannot tell the user that
    /// the property is being checked at all.
    pub extractable: bool,
}

/// One cached view of the device key.
#[derive(Debug)]
struct CachedKey {
    key: DeviceKey,
    info: CurrentKeyInfo,
    public_key_b64: String,
    /// The credential and the millisecond it was signed at.
    jwt: Option<(CoordinatorJwt, u64)>,
}

/// The device key's lifecycle, over a host's key store.
///
/// Reads take `&self` because a host signs on every request and on every dial,
/// and a signer that needed `&mut` would be unusable from a request handler. The
/// two destructive operations take `&mut self` so that a caller holding this in
/// a shared struct cannot rotate or reset by accident.
pub struct DeviceKeyManager<'host> {
    store: &'host dyn SecureKeyStore,
    flags: &'host dyn KeyValueStore,
    probe: &'host dyn DeviceKeyProbe,
    clock: &'host dyn Clock,
    cached: RefCell<Option<CachedKey>>,
}

/// Hand-written because the four collaborators are trait objects, and a derive
/// would demand `Debug` of every host that ever implements one.
///
/// Only the cached fingerprint is printed. A store, a probe and a clock are the
/// host's business and a device key's identity is this type's, so a diagnostic
/// that names the key is the one worth having.
impl std::fmt::Debug for DeviceKeyManager<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached = self.cached.borrow();
        formatter
            .debug_struct("DeviceKeyManager")
            .field(
                "fingerprint",
                &cached.as_ref().map(|cached| cached.info.fingerprint.as_str()),
            )
            .field("credential_cached", &cached.as_ref().is_some_and(|c| c.jwt.is_some()))
            .finish()
    }
}

impl<'host> DeviceKeyManager<'host> {
    /// A manager over one key store.
    ///
    /// `flags` is the durable key/value store the minted-once flag lives in;
    /// `probe` is the host's classifier for a credential; `clock` is the host's
    /// clock, read here rather than from `SystemTime` so the cache window is
    /// testable.
    pub fn new(
        store: &'host dyn SecureKeyStore,
        flags: &'host dyn KeyValueStore,
        probe: &'host dyn DeviceKeyProbe,
        clock: &'host dyn Clock,
    ) -> Self {
        Self {
            store,
            flags,
            probe,
            clock,
            cached: RefCell::new(None),
        }
    }

    /// Drop the cached key and credential.
    ///
    /// For a host that learns of a change made by another tab: a cached
    /// credential names the key that was just retired, and presenting it is a
    /// 401 that reads as a coordinator problem.
    pub fn forget_cached_key(&self) {
        self.cached.borrow_mut().take();
    }

    /// This device's key, generating one on a profile that has never had any.
    pub fn load_or_generate(&self) -> Result<CurrentKeyInfo, KeyStoreError> {
        if let Some(info) = self
            .cached
            .borrow()
            .as_ref()
            .map(|cached| cached.info.clone())
        {
            return Ok(info);
        }
        if self.store.read_rotation_stage()?.is_some() {
            match self.recover_stage()? {
                RotationRecovery::Ambiguous => return Err(KeyStoreError::ProbeAmbiguous),
                RotationRecovery::Promoted
                | RotationRecovery::Discarded
                | RotationRecovery::None => {}
            }
        }
        match self.store.read_current()? {
            Some(existing) => self.adopt(existing, DEVICE_KEY_SLOT),
            None => self.first_boot(),
        }
    }

    /// The device key's public half, standard base64, as `ssh_pubkey_b64` wants.
    pub fn public_key_b64(&self) -> Result<String, KeyStoreError> {
        self.load_or_generate()?;
        self.cached
            .borrow()
            .as_ref()
            .map(|cached| cached.public_key_b64.clone())
            .ok_or(KeyStoreError::MissingCurrentKey)
    }

    /// A credential for the coordinator, minted or reused from the cache.
    pub fn sign_coordinator_jwt(&self) -> Result<CoordinatorJwt, KeyStoreError> {
        let now_ms = self.clock.now_ms();
        if let Some(reusable) = self.reusable_token(now_ms) {
            return Ok(reusable);
        }
        // Load first: a fresh profile has nothing cached, and signing is the
        // call a host makes first. Reading the cache without loading would
        // refuse the very request that mints the key.
        self.load_or_generate()?;
        let key = self.cached_key()?;
        let token = self.sign_with(key, DEVICE_KEY_SLOT, now_ms)?;
        if let Some(cached) = self.cached.borrow_mut().as_mut() {
            cached.jwt = Some((token.clone(), now_ms));
        }
        Ok(token)
    }

    /// The bearer for one request, or `None` when signing failed.
    ///
    /// **A signing failure dispatches the request unauthenticated; it never
    /// drops the request.** The rule is `bearer_for_signing`'s, named here so
    /// the Connect client has one call to make rather than a `match` to
    /// re-derive.
    pub fn current_bearer(&self) -> Option<String> {
        bearer_for_signing(self.sign_coordinator_jwt())
    }

    /// Whether the coordinator has rejected this device, which is the only
    /// thing that admits a reset.
    pub fn is_reset_eligible(&self) -> Result<bool, KeyStoreError> {
        let Some(current) = self.store.read_current()? else {
            return Ok(false);
        };
        Ok(self.probe_key(current)? == KeyAdmission::DeviceRejected)
    }

    /// Rotate to a new key, and promote the replacement once the coordinator has
    /// it.
    pub fn rotate_current(
        &mut self,
        rotator: &mut dyn DeviceKeyRotator,
        label: &str,
    ) -> Result<RotationOutcome, RotationError> {
        if self.store.read_rotation_stage()?.is_some() {
            match self.recover_stage()? {
                RotationRecovery::Ambiguous => return Err(KeyStoreError::ProbeAmbiguous.into()),
                RotationRecovery::Promoted => return Ok(RotationOutcome::Recovered),
                RotationRecovery::Discarded | RotationRecovery::None => {}
            }
        }
        let current = self
            .store
            .read_current()?
            .ok_or(KeyStoreError::MissingCurrentKey)?;
        let replacement = self.store.generate()?;
        let stage = RotationStage {
            operation_id: format!("rotate-{}", replacement.generation()),
            key: replacement,
        };
        // The stage is durable BEFORE the coordinator is asked. A crash in the
        // gap leaves a replacement on disk with a known answer to look for, and
        // the alternative — asking first — leaves an authorized key at the
        // coordinator that this browser has no copy of.
        self.store.add_rotation_stage(&stage)?;
        let descriptor = self.store.describe(replacement)?;
        let bearer =
            bearer_for_signing(self.sign_with(current, DEVICE_KEY_SLOT, self.clock.now_ms())?);
        rotator
            .rotate_current(&RotationRequest {
                public_key_b64: public_key_b64(&descriptor.public_key),
                label: label.to_string(),
                bearer,
            })
            .map_err(RotationError::Refused)?;
        self.store.promote_rotation_stage(&stage)?;
        self.cached.borrow_mut().take();
        tracing::info!(target: "auth", operation = %stage.operation_id, "auth.rotation_promoted");
        Ok(RotationOutcome::Rotated)
    }

    /// Remove a device key the coordinator has already rejected.
    pub fn reset(&mut self) -> Result<ResetOutcome, KeyStoreError> {
        if self.store.read_rotation_stage()?.is_some() {
            match self.recover_stage()? {
                RotationRecovery::Ambiguous => return Err(KeyStoreError::ProbeAmbiguous),
                RotationRecovery::Promoted => return Ok(ResetOutcome::Recovered),
                RotationRecovery::Discarded | RotationRecovery::None => {}
            }
        }
        let Some(current) = self.store.read_current()? else {
            return Ok(ResetOutcome::NotPaired);
        };
        match self.probe_key(current)? {
            KeyAdmission::DeviceRejected => {}
            KeyAdmission::Ambiguous => return Err(KeyStoreError::ProbeAmbiguous),
            KeyAdmission::Authorized => return Err(KeyStoreError::ResetRefused),
        }
        self.store.delete_current()?;
        self.cached.borrow_mut().take();
        tracing::info!(target: "auth", "auth.device_key_reset");
        Ok(ResetOutcome::Unpaired)
    }

    /// A first boot: mint, `add`, and adopt the winner if the `add` was refused.
    fn first_boot(&self) -> Result<CurrentKeyInfo, KeyStoreError> {
        if self.flags.get(KEY_MINTED_FLAG).as_deref() == Some("1") {
            tracing::warn!(target: "auth", slot = DEVICE_KEY_SLOT, "auth.key_evicted");
        } else {
            tracing::info!(target: "auth", "auth.key_first_boot");
        }
        let generated = self.store.generate()?;
        match self.store.add_current(generated) {
            Ok(()) => {
                self.flags.set(KEY_MINTED_FLAG, "1");
                self.adopt(generated, DEVICE_KEY_SLOT)
            }
            Err(KeyStoreError::AlreadyPresent) => {
                let winner = self
                    .store
                    .read_current()?
                    .ok_or(KeyStoreError::MissingCurrentKey)?;
                tracing::info!(target: "auth", "auth.key_first_boot_race_lost");
                self.adopt(winner, DEVICE_KEY_SLOT)
            }
            Err(error) => Err(error),
        }
    }

    /// Take a key into use, refusing one whose private half is extractable.
    fn adopt(
        &self,
        key: DeviceKey,
        slot: &'static str,
    ) -> Result<CurrentKeyInfo, KeyStoreError> {
        let descriptor = self.store.describe(key)?;
        if descriptor.extractable {
            return Err(KeyStoreError::ExtractableKeyRefused { slot });
        }
        let info = CurrentKeyInfo {
            fingerprint: descriptor.fingerprint.clone(),
            extractable: false,
        };
        *self.cached.borrow_mut() = Some(CachedKey {
            key,
            info: info.clone(),
            public_key_b64: public_key_b64(&descriptor.public_key),
            jwt: None,
        });
        Ok(info)
    }

    /// Resolve a leftover rotation stage by probing, then promote or discard.
    fn recover_stage(&self) -> Result<RotationRecovery, KeyStoreError> {
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

    /// Sign a credential naming one specific key, bypassing the cache.
    fn sign_with(
        &self,
        key: DeviceKey,
        slot: &'static str,
        now_ms: u64,
    ) -> Result<CoordinatorJwt, KeyStoreError> {
        let descriptor = self.store.describe(key)?;
        if descriptor.extractable {
            return Err(KeyStoreError::ExtractableKeyRefused { slot });
        }
        let unsigned = build_unsigned_jwt(&descriptor.fingerprint, now_ms);
        let signature = self.store.sign(key, unsigned.signing_input.as_bytes())?;
        Ok(CoordinatorJwt::mint(&unsigned, &signature, now_ms))
    }

    /// Ask the coordinator about one specific key.
    fn probe_key(&self, key: DeviceKey) -> Result<KeyAdmission, KeyStoreError> {
        let token = self.sign_with(key, ROTATION_STAGE_SLOT, self.clock.now_ms())?;
        Ok(self.probe.probe_bearer(token.token()))
    }

    fn cached_key(&self) -> Result<DeviceKey, KeyStoreError> {
        self.cached
            .borrow()
            .as_ref()
            .map(|cached| cached.key)
            .ok_or(KeyStoreError::MissingCurrentKey)
    }

    /// The cached credential, if it is still inside its reuse window.
    fn reusable_token(&self, now_ms: u64) -> Option<CoordinatorJwt> {
        let cached = self.cached.borrow();
        let (token, signed_at_ms) = cached.as_ref()?.jwt.as_ref()?;
        // A clock that moved backwards produces a saturating age far past the
        // window rather than a negative one, so the credential is re-signed
        // instead of served until the clock catches up.
        (now_ms.saturating_sub(*signed_at_ms) < JWT_CACHE_TTL_MS).then(|| token.clone())
    }
}
