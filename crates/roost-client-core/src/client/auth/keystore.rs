//! The device-key store: the ONE trait a host must satisfy to be able to pair.
//!
//! The contract is `protocol/spec/auth-and-pairing.md:24` — the browser
//! generates Ed25519 with `extractable=false` and IndexedDB owns the key's
//! atomic lifecycle. This file is the Rust statement of both halves, and the
//! shape of the trait is where the non-extractable half is actually enforced:
//!
//! **No method on this trait returns private key material.** `generate` hands
//! back an opaque [`DeviceKey`], and every later call takes that handle. There
//! is no `export`, no `raw_private`, no `seed`, and no way to reach the bytes a
//! host holds inside the handle — not by accident of omission, but because the
//! private half never appears in this crate's types at all. A host that wanted
//! to leak it would have to add a method here first, which is a reviewable edit
//! to a file whose only job is to make that edit conspicuous.
//!
//! The second load-bearing detail is that there is **no `put`**. First boot
//! writes with [`SecureKeyStore::add_current`], and a second `add` of the same
//! slot is [`KeyStoreError::AlreadyPresent`] rather than an overwrite — which is
//! what makes two browsers opening the pairing flow at once end with one device
//! key instead of one silently replacing the other
//! (`apps/web/src/client/auth/web-key-storage.ts:144`).
//!
//! Ported from `apps/web/src/client/auth/web-key-storage.ts`, whose storage
//! mechanism is the platform's business and whose discipline is this file's.

use std::fmt;

/// The slot the current device key lives in.
///
/// Restated from `web-key-storage.ts:112` because the slot name is part of the
/// on-disk shape a host must reproduce exactly, and a host that invents one
/// silently orphans the key a previous release wrote.
pub const DEVICE_KEY_SLOT: &str = "ed25519";

/// The slot an in-progress rotation's replacement key lives in.
///
/// A distinct slot from [`DEVICE_KEY_SLOT`] on purpose: a rotation has a window
/// in which BOTH keys are valid at the coordinator, and the only way to survive
/// an interrupted rotation is for the replacement to exist locally before the
/// coordinator is asked and to be promoted only after it answers.
pub const ROTATION_STAGE_SLOT: &str = "ed25519-rotation-v1";

/// The flag recording that a device key has ever been minted in this browser
/// profile.
///
/// Not security state — it answers "did this profile lose its key?", which is
/// what turns a first boot into a silent first boot and an eviction into a
/// diagnosable one (`web-key.ts:22,39-46`).
pub const KEY_MINTED_FLAG: &str = "roostKeyMinted";

/// A device key, as an opaque handle the store minted.
///
/// Copy, comparable and hashable so it can key a map, and nothing else. It has
/// no accessor, no `Display` and a `Debug` that prints only the store's own
/// generation counter, so a handle cannot be logged into a crash report in a
/// form that helps anyone reconstruct the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceKey(u64);

impl DeviceKey {
    /// Mint a handle. `SecureKeyStore::generate` is the only caller a host needs,
    /// and a test needs it to plant a key with chosen properties.
    pub const fn from_generation(generation: u64) -> Self {
        Self(generation)
    }

    /// The store's generation counter for this handle.
    ///
    /// A locator, not a secret: two handles from the same store never share
    /// one, which is what lets a store hold its private material in a plain
    /// array and still answer "which key is this" in a diagnostic.
    pub const fn generation(self) -> u64 {
        self.0
    }
}

/// Everything a caller may know about a key, and nothing more.
///
/// One call rather than three because each of them would be a separate trip into
/// the host's transaction, and because a host that answered them at two
/// different moments could describe a key it has already replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyDescriptor {
    /// The lowercase-hex SHA-256 of the raw public key: the `kid`, the `sub`, and
    /// the coordinator's row name.
    ///
    /// The host owns the HASH — this crate has no digest primitive, which is
    /// why `roost_protocol::fingerprint` takes 32 bytes and renders them rather
    /// than hashing. Render with `roost_protocol::fingerprint::fingerprint_hex`
    /// so there is exactly one spelling of a fingerprint in the tree.
    pub fingerprint: String,
    /// The raw 32-byte ed25519 public key. Never the private half.
    pub public_key: [u8; 32],
    /// Whether the private half can be read out of this host.
    ///
    /// Asked rather than assumed, so a host that substituted an extractable key
    /// is caught by the caller instead of being indistinguishable from one that
    /// did not.
    pub extractable: bool,
}

/// What the coordinator said when this client presented one key's credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAdmission {
    /// The coordinator resolved this key to exactly one authorized device.
    Authorized,
    /// The coordinator refused this key as a device. Terminal for the key: it is
    /// revoked, or it was never authorized, and neither improves by retrying.
    DeviceRejected,
    /// The probe could not tell — offline, a 500, a timeout.
    ///
    /// A distinct case from [`KeyAdmission::DeviceRejected`] because the entire
    /// rotation recovery turns on the difference: an unreachable coordinator
    /// must never be read as a rejection, because acting on that reading
    /// deletes a working device key.
    Ambiguous,
}

/// A rotation that has generated its replacement key but has not been promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationStage {
    /// Identifies THIS rotation attempt, so a promotion that arrives after a
    /// different attempt took the slot is refused instead of promoting a key the
    /// coordinator was never told about.
    pub operation_id: String,
    /// The replacement key. Not yet the device key.
    pub key: DeviceKey,
}

/// Why a key-store or device-key operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStoreError {
    /// An `add` lost a race: the slot was already occupied.
    ///
    /// The winner's key is still there, and the caller re-reads it. This is the
    /// normal outcome of two first boots, not a failure of either.
    AlreadyPresent,
    /// There is no current device key. Only a reset or an interrupted first boot
    /// produces this, and both are states the caller must be able to name.
    MissingCurrentKey,
    /// There is no rotation stage to read, promote or delete.
    MissingRotationStage,
    /// The stage slot holds a DIFFERENT rotation than the one being promoted.
    ///
    /// Never silently ignored: promoting it would install a key the coordinator
    /// has not authorized, and discarding it would delete a rotation that is
    /// mid-flight.
    RotationStageChanged,
    /// A probe the coordinator could not answer, and a decision that depended
    /// on it.
    ///
    /// Never resolved by guessing. The two decisions that read a probe are
    /// promoting a rotation stage and resetting a device key; in the first, a
    /// wrong guess installs a key the coordinator never authorized, in the
    /// second it deletes a working one. Both lock the device out, so the only
    /// safe answer is to refuse and wait for the coordinator to be reachable.
    ProbeAmbiguous,
    /// A reset was refused because the key is still, or may still be, valid.
    ///
    /// A revoked device has to be recoverable, and it must not be recoverable
    /// by accident — so the coordinator's explicit rejection is the only thing
    /// that admits it.
    ResetRefused,
    /// The store handed back a key whose private half is extractable.
    ///
    /// Refused rather than used. The spec requires non-extractable
    /// (`auth-and-pairing.md:24`), and a key that can be read out of the page's
    /// own storage is not a weaker version of that guarantee, it is the absence
    /// of it.
    ExtractableKeyRefused {
        /// Which slot the offending key was found in.
        slot: &'static str,
    },
    /// Durable storage is not available in this host.
    ///
    /// A hard error, never a degraded mode: a device key that cannot outlive a
    /// reload is a NEW device on every reload, which pairs again, enrolls again,
    /// and leaves the account growing orphaned device rows.
    PersistenceUnavailable {
        /// What the host reported, for a human reading the diagnostic.
        detail: String,
    },
    /// The host could not generate a key.
    Generation {
        /// What the host reported.
        detail: String,
    },
    /// The host could not sign.
    Signing {
        /// What the host reported. Never the bytes, and never any part of the key.
        detail: String,
    },
}

impl fmt::Display for KeyStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyPresent => write!(
                formatter,
                "the device-key slot is already occupied: this key lost the first-boot race"
            ),
            Self::MissingCurrentKey => write!(formatter, "there is no current device key"),
            Self::MissingRotationStage => write!(formatter, "there is no rotation stage"),
            Self::RotationStageChanged => write!(
                formatter,
                "the rotation stage belongs to a different operation and was left alone"
            ),
            Self::ProbeAmbiguous => write!(
                formatter,
                "the coordinator could not be asked; retry when it is reachable"
            ),
            Self::ResetRefused => write!(
                formatter,
                "reset is allowed only after the coordinator has rejected this device key"
            ),
            Self::ExtractableKeyRefused { slot } => write!(
                formatter,
                "the key in slot {slot} is extractable; the ceremony requires a non-extractable key"
            ),
            Self::PersistenceUnavailable { detail } => {
                write!(formatter, "durable key storage is unavailable: {detail}")
            }
            Self::Generation { detail } => {
                write!(formatter, "no device key could be generated: {detail}")
            }
            Self::Signing { detail } => {
                write!(formatter, "the device key could not sign: {detail}")
            }
        }
    }
}

impl std::error::Error for KeyStoreError {}

/// The browser's non-extractable Ed25519 device key, as a host must hold it.
///
/// Every method takes `&self`: two tabs, two `DeviceKeyManager`s and a test
/// harness all reach ONE store, and a store that needed `&mut` would make the
/// first-boot race untestable by construction. Implementations hold their
/// transaction state in interior mutability, the way `MemoryKeyValueStore` does
/// in `platform`.
///
/// The ordering of the writes is the ceremony:
///
/// 1. `generate` → `add_rotation_stage` → the coordinator is asked to rotate →
///    `promote_rotation_stage`. A crash anywhere leaves a stage the next boot
///    resolves by probing, never a key half-installed.
/// 2. `generate` → `add_current`, and the loser of that `add` re-reads the winner
///    instead of overwriting it.
pub trait SecureKeyStore {
    /// Generate a NEW non-extractable key inside the store and return its handle.
    ///
    /// The private half never leaves the store, so a caller cannot leak it even
    /// by accident.
    fn generate(&self) -> Result<DeviceKey, KeyStoreError>;

    /// Describe `key`: its fingerprint, its public bytes, and whether it is
    /// extractable.
    fn describe(&self, key: DeviceKey) -> Result<KeyDescriptor, KeyStoreError>;

    /// The current device key, or `None` on a profile that has never minted one.
    fn read_current(&self) -> Result<Option<DeviceKey>, KeyStoreError>;

    /// Write `key` into the current-key slot, REFUSING an occupied slot.
    ///
    /// An `add`, never a `put`. `put` here is how two browsers opening the
    /// pairing flow at once would leave one device silently replaced by another,
    /// with the loser's public key already enrolled at the coordinator.
    fn add_current(&self, key: DeviceKey) -> Result<(), KeyStoreError>;

    /// Drop the current device key, reporting whether there was one.
    ///
    /// The revoke path. Nothing else in this trait removes a device's identity,
    /// so a caller cannot do it while tidying up.
    fn delete_current(&self) -> Result<bool, KeyStoreError>;

    /// Sign `message` with `key`.
    fn sign(&self, key: DeviceKey, message: &[u8]) -> Result<Vec<u8>, KeyStoreError>;

    /// The in-progress rotation's stage, or `None` when no rotation has started.
    fn read_rotation_stage(&self) -> Result<Option<RotationStage>, KeyStoreError>;

    /// Write a rotation stage, REFUSING an occupied slot.
    ///
    /// Also an `add`: a second concurrent rotation must lose here rather than
    /// overwrite the first one's replacement key, or the first rotation's
    /// promotion would install the second rotation's key.
    fn add_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError>;

    /// Drop the rotation stage only if it is still `operation_id`'s.
    ///
    /// The identity check is the whole point: a discard that raced a newer
    /// rotation would delete a live stage.
    fn delete_rotation_stage(&self, operation_id: &str) -> Result<bool, KeyStoreError>;

    /// Make `stage`'s key the current key and drop the stage, as ONE transaction.
    ///
    /// Refuses [`KeyStoreError::RotationStageChanged`] when the slot holds a
    /// different operation. This is the only write in the trait that overwrites
    /// the current-key slot, and it may only do so from a stage the caller
    /// generated and the coordinator has already acknowledged.
    fn promote_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError>;
}
