#![cfg(unix)]
//! The always-on byte window and the capture file owner: the window caps at
//! 256 KiB and reports absolute bounds, its tail survives into a written
//! bundle, captures are 0600 in a 0700 directory (tightening one that existed
//! looser), a duplicate id is refused, and retention is COMBINED over legacy
//! `bytecap-*.bin` and incident bundles. Ports `apps/worker/tests/byte-capture.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use capture_support::{AT_MS, RECORDING_ID, process_identity, read_bundle, scratch};
use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureCoverage, TerminalCaptureCoverageReport, TerminalCaptureDropCounters,
    TerminalCaptureLayer, TerminalCaptureProcessIdentity, TerminalCoverageReason,
    TerminalWorkerSamplingStats, TerminalWorkerSection, terminal_capture_file_name,
};
use roost_protocol::terminal_capture::{TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode};
use roost_protocol::viewport::TerminalGeometry;
use roost_worker::capture::bundle_writer::{IncidentBundleInput, write_terminal_incident_bundle};
use roost_worker::capture::byte_window::ByteWindows;
use roost_worker::capture::storage::{CaptureStorage, SweepReserve, sweep_capture_retention};
use serde_json::json;

const RING_CAP: usize = 256 * 1024;
const CAPTURE_A: &str = "aaaaaaaa-0000-4000-8000-00000000aaaa";
const CAPTURE_B: &str = "bbbbbbbb-0000-4000-8000-00000000bbbb";
const SESSION: &str = "11111111-2222-4333-8444-555555555555";

/// A capture directory that does not exist yet, so mkdir's own mode is tested.
struct Scratch {
    root: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let root = scratch(label);
        fs::create_dir_all(&root).unwrap();
        Self {
            dir: root.join("RoostWorker"),
            root,
        }
    }

    fn storage(&self) -> Arc<CaptureStorage> {
        Arc::new(CaptureStorage::new(
            self.dir.clone(),
            tokio::runtime::Handle::current(),
        ))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn age(path: &Path, by: Duration) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - by).unwrap();
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn worker_section() -> TerminalWorkerSection {
    let process = process_identity();
    TerminalWorkerSection {
        layer: TerminalCaptureLayer::Worker,
        captured_at_ms: AT_MS,
        process: TerminalCaptureProcessIdentity {
            layer: TerminalCaptureLayer::Worker,
            process_id: process.process_id,
            git_sha: process.git_sha,
            artifact_version: process.artifact_version,
            wasm_identity: None,
            worker_fp: Some(process.worker_fp),
            viewer_id: None,
            user_agent: None,
        },
        stream: None,
        geometry: Some(TerminalGeometry { cols: 80, rows: 24 }),
        dropped: TerminalCaptureDropCounters::default(),
        omissions: Vec::new(),
        segments: Vec::new(),
        emissions: Vec::new(),
        core_samples: Vec::new(),
        sampling: TerminalWorkerSamplingStats::default(),
        resizes: Vec::new(),
        raw: Vec::new(),
        byte_capture: None,
        core_scrollback_tail: Vec::new(),
        history_rows: Vec::new(),
        history_ranges: Vec::new(),
        scrollback_total: 0,
        scrollback_origin: "0".to_owned(),
    }
}

fn complete() -> TerminalCaptureCoverageReport {
    let complete = || {
        (
            TerminalCaptureCoverage::Complete,
            vec![TerminalCoverageReason::Complete],
        )
    };
    let (
        (cell_replay, cell_replay_reasons),
        (core_replay, core_replay_reasons),
        (core_comparison, core_comparison_reasons),
    ) = (complete(), complete(), complete());
    TerminalCaptureCoverageReport {
        cell_replay,
        cell_replay_reasons,
        core_replay,
        core_replay_reasons,
        core_comparison,
        core_comparison_reasons,
    }
}

#[test]
fn the_window_appends_and_caps_with_absolute_bounds() {
    let mut windows = ByteWindows::default();
    windows.push("sid", &[0xAA; 100_000], 100_000);
    windows.push("sid", &[0xBB; 100_000], 200_000);
    let tail = windows.snapshot("sid").unwrap();
    assert_eq!(
        (
            tail.byte_length,
            tail.end_offset.as_str(),
            tail.start_offset.as_str()
        ),
        (200_000, "200000", "0")
    );

    windows.push("sid", &[0xCC; 200_000], 400_000);
    let capped = windows.snapshot("sid").unwrap();
    assert_eq!(capped.byte_length, RING_CAP as u64);
    assert_eq!(capped.end_offset, "400000");
    assert_eq!(capped.start_offset, (400_000 - RING_CAP).to_string());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&capped.base64)
        .unwrap();
    assert_eq!((bytes[0], bytes[bytes.len() - 1]), (0xBB, 0xCC));
}

#[test]
fn a_dropped_or_unknown_window_reports_nothing() {
    let mut windows = ByteWindows::default();
    windows.push("sid", &[0; 1000], 1000);
    assert!(windows.snapshot("sid").is_some());
    windows.drop_session("sid");
    assert_eq!(windows.snapshot("sid"), None);
    assert_eq!(windows.snapshot("never-pushed"), None);
}

#[tokio::test]
async fn the_retained_raw_tail_survives_into_a_written_incident_bundle() {
    let scratch = Scratch::new("bundle-tail");
    let mut windows = ByteWindows::default();
    windows.push("sid", &[1, 2, 3, 4, 5], 5);
    let worker = TerminalWorkerSection {
        byte_capture: windows.snapshot("sid"),
        ..worker_section()
    };
    let trigger = json!({
        "reason": "manual", "origin": "worker", "at_ms": AT_MS, "stream_id": null, "grid_epoch": null,
        "seq": null, "detail": null, "occurrence_count": 0,
    });
    let input = IncidentBundleInput {
        capture_id: CAPTURE_A.to_owned(),
        recording_id: RECORDING_ID.to_owned(),
        session_id: SESSION.to_owned(),
        written_at_ms: AT_MS,
        trigger: trigger.clone(),
        worker_trigger: trigger,
        coverage: complete(),
        worker,
        coordinator: None,
        browser: None,
    };
    let written = write_terminal_incident_bundle(&scratch.storage(), input)
        .await
        .unwrap();
    let bundle = read_bundle(&written.stored.path.display().to_string()).await;
    assert_eq!(
        bundle["worker"]["byte_capture"],
        json!({ "end_offset": "5", "start_offset": "0", "byte_length": 5, "base64": "AQIDBAU=" })
    );
}

#[tokio::test]
async fn captures_land_owner_only_and_a_looser_existing_dir_is_tightened() {
    let fresh = Scratch::new("owner-only");
    let written = fresh
        .storage()
        .write_terminal_incident_file(CAPTURE_A, b"sec")
        .unwrap();
    assert_eq!((mode(&fresh.dir), mode(&written.path)), (0o700, 0o600));

    let loose = Scratch::new("tighten");
    fs::create_dir_all(&loose.dir).unwrap();
    fs::set_permissions(&loose.dir, fs::Permissions::from_mode(0o755)).unwrap();
    let written = loose
        .storage()
        .write_terminal_incident_file(CAPTURE_B, b"sec")
        .unwrap();
    assert_eq!((mode(&loose.dir), mode(&written.path)), (0o700, 0o600));
}

#[tokio::test]
async fn a_duplicate_capture_id_is_refused_never_overwritten() {
    let scratch = Scratch::new("duplicate");
    let storage = scratch.storage();
    let first = storage
        .write_terminal_incident_file(CAPTURE_A, &[0x01])
        .unwrap();
    assert_eq!(
        storage.write_terminal_incident_file(CAPTURE_A, &[0x02, 0x02]),
        Err(TerminalCaptureErrorCode::StorageFailed)
    );
    assert_eq!(fs::read(&first.path).unwrap(), vec![0x01]);
}

#[tokio::test]
async fn an_unwritable_capture_directory_reports_storage_failed() {
    let scratch = Scratch::new("blocked");
    fs::write(scratch.root.join("blocker"), "x").unwrap();
    let blocked = Arc::new(CaptureStorage::new(
        scratch.root.join("blocker").join("nested"),
        tokio::runtime::Handle::current(),
    ));
    assert_eq!(
        blocked.write_terminal_incident_file(CAPTURE_A, &[0x01]),
        Err(TerminalCaptureErrorCode::StorageFailed)
    );
}

#[test]
fn retention_removes_both_capture_families_past_the_window_and_nothing_else() {
    let scratch = Scratch::new("retention-age");
    fs::create_dir_all(&scratch.dir).unwrap();
    let stale = [
        "bytecap-old-1.bin".to_owned(),
        terminal_capture_file_name(CAPTURE_A),
        "keeper.err.log".to_owned(),
    ];
    let fresh = [
        "bytecap-new-1.bin".to_owned(),
        terminal_capture_file_name(CAPTURE_B),
    ];
    for name in stale.iter().chain(&fresh) {
        fs::write(scratch.dir.join(name), "x").unwrap();
    }
    for name in &stale {
        age(
            &scratch.dir.join(name),
            Duration::from_millis(TERMINAL_CAPTURE_LIMITS.retention_ms + 60_000),
        );
    }
    sweep_capture_retention(&scratch.dir, SweepReserve::default());
    // The stale neighbour log survives: deletion is limited to names this owner made.
    let mut expected = vec![
        "bytecap-new-1.bin".to_owned(),
        "keeper.err.log".to_owned(),
        terminal_capture_file_name(CAPTURE_B),
    ];
    expected.sort();
    assert_eq!(listing(&scratch.dir), expected);
}

#[tokio::test]
async fn the_combined_file_cap_evicts_oldest_first_across_both_families() {
    let scratch = Scratch::new("retention-count");
    fs::create_dir_all(&scratch.dir).unwrap();
    let mut names = Vec::new();
    for idx in 0..TERMINAL_CAPTURE_LIMITS.storage_files + 2 {
        let name = if idx % 2 == 0 {
            format!("bytecap-sid-{idx}.bin")
        } else {
            format!("terminal-incident-00000000-0000-4000-8000-{idx:012}.json.gz")
        };
        fs::write(scratch.dir.join(&name), "x").unwrap();
        age(
            &scratch.dir.join(&name),
            Duration::from_secs(10 * (idx as u64 + 1)),
        );
        names.push(name);
    }
    // The PERIODIC sweep holds the cap exactly; it never evicts for a write
    // nobody issued.
    sweep_capture_retention(&scratch.dir, SweepReserve::default());
    let kept = listing(&scratch.dir);
    assert_eq!(kept.len(), TERMINAL_CAPTURE_LIMITS.storage_files);
    assert!(kept.contains(&names[0]), "the newest survives");
    assert!(
        !kept.contains(names.last().unwrap()),
        "the oldest goes first"
    );

    // The WRITE path reserves one slot for the file it is about to store.
    scratch
        .storage()
        .write_terminal_incident_file(CAPTURE_A, &[0x01])
        .unwrap();
    assert_eq!(
        listing(&scratch.dir).len(),
        TERMINAL_CAPTURE_LIMITS.storage_files
    );
}

#[test]
fn the_combined_byte_cap_evicts_even_when_the_file_count_is_fine() {
    let scratch = Scratch::new("retention-bytes");
    fs::create_dir_all(&scratch.dir).unwrap();
    let huge = scratch.dir.join("bytecap-huge.bin");
    // Sparse: the apparent size crosses the cap without consuming the disk.
    fs::File::create(&huge)
        .unwrap()
        .set_len(TERMINAL_CAPTURE_LIMITS.storage_bytes as u64)
        .unwrap();
    fs::write(scratch.dir.join(terminal_capture_file_name(CAPTURE_B)), "x").unwrap();
    age(&huge, Duration::from_secs(60));
    sweep_capture_retention(&scratch.dir, SweepReserve::default());
    assert_eq!(
        listing(&scratch.dir),
        vec![terminal_capture_file_name(CAPTURE_B)]
    );
}
