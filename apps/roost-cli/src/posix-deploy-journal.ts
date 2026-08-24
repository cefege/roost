// Shared schema/confinement/decision core for the POSIX deploy-journal quartet
// (macOS worker, Linux worker, localhost worker, coordinator). Each platform
// module keeps its own durable bytes, proof commands, and recovery drivers;
// this file owns only the pieces they must not fork: the phase union, the
// plain-object parse gate, the release-id suffix pattern, the two canonical
// absolute-path predicates, and the prepared⇒clean / health⇒commit|rollback
// decision. Callers: deploy-macos-journal.ts, linux-deploy-journal.ts,
// local-worker-deploy-journal.ts, coordinator-deploy-journal.ts.
//
// TWO path predicates survive deliberately: normalize() semantics accept a
// trailing slash ("/a/" normalizes to itself) while resolve() semantics
// reject it. The macOS journal mirrors its remote bun -e program
// (normalize-based); the local-node journals (push/deploy-local) have always
// checked resolve() === value. Unifying them would silently reclassify
// trailing-slash paths at a rollback-critical trust boundary.

import { isAbsolute, posix, resolve } from "node:path";

export type PosixDeployJournalPhase = "prepared" | "activating" | "activated";

/** Release-id suffix `<8hex>-<4hex>-4<3hex>-[89ab]<3hex>-<12hex>` (v4 UUID).
 * Byte-identical in the macOS and coordinator journals; the localhost worker
 * journal deliberately accepts v1-v5 and case-insensitive hex instead. */
export const POSIX_RELEASE_ID_SUFFIX_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** Full lowercase git object id, 40 or 64 hex chars. Identical language in
 * the coordinator and Linux journals (`{40}(?:{24})?`). */
export const POSIX_FULL_GIT_SHA_RE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

/** First gate every persisted journal passes before field validation:
 * reject non-object payloads. `message` stays caller-owned so each
 * platform's pinned error text is unchanged. */
export function posixJournalObjectValue(
  value: unknown,
  message: string,
): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(message);
  }
  return value as Record<string, unknown>;
}

/** normalize()-based canonical absolute POSIX path: rejects relative paths,
 * `.`/`..` segments, doubled separators, CR/LF/NUL — but accepts a trailing
 * slash. Mirrors the remote macOS journal program's `canonicalAbsolute`. */
export function isCanonicalAbsolutePosixPath(value: string): boolean {
  return posix.isAbsolute(value)
    && posix.normalize(value) === value
    && !/[\r\n\0]/.test(value);
}

/** resolve()-based canonical absolute path for local-node journals: same
 * rejections plus a trailing slash (resolve strips it, so it can never
 * round-trip). Never call this for paths consumed by the remote macOS
 * program — the two disagree exactly on trailing slashes. */
export function isResolvedCanonicalAbsolutePath(value: string): boolean {
  return isAbsolute(value) && resolve(value) === value;
}

/** The one recovery decision every POSIX deploy journal encodes: a prepared
 * journal is always discarded, otherwise target health alone decides between
 * commit and rollback. Platforms map "commit"/"rollback" onto their own
 * labels (commit-target / rollback-prior / plan kinds / decisions). */
export function posixDeployJournalDecision(
  phase: PosixDeployJournalPhase,
  targetHealthy: boolean,
): "clean-prepared" | "commit" | "rollback" {
  if (phase === "prepared") return "clean-prepared";
  return targetHealthy ? "commit" : "rollback";
}
