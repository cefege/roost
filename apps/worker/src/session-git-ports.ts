// Per-session git branch/remote + PR + listening-ports resolution & polling.
// Split out of session-manager.ts (400-line cap); bodies byte-for-byte
// unchanged, called with a SessionManager `this`.

import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import { readGitBranch, watchGitBranch, readGitRemote } from "./git-branch.ts";
import { readGitPr, prStatusEq } from "./pr-status.ts";
import { readListeningPorts, portsEq } from "./listening-ports.ts";

/** Resolve the session's git branch, emit a `git` event on change, and watch
 *  .git/HEAD for later checkouts. Idempotent per cwd — restart on cwd-change
 *  by disposing the prior watch first. Non-repo folders resolve to null and
 *  emit nothing (git_branch stays absent). See git-branch.ts. */
export function _startGitBranch(this: SessionManager, rec: SessionRecord): void {
	// PR poll lifecycle rides here too (same spawn + cwd-change restart path).
	// Clear a prior timer so cwd-change (dispose+restart) doesn't stack polls.
	if (rec.prPollTimer) {
		clearInterval(rec.prPollTimer);
		rec.prPollTimer = null;
	}
	const apply = (branch: string | null) => {
		// Skip if the session closed while git resolved, or nothing changed
		// (undefined initial + null resolve = non-repo → no noise event).
		if (this.sessions.get(rec.channelId) !== rec) return;
		if (branch === (rec.git_branch ?? null)) return;
		rec.git_branch = branch;
		this.emitEvent({
			kind: "git",
			session_id: rec.sessionId,
			branch,
			ts: Date.now(),
		});
		void this._resolvePr(rec); // branch changed → PR for the new branch differs
	};
	void readGitBranch(rec.cwd).then(apply);
	void watchGitBranch(rec.cwd, apply).then((dispose) => {
		if (this.sessions.get(rec.channelId) === rec)
			rec.gitWatchDispose = dispose;
		else dispose();
	});
	// GitHub owner/repo — stable, resolve once. Emit a git event carrying the
	// current branch + the remote so bare #123 / commit-SHA links can resolve.
	void readGitRemote(rec.cwd).then((remote) => {
		if (this.sessions.get(rec.channelId) !== rec) return;
		if (!remote || remote === (rec.git_remote ?? null)) return;
		rec.git_remote = remote;
		this.emitEvent({
			kind: "git",
			session_id: rec.sessionId,
			branch: rec.git_branch ?? null,
			remote,
			ts: Date.now(),
		});
		void this._resolvePr(rec); // github repo confirmed → resolve PR now
	});
	// 90s PR-status poll: re-runs `gh pr list` so a merge / new check result
	// reflects without a branch change. Skips itself when the branch/repo isn't
	// resolved yet (_resolvePr no-ops). Cleared on close + cwd-change.
	rec.prPollTimer = setInterval(() => void this._resolvePr(rec), 90_000);
}

/** Start the listening-ports poll for a session: resolve now + every 90s.
 *  Clears a prior timer (cwd-change restart). Cleared on close. */
export function _startPorts(this: SessionManager, rec: SessionRecord): void {
	if (rec.portsPollTimer) {
		clearInterval(rec.portsPollTimer);
		rec.portsPollTimer = null;
	}
	void this._resolvePorts(rec);
	rec.portsPollTimer = setInterval(
		() => void this._resolvePorts(rec),
		90_000,
	);
}

/** Resolve the session process tree's LISTEN ports and emit a `ports` event
 *  on change. [] when nothing listens / lsof unavailable. */
export async function _resolvePorts(this: SessionManager, rec: SessionRecord): Promise<void> {
	if (this.sessions.get(rec.channelId) !== rec) return;
	const ports = await readListeningPorts(rec.childPid);
	if (this.sessions.get(rec.channelId) !== rec) return;
	if (portsEq(ports, rec.ports)) return;
	rec.ports = ports;
	this.emitEvent({
		kind: "ports",
		session_id: rec.sessionId,
		ports,
		ts: Date.now(),
	});
}

/** Resolve the session branch's GitHub PR status and emit a `pr` event on
 *  change. No-op unless the folder is a resolved github repo with a branch.
 *  Every gh failure yields null → the badge clears / stays absent. */
export async function _resolvePr(this: SessionManager, rec: SessionRecord): Promise<void> {
	if (this.sessions.get(rec.channelId) !== rec) return;
	const branch = rec.git_branch;
	if (!branch || !rec.git_remote) return; // not a github repo / branch unknown
	const status = await readGitPr(rec.cwd, branch);
	if (this.sessions.get(rec.channelId) !== rec) return;
	if (prStatusEq(status, rec.pr)) return;
	rec.pr = status;
	this.emitEvent({
		kind: "pr",
		session_id: rec.sessionId,
		number: status?.number ?? null,
		state: status?.state ?? null,
		checks: status?.checks ?? null,
		url: status?.url ?? null,
		ts: Date.now(),
	});
}
