//! The client-minted id every agent command carries, so the coordinator
//! applies a retried RPC once. Called by the composer, message actions, plan
//! and ask cards; reads randomness from `window.crypto`.

/// 128 random bits as lowercase hex.
#[cfg(target_arch = "wasm32")]
pub(super) fn new_request_id() -> String {
    let mut bytes = [0_u8; 16];
    if let Some(crypto) = web_sys::window().and_then(|window| window.crypto().ok()) {
        let _ = crypto.get_random_values_with_u8_array(&mut bytes);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
