//! The sidebar's native decisions: v2 `apps/web/tests/sessionTitle.test.ts`
//! and `machineIdentity.test.ts`, plus the row's exact route match, the swipe
//! release rule and the upward machine-menu anchor from `SessionRow.tsx` and
//! `SidebarNewTerminal.tsx`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientCore;
use roost_protocol::wire::{
    ChannelId, HostIdentity, Session, SessionId, SessionKind, SessionStatus, Worker, WorkerFp,
    WorkerOs,
};
use roost_web::components::machines::machine_identity::{
    LinuxDistributionBrand, machine_identity_presentation,
};
use roost_web::components::sidebar::row_swipe::{RowSwipe, SwipeRelease};
use roost_web::components::sidebar::session_row::session_row_is_active;
use roost_web::components::sidebar::sidebar_new_terminal::machine_menu_anchor;
use roost_web::session_naming::{folder_headline, program_subtitle, session_title};

const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";

fn session(custom_title: Option<&str>) -> Session {
    Session {
        id: SessionId::try_from(SESSION.to_owned()).expect("an id"),
        worker_fp: WorkerFp::try_from(FP.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(1_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: "/home/user/project".to_owned(),
        spawn_cwd: None,
        workspace_id: None,
        status: SessionStatus::Open,
        created_at: 1_000,
        closed_at: None,
        custom_title: custom_title.map(str::to_owned),
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

fn has_lone_surrogate(text: &str) -> bool {
    String::from_utf16(&text.encode_utf16().collect::<Vec<u16>>()).is_err()
}

#[test]
fn an_astral_emoji_straddling_the_cap_is_dropped_whole() {
    let core = ClientCore::in_memory("tab");
    let title = format!("{}\u{1F680}", "a".repeat(79));
    assert_eq!(title.encode_utf16().count(), 81);
    let result = folder_headline(core.store(), &session(Some(&title)));
    assert!(result.encode_utf16().count() <= 80);
    assert!(!has_lone_surrogate(&result) && !result.contains('\u{FFFD}'));
    assert_eq!(result, "a".repeat(79));
}

#[test]
fn a_zwj_family_at_the_cap_is_not_split_mid_cluster() {
    let core = ClientCore::in_memory("tab");
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let title = format!("{}{family}", "b".repeat(75));
    let result = session_title(core.store(), &session(Some(&title)));
    assert_eq!(result, "b".repeat(75));
    assert!(!result.contains('\u{200D}'));
}

#[test]
fn a_short_title_is_unchanged_and_the_osc_title_is_capped() {
    let mut core = ClientCore::in_memory("tab");
    let named = session(Some("hello"));
    assert_eq!(folder_headline(core.store(), &named), "hello");
    assert_eq!(session_title(core.store(), &named), "hello");
    core.store_mut()
        .terminal_titles
        .insert(SESSION.to_owned(), "c".repeat(120));
    let subtitle = program_subtitle(core.store(), &session(None)).expect("an OSC title");
    assert_eq!(subtitle.len(), 80);
    assert_eq!(folder_headline(core.store(), &session(None)), "project");
}

fn worker(os: WorkerOs, model: Option<&str>, chip: Option<&str>, distro: Option<&str>) -> Worker {
    Worker {
        fp: WorkerFp::try_from(FP.to_owned()).expect("a fingerprint"),
        label: "mike-ubuntu-laptop".to_owned(),
        os,
        host_identity: Some(HostIdentity {
            hardware_model: model.map(str::to_owned),
            chip: chip.map(str::to_owned),
            linux_distribution: distro.map(str::to_owned),
        }),
        git_sha: None,
        host_metrics: None,
        registered_at_ms: 1,
        last_seen_ms: 1,
        reachable_addr: None,
        keeper_runtime: None,
        terminal_core_capacity: None,
    }
}

#[test]
fn a_verified_macbook_shows_its_apple_chip() {
    let mac = machine_identity_presentation(Some(&worker(
        WorkerOs::Darwin,
        Some("MacBookPro18,3"),
        Some("Apple M3 Pro"),
        None,
    )));
    assert_eq!((mac.icon, mac.label.as_str()), ("laptop_mac", "MacBook"));
    assert_eq!(mac.apple_chip_badge.as_deref(), Some("M3 Pro"));
    assert_eq!(mac.title, "MacBook · Apple M3 Pro");
    let desktop = machine_identity_presentation(Some(&worker(WorkerOs::Darwin, Some("Mac14,3"), Some("Apple X9"), None)));
    assert_eq!((desktop.icon, desktop.apple_chip_badge), ("desktop_mac", None));
}

#[test]
fn a_distribution_mark_needs_a_recognized_worker_identity_not_a_label() {
    let ubuntu = machine_identity_presentation(Some(&worker(WorkerOs::Linux, None, None, Some("Ubuntu 24.04.3 LTS"))));
    assert_eq!(ubuntu.label, "Ubuntu Linux");
    assert_eq!(ubuntu.linux_brand, Some(LinuxDistributionBrand::Ubuntu));
    let debian = machine_identity_presentation(Some(&worker(WorkerOs::Linux, None, None, Some("Debian GNU/Linux 12"))));
    assert_eq!(debian.linux_brand, Some(LinuxDistributionBrand::Debian));
    let unknown = machine_identity_presentation(Some(&worker(WorkerOs::Linux, None, None, Some("Kestrel OS"))));
    assert_eq!((unknown.label.as_str(), unknown.linux_brand), ("Linux", None));
    let prefix_only = machine_identity_presentation(Some(&worker(WorkerOs::Linux, None, None, Some("Ubuntustudio"))));
    assert_eq!(prefix_only.linux_brand, None);
}

#[test]
fn a_worker_reported_windows_laptop_differs_from_a_desktop() {
    let laptop = machine_identity_presentation(Some(&worker(WorkerOs::Win32, Some("Surface Laptop 7"), None, None)));
    assert_eq!((laptop.icon, laptop.label.as_str()), ("laptop_windows", "Windows laptop"));
    let desktop = machine_identity_presentation(Some(&worker(WorkerOs::Win32, Some("Surface Studio 2"), None, None)));
    assert_eq!((desktop.icon, desktop.label.as_str()), ("desktop_windows", "Windows PC"));
    assert_eq!(machine_identity_presentation(None).label, "Computer");
}

#[test]
fn a_row_is_selected_only_by_its_own_route() {
    assert!(session_row_is_active(&format!("/s/{SESSION}"), SESSION, 1));
    assert!(session_row_is_active(&format!("/s/{SESSION}/detail"), SESSION, 1));
    assert!(!session_row_is_active(&format!("/s/{SESSION}0"), SESSION, 1));
    assert!(session_row_is_active("/w/ws-1/t/1", SESSION, 1));
    assert!(!session_row_is_active("/w/ws-1/t/10", SESSION, 1), "channel 1 is not channel 10");
    assert!(!session_row_is_active("/settings", SESSION, 1));
}

#[test]
fn a_swipe_closes_only_past_the_threshold_and_a_vertical_drag_never_claims() {
    let mut swipe = RowSwipe::default();
    swipe.start(300.0, 100.0);
    assert!(!swipe.track(296.0, 100.0, 400.0), "inside the slop no axis is chosen");
    assert!(swipe.track(250.0, 102.0, 400.0));
    assert_eq!(swipe.release(400.0), SwipeRelease::SpringBack);
    assert_eq!(swipe.offset_x(), 0.0);
    assert!(swipe.take_swiped(), "a drag past a tap suppresses the click");
    assert!(!swipe.take_swiped());

    swipe.start(300.0, 100.0);
    assert!(swipe.track(150.0, 100.0, 400.0));
    assert_eq!(swipe.release(400.0), SwipeRelease::Close);
    assert_eq!(swipe.offset_x(), -400.0);

    let mut vertical = RowSwipe::default();
    vertical.start(300.0, 100.0);
    assert!(!vertical.track(290.0, 160.0, 400.0));
    assert!(!vertical.track(100.0, 160.0, 400.0), "the list keeps a vertical gesture");
    assert_eq!(vertical.release(400.0), SwipeRelease::SpringBack);

    let mut rightward = RowSwipe::default();
    rightward.start(100.0, 100.0);
    rightward.track(300.0, 100.0, 400.0);
    assert_eq!(rightward.offset_x(), 0.0, "left only");
}

#[test]
fn the_machine_menu_opens_upward_from_its_trigger() {
    let anchor = machine_menu_anchor(280.0, 900.0, 932.0, 1_200.0, 1_000.0);
    assert_eq!(anchor.right, 920.0);
    assert_eq!(anchor.bottom, 104.0, "the anchor gap above the trigger's top");
}
