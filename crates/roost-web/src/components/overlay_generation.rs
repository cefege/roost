//! Per-host completion fence for product-controlled Libero openings.
//! Modal and drawer hosts capture a generation for every opening and async action.
//! A stale completion cannot clear a newer product request.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayGeneration {
    sequence: u64,
    active: Option<u64>,
}

impl OverlayGeneration {
    pub fn begin(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.active = Some(self.sequence);
        self.sequence
    }

    pub fn invalidate(&mut self) {
        self.active = None;
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.active == Some(generation)
    }

    pub fn finish(&mut self, generation: u64) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.active = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::OverlayGeneration;

    #[test]
    fn an_old_completion_cannot_close_the_replacement_opening() {
        let mut openings = OverlayGeneration::default();
        let first = openings.begin();
        openings.invalidate();
        let second = openings.begin();
        assert!(!openings.finish(first));
        assert!(openings.is_current(second));
        assert!(openings.finish(second));
        assert!(!openings.finish(second));
    }
}
