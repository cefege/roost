/// Several owners of the link's lifecycle behind the one port the link calls.
/// v2's `CoordLinkDeps` callbacks each reach several owners —
/// `coord-link-deps.ts` `onSnapshotReady` replays terminal metadata, resumes
/// the cell sink AND resends agent status — in registration order.
#[derive(Debug)]
pub struct LinkLifecycles {
    owners: Vec<Arc<dyn LinkLifecyclePort>>,
}

impl LinkLifecycles {
    pub fn new(owners: Vec<Arc<dyn LinkLifecyclePort>>) -> Self {
        Self { owners }
    }
}

impl LinkLifecyclePort for LinkLifecycles {
    fn on_open(&self) {
        self.owners.iter().for_each(|owner| owner.on_open());
    }
    fn on_hello_ack(&self, terminal_metadata_negotiated: bool) {
        self.owners
            .iter()
            .for_each(|owner| owner.on_hello_ack(terminal_metadata_negotiated));
    }
    fn on_detach(&self) {
        self.owners.iter().for_each(|owner| owner.on_detach());
    }
    fn on_writable(&self) {
        self.owners.iter().for_each(|owner| owner.on_writable());
    }
    fn on_snapshot_ready(&self) {
        self.owners.iter().for_each(|owner| owner.on_snapshot_ready());
    }
}
