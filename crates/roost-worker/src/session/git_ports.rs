//! Per-session git branch/remote, pull-request and listening-ports facts: each
//! reading the folder watcher takes is compared to the record and, on change,
//! written to it and published as a `git`, `pr` or `ports` session event.
//! Ports v2 `apps/worker/src/session/session-git-ports.ts` (`_startGitBranch`
//! `apply`, the `readGitRemote` continuation, `_resolvePr`, `_resolvePorts`,
//! `_startPorts`). `runtime::heart_owners` attaches it to the manager; the
//! polling itself is `host::sampling::HostWatchers`.

use std::sync::{Arc, Weak};

use roost_host::HostPlatform;
use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::event::SessionEvent;

use super::lifecycle::{SessionManager, SessionTable};
use super::sinks::SessionEventSink;
use crate::host::ports::ports_eq;
use crate::host::sampling::{FolderFactsSink, FolderReading, HostWatchers};

/// The folder facts of every live session, and the watchers that read them.
pub struct SessionFolderFacts {
    watchers: HostWatchers,
    table: Arc<SessionTable>,
    events: Arc<dyn SessionEventSink>,
    clock: Arc<dyn EventClock>,
    platform: HostPlatform,
    /// The worker's runtime: a watcher thread publishes through it.
    runtime: tokio::runtime::Handle,
}

impl std::fmt::Debug for SessionFolderFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionFolderFacts")
            .field("watchers", &self.watchers)
            .finish_non_exhaustive()
    }
}

impl SessionFolderFacts {
    /// Build over `manager` and register on it: a session whose folder is
    /// (re)established starts its watcher, a closed session stops it (v2
    /// `_dropChannelState` clears the watch and both polls).
    pub fn attach(
        manager: &SessionManager,
        platform: HostPlatform,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let facts = Arc::new(Self {
            watchers: HostWatchers::new(),
            table: Arc::clone(&manager.sessions),
            events: Arc::clone(&manager.events),
            clock: Arc::clone(&manager.clock),
            platform,
            runtime,
        });
        let started = Arc::downgrade(&facts);
        manager.on_session_folder(Arc::new(move |session_id, channel_id| {
            if let Some(facts) = started.upgrade() {
                facts.start(session_id, channel_id);
            }
        }));
        let stopped = Arc::downgrade(&facts);
        manager.on_session_closed(Arc::new(move |session_id| {
            if let Some(facts) = stopped.upgrade() {
                facts.stop(session_id);
            }
        }));
        facts
    }

    /// v2 `_startGitBranch` + `_startPorts`: (re)start the session's watcher
    /// over its CURRENT folder and child pid, replacing any prior one.
    pub fn start(self: &Arc<Self>, session_id: &SessionId, channel_id: u16) {
        let Some((folder, root_pid)) = self
            .table
            .with_channel_record(channel_id, |record| {
                (record.session_id() == session_id)
                    .then(|| (record.identity.cwd.clone(), record.child_pid))
            })
            .flatten()
        else {
            tracing::debug!(%session_id, channel_id, "no live record to watch the folder of");
            return;
        };
        self.watchers.stop(session_id.as_str());
        let sink = Arc::new(SessionFacts {
            facts: Arc::downgrade(self),
            session_id: session_id.clone(),
            channel_id,
        });
        self.watchers
            .watch(session_id.as_str(), &folder, root_pid, self.platform, sink);
    }

    /// Stop the session's watcher. `true` when there was one.
    pub fn stop(&self, session_id: &SessionId) -> bool {
        self.watchers.stop(session_id.as_str())
    }

    /// Stop every watcher, at worker shutdown.
    pub fn stop_all(&self) -> usize {
        self.watchers.stop_all()
    }

    /// Whether `session_id` has a running watcher.
    pub fn is_watching(&self, session_id: &SessionId) -> bool {
        self.watchers.is_watching(session_id.as_str())
    }

    /// Apply one reading to the record on `channel_id`, if it is still
    /// `session_id`'s (v2 `sessions.get(rec.channelId) !== rec`), and publish
    /// the event when it changed what the session shows. `true` on a change.
    ///
    /// Must be called off the runtime's threads (the watcher thread): the
    /// publish blocks on the runtime so events leave in the order they happened.
    pub fn apply(&self, session_id: &SessionId, channel_id: u16, reading: FolderReading) -> bool {
        let Some(entry) = self.table.record_of_channel(channel_id) else {
            return false;
        };
        let ts = self.clock.now_epoch_ms();
        let event = {
            let mut record = entry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if record.session_id() != session_id {
                return false;
            }
            match reading {
                // An unresolved branch and a non-repository both read as none,
                // so a folder that is not a repository publishes nothing.
                FolderReading::Branch(branch) => {
                    if record.git_branch.as_ref().and_then(Option::as_ref) == branch.as_ref() {
                        return false;
                    }
                    record.git_branch = Some(branch.clone());
                    SessionEvent::Git {
                        session_id: session_id.clone(),
                        branch,
                        remote: None,
                        ts,
                        trace_id: None,
                    }
                }
                FolderReading::Remote(remote) => {
                    let Some(remote) = remote else {
                        return false;
                    };
                    if record.git_remote.as_ref().and_then(Option::as_ref) == Some(&remote) {
                        return false;
                    }
                    record.git_remote = Some(Some(remote.clone()));
                    SessionEvent::Git {
                        session_id: session_id.clone(),
                        branch: record.git_branch.clone().flatten(),
                        remote: Some(remote),
                        ts,
                        trace_id: None,
                    }
                }
                FolderReading::PullRequest(status) => {
                    if record.pr.as_ref().and_then(Option::as_ref) == status.as_ref() {
                        return false;
                    }
                    record.pr = Some(status.clone());
                    SessionEvent::Pr {
                        session_id: session_id.clone(),
                        number: status.as_ref().map(|pr| i64::from(pr.number)),
                        state: status.as_ref().map(|pr| pr.state),
                        checks: status.as_ref().map(|pr| pr.checks),
                        url: status.map(|pr| pr.url),
                        ts,
                        trace_id: None,
                    }
                }
                // An unsampled record reads as no ports, so a session nobody
                // listens in publishes nothing (v2 `portsEq(x, undefined)`).
                FolderReading::Ports(ports) => {
                    if ports_eq(&ports, record.ports.as_deref().unwrap_or_default()) {
                        return false;
                    }
                    record.ports = Some(ports.clone());
                    SessionEvent::Ports {
                        session_id: session_id.clone(),
                        ports: ports.into_iter().map(i64::from).collect(),
                        ts,
                        trace_id: None,
                    }
                }
            }
        };
        self.publish(session_id, &event);
        true
    }

    /// v2 `_resolvePr`'s guard: the branch to ask about, when the record is
    /// still this session's and both its branch and its GitHub remote are known.
    pub fn pull_request_branch(&self, session_id: &SessionId, channel_id: u16) -> Option<String> {
        self.table
            .with_channel_record(channel_id, |record| {
                if record.session_id() != session_id {
                    return None;
                }
                record.git_remote.as_ref()?.as_ref()?;
                record.git_branch.clone().flatten()
            })
            .flatten()
    }

    fn publish(&self, session_id: &SessionId, event: &SessionEvent) {
        match self.runtime.block_on(self.events.emit(event, None)) {
            Ok(()) => {
                tracing::info!(%session_id, event = ?event, "a session's folder fact changed and was published")
            }
            Err(error) => tracing::warn!(
                %session_id,
                %error,
                "a session's folder fact changed and could not be published"
            ),
        }
    }
}

/// One session's sink, bound to the record generation it was started for.
struct SessionFacts {
    facts: Weak<SessionFolderFacts>,
    session_id: SessionId,
    channel_id: u16,
}

impl FolderFactsSink for SessionFacts {
    fn apply(&self, reading: FolderReading) -> bool {
        self.facts
            .upgrade()
            .is_some_and(|facts| facts.apply(&self.session_id, self.channel_id, reading))
    }

    fn pull_request_branch(&self) -> Option<String> {
        self.facts
            .upgrade()?
            .pull_request_branch(&self.session_id, self.channel_id)
    }
}
