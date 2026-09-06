// Shared metadata and attention projection for navigation surfaces.
// The search page, sidebar filter, and command palette consume one scalar index.
// Root-store sessions and existing title, routability, status, and seen owners remain authoritative.

import { createMemo, createRoot, type Accessor } from "solid-js";
import type { AgentStatus, Session } from "@roost/shared/wire";
import { seenAgentRevision } from "../lib/agentSeen.ts";
import {
  deriveAgentStatusLevel,
  type AgentStatusLevel,
} from "../lib/agentStatus.ts";
import { workspaceForFolder } from "../lib/folderKey.ts";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { sessionHref } from "../routes.ts";
import { rootStore } from "./root.ts";
import { workerOnline } from "./sync-routable.ts";

export type NavigationSearchAttention = "blocked" | "done";

/** A detached, scalar search row. No Solid store proxy escapes this projection. */
export interface NavigationSearchDocument {
  readonly sessionId: string;
  readonly href: string;
  readonly displayTitle: string;
  readonly customTitle: string | null;
  readonly terminalTitle: string | null;
  readonly cwd: string;
  readonly spawnCwd: string | null;
  readonly workspaceId: string | null;
  readonly workspaceName: string | null;
  readonly workerLabel: string;
  readonly workerFp: string;
  readonly gitBranch: string | null;
  readonly gitRemote: string | null;
  readonly pullRequestNumber: number | null;
  readonly pullRequestState: "open" | "merged" | "closed" | "draft" | null;
  readonly pullRequestChecks: "passing" | "failing" | "pending" | "none" | null;
  readonly pullRequestUrl: string | null;
  readonly portLabel: string | null;
  readonly ports: readonly {
    readonly port: number;
    readonly label: string;
    readonly href: string | null;
  }[];
  readonly searchText: string;
  readonly activityAt: number;
  readonly available: boolean;
  readonly agentStatus: AgentStatusLevel;
  readonly agentAttention: NavigationSearchAttention | null;
  readonly agentUnseen: boolean;
  readonly agentUpdatedAt: number | null;
  readonly agentMessage: string | null;
}

/** Normalize once for every navigation-search consumer. */
export function normalizeNavigationSearchQuery(value: string): string {
  return value.normalize("NFKC").toLowerCase().trim().replace(/\s+/gu, " ");
}

/** Match every query term against the document's shared normalized metadata. */
export function matchesNavigationSearchDocument(
  document: NavigationSearchDocument,
  query: string,
): boolean {
  const normalized = normalizeNavigationSearchQuery(query);
  if (!normalized) return true;
  return matchesNormalizedTerms(document, normalized.split(" "));
}

export function filterNavigationSearchDocuments(
  documents: readonly NavigationSearchDocument[],
  query: string,
): readonly NavigationSearchDocument[] {
  const normalized = normalizeNavigationSearchQuery(query);
  if (!normalized) return documents;
  const terms = normalized.split(" ");
  return documents.filter((document) => matchesNormalizedTerms(document, terms));
}

/** Select attention rows without acknowledging them. Navigation remains the seen-state owner. */
export function attentionNavigationDocuments(
  documents: readonly NavigationSearchDocument[],
): NavigationSearchDocument[] {
  return documents
    .filter((document) => document.agentAttention !== null)
    .sort(compareAttentionDocuments);
}

export function _projectNavigationSearchDocuments(): readonly NavigationSearchDocument[] {
  return Object.values(rootStore.sessions)
    .map(projectSession)
    .sort(compareMetadataDocuments);
}

export const navigationSearchDocuments: Accessor<readonly NavigationSearchDocument[]> = createRoot(() =>
  createMemo(_projectNavigationSearchDocuments),
);

function projectSession(session: Session): NavigationSearchDocument {
  const workspace = workspaceForFolder(session.worker_fp, session.cwd);
  const worker = rootStore.workers[session.worker_fp];
  const status = rootStore.agent_status[session.id] as AgentStatus | undefined;
  const acknowledgedRevision = seenAgentRevision(session.id);
  const agentStatus = deriveAgentStatusLevel(status, acknowledgedRevision);
  const agentAttention: NavigationSearchAttention | null = status?.state === "blocked"
    ? "blocked"
    : agentStatus === "done" ? "done" : null;
  const agentUnseen = agentAttention === "blocked" && status
    ? status.revision > acknowledgedRevision
    : agentAttention === "done";
  const customTitle = cleanOptional(session.custom_title);
  const terminalTitle = cleanOptional(rootStore.terminal_title[session.id]);
  const spawnCwd = cleanOptional(session.spawn_cwd);
  const workspaceName = cleanOptional(workspace?.name);
  const workerLabel = cleanOptional(worker?.label) ?? String(session.worker_fp);
  const gitBranch = cleanOptional(session.git_branch);
  const gitRemote = cleanOptional(session.git_remote);
  const pullRequestUrl = cleanOptional(session.pr_url);
  const ports = [...new Set(session.ports ?? [])]
    .sort((left, right) => left - right)
    .map(port => ({
      port,
      label: `:${port}`,
      href: worker?.reachable_addr
        ? `http://${worker.reachable_addr}:${port}`
        : null,
    }));
  const portLabel = ports.length > 0
    ? ports.map(port => port.label).join(" ")
    : null;
  const agentMessage = cleanOptional(status?.message);
  const displayTitle = sessionTitle(session);
  const pullRequestNumber = session.pr_number ?? null;
  const pullRequestState = session.pr_state ?? null;
  const pullRequestChecks = session.pr_checks ?? null;
  const available = session.status === "open" && !!worker && workerOnline(worker);

  const searchText = normalizeNavigationSearchQuery([
    session.id,
    displayTitle,
    customTitle,
    terminalTitle,
    session.cwd,
    spawnCwd,
    workspaceName,
    workerLabel,
    session.worker_fp,
    gitBranch,
    gitRemote,
    pullRequestNumber,
    pullRequestNumber === null ? null : `#${pullRequestNumber}`,
    pullRequestState,
    pullRequestChecks,
    pullRequestUrl,
    ...ports.flatMap(port => [port.label, port.href]),
    status?.agent_id,
    agentStatus,
    agentMessage,
    available ? "available online" : "unavailable offline",
  ].filter((value): value is string | number => value !== null && value !== undefined)
    .join("\n"));

  return {
    sessionId: session.id,
    href: sessionHref(session.id),
    displayTitle,
    customTitle,
    terminalTitle,
    cwd: session.cwd,
    workspaceId: workspace?.id ?? null,
    spawnCwd,
    workspaceName,
    workerLabel,
    workerFp: session.worker_fp,
    gitBranch,
    gitRemote,
    pullRequestNumber,
    pullRequestState,
    pullRequestChecks,
    pullRequestUrl,
    portLabel,
    ports,
    searchText,
    activityAt: session.status === "open"
      ? rootStore.last_activity[session.id] ?? session.created_at
      : session.closed_at ?? session.created_at,
    available,
    agentStatus,
    agentAttention,
    agentUnseen,
    agentUpdatedAt: status?.updated_at ?? null,
    agentMessage,
  };
}


function cleanOptional(value: string | null | undefined): string | null {
  const cleaned = value?.trim();
  return cleaned ? cleaned : null;
}

function compareMetadataDocuments(
  left: NavigationSearchDocument,
  right: NavigationSearchDocument,
): number {
  return right.activityAt - left.activityAt
    || (left.sessionId < right.sessionId ? -1 : left.sessionId > right.sessionId ? 1 : 0);
}

function compareAttentionDocuments(
  left: NavigationSearchDocument,
  right: NavigationSearchDocument,
): number {
  const leftKind = left.agentAttention === "blocked" ? 0 : 1;
  const rightKind = right.agentAttention === "blocked" ? 0 : 1;
  return leftKind - rightKind
    || Number(right.agentUnseen) - Number(left.agentUnseen)
    || (right.agentUpdatedAt ?? 0) - (left.agentUpdatedAt ?? 0)
    || (left.sessionId < right.sessionId ? -1 : left.sessionId > right.sessionId ? 1 : 0);
}

function matchesNormalizedTerms(
  document: NavigationSearchDocument,
  terms: readonly string[],
): boolean {
  return terms.every((term) => document.searchText.includes(term));
}
