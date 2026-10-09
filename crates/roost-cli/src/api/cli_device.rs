//! This machine's enrolled `roost api` device: an ed25519 key paired with a
//! coordinator by `roost api login`, kept in `~/.config/roost/cli-device.json`
//! (owner-only), and the EdDSA bearer it signs for each call. Called by
//! `api::credentials`; the JWT shape is `roost_client_core`'s, the one the
//! browser presents, so the coordinator verifies both the same way.

use std::path::PathBuf;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use roost_host::EnvSource;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::command_error::CommandFailure;

/// The file a login writes, under the user's configuration root.
const DEVICE_FILE: &str = "cli-device.json";

/// One enrolled device, as stored.
#[derive(Clone, Serialize, Deserialize)]
pub struct CliDevice {
    /// The coordinator the device was paired with.
    pub origin: String,
    /// The lowercase-hex SHA-256 of the public key: the coordinator's row name.
    pub fingerprint: String,
    /// The ed25519 seed, standard base64. Never printed.
    seed_b64: String,
}

/// A device's debug form names the coordinator and the fingerprint only.
impl std::fmt::Debug for CliDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliDevice")
            .field("origin", &self.origin)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl CliDevice {
    /// A fresh key for `origin`, not yet enrolled anywhere.
    pub fn generate(origin: &str) -> Result<Self, CommandFailure> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|error| {
            CommandFailure::generic(format!("no entropy for a device key: {error}"))
        })?;
        let key = SigningKey::from_bytes(&seed);
        Ok(Self {
            origin: origin.trim_end_matches('/').to_owned(),
            fingerprint: fingerprint_of(&key),
            seed_b64: BASE64_STANDARD.encode(seed),
        })
    }

    /// The public key as the pairing redeem's `ssh_pubkey_b64` field spells it.
    pub fn public_key_b64(&self) -> Result<String, CommandFailure> {
        let key = self.signing_key()?;
        Ok(roost_client_core::client::auth::jwt::public_key_b64(
            key.verifying_key().as_bytes(),
        ))
    }

    /// A bearer valid for five minutes from `now_ms`.
    pub fn bearer(&self, now_ms: u64) -> Result<String, CommandFailure> {
        let key = self.signing_key()?;
        let unsigned =
            roost_client_core::client::auth::jwt::build_unsigned_jwt(&self.fingerprint, now_ms);
        let signature = key.sign(unsigned.signing_input.as_bytes());
        Ok(unsigned.assemble(&signature.to_bytes()))
    }

    fn signing_key(&self) -> Result<SigningKey, CommandFailure> {
        let seed: [u8; 32] = BASE64_STANDARD
            .decode(&self.seed_b64)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| CommandFailure::generic("the stored CLI device key is corrupt"))?;
        Ok(SigningKey::from_bytes(&seed))
    }
}

fn fingerprint_of(key: &SigningKey) -> String {
    let digest: [u8; 32] = Sha256::digest(key.verifying_key().as_bytes()).into();
    roost_protocol::fingerprint::fingerprint_hex(&digest)
}

/// The host clock in epoch milliseconds, for a bearer's `iat`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Where the device file lives.
pub fn device_path(env: &dyn EnvSource) -> Result<PathBuf, CommandFailure> {
    let root = roost_host::paths::config_root(env)
        .map_err(|error| CommandFailure::generic(format!("no configuration directory: {error}")))?;
    Ok(root.join("roost").join(DEVICE_FILE))
}

/// The enrolled device, or `None` when this machine has not logged in.
pub fn load(env: &dyn EnvSource) -> Option<CliDevice> {
    let path = device_path(env).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Store `device`, readable by the owner only.
pub fn save(env: &dyn EnvSource, device: &CliDevice) -> Result<PathBuf, CommandFailure> {
    let path = device_path(env)?;
    let failure = |error: std::io::Error| {
        CommandFailure::generic(format!("cannot write {}: {error}", path.display()))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failure)?;
    }
    let text = serde_json::to_string_pretty(device)
        .map_err(|error| CommandFailure::generic(format!("cannot encode the device: {error}")))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    use std::io::Write as _;
    options
        .open(&path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(failure)?;
    Ok(path)
}

/// Forget the enrolled device. Absent is not an error.
pub fn remove(env: &dyn EnvSource) -> Result<(), CommandFailure> {
    let path = device_path(env)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CommandFailure::generic(format!(
            "cannot remove {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::CliDevice;

    #[test]
    fn a_bearer_names_the_fingerprint_and_carries_a_valid_signature() {
        use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
        use ed25519_dalek::Verifier as _;

        let device = CliDevice::generate("https://coord.example/").unwrap();
        assert_eq!(device.origin, "https://coord.example");
        assert_eq!(device.fingerprint.len(), 64);
        let token = device.bearer(1_700_000_000_000).unwrap();
        let mut parts = token.rsplitn(2, '.');
        let signature = BASE64_URL_SAFE_NO_PAD
            .decode(parts.next().unwrap())
            .unwrap();
        let signed = parts.next().unwrap();
        let payload: serde_json::Value = serde_json::from_slice(
            &BASE64_URL_SAFE_NO_PAD
                .decode(signed.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["sub"], device.fingerprint.as_str());
        let key = device.signing_key().unwrap().verifying_key();
        let signature = ed25519_dalek::Signature::from_slice(&signature).unwrap();
        assert!(key.verify(signed.as_bytes(), &signature).is_ok());
    }
}
