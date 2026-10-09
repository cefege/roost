//! The pane's on-demand terminal-image reads, deduplicated while each key is
//! in flight and settled into the renderer after its read completes.
//!
//! Owned by one `PaneShared`; requests use its elected direct route when
//! available and otherwise use the coordinator RPC.

use roost_client_core::client::rpc::calls::terminal_image::TerminalImage;

use super::PaneShared;

/// Start reads for image keys currently referenced by the painted frame.
pub(super) fn fetch_wanted(shared: &PaneShared) {
    let wanted = shared.renderer.borrow().wanted_image_keys();
    let pending = {
        let mut state = shared.state.borrow_mut();
        wanted
            .into_iter()
            .filter(|key| state.image_reads.insert(*key))
            .collect::<Vec<_>>()
    };
    for image_key in pending {
        fetch_one(shared, image_key);
    }
}

fn fetch_one(shared: &PaneShared, image_key: u64) {
    let Some(route) = shared.pump.elected_direct_history_route(&shared.session_id) else {
        fetch_from_coordinator(shared, image_key);
        return;
    };
    let weak = shared.weak_self();
    let session_id = shared.session_id.clone();
    shared
        .pump
        .read_direct_image(&route, &session_id, image_key, move |result| {
            let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) else {
                return;
            };
            settle(&shared, image_key, result);
        });
}

fn fetch_from_coordinator(shared: &PaneShared, image_key: u64) {
    let rpc = shared.pump.rpc();
    let weak = shared.weak_self();
    let call = TerminalImage {
        session_id: shared.session_id.clone(),
        image_key,
    };
    wasm_bindgen_futures::spawn_local(async move {
        let answer = rpc.call(&call).await.map_err(|error| error.to_string());
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            settle(&shared, image_key, answer);
        }
    });
}

fn settle(shared: &PaneShared, image_key: u64, result: Result<Vec<u8>, String>) {
    shared.state.borrow_mut().image_reads.remove(&image_key);
    let mut renderer = shared.renderer.borrow_mut();
    match result {
        Ok(png) => renderer.install_image(image_key, &png),
        Err(error) => {
            tracing::debug!(target: "terminal_image", session_id = %shared.session_id,
                image_key, %error, "terminal image read failed");
            renderer.image_failed(image_key);
        }
    }
}
