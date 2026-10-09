//! The sidebar's machine hand-offs: which platform offers what in which menu,
//! the host check, the `vnc://` / `smb://` hrefs, the UNC path and the `.rdp`
//! file. Ports v2 `apps/web/tests/machineActions.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::{Worker, WorkerFp, WorkerOs};
use roost_web::machine_actions::{
    InvalidMachineAddress, MachineAction, MachineLaunch, MachineMenuKind, checked_host,
    machine_actions, machine_launch, reachable_host, remote_desktop_file, remote_desktop_file_name,
};

const HOST: &str = "mac-mini.tail1234.ts.net";

fn worker(os: WorkerOs, reachable_addr: Option<&str>) -> Worker {
    Worker {
        fp: WorkerFp::try_from("a".repeat(64)).expect("a fingerprint"),
        label: "worker-mac-mini".to_owned(),
        os,
        host_identity: None,
        git_sha: None,
        host_metrics: None,
        registered_at_ms: 1,
        last_seen_ms: 1,
        reachable_addr: reachable_addr.map(str::to_owned),
        keeper_runtime: None,
        terminal_core_capacity: None,
    }
}

#[test]
fn each_platform_offers_its_own_hand_offs_per_menu() {
    use MachineAction::{CopyNetworkSharePath, OpenInFinder, RemoteDesktop, ScreenSharing};
    use MachineMenuKind::{Folder, Session};
    assert_eq!(machine_actions(WorkerOs::Darwin, Folder), &[ScreenSharing]);
    assert_eq!(
        machine_actions(WorkerOs::Darwin, Session),
        &[OpenInFinder, ScreenSharing]
    );
    assert_eq!(machine_actions(WorkerOs::Win32, Folder), &[RemoteDesktop]);
    assert_eq!(
        machine_actions(WorkerOs::Win32, Session),
        &[CopyNetworkSharePath, RemoteDesktop]
    );
    assert!(machine_actions(WorkerOs::Linux, Folder).is_empty());
    assert!(machine_actions(WorkerOs::Linux, Session).is_empty());
}

#[test]
fn labels_and_test_ids_match_v2() {
    let named: Vec<_> = [
        MachineAction::OpenInFinder,
        MachineAction::ScreenSharing,
        MachineAction::CopyNetworkSharePath,
        MachineAction::RemoteDesktop,
    ]
    .iter()
    .map(|action| (action.label(), action.test_id()))
    .collect();
    assert_eq!(
        named,
        [
            ("Open in Finder", "finder"),
            ("Screen sharing", "screen-share"),
            ("Copy network share path", "network-share"),
            ("Remote Desktop", "remote-desktop"),
        ]
    );
}

#[test]
fn only_the_reported_address_is_a_host_never_the_label() {
    assert_eq!(
        reachable_host(&worker(WorkerOs::Darwin, Some(HOST))),
        Some(HOST)
    );
    assert_eq!(reachable_host(&worker(WorkerOs::Darwin, None)), None);
    assert_eq!(reachable_host(&worker(WorkerOs::Darwin, Some("  "))), None);
}

#[test]
fn the_host_check_refuses_anything_that_is_not_a_bare_host() {
    assert_eq!(checked_host(&format!("  {HOST} ")), Ok(HOST));
    assert_eq!(checked_host("100.101.102.103"), Ok("100.101.102.103"));
    for refused in [
        "",
        "   ",
        "host/path",
        "host\\share",
        "host\r\nfull address:s:evil",
        "host\nx",
        "two words",
        "user@host",
        "host?query",
        "host#fragment",
        "host\u{7}",
    ] {
        assert_eq!(
            checked_host(refused),
            Err(InvalidMachineAddress(refused.trim().to_owned())),
            "{refused:?} must be refused"
        );
    }
}

#[test]
fn the_mac_hand_offs_are_vnc_and_smb_urls() {
    assert_eq!(
        machine_launch(MachineAction::ScreenSharing, HOST),
        Ok(MachineLaunch::Navigate {
            href: format!("vnc://{HOST}")
        })
    );
    assert_eq!(
        machine_launch(MachineAction::OpenInFinder, HOST),
        Ok(MachineLaunch::Navigate {
            href: format!("smb://{HOST}")
        })
    );
}

#[test]
fn the_windows_share_is_a_unc_root_on_the_clipboard() {
    assert_eq!(
        machine_launch(
            MachineAction::CopyNetworkSharePath,
            "win-box.tail1234.ts.net"
        ),
        Ok(MachineLaunch::CopyText {
            text: "\\\\win-box.tail1234.ts.net\\".to_owned()
        })
    );
}

#[test]
fn remote_desktop_downloads_a_crlf_rdp_file_named_for_the_host() {
    let expected = "full address:s:win-box.tail1234.ts.net\r\n\
                    prompt for credentials:i:1\r\n\
                    authentication level:i:2\r\n\
                    redirectclipboard:i:1\r\n";
    assert_eq!(
        remote_desktop_file("win-box.tail1234.ts.net").as_deref(),
        Ok(expected)
    );
    assert_eq!(
        machine_launch(MachineAction::RemoteDesktop, "win-box.tail1234.ts.net"),
        Ok(MachineLaunch::Download {
            file_name: "win-box.tail1234.ts.net.rdp".to_owned(),
            mime_type: "application/x-rdp",
            contents: expected.to_owned(),
        })
    );
    assert_eq!(
        remote_desktop_file_name("[fd7a::1]:3389"),
        "-fd7a-1-3389.rdp"
    );
}

#[test]
fn every_launch_refuses_an_unsafe_host() {
    for action in [
        MachineAction::OpenInFinder,
        MachineAction::ScreenSharing,
        MachineAction::CopyNetworkSharePath,
        MachineAction::RemoteDesktop,
    ] {
        assert!(
            machine_launch(action, "host\r\nevil").is_err(),
            "{action:?} must refuse a line break"
        );
    }
}
