//! The coordinator connection generation direct-peer signaling is accepted
//! from: the first valid generation after a detach is adopted, and a
//! different one is refused until the link detaches again. Shared by the
//! terminal peer owner (`peer::direct`) and the attachment peer owner; cleared
//! by `peer::direct` on link detach and on direct retire. Ports
//! `useCoordinatorGeneration`/`clearCoordinatorGeneration`/`validCoordinatorGeneration`
//! of v2 `apps/worker/src/boot/boot-local-terminal.ts`.

use std::sync::{Arc, Mutex};

use super::packet_budget::lock;

const MAX_GENERATION_BYTES: usize = 128;

/// v2's `coordinatorGeneration` variable. Clone shares it.
#[derive(Debug, Clone, Default)]
pub struct CoordinatorGeneration {
    current: Arc<Mutex<Option<String>>>,
}

impl CoordinatorGeneration {
    pub fn new() -> Self {
        Self::default()
    }

    /// v2 `isCurrentCoordinator`.
    pub fn is_current(&self, generation: &str) -> bool {
        lock(&self.current).as_deref() == Some(generation)
    }

    /// v2 `useCoordinatorGeneration`: adopt the generation, or refuse one that
    /// is malformed or differs from the one already adopted.
    pub fn use_generation(&self, generation: &str) -> bool {
        if !valid_generation(generation) {
            return false;
        }
        let mut current = lock(&self.current);
        match current.as_deref() {
            Some(adopted) => adopted == generation,
            None => {
                *current = Some(generation.to_owned());
                tracing::debug!("a coordinator generation was adopted for direct peer signaling");
                true
            }
        }
    }

    /// v2 `clearCoordinatorGeneration`.
    pub fn clear(&self) {
        if lock(&self.current).take().is_some() {
            tracing::debug!("the coordinator generation for direct peer signaling was cleared");
        }
    }
}

/// v2 `validCoordinatorGeneration`: 1–128 UTF-8 bytes, no control byte.
fn valid_generation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_GENERATION_BYTES
        && !value.bytes().any(|byte| byte <= 0x1f || byte == 0x7f)
}

#[cfg(test)]
mod tests {
    use super::CoordinatorGeneration;

    #[test]
    fn one_generation_is_adopted_until_cleared() {
        let generation = CoordinatorGeneration::new();
        assert!(!generation.is_current("first"));
        assert!(generation.use_generation("first"));
        assert!(generation.use_generation("first"));
        assert!(!generation.use_generation("second"));
        assert!(generation.is_current("first"));
        generation.clear();
        assert!(!generation.is_current("first"));
        assert!(generation.use_generation("second"));
        assert!(!generation.use_generation(""));
        assert!(!generation.use_generation(&"g".repeat(129)));
        assert!(!generation.use_generation("tab\tseparated"));
    }
}
