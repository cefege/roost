// Agents-sidebar projection contracts: only retained known statuses become rows.
// Fixtures use the real root-store metadata and folder ordering owners consumed by SidebarAgents.
// Each case resets routability and acknowledgement state so grouping remains deterministic.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  asChannelId,
  asSessionId,
  asWorkerFp,
  type AgentStatus,
  type Session,
  type Worker,
} from "@roost/shared/wire";
import { reconcile } from "solid-js/store";
import { markAgentSeen, resetAgentSeenForTest, seenAgentRevision } from "../src/lib/agentSeen.ts";
import { buildFolderGroups } from "../src/lib/folderGroups.ts";
import { folderKeyOf } from "../src/lib/folderKey.ts";
import { projectSidebarAgentGroups } from "../src/components/sidebar/sidebarAgentsProjection.ts";
import { _projectNavigationSearchDocuments } from "../src/store/navigation-search.ts";
import {
  clearAuthScopedRootData,
  deleteStoreRecord,
  rootStore,
  setRootStore,
} from "../src/store/root.ts";
import { setRoutableFps } from "../src/store/sync-routable.ts";

const FIRST_FP = asWorkerFp("a".repeat(64));
const SECOND_FP = asWorkerFp("b".repeat(64));
const SESSION_A = asSessionId("30000000-0000-4000-8000-000000000001");
const SESSION_B = asSessionId("30000000-0000-4000-8000-000000000002");

type FixtureSessionId = typeof SESSION_A | typeof SESSION_B;

function worker(fp: typeof FIRST_FP | typeof SECOND_FP, label: string): Worker {
  return {
    fp,
    label,
    os: "linux",
    host_identity: null,
    git_sha: null,
    host_metrics: null,
    registered_at_ms: 1,
    last_seen_ms: Date.now(),
    reachable_addr: null,
    keeper_runtime: null,
    terminal_core_capacity: null,
  };
}

function session(id: FixtureSessionId, overrides: Partial<Session> = {}): Session {
  return {
    id,
    worker_fp: FIRST_FP,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/tmp/roost",
    spawn_cwd: "/tmp/roost",
    workspace_id: null,
    status: "open",
    created_at: 1_000,
    closed_at: null,
    custom_title: null,
    ...overrides,
  };
}

function agentStatus(
  sessionId: FixtureSessionId,
  state: AgentStatus["state"],
  revision: number,
  completedRevision = 0,
): AgentStatus {
  return {
    session_id: sessionId,
    agent_id: "omp" as AgentStatus["agent_id"],
    state,
    revision,
    completed_revision: completedRevision,
    updated_at: revision,
    active: true,
    occupant_exited: false,
    status_epoch: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" as AgentStatus["status_epoch"],
    occupant_id: "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa" as AgentStatus["occupant_id"],
    source: "integration",
  };
}

function seedSessions(values: readonly Session[]): void {
  setRootStore("workers", reconcile(Object.fromEntries(
    values.map((value) => [value.worker_fp, worker(value.worker_fp as typeof FIRST_FP | typeof SECOND_FP, value.worker_fp)]),
  )));
  setRootStore("sessions", reconcile(Object.fromEntries(
    values.map((value) => [value.id, value]),
  )));
}

function seedAgentStatuses(values: readonly AgentStatus[]): void {
  setRootStore("agent_status", reconcile(Object.fromEntries(
    values.map((value) => [value.session_id, value]),
  )));
}

function projectAgents(
  sessions: readonly Session[] = Object.values(rootStore.sessions),
  statuses: readonly AgentStatus[] = Object.values(rootStore.agent_status),
) {
  const sessionById = Object.fromEntries(sessions.map((value) => [value.id, value]));
  const statusBySessionId = Object.fromEntries(statuses.map((value) => [value.session_id, value]));
  return projectSidebarAgentGroups({
    documents: _projectNavigationSearchDocuments(),
    folderGroups: buildFolderGroups([...sessions]),
    sessions: sessionById,
    agentStatuses: statusBySessionId,
    seenRevision: seenAgentRevision,
  });
}

beforeEach(() => {
  clearAuthScopedRootData();
  resetAgentSeenForTest();
  setRoutableFps(new Set([FIRST_FP, SECOND_FP]));
});

afterEach(() => {
  clearAuthScopedRootData();
  resetAgentSeenForTest();
  setRoutableFps(new Set<string>());
});

describe("Agents sidebar projection", () => {
  test("omits a document when its retained status is removed", () => {
    const active = agentStatus(SESSION_A, "working", 1);
    seedSessions([session(SESSION_A)]);
    seedAgentStatuses([active]);

    expect(projectAgents([session(SESSION_A)], [active]))
      .toMatchObject([{ rows: [{ document: { sessionId: SESSION_A }, level: "working" }] }]);

    expect(projectAgents([session(SESSION_A)], [])).toEqual([]);
  });

  test("omits a session whose navigation document has unknown agent status", () => {
    seedSessions([session(SESSION_A)]);

    const documents = _projectNavigationSearchDocuments();
    expect(documents).toMatchObject([{ sessionId: SESSION_A, agentStatus: "unknown" }]);
    expect(projectAgents()).toEqual([]);
  });

  test("keeps identical folder paths on distinct workers in their existing group order", () => {
    const first = session(SESSION_A, { cwd: "/tmp/shared", created_at: 1_000 });
    const second = session(SESSION_B, {
      worker_fp: SECOND_FP,
      cwd: "/tmp/shared",
      created_at: 2_000,
    });
    setRootStore("workers", reconcile({
      [FIRST_FP]: worker(FIRST_FP, "First worker"),
      [SECOND_FP]: worker(SECOND_FP, "Second worker"),
    }));
    seedSessions([first, second]);
    seedAgentStatuses([
      agentStatus(SESSION_A, "working", 1),
      agentStatus(SESSION_B, "working", 1),
    ]);

    const groups = projectAgents([first, second], [
      agentStatus(SESSION_A, "working", 1),
      agentStatus(SESSION_B, "working", 1),
    ]);
    expect(groups.map((group) => ({
      key: group.folder.key,
      sessionIds: group.rows.map((row) => row.document.sessionId),
    }))).toEqual([
      { key: folderKeyOf(second), sessionIds: [SESSION_B] },
      { key: folderKeyOf(first), sessionIds: [SESSION_A] },
    ]);
  });

  test("presents an unseen idle completion as done until acknowledgement", () => {
    const completed = agentStatus(SESSION_A, "idle", 4, 4);
    seedSessions([session(SESSION_A)]);
    seedAgentStatuses([completed]);

    expect(projectAgents([session(SESSION_A)], [completed])).toMatchObject([{ rows: [{ level: "done" }] }]);
    markAgentSeen(completed);
    expect(projectAgents([session(SESSION_A)], [completed])).toMatchObject([{ rows: [{ level: "idle" }] }]);
  });
});
