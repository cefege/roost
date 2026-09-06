// Browser-profile acknowledgement state for exact coding-agent occupants.
// Revisions advance only within one epoch/occupant pair; storage merges each
// identity independently so another tab cannot acknowledge a replacement.

import { createSignal } from "solid-js";
import {
  AgentOccupantId,
  AgentStatusSource,
  SessionId,
  StatusEpoch,
  type AgentStatus,
} from "@roost/shared/wire";
import {
  agentStatusOccupantKey,
  agentStatusRevisionToken,
  type AgentStatusRevisionToken,
} from "./agentStatus.ts";

const STORAGE_KEY = "roost.agentSeen.v2";
const LEGACY_STORAGE_KEY = "roost.agentSeen.v1";
const LEGACY_IDENTITY_KEY = "legacy";
const PERSIST_DEBOUNCE_MS = 250;

interface StoredAgentSeenV2 {
  schema_version: 2;
  tokens: AgentStatusRevisionToken[];
}

function validRevision(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function parseStoredV2(raw: string | null): AgentStatusRevisionToken[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as {
      schema_version?: unknown;
      tokens?: unknown;
    };
    if (parsed?.schema_version !== 2 || !Array.isArray(parsed.tokens)) return [];
    const tokens: AgentStatusRevisionToken[] = [];
    for (const candidate of parsed.tokens) {
      if (!candidate || typeof candidate !== "object") continue;
      const value = candidate as Record<string, unknown>;
      const sessionId = SessionId.safeParse(value.session_id);
      if (!sessionId.success || !validRevision(value.revision)) continue;
      const identityAbsent = value.status_epoch === undefined
        && value.occupant_id === undefined
        && value.source === undefined;
      if (identityAbsent) {
        tokens.push({
          session_id: sessionId.data,
          revision: value.revision,
        });
        continue;
      }
      const statusEpoch = StatusEpoch.safeParse(value.status_epoch);
      const occupantId = AgentOccupantId.safeParse(value.occupant_id);
      const source = AgentStatusSource.safeParse(value.source);
      if (!statusEpoch.success || !occupantId.success || !source.success) continue;
      tokens.push({
        session_id: sessionId.data,
        revision: value.revision,
        status_epoch: statusEpoch.data,
        occupant_id: occupantId.data,
        source: source.data,
      });
    }
    return tokens;
  } catch {
    return [];
  }
}

function parseLegacyStored(raw: string | null): AgentStatusRevisionToken[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return [];
    const tokens: AgentStatusRevisionToken[] = [];
    for (const [candidateSessionId, revision] of Object.entries(parsed)) {
      const sessionId = SessionId.safeParse(candidateSessionId);
      if (!sessionId.success || !validRevision(revision)) continue;
      tokens.push({ session_id: sessionId.data, revision });
    }
    return tokens;
  } catch {
    return [];
  }
}

function readStoredTokens(): AgentStatusRevisionToken[] {
  try {
    if (typeof localStorage === "undefined") return [];
    return [
      ...parseStoredV2(localStorage.getItem(STORAGE_KEY)),
      ...parseLegacyStored(localStorage.getItem(LEGACY_STORAGE_KEY)),
    ];
  } catch {
    return [];
  }
}

const seenBySession = new Map<string, Map<string, AgentStatusRevisionToken>>();
const [version, setVersion] = createSignal(0);
let persistTimer: ReturnType<typeof setTimeout> | null = null;

function tokenIdentityKey(token: AgentStatusRevisionToken): string {
  return agentStatusOccupantKey(token) ?? LEGACY_IDENTITY_KEY;
}

function mergeStored(tokens: readonly AgentStatusRevisionToken[], notify = true): boolean {
  let changed = false;
  for (const token of tokens) {
    const identityKey = tokenIdentityKey(token);
    let sessionTokens = seenBySession.get(token.session_id);
    if (!sessionTokens) {
      sessionTokens = new Map();
      seenBySession.set(token.session_id, sessionTokens);
    }
    const current = sessionTokens.get(identityKey);
    if (current && current.revision >= token.revision) continue;
    sessionTokens.set(identityKey, { ...token });
    changed = true;
  }
  if (changed && notify) setVersion((value) => value + 1);
  return changed;
}

mergeStored(readStoredTokens(), false);

export function seenAgentRevision(status: AgentStatus | null | undefined): number {
  version();
  if (!status) return 0;
  const occupantKey = agentStatusOccupantKey(status);
  const identityKey = occupantKey ?? LEGACY_IDENTITY_KEY;
  return seenBySession
    .get(status.session_id)
    ?.get(identityKey)
    ?.revision ?? (occupantKey === null ? 0 : -1);
}

export function markAgentSeen(status: AgentStatus): boolean {
  const revision = status.revision;
  const token = agentStatusRevisionToken(status);
  const occupantKey = agentStatusOccupantKey(token);
  const currentRevision = seenBySession
    .get(status.session_id)
    ?.get(occupantKey ?? LEGACY_IDENTITY_KEY)
    ?.revision ?? (occupantKey === null ? 0 : -1);
  if (revision <= currentRevision) return false;
  mergeStored([token]);
  schedulePersist();
  return true;
}

function schedulePersist(): void {
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    flushAgentSeen();
  }, PERSIST_DEBOUNCE_MS);
}

/** Merge before writing so acknowledgements made independently by two tabs survive. */
export function flushAgentSeen(): void {
  if (persistTimer) {
    clearTimeout(persistTimer);
    persistTimer = null;
  }
  try {
    if (typeof localStorage === "undefined") return;
    mergeStored(parseStoredV2(localStorage.getItem(STORAGE_KEY)));
    mergeStored(parseLegacyStored(localStorage.getItem(LEGACY_STORAGE_KEY)));
    const tokens = [...seenBySession.values()].flatMap((sessionTokens) =>
      [...sessionTokens.values()]
    );
    const stored: StoredAgentSeenV2 = { schema_version: 2, tokens };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(stored));
    localStorage.removeItem(LEGACY_STORAGE_KEY);
  } catch {
    // Private mode and quota failures leave the in-memory acknowledgement intact.
  }
}

/** Install cross-tab merge and last-chance pagehide persistence. */
export function startAgentSeenPersistence(): () => void {
  if (typeof window === "undefined") return () => {};
  try {
    if (localStorage.getItem(LEGACY_STORAGE_KEY) !== null) flushAgentSeen();
  } catch {
    // Profile storage is optional.
  }
  const onStorage = (event: StorageEvent) => {
    if (event.key === STORAGE_KEY) {
      if (mergeStored(parseStoredV2(event.newValue))) schedulePersist();
    } else if (event.key === LEGACY_STORAGE_KEY) {
      if (mergeStored(parseLegacyStored(event.newValue))) schedulePersist();
    }
  };
  const onPageHide = () => flushAgentSeen();
  window.addEventListener("storage", onStorage);
  window.addEventListener("pagehide", onPageHide);
  return () => {
    window.removeEventListener("storage", onStorage);
    window.removeEventListener("pagehide", onPageHide);
    flushAgentSeen();
  };
}

/** Clear account-scoped session notification acknowledgements. */
export function clearAgentSeenForLogout(): void {
  clearTimeout(persistTimer ?? undefined);
  persistTimer = null;
  seenBySession.clear();
  setVersion((value) => value + 1);
  try {
    localStorage.removeItem(STORAGE_KEY);
    localStorage.removeItem(LEGACY_STORAGE_KEY);
  } catch {
    // Profile storage is optional.
  }
}

export function resetAgentSeenForTest(): void {
  clearAgentSeenForLogout();
}
