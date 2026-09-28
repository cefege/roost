//! The smoke members that reach the coordinator or the input path — spawn,
//! kill, workspace create, scoped cleanup, raw input, the retained-marker scan,
//! the attachment probe and the chunked download — and the backdoor as the
//! host `runFlow`/`runRenderStress` drive. wasm32 only. Ports
//! `apps/web/src/smoke/{smokeCreatedResources,smokeRetainedMarkerScan,smokeFileTransferProbes,smokeTerminalInputController}.ts`.

use std::collections::BTreeMap;

use roost_client_core::client::rpc::calls::attachments::ProbeAttachment;
use roost_client_core::client::rpc::calls::files::ReadFileChunk;
use roost_client_core::client::rpc::calls::sessions::{KillSession, SpawnSession};
use roost_client_core::client::rpc::calls::workspaces::{CreateWorkspace, DeleteWorkspace, ListWorkspaces};
use roost_client_core::terminal::input::smoke_observer::ObservedAdmission;
use roost_client_core::{ClientEvent, InputOutcome};
use serde_json::{Value, json};

use super::backdoor::SmokeBackdoor;
use super::call::{DEFAULT_PAINT_TIMEOUT_MS, unported_refusal};
use super::created_resources::{CleanupReport, CleanupRpc, cleanup_created, free_workspace_name};
use super::dispatch::navigate_through_router;
use super::dom;
use super::file_transfer::{ChunkReader, download_whole_file, lowercase_hex};
use super::harness::{FlowHost, StressHost};
use super::marker_scan::{SmokeMarkerScan, scan_painted_rows};
use super::paint_wait::{next_frame, random_uuid, sleep_ms, wait_for_painted_marker};
use super::retained_scan::{RetainedScanPager, retained_page_rows};
use crate::platform::connect::CoordRpc;
use crate::platform::worker_paths::worker_path_basename;

/// How often `input()` looks for its batch's settled outcome.
const INPUT_OUTCOME_POLL_MS: i32 = 16;

impl SmokeBackdoor {
    /// `input(session, text)`: admit through the real input path, then wait
    /// for the batch's own settled outcome.
    pub(super) async fn input(&self, session_id: &str, text: &str) -> Result<(), String> {
        let core = self.pump.core();
        let take_admission = || {
            core.borrow_mut()
                .store_mut()
                .input
                .smoke_observer
                .as_mut()
                .map(|observer| observer.take_admission())
        };
        // A keystroke typed into a pane since the last probe left its own answer.
        drop(take_admission());
        self.pump.dispatch(ClientEvent::TerminalInput {
            session_id: session_id.to_owned(),
            view_id: None,
            bytes: text.as_bytes().to_vec(),
        });
        let input_seq = match take_admission().flatten() {
            Some(ObservedAdmission::Admitted { input_seq }) => input_seq,
            Some(ObservedAdmission::Refused { reason }) => return Err(reason),
            None => return Err("terminal input observer is not armed".to_owned()),
        };
        loop {
            let settled = core
                .borrow_mut()
                .store_mut()
                .input
                .smoke_observer
                .as_mut()
                .and_then(|observer| observer.take_outcome(input_seq));
            match settled {
                Some(InputOutcome::Accepted { .. }) => return Ok(()),
                Some(InputOutcome::Rejected { reason, .. } | InputOutcome::Ambiguous { reason, .. }) => {
                    return Err(reason);
                }
                None => sleep_ms(INPUT_OUTCOME_POLL_MS).await,
            }
        }
    }

    pub(super) async fn spawn_shell_call(
        &self,
        worker_fp: &str,
        folder: &str,
        session_id: Option<String>,
    ) -> Result<Value, String> {
        let request = SpawnSession {
            worker_fp: worker_fp.to_owned(),
            kind: "shell".to_owned(),
            folder: folder.to_owned(),
            cols: None,
            rows: None,
            session_id,
        };
        let spawned = self.pump.rpc().call(&request).await.map_err(|error| error.to_string())?;
        self.created.borrow_mut().track_session(&spawned.session_id);
        self.persist_created();
        tracing::info!(target: "smoke", session_id = %spawned.session_id, "smoke shell spawned");
        Ok(json!({ "session_id": spawned.session_id, "channel_id": spawned.channel_id }))
    }

    pub(super) async fn kill_call(&self, session_id: &str) -> Result<Value, String> {
        let request = KillSession { session_id: session_id.to_owned(), force: false };
        let accepted = self.pump.rpc().call(&request).await.map_err(|error| error.to_string())?;
        Ok(json!({ "accepted": accepted }))
    }

    pub(super) async fn create_workspace_call(
        &self,
        worker_fp: &str,
        folder: &str,
        session_id: &str,
    ) -> Result<Value, String> {
        let name = {
            let core = self.pump.core();
            let core = core.borrow();
            let store = core.store();
            let existing: Vec<String> = store
                .workspaces
                .values()
                .filter(|workspace| workspace.worker_fp.as_str() == worker_fp)
                .map(|workspace| workspace.name.clone())
                .collect();
            let worker_os = store.workers.get(worker_fp).map(|worker| worker.os.as_str());
            free_workspace_name(&existing, worker_path_basename(worker_os, folder).as_deref())
        };
        let request = CreateWorkspace {
            worker_fp: worker_fp.to_owned(),
            name,
            folder_path: folder.to_owned(),
            color: None,
            attach_session_ids: vec![session_id.to_owned()],
        };
        let workspace = self.pump.rpc().call(&request).await.map_err(|error| error.to_string())?;
        let id = workspace.id.to_string();
        self.created.borrow_mut().track_workspace(&id);
        self.persist_created();
        let channel = self.pump.core().borrow().store().sessions.sessions().iter()
            .find(|(known, _)| known.as_str() == session_id)
            .map_or(0, |(_, session)| session.channel.as_u32());
        Ok(json!({ "id": id, "channel": channel }))
    }

    pub(super) async fn cleanup_created_call(&self) -> CleanupReport {
        let (sessions, workspaces) = self.created.borrow_mut().take_all();
        let report = cleanup_created(&*self.pump.rpc(), sessions, workspaces).await;
        self.persist_created();
        report
    }

    pub(super) async fn retained_marker_scan(
        &self,
        session_id: &str,
        prefix: &str,
        page_rows: Option<f64>,
    ) -> Result<Value, String> {
        let page_rows = retained_page_rows(page_rows)?;
        let grid_epoch = self.pump.core().borrow().store().terminal(session_id)
            .map(|replica| replica.frame_counts.grid_epoch().to_owned())
            .unwrap_or_default();
        let mut pager = RetainedScanPager::new(session_id, &grid_epoch, page_rows)?;
        let rpc = self.pump.rpc();
        loop {
            let request = pager.next_request()?;
            let page = rpc.call(&request).await.map_err(|error| error.to_string())?;
            if pager.accept(&page)? {
                break;
            }
        }
        serde_json::to_value(pager.finish(prefix)).map_err(|error| error.to_string())
    }

    pub(super) async fn attachment_probe_call(
        &self,
        session_id: String,
        sha256: String,
        size: u64,
        filename: String,
    ) -> Result<Value, String> {
        let request = ProbeAttachment { session_id, sha256, size, filename, short_path: false };
        let answer = self.pump.rpc().call(&request).await.map_err(|error| error.to_string())?;
        Ok(json!({ "hit": answer.hit, "abs_path": answer.abs_path }))
    }

    pub(super) async fn download_worker_file_call(&self, worker_fp: &str, path: &str) -> Result<Value, String> {
        let rpc = self.pump.rpc();
        let file = download_whole_file(&WorkerFile { rpc: &rpc, worker_fp, path }).await?;
        let digest = sha256(&file).await?;
        Ok(json!({ "bytes": file.len(), "sha256": lowercase_hex(&digest) }))
    }
}

/// SHA-256 through `crypto.subtle`.
async fn sha256(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let subtle = dom::window()
        .and_then(|window| window.crypto().ok())
        .ok_or_else(|| "crypto is unavailable".to_owned())?
        .subtle();
    let pending = subtle
        .digest_with_str_and_u8_array("SHA-256", bytes)
        .map_err(|error| format!("digest refused: {error:?}"))?;
    let buffer = wasm_bindgen_futures::JsFuture::from(pending)
        .await
        .map_err(|error| format!("digest failed: {error:?}"))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

struct WorkerFile<'a> {
    rpc: &'a CoordRpc,
    worker_fp: &'a str,
    path: &'a str,
}

impl ChunkReader for WorkerFile<'_> {
    async fn read_chunk(&self, offset: u64, len: u32) -> Result<(Vec<u8>, bool), String> {
        let request = ReadFileChunk {
            worker_fp: self.worker_fp.to_owned(),
            path: self.path.to_owned(),
            offset,
            len,
        };
        let chunk = self.rpc.call(&request).await.map_err(|error| error.to_string())?;
        Ok((chunk.data, chunk.eof))
    }
}

impl CleanupRpc for CoordRpc {
    async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        let request = KillSession { session_id: session_id.to_owned(), force: false };
        self.call(&request).await.map(drop).map_err(|error| error.to_string())
    }

    async fn workspace_versions(&self) -> Result<BTreeMap<String, u64>, String> {
        let listed = self.call(&ListWorkspaces).await.map_err(|error| error.to_string())?;
        Ok(listed
            .into_iter()
            .filter_map(|(id, workspace)| u64::try_from(workspace.version).ok().map(|version| (id, version)))
            .collect())
    }

    async fn delete_workspace(&self, workspace_id: &str, version: u64) -> Result<(), String> {
        let request = DeleteWorkspace { id: workspace_id.to_owned(), if_version: version };
        self.call(&request).await.map(drop).map_err(|error| error.to_string())
    }
}

impl FlowHost for SmokeBackdoor {
    fn workers_by_recency(&self) -> Vec<String> {
        let core = self.pump.core();
        let core = core.borrow();
        let mut workers: Vec<(&String, i64)> = core
            .store()
            .workers
            .iter()
            .map(|(fp, worker)| (fp, worker.last_seen_ms))
            .collect();
        workers.sort_by(|left, right| right.1.cmp(&left.1));
        workers.into_iter().map(|(fp, _)| fp.clone()).collect()
    }

    fn has_worker(&self, worker_fp: &str) -> bool {
        self.pump.core().borrow().store().workers.contains_key(worker_fp)
    }

    async fn spawn_shell(&self, worker_fp: &str, folder: &str) -> Result<String, String> {
        let spawned = self.spawn_shell_call(worker_fp, folder, None).await?;
        Ok(spawned["session_id"].as_str().unwrap_or_default().to_owned())
    }

    fn open_session_route(&self, session_id: &str) {
        navigate_through_router(&format!("/s/{session_id}"));
    }

    fn pane_rendered(&self, session_id: &str) -> (bool, usize) {
        let probe = dom::render_probe(session_id);
        (probe.found, probe.non_empty_rows)
    }

    async fn create_workspace(&self, worker_fp: &str, folder: &str, session_id: &str) -> Result<Value, String> {
        self.create_workspace_call(worker_fp, folder, session_id).await
    }

    fn marker_nonce(&self) -> String {
        random_uuid().chars().take(8).collect()
    }

    async fn input(&self, session_id: &str, text: &str) -> Result<(), String> {
        SmokeBackdoor::input(self, session_id, text).await
    }

    async fn wait_for_painted_marker(&self, session_id: &str, marker: &str) -> Result<Value, String> {
        let (proof, _) = wait_for_painted_marker(self, session_id, marker, DEFAULT_PAINT_TIMEOUT_MS).await?;
        Ok(proof)
    }

    async fn terminal_stream_probe(&self, _session_id: &str) -> Result<Value, String> {
        Err(unported_refusal("terminalStreamProbe").unwrap_or_default().to_owned())
    }

    async fn cleanup_created(&self) -> (Value, bool) {
        let report = self.cleanup_created_call().await;
        let clean = report.errors.is_empty();
        (serde_json::to_value(report).unwrap_or(Value::Null), clean)
    }

    async fn next_frame(&self) {
        next_frame().await;
    }
}

impl StressHost for SmokeBackdoor {
    fn deck_size(&self) -> Option<(f64, f64)> {
        let deck = dom::terminal_deck()?;
        let rect = dom::rect_of(&deck);
        Some((rect.width, rect.height))
    }

    fn deck_style(&self) -> Option<String> {
        dom::terminal_deck()?.get_attribute("style")
    }

    fn set_deck_size(&self, width_px: i64, height_px: i64) {
        if let Some(deck) = dom::terminal_deck() {
            let style = deck.style();
            let _ = style.set_property("width", &format!("{width_px}px"));
            let _ = style.set_property("height", &format!("{height_px}px"));
        }
    }

    fn restore_deck_style(&self, original: Option<&str>) {
        if let Some(deck) = dom::terminal_deck() {
            let _ = match original {
                Some(style) => deck.set_attribute("style", style),
                None => deck.remove_attribute("style"),
            };
        }
    }

    fn marker_scan(&self, session_id: &str, prefix: &str) -> SmokeMarkerScan {
        let rows = dom::painted_row_texts(session_id);
        scan_painted_rows(rows.iter().map(String::as_str), prefix)
    }

    fn cell_frame_count(&self, session_id: &str) -> u64 {
        self.pump.core().borrow().store().terminal(session_id).map_or(0, |replica| replica.frame_counts.frames())
    }

    fn renders_cells(&self, session_id: &str) -> bool {
        dom::render_probe(session_id).found
    }

    async fn next_frame(&self) {
        next_frame().await;
    }
}
