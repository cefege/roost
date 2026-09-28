//! Fleet target resolution and the participant/deferred split for `roost push`.
//! Called by the push command body, by the keeper admission module and by the
//! rollout; depends on the worker roster shape `status` already publishes and
//! on nothing else in the crate.
//!
//! Two rules are the whole of this file, and both are refusals rather than
//! conveniences. A target is matched against the registry by FINGERPRINT first
//! and by label or reachable address second, and a target matching more than one
//! row is refused rather than guessed: an operator who typed a short hostname
//! must never have one of two identically named machines silently chosen. And a
//! machine this rollout cannot converge now is DEFERRED with a reason, never
//! failed, because one sleeping laptop is not a reason to keep the rest of the
//! fleet off a commit.

use std::collections::BTreeSet;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::status::report::WorkerStatus;

/// A refusal the operator caused, carrying the exit code its own cause earns.
///
/// Codes come from `deploy::codes` and are never inlined, because `roost push`
/// and `roost deploy` share them and a wrapper that reads one number from one
/// command and a different number from the other retries the wrong thing.
///
/// Every refusal in this file is a MALFORMED ARGUMENT, and `codes::REJECTED_INVOCATION`
/// is the constant for that: `docs/phase6-cli-contract.md` § Exit codes gives
/// code 2 the words "a usage error: a bad flag, a malformed argument, an unknown
/// verb", which is exactly what an ambiguous or unsafe target is. The constant is
/// named for the transport because it doubles as clap's own code for a rejected
/// invocation, and a target the operator typed that this process will not dial is
/// a rejected invocation either way. The contract is the authority here; the
/// constant's comment is the module's gloss on it, and where the two read
/// differently the contract wins — which is also what v2 shipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRefusal {
    pub code: u8,
    pub message: String,
}

impl PushRefusal {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The refusal as the one error every `roost` command returns.
    pub fn into_failure(self) -> CommandFailure {
        CommandFailure::new(self.code, self.message)
    }
}

/// A target of the rollout: the machine's registry identity and the address ssh
/// reaches it at. Both halves are needed and they are different: the fingerprint
/// is what a journal records and what a keeper action is addressed to, and the
/// address is what this process dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRolloutTarget {
    pub fingerprint: String,
    pub host: String,
}

/// One machine of the registry.
pub type RegistryWorker<'a> = &'a WorkerStatus;

/// What one target name resolved to, including the answer "not uniquely".
///
/// `ambiguous` is a separate value rather than a `None` worker because the two
/// refusals an operator needs are different sentences — "no such machine" and
/// "that name is two machines" — and collapsing them into one `None` would print
/// the wrong remedy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetMatch<'a> {
    pub worker: Option<RegistryWorker<'a>>,
    pub ambiguous: bool,
}

/// A worker fingerprint in full: 64 lowercase hex characters.
///
/// A short or mixed-case fingerprint is refused rather than normalised, because
/// it is the coordinator's own identity column and a value that shape did not
/// come from a worker this build can address.
pub fn is_full_worker_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A host name reduced to the form two registries agree on: trimmed, without a
/// trailing root dot, lowercased.
///
/// Re-implemented here rather than imported from v2's Windows deploy channel,
/// which v3 does not ship; the rule is three lines and it is the rule every
/// comparison in this file depends on.
pub fn normalized_host(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_suffix('.')
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

/// The first label of a host, which is what an operator types.
fn host_label(value: &str) -> String {
    normalized_host(value)
        .split('.')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A target this process is willing to hand to ssh, or `None`.
///
/// The character set is ssh's: a leading alphanumeric and then alphanumerics,
/// dot, underscore, colon and dash. Anything else — a space, a slash, a shell
/// metacharacter, a leading dash that would read as an option — is refused here
/// rather than quoted and hoped for.
pub fn safe_ssh_target(value: &str) -> Option<String> {
    let target = value.trim();
    let mut characters = target.chars();
    let first = characters.next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    let permitted =
        characters.all(|character| character.is_ascii_alphanumeric() || "._:-".contains(character));
    permitted.then(|| target.to_string())
}

/// Resolve one target name against the registry: fingerprint first, then an
/// exact label or reachable address, then the same as a bare host label.
pub fn resolve_worker_target<'a>(workers: &'a [WorkerStatus], target: &str) -> TargetMatch<'a> {
    let wanted = normalized_host(target);
    let by_fingerprint: Vec<RegistryWorker<'a>> = workers
        .iter()
        .filter(|worker| normalized_host(&worker.fingerprint) == wanted)
        .collect();
    if let Some(matched) = exact(by_fingerprint) {
        return matched;
    }
    let by_address: Vec<RegistryWorker<'a>> = workers
        .iter()
        .filter(|worker| {
            addresses(worker)
                .into_iter()
                .any(|value| normalized_host(value) == wanted)
        })
        .collect();
    if let Some(matched) = exact(by_address) {
        return matched;
    }
    let label = host_label(&wanted);
    let by_label: Vec<RegistryWorker<'a>> = workers
        .iter()
        .filter(|worker| {
            addresses(worker)
                .into_iter()
                .any(|value| host_label(value) == label)
        })
        .collect();
    exact(by_label).unwrap_or(TargetMatch {
        worker: None,
        ambiguous: false,
    })
}

fn exact<'a>(matches: Vec<RegistryWorker<'a>>) -> Option<TargetMatch<'a>> {
    match matches.as_slice() {
        [] => None,
        [worker] => Some(TargetMatch {
            worker: Some(worker),
            ambiguous: false,
        }),
        _ => Some(TargetMatch {
            worker: None,
            ambiguous: true,
        }),
    }
}

/// The two names an operator may know a machine by, and the address the rest of
/// the fleet reaches it at.
fn addresses(worker: &WorkerStatus) -> Vec<&str> {
    [
        Some(worker.label.as_str()),
        worker.reachable_addr.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The whole registry as rollout candidates, refusing a target that resolves to
/// nothing, to more than one machine, or not to a full fingerprint.
///
/// The roster IS the target set: a push with no arguments is an atomic
/// whole-fleet transaction, and a machine the operator does not name is a
/// machine that would be left on the old commit for reasons nobody printed.
pub fn resolve_push_targets(
    workers: &[WorkerStatus],
) -> Result<Vec<FleetRolloutTarget>, PushRefusal> {
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for worker in workers {
        let host = worker
            .reachable_addr
            .clone()
            .unwrap_or_else(|| worker.label.clone());
        let resolved = resolve_worker_target(workers, &host);
        if resolved.ambiguous {
            return Err(PushRefusal::new(
                codes::REJECTED_INVOCATION,
                format!(
                    "ambiguous push target {host:?}: it matches more than one registered worker; \
                     use the exact full address"
                ),
            ));
        }
        let row = match resolved.worker {
            Some(row) => row,
            None => {
                return Err(PushRefusal::new(
                    codes::REJECTED_INVOCATION,
                    format!("{host}: missing from the coordinator worker inventory"),
                ));
            }
        };
        if !is_full_worker_fingerprint(&row.fingerprint) {
            return Err(PushRefusal::new(
                codes::REJECTED_INVOCATION,
                format!(
                    "{}: the coordinator reported a worker fingerprint that is not a full 64-hex \
                     identity",
                    row.label
                ),
            ));
        }
        let safe = safe_ssh_target(&host).ok_or_else(|| {
            PushRefusal::new(
                codes::REJECTED_INVOCATION,
                format!("invalid ssh deployment target {host:?}"),
            )
        })?;
        if seen.insert(normalized_host(&safe)) {
            targets.push(FleetRolloutTarget {
                fingerprint: row.fingerprint.clone(),
                host: safe,
            });
        }
    }
    if targets.is_empty() {
        return Err(PushRefusal::new(
            codes::REJECTED_INVOCATION,
            "atomic push requires at least one registered worker".to_string(),
        ));
    }
    Ok(targets)
}

/// Identity integrity over the WHOLE registry.
///
/// A duplicate or malformed fingerprint makes every per-machine proof ambiguous
/// — two rows claiming one machine is a registry that cannot be addressed at all
/// — so it refuses the push outright. Version skew is not an identity defect and
/// only defers a machine.
pub fn fleet_worker_identity_problems(workers: &[WorkerStatus]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for worker in workers {
        if !seen.insert(worker.fingerprint.as_str()) {
            problems.push(format!(
                "{}: duplicate coordinator worker identity",
                worker.fingerprint
            ));
            continue;
        }
        if !is_full_worker_fingerprint(&worker.fingerprint) {
            problems.push(format!("{}: invalid worker fingerprint", worker.label));
        }
    }
    problems
}

/// A registered machine this rollout leaves alone, with the reason printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredFleetWorker {
    pub fingerprint: String,
    pub label: String,
    pub reason: String,
}

/// The split the rollout is built from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetRolloutPartition {
    pub participants: Vec<FleetRolloutTarget>,
    pub deferred: Vec<DeferredFleetWorker>,
}

/// Split the candidates into the machines this push converges now and the ones
/// it defers to their own catch-up.
///
/// A participant must be reachable, heartbeat-fresh and already on `prior_sha`.
/// The third is the load-bearing one: a participant's deploy is proved against
/// the commit the fleet is leaving, so admitting a machine that has drifted
/// drags the whole fleet into a rollback to fix one machine that was never on
/// this rollout to begin with.
pub fn partition_fleet_for_rollout(
    candidates: &[FleetRolloutTarget],
    inventory: &[WorkerStatus],
    routable: &BTreeSet<String>,
    prior_sha: &str,
) -> FleetRolloutPartition {
    let mut partition = FleetRolloutPartition::default();
    for candidate in candidates {
        let worker = inventory
            .iter()
            .find(|row| row.fingerprint == candidate.fingerprint);
        let reason = match worker {
            None => Some("no longer registered".to_string()),
            Some(_) if !routable.contains(&candidate.fingerprint) => {
                Some("not reachable".to_string())
            }
            Some(row) if row.stale => Some("stale".to_string()),
            Some(row) if row.git_sha.as_deref() != Some(prior_sha) => Some(format!(
                "reports {}, prior is {}",
                short_sha(row.git_sha.as_deref()),
                short_sha(Some(prior_sha))
            )),
            Some(_) => None,
        };
        match reason {
            None => partition.participants.push(candidate.clone()),
            Some(reason) => partition.deferred.push(DeferredFleetWorker {
                fingerprint: candidate.fingerprint.clone(),
                label: worker.map_or_else(|| candidate.host.clone(), |row| row.label.clone()),
                reason,
            }),
        }
    }
    partition
}

fn short_sha(sha: Option<&str>) -> String {
    match sha {
        None => "no SHA".to_string(),
        Some(sha) => sha.chars().take(8).collect(),
    }
}

/// The operator's whole view of a partial fleet, on stdout beside a successful
/// push: which machines this push left alone, why, and how each one catches up.
///
/// The phrase is `update pending` and it is deliberately NOT the constant in
/// `status::update_state` that words a machine's row as `Update pending —
/// offline`. That one classifies a machine against the running coordinator; this
/// one states that a whole-fleet rollout left it for its own catch-up. Same
/// words, different facts, so they never share a string.
pub fn deferred_fleet_report_lines(deferred: &[DeferredFleetWorker]) -> Vec<String> {
    if deferred.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "\n>> {} machine{} deferred — update pending:",
        deferred.len(),
        if deferred.len() == 1 { "" } else { "s" }
    )];
    lines.extend(
        deferred
            .iter()
            .map(|machine| format!("   {}: {}", machine.label, machine.reason)),
    );
    lines.push(
        "   Each updates automatically when it next attaches to the coordinator, or immediately \
         with `roost deploy <host>`."
            .to_string(),
    );
    lines
}
