// Navigation-search projection contracts: scalar metadata, shared matching, and attention semantics.
// Tests drive the real root-store, routability, title, and seen owners consumed by the app-lifetime accessor.
// Every fixture is dashboard-local and reset between cases so ordering assertions stay deterministic.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  asChannelId,
  asSessionId,
  asWorkerFp,
  asWorkspaceId,
  type AgentStatus,
  type Session,
  type Worker,
  type Workspace,
} from "@roost/shared/wire";
import { reconcile } from "solid-js/store";
import { markAgentSeen, resetAgentSeenForTest } from "../src/lib/agentSeen.ts";
import {
  _projectNavigationSearchDocuments,
  attentionNavigationDocuments,
  filterNavigationSearchDocuments,
  matchesNavigationSearchDocument,
  normalizeNavigationSearchQuery,
} from "../src/store/navigation-search.ts";
import { clearDashboardScopedRootData, setRootStore } from "../src/store/root.ts";
import { setRoutableFps } from "../src/store/sync-routable.ts";

const ONLINE_FP = asWorkerFp("a".repeat(64));
const OFFLINE_FP = asWorkerFp("b".repeat(64));
const WORKSPACE_ID = asWorkspaceId("10000000-0000-4000-8000-000000000001");
const SESSION_A = asSessionId("20000000-0000-4000-8000-000000000001");
const SESSION_B = asSessionId("20000000-0000-4000-8000-000000000002");
const SESSION_C = asSessionId("20000000-0000-4000-8000-000000000003");
const SESSION_D = asSessionId("20000000-0000-4000-8000-000000000004");
const SESSION_E = asSessionId("20000000-0000-4000-8000-000000000005");

function worker(fp: typeof ONLINE_FP, label: string): Worker {
  return {
    fp,
    label,
    os: "linux",
    git_sha: null,
    host_metrics: null,
    registered_at_ms: 1,
    last_seen_ms: Date.now(),
    reachable_addr: null,
    keeper_runtime: null,
  };
}

function session(
  id: typeof SESSION_A,
  overrides: Partial<Session> = {},
): Session {
  return {
    id,
    worker_fp: ONLINE_FP,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/srv/roost",
    spawn_cwd: "/srv/roost",
    workspace_id: null,
    status: "open",
    created_at: 1_000,
    closed_at: null,
    custom_title: null,
    ...overrides,
  };
}

function workspace(): Workspace {
  return {
    id: WORKSPACE_ID,
    worker_fp: ONLINE_FP,
    name: "Search Workspace",
    folder_path: "/srv/roost",
    color: null,
    position: 0,
    version: 1,
    created_at_ms: 1,
    updated_at_ms: 1,
    session_ids: [SESSION_A],
  };
}

function agentStatus(
  sessionId: typeof SESSION_A,
  state: AgentStatus["state"],
  revision: number,
  completedRevision: number,
  updatedAt: number,
  message?: string,
): AgentStatus {
  return {
    session_id: sessionId,
    agent_id: "omp" as AgentStatus["agent_id"],
    state,
    message,
    revision,
    completed_revision: completedRevision,
    updated_at: updatedAt,
    active: true,
  };
}

function seedSessions(values: readonly Session[]): void {
  setRootStore("sessions", reconcile(Object.fromEntries(
    values.map((value) => [value.id, value]),
  )));
}

function seedAgentStatuses(values: readonly AgentStatus[]): void {
  setRootStore("agent_status", reconcile(Object.fromEntries(
    values.map((value) => [value.session_id, value]),
  )));
}

beforeEach(() => {
  clearDashboardScopedRootData();
  resetAgentSeenForTest();
  setRoutableFps(new Set([ONLINE_FP]));
});

afterEach(() => {
  clearDashboardScopedRootData();
  resetAgentSeenForTest();
  setRoutableFps(new Set<string>());
});

describe("navigation search metadata", () => {
  test("normalizes compatibility, case, and whitespace once for every matcher", () => {
    expect(normalizeNavigationSearchQuery("  ＲＯＯＳＴ\n  Main  ")).toBe("roost main");
  });

  test("projects every named metadata field as a detached scalar search row", () => {
    setRootStore("workers", ONLINE_FP, {
      ...worker(ONLINE_FP, "Build Machine"),
      reachable_addr: "build.example.test",
    });
    setRootStore("workspaces", WORKSPACE_ID, {
      ...workspace(),
      folder_path: "/srv/roost/packages/web",
    });
    seedSessions([session(SESSION_A, {
      cwd: "/srv/roost/packages/web",
      spawn_cwd: "/srv/roost",
      workspace_id: null,
      custom_title: "Release shell",
      git_branch: "feature/search",
      git_remote: "acme/roost",
      pr_number: 417,
      pr_state: "draft",
      pr_checks: "pending",
      pr_url: "https://github.com/acme/roost/pull/417",
      ports: [5174, 3000, 5174],
    })]);
    setRootStore("terminal_title", SESSION_A, "vite — web");
    setRootStore("last_activity", SESSION_A, 9_000);
    seedAgentStatuses([
      agentStatus(SESSION_A, "working", 4, 0, 8_500, "Indexing metadata"),
    ]);

    const document = _projectNavigationSearchDocuments()[0];
    if (!document) throw new Error("navigation projection omitted seeded session");
    expect(document).toMatchObject({
      sessionId: SESSION_A,
      href: `/s/${SESSION_A}`,
      displayTitle: "Release shell",
      customTitle: "Release shell",
      terminalTitle: "vite — web",
      cwd: "/srv/roost/packages/web",
      spawnCwd: "/srv/roost",
      workspaceName: "Search Workspace",
      workspaceId: WORKSPACE_ID,
      workerFp: ONLINE_FP,
      workerLabel: "Build Machine",
      gitBranch: "feature/search",
      gitRemote: "acme/roost",
      pullRequestNumber: 417,
      pullRequestState: "draft",
      pullRequestChecks: "pending",
      pullRequestUrl: "https://github.com/acme/roost/pull/417",
      portLabel: ":3000 :5174",
      ports: [
        { port: 3000, label: ":3000", href: "http://build.example.test:3000" },
        { port: 5174, label: ":5174", href: "http://build.example.test:5174" },
      ],
      activityAt: 9_000,
      available: true,
      agentStatus: "working",
      agentAttention: null,
      agentUnseen: false,
      agentUpdatedAt: 8_500,
      agentMessage: "Indexing metadata",
    });
    expect(Object.entries(document)
      .filter(([key]) => key !== "ports")
      .every(([, value]) =>
        value === null || ["string", "number", "boolean"].includes(typeof value)
      )).toBe(true);

    for (const query of [
      "release", "vite web", "packages/web", "/srv/roost", "search workspace",
      "build machine", "feature/search", "acme/roost", "#417", "draft pending",
      ":5174", "omp", "indexing metadata", "online",
    ]) {
      expect(matchesNavigationSearchDocument(document, query)).toBe(true);
    }
    expect(filterNavigationSearchDocuments(_projectNavigationSearchDocuments(), "WORKSPACE feature")).toEqual([
      document,
    ]);
    expect(matchesNavigationSearchDocument(document, "missing metadata")).toBe(false);
  });

  test("retains unavailable rows and orders metadata by activity then session ID", () => {
    setRootStore("workers", ONLINE_FP, worker(ONLINE_FP, "Online"));
    setRootStore("workers", OFFLINE_FP, worker(OFFLINE_FP as typeof ONLINE_FP, "Offline"));
    seedSessions([
      session(SESSION_C, { created_at: 4_000 }),
      session(SESSION_A, { created_at: 4_000, worker_fp: OFFLINE_FP }),
      session(SESSION_B, { created_at: 2_000 }),
    ]);
    setRootStore("last_activity", SESSION_B, 7_000);

    const documents = _projectNavigationSearchDocuments();
    expect(documents.map((document) => document.sessionId)).toEqual([
      SESSION_B,
      SESSION_A,
      SESSION_C,
    ]);
    expect(documents.find((document) => document.sessionId === SESSION_A)).toMatchObject({
      workerLabel: "Offline",
      activityAt: 4_000,
      available: false,
    });
    expect(filterNavigationSearchDocuments(documents, "offline").map((document) => document.sessionId))
      .toEqual([SESSION_A]);
  });
});

describe("navigation attention", () => {
  test("sorts blocked before done, unseen before seen, timestamp then session ID", () => {
    setRootStore("workers", ONLINE_FP, worker(ONLINE_FP, "Online"));
    seedSessions([
      session(SESSION_A),
      session(SESSION_B),
      session(SESSION_C),
      session(SESSION_D),
      session(SESSION_E),
    ]);
    seedAgentStatuses([
      agentStatus(SESSION_A, "blocked", 5, 0, 100),
      agentStatus(SESSION_B, "blocked", 7, 0, 900),
      agentStatus(SESSION_C, "blocked", 6, 0, 100),
      agentStatus(SESSION_D, "idle", 4, 4, 1_000),
      agentStatus(SESSION_E, "working", 3, 0, 2_000),
    ]);
    markAgentSeen(SESSION_B, 7);

    const attention = attentionNavigationDocuments(_projectNavigationSearchDocuments());
    expect(attention.map((document) => document.sessionId)).toEqual([
      SESSION_A,
      SESSION_C,
      SESSION_B,
      SESSION_D,
    ]);
    expect(attention.map((document) => [document.agentAttention, document.agentUnseen])).toEqual([
      ["blocked", true],
      ["blocked", true],
      ["blocked", false],
      ["done", true],
    ]);
  });

  test("acknowledgement keeps blocked attention but removes done, including offline rows", () => {
    setRootStore("workers", OFFLINE_FP, worker(OFFLINE_FP as typeof ONLINE_FP, "Sleeping"));
    seedSessions([
      session(SESSION_A, { worker_fp: OFFLINE_FP }),
      session(SESSION_B, { worker_fp: OFFLINE_FP }),
    ]);
    seedAgentStatuses([
      agentStatus(SESSION_A, "blocked", 8, 0, 800),
      agentStatus(SESSION_B, "idle", 9, 9, 900),
    ]);

    expect(attentionNavigationDocuments(_projectNavigationSearchDocuments()).map((document) => ({
      id: document.sessionId,
      available: document.available,
      attention: document.agentAttention,
    }))).toEqual([
      { id: SESSION_A, available: false, attention: "blocked" },
      { id: SESSION_B, available: false, attention: "done" },
    ]);

    markAgentSeen(SESSION_A, 8);
    markAgentSeen(SESSION_B, 9);
    expect(attentionNavigationDocuments(_projectNavigationSearchDocuments()).map((document) => ({
      id: document.sessionId,
      unseen: document.agentUnseen,
      attention: document.agentAttention,
    }))).toEqual([
      { id: SESSION_A, unseen: false, attention: "blocked" },
    ]);
  });
});
