// Worker event metadata becomes durable and must be bounded before database insertion.
// Event-log callers normalize through this module so replay and projections receive
// the same whole-code-point UTF-8 values without input-sized temporary buffers.

import type { Session, SessionEvent } from "@roost/shared/wire";

export const MAX_PERSISTED_UTF8_BYTES = 4_096;
export const MAX_WORKER_SNAPSHOT_SESSIONS = 1_024;

/**
 * Return the longest whole-code-point prefix whose UTF-8 representation fits
 * in maxBytes. Work and output are bounded by maxBytes even when value is an
 * attacker-controlled, arbitrarily large string; unlike TextEncoder.encode(),
 * this never allocates a buffer proportional to the input.
 */
export function truncatePersistedUtf8(
  value: string,
  maxBytes = MAX_PERSISTED_UTF8_BYTES,
): string {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 0) {
    throw new RangeError("maxBytes must be a non-negative safe integer");
  }
  if (maxBytes === 0) return "";

  let bytes = 0;
  let index = 0;
  while (index < value.length) {
    const first = value.charCodeAt(index);
    let codeUnits = 1;
    let encodedBytes: number;
    if (first <= 0x7f) {
      encodedBytes = 1;
    } else if (first <= 0x7ff) {
      encodedBytes = 2;
    } else if (
      first >= 0xd800
      && first <= 0xdbff
      && index + 1 < value.length
    ) {
      const second = value.charCodeAt(index + 1);
      if (second >= 0xdc00 && second <= 0xdfff) {
        codeUnits = 2;
        encodedBytes = 4;
      } else {
        // TextEncoder encodes an unpaired surrogate as U+FFFD.
        encodedBytes = 3;
      }
    } else {
      // BMP scalar values and unpaired low surrogates (U+FFFD) both occupy
      // three bytes in UTF-8.
      encodedBytes = 3;
    }

    if (bytes + encodedBytes > maxBytes) return value.slice(0, index);
    bytes += encodedBytes;
    index += codeUnits;
  }
  return value;
}

function normalizePersistedSession(session: Session): Session {
  const cwd = truncatePersistedUtf8(session.cwd);
  const spawnCwd = session.spawn_cwd == null
    ? session.spawn_cwd
    : truncatePersistedUtf8(session.spawn_cwd);
  const customTitle = session.custom_title == null
    ? session.custom_title
    : truncatePersistedUtf8(session.custom_title);
  const gitBranch = session.git_branch == null
    ? session.git_branch
    : truncatePersistedUtf8(session.git_branch);
  const gitRemote = session.git_remote == null
    ? session.git_remote
    : truncatePersistedUtf8(session.git_remote);
  const prUrl = session.pr_url == null
    ? session.pr_url
    : truncatePersistedUtf8(session.pr_url);

  if (
    cwd === session.cwd
    && spawnCwd === session.spawn_cwd
    && customTitle === session.custom_title
    && gitBranch === session.git_branch
    && gitRemote === session.git_remote
    && prUrl === session.pr_url
  ) {
    return session;
  }
  return {
    ...session,
    cwd,
    spawn_cwd: spawnCwd,
    custom_title: customTitle,
    git_branch: gitBranch,
    git_remote: gitRemote,
    pr_url: prUrl,
  };
}

/** Bound every worker-controlled display/path/git/URL string before either the
 * durable event JSON or the sessions projection sees it. Returning one shared
 * normalized value keeps replay and projection byte-for-byte consistent. */
export function normalizePersistedWorkerEvent(event: SessionEvent): SessionEvent {
  switch (event.kind) {
    case "opened": {
      const cwd = truncatePersistedUtf8(event.cwd);
      return cwd === event.cwd ? event : { ...event, cwd };
    }
    case "cwd": {
      const cwd = truncatePersistedUtf8(event.cwd);
      return cwd === event.cwd ? event : { ...event, cwd };
    }
    case "renamed": {
      const customTitle = truncatePersistedUtf8(event.custom_title);
      return customTitle === event.custom_title
        ? event
        : { ...event, custom_title: customTitle };
    }
    case "git": {
      const branch = event.branch == null
        ? event.branch
        : truncatePersistedUtf8(event.branch);
      const remote = event.remote === undefined
        ? undefined
        : truncatePersistedUtf8(event.remote);
      return branch === event.branch && remote === event.remote
        ? event
        : { ...event, branch, remote };
    }
    case "pr": {
      const url = event.url == null ? event.url : truncatePersistedUtf8(event.url);
      return url === event.url ? event : { ...event, url };
    }
    case "snapshot": {
      let changed = false;
      const sessions = event.sessions.map((session) => {
        const normalized = normalizePersistedSession(session);
        changed ||= normalized !== session;
        return normalized;
      });
      return changed ? { ...event, sessions } : event;
    }
    default:
      return event;
  }
}
