//! What this pane's terminal links resolve AGAINST and open INTO. The link
//! attachment is built with a file resolver or it installs none: the detector
//! classifies a terminal path only when a resolver will mint a `/file/…` route
//! for it, so a pane without one leaves every printed path as the plain text it
//! arrived as. The folder is read per call because a `cd` moves it under the
//! pane. Ports `resolveFile` and `onOpenFile` of
//! `apps/web/src/components/terminal/cell-terminal-input.ts` and
//! `cell-terminal-interactions.ts`.

use std::rc::Weak;

use roost_client_core::store::selectors::session_by_id;
use roost_web_terminal::links::{FileOpener, FileResolver};

use super::PaneShared;
use crate::terminal_file_link::resolve_terminal_file;
use crate::terminal_href::worker_os;

/// The resolver this pane's link attachment scans with. It holds the pane
/// weakly: a scan scheduled by a pane that has since unmounted must find
/// nothing and mint nothing, not keep the pane's renderer alive.
pub(super) fn file_resolver(shared: Weak<PaneShared>) -> FileResolver {
    Box::new(move |raw_path, line, file_authority| {
        let shared = shared.upgrade()?;
        if shared.disposed.get() {
            return None;
        }
        let (cwd, worker_os) = {
            let core = shared.pump.core();
            let core = core.try_borrow().ok()?;
            let store = core.store();
            let session = session_by_id(store, &shared.session_id)?;
            (
                session.cwd.clone(),
                worker_os(store, &shared.worker_fp).map(str::to_owned),
            )
        };
        resolve_terminal_file(
            worker_os.as_deref(),
            &shared.worker_fp,
            &cwd,
            raw_path,
            line,
            file_authority,
        )
    })
}

/// What a tapped file link opens. The pane is imperative and mounted from a DOM
/// event handler, where no Dioxus context is readable, so the router's handler
/// arrives with the mount: a file link with no opener swallows the click and
/// opens nothing, which is worse than no link at all.
pub(super) fn file_opener(shared: Weak<PaneShared>) -> FileOpener {
    Box::new(move |href| {
        if let Some(shared) = shared.upgrade().filter(|shared| !shared.disposed.get()) {
            shared.navigate.call(href.to_owned());
        }
    })
}
