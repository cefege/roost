//! The document's cryptographic randomness, for values that are SECRETS.
//!
//! `platform::tab_id` also reads `crypto.getRandomValues`, and it falls back to
//! `Math.random` where that is missing. That fallback is right for a tab id — a
//! collision costs one browser a second claim and nothing else — and wrong for a
//! pairing verification code, which a reader reads aloud to the person approving
//! it. `Math.random` output is predictable from four earlier draws, so a code
//! filled from it is a code an attacker can guess without touching the wire.
//!
//! So the two sources are separate on purpose and this is the strict one: a
//! document with no `crypto.getRandomValues` gets a REFUSAL, and the ceremony
//! that asked for bytes says so instead of inventing them. One source for
//! secrets, one for identifiers, and the difference is a documented contract
//! rather than a flag someone forgets to set.
use roost_client_core::client::auth::{CeremonyError, RandomSource};

/// `crypto.getRandomValues`, refusing where the document has none.
///
/// A native build has no document and no Web Crypto, so it refuses every fill
/// rather than reaching for the OS: this module is the BROWSER's source, and a
/// native caller that wants real entropy is not this caller. The refusal is
/// what a ceremony needs to hear — a code it cannot fill safely is a code it
/// must not invent.
#[derive(Debug, Default, Clone, Copy)]
pub struct BrowserRandomSource;

impl RandomSource for BrowserRandomSource {
    fn fill_bytes(&self, out: &mut [u8]) -> Result<(), CeremonyError> {
        #[cfg(target_arch = "wasm32")]
        {
            let filled = web_sys::window()
                .and_then(|window| window.crypto().ok())
                .is_some_and(|crypto| crypto.get_random_values_with_u8_array(out).is_ok());
            if !filled {
                return Err(CeremonyError::Entropy {
                    detail: "crypto.getRandomValues is unavailable in this document".to_owned(),
                });
            }
            return Ok(());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = out;
            Err(CeremonyError::Entropy {
                detail: "this build has no browser crypto".to_owned(),
            })
        }
    }
}
