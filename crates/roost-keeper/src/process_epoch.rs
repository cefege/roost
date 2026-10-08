//! The keeper process's epoch: a v4 uuid minted once when the daemon's
//! `Keeper` is built and reported in every `Hello`, so a worker can tell this
//! incarnation from a later process that reused its pid. Ports the
//! `processEpoch = randomUUID()` of v2 `apps/worker/src/keeper/multiplexed-main.ts`.
//! Called by `keeper::Keeper::new`; read by `keeper_ops::Keeper::hello_response`.

const UUID_BYTES: usize = 16;

/// A fresh v4 uuid, or `None` when the host's entropy source is unreadable.
///
/// `None` is reported as an absent epoch, which a worker reads as an unproven
/// keeper identity: an epoch that is not random proves nothing about which
/// incarnation answered.
pub(crate) fn mint_process_epoch() -> Option<String> {
    let mut bytes = [0_u8; UUID_BYTES];
    if let Err(error) = getrandom::fill(&mut bytes) {
        tracing::error!(%error, "keeper: no process epoch, the entropy source is unreadable");
        return None;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut rendered = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            rendered.push('-');
        }
        rendered.push(char::from(HEX[usize::from(byte >> 4)]));
        rendered.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Some(rendered)
}
