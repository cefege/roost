//! The upload id one file's whole conversation names, and the content digest
//! the dedup probe compares.
//!
//! Both are browser capabilities with no native equivalent, so both are
//! refusal-returning rather than panicking: an upload that cannot be named
//! cannot be sent, and an upload that cannot be hashed skips its probe and
//! still lands.
//! Ports `crypto.randomUUID()` and `crypto.subtle.digest("SHA-256", …)` from
//! `apps/web/src/lib/attachments.ts`.

/// The upload id every frame, card and chunk for one file names.
///
/// A v4 UUID, because the authority validates the layout: an id that is not a
/// UUID is a grant a peer could not bind, and a card a coordinator could not
/// correlate. `crypto.randomUUID` exists only in a secure context, so an
/// insecure origin mints nothing rather than a guessable id.
#[cfg(target_arch = "wasm32")]
pub fn mint_upload_id() -> Option<String> {
    web_sys::window()
        .filter(web_sys::Window::is_secure_context)
        .and_then(|window| window.crypto().ok())
        .map(|crypto| crypto.random_uuid())
}

/// No browser crypto, so no id. The caller reports the refusal on the card.
#[cfg(not(target_arch = "wasm32"))]
pub fn mint_upload_id() -> Option<String> {
    None
}

/// Lowercase hex SHA-256 of the whole content, which is what the worker's dedup
/// probe compares against.
#[cfg(target_arch = "wasm32")]
pub async fn content_digest(bytes: &[u8]) -> Result<String, String> {
    use wasm_bindgen_futures::JsFuture;

    let subtle = web_sys::window()
        .and_then(|window| window.crypto().ok())
        .ok_or_else(|| "crypto is unavailable".to_owned())?
        .subtle();
    let pending = subtle
        .digest_with_str_and_u8_array("SHA-256", bytes)
        .map_err(|error| format!("digest refused: {error:?}"))?;
    let buffer = JsFuture::from(pending)
        .await
        .map_err(|error| format!("digest failed: {error:?}"))?;
    let digest = js_sys::Uint8Array::new(&buffer).to_vec();
    Ok(lowercase_hex(&digest))
}

/// Lowercase hex, two digits a byte.
///
/// A local copy rather than the smoke probe's helper, because that one is
/// compiled only under the `smoke` feature and a release bundle has no
/// business depending on the oracle to hash a user's file.
#[cfg(target_arch = "wasm32")]
fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

/// No digest without a browser, which is what makes the probe fall through to
/// an ordinary upload rather than fail the file.
#[cfg(not(target_arch = "wasm32"))]
pub async fn content_digest(_bytes: &[u8]) -> Result<String, String> {
    Err("this build has no content digest".to_owned())
}

/// Unix milliseconds, the clock every transfer-card transition is stamped
/// with. A card's dismissal deadline and its ETA are both wall-clock facts, and
/// the client core's monotonic clock is anchored to the navigation, so the two
/// would not be comparable if they came from the same source.
pub fn now_ms() -> u64 {
    use crate::platform::clock::WallClock;
    WallClock.now_ms()
}
