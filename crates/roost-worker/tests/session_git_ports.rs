#![cfg(unix)]
//! Pins v2 `apps/worker/src/session/session-git-ports.ts` as the session side
//! of the folder watcher applies it: a reading that changes what the session
//! shows is written to the record and published once (`git`, `pr`, `ports`), an
//! unchanged one is not, a non-repository or an unsampled record publishes
//! nothing, a reading for a record that is no longer this session's is dropped,
//! and the watcher follows the session's lifecycle hooks. Readings are applied
//! from the test thread, off the runtime, as the watcher thread applies them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "session_support/mod.rs"]
mod session_support;

use std::sync::Arc;

use roost_host::HostPlatform;
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::{PullRequestChecks, PullRequestState};
use roost_worker::host::PrStatus;
use roost_worker::host::sampling::FolderReading;
use roost_worker::session::git_ports::SessionFolderFacts;
use session_support::{Harness, OTHER, SESSION, session_id};

struct Fixture {
    harness: Harness,
    facts: Arc<SessionFolderFacts>,
    runtime: tokio::runtime::Runtime,
}

fn fixture() -> Fixture {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let harness = Harness::new();
    harness.install(SESSION, 7, "/tmp", "/tmp");
    let facts = SessionFolderFacts::attach(
        &harness.manager,
        HostPlatform::Linux,
        runtime.handle().clone(),
    );
    Fixture {
        harness,
        facts,
        runtime,
    }
}

impl Fixture {
    fn apply(&self, reading: FolderReading) -> bool {
        self.facts.apply(&session_id(SESSION), 7, reading)
    }

    fn folder_events(&self) -> Vec<SessionEvent> {
        self.harness
            .sink
            .published()
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    SessionEvent::Git { .. } | SessionEvent::Pr { .. } | SessionEvent::Ports { .. }
                )
            })
            .collect()
    }
}

fn pull_request(checks: PullRequestChecks) -> PrStatus {
    PrStatus {
        number: 412,
        state: PullRequestState::Open,
        checks,
        url: "https://github.com/o/r/pull/412".to_string(),
    }
}

#[test]
fn a_changed_branch_publishes_one_git_event_and_a_repeat_publishes_none() {
    let fixture = fixture();
    assert!(
        !fixture.apply(FolderReading::Branch(None)),
        "a non-repository published a branch"
    );
    assert!(fixture.apply(FolderReading::Branch(Some("main".to_string()))));
    assert!(!fixture.apply(FolderReading::Branch(Some("main".to_string()))));
    let events = fixture.folder_events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        SessionEvent::Git { branch: Some(branch), remote: None, .. } if branch == "main"
    ));
    assert!(
        fixture.apply(FolderReading::Branch(None)),
        "leaving the repository is a change"
    );
    let branch = fixture
        .harness
        .table
        .with_record(&session_id(SESSION), |record| record.git_branch.clone())
        .unwrap();
    assert_eq!(branch, Some(None));
}

#[test]
fn a_github_remote_is_published_with_the_branch_and_unlocks_the_pull_request() {
    let fixture = fixture();
    let session = session_id(SESSION);
    fixture.apply(FolderReading::Branch(Some("feature".to_string())));
    assert_eq!(
        fixture.facts.pull_request_branch(&session, 7),
        None,
        "no remote, no PR lookup"
    );
    assert!(!fixture.apply(FolderReading::Remote(None)));
    assert!(fixture.apply(FolderReading::Remote(Some("o/r".to_string()))));
    assert!(!fixture.apply(FolderReading::Remote(Some("o/r".to_string()))));
    let events = fixture.folder_events();
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Git { branch: Some(branch), remote: Some(remote), .. })
            if branch == "feature" && remote == "o/r"
    ));
    assert_eq!(
        fixture.facts.pull_request_branch(&session, 7).as_deref(),
        Some("feature")
    );
}

#[test]
fn a_pull_request_change_publishes_its_fields_and_its_disappearance_publishes_nulls() {
    let fixture = fixture();
    assert!(
        !fixture.apply(FolderReading::PullRequest(None)),
        "no PR before and after is no change"
    );
    assert!(fixture.apply(FolderReading::PullRequest(Some(pull_request(
        PullRequestChecks::Pending
    )))));
    assert!(!fixture.apply(FolderReading::PullRequest(Some(pull_request(
        PullRequestChecks::Pending
    )))));
    assert!(fixture.apply(FolderReading::PullRequest(Some(pull_request(
        PullRequestChecks::Passing
    )))));
    assert!(fixture.apply(FolderReading::PullRequest(None)));
    let events = fixture.folder_events();
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        SessionEvent::Pr {
            number: Some(412),
            state: Some(PullRequestState::Open),
            checks: Some(PullRequestChecks::Pending),
            url: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &events[2],
        SessionEvent::Pr {
            number: None,
            state: None,
            checks: None,
            url: None,
            ..
        }
    ));
}

#[test]
fn ports_publish_only_on_change_and_an_unsampled_record_equals_none_listening() {
    let fixture = fixture();
    assert!(!fixture.apply(FolderReading::Ports(Vec::new())));
    assert!(fixture.apply(FolderReading::Ports(vec![5173, 8080])));
    assert!(!fixture.apply(FolderReading::Ports(vec![5173, 8080])));
    assert!(fixture.apply(FolderReading::Ports(Vec::new())));
    let events = fixture.folder_events();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], SessionEvent::Ports { ports, .. } if *ports == vec![5173, 8080]));
    assert!(matches!(&events[1], SessionEvent::Ports { ports, .. } if ports.is_empty()));
}

/// v2 `sessions.get(rec.channelId) !== rec`: a watcher started for one record
/// generation cannot write into another session that now owns the channel.
#[test]
fn a_reading_for_a_channel_now_held_by_another_session_is_dropped() {
    let fixture = fixture();
    let other = session_id(OTHER);
    assert!(
        !fixture
            .facts
            .apply(&other, 7, FolderReading::Branch(Some("main".to_string())))
    );
    assert!(
        !fixture
            .facts
            .apply(&session_id(SESSION), 9, FolderReading::Ports(vec![22]))
    );
    assert!(fixture.folder_events().is_empty());
}

#[test]
fn the_folder_hook_starts_a_watcher_and_the_session_closing_stops_it() {
    let fixture = fixture();
    let session = session_id(SESSION);
    assert!(!fixture.facts.is_watching(&session));
    fixture.harness.manager.notify_session_folder(&session, 7);
    assert!(fixture.facts.is_watching(&session));
    fixture
        .runtime
        .block_on(fixture.harness.manager.close_channel(7, Some(0)))
        .unwrap();
    assert!(
        !fixture.facts.is_watching(&session),
        "a closed session kept its folder watcher"
    );
}
