//! Owns route-to-worker scope lookup and the destination used when the preview
//! closes. `FileViewer` keeps the route lifecycle; these helpers read one
//! consistent registry snapshot and the remembered terminal route.

use roost_client_core::Store;
use roost_client_core::store::sidebar::memory::last_terminal_path;
use roost_client_core::sync::SyncDomain;

use crate::components::layout::shell_style::is_terminal_path;
use crate::pump::Pump;
use crate::routes::Route;
use crate::terminal_href::worker_os;

/// Whether the route's worker is known, whether its registry has published,
/// and the platform that determines how the route's path is decoded.
pub fn read_scope(pump: &Pump, route: &Route) -> (bool, bool, Option<String>) {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let worker_fp = match route {
        Route::File { worker_fp, .. } => worker_fp.as_str(),
        _ => "",
    };
    (
        store.workers.contains_key(worker_fp),
        registry_hydrated(store),
        worker_os(store, worker_fp).map(str::to_owned),
    )
}

/// Whether the worker registry has published a snapshot yet.
fn registry_hydrated(store: &Store) -> bool {
    store.sync.domain_is_ready(SyncDomain::Workers)
}

/// Return to the last terminal route when it is still a terminal, otherwise
/// return to the home route.
pub fn close_destination(pump: &Pump) -> String {
    let core = pump.core();
    let core = core.borrow();
    last_terminal_path(core.storage())
        .filter(|path| is_terminal_path(path))
        .unwrap_or_else(|| "/".to_owned())
}
