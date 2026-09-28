//! The credential source the key-file cases share, over a real key file at a
//! real mode inside a scratch directory, because a `MapEnv` value cannot stand
//! in for a file the mode check reads.
//!
//! Owned by `credential_support` beside `scratch.rs`; two test binaries need
//! this one fixture, and a copied helper is two definitions of one fixture.

use roost_worker::runtime::credential::WorkerKeyCredential;

use super::scratch::Scratch;

/// A source over a key file inside `scratch`, freshly generated on first mint.
pub fn source_in(scratch: &Scratch) -> (WorkerKeyCredential, std::path::PathBuf) {
    let key_path = scratch.path("coordinator_ed25519.key");
    (WorkerKeyCredential::new(key_path.clone()), key_path)
}
