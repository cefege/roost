// Owns one concept: the schema version of the worker's durable session-event
// store, how a remote host reads it, and when a forward migration of that
// store makes a deploy rollback impossible. The macOS and Linux deploy
// journals record the versions they observed; their recovery drivers compare
// them before declaring a retained journal unrecoverable and rolling forward.

/** The durable session-event store sits one directory above the journal. */
export const DURABLE_WORKER_STATE_FILE = "session-event-outbox.sqlite";

/** Linux journal fields are text: decimal digits, or "" when unobserved. */
export const DURABLE_WORKER_STATE_VERSION_TEXT_RE = /^(?:0|[1-9][0-9]*)$/;

export type DurableWorkerStatePlatform = "macOS" | "Linux";

/**
 * Raised by a prior-release proof that can never pass: the release the
 * rollback restored cannot run, because the target release migrated the
 * durable store past it. Recovery turns this into a roll-forward outcome
 * instead of retaining a journal that would fail the same proof forever.
 */
export class DurableStateRollForwardRequired extends Error {
  constructor(
    readonly platform: DurableWorkerStatePlatform,
    readonly priorVersion: number,
    readonly targetVersion: number,
  ) {
    super(
      `${platform} rollback is impossible: the target release migrated durable worker state `
        + `from schema ${priorVersion} to ${targetVersion}, so the prior release cannot run`,
    );
    this.name = "DurableStateRollForwardRequired";
  }
}

/** SQLite `user_version` is an unsigned 32-bit header field. */
export function isDurableStateVersion(value: unknown): value is number {
  return typeof value === "number"
    && Number.isSafeInteger(value)
    && value >= 0
    && value <= 0xffffffff;
}

/**
 * A prior release is only provably unrunnable when both observations exist:
 * the version it was running with before activation, and the higher version
 * the target left behind. A missing observation never authorizes a
 * roll-forward, so journals written before this fact existed keep rolling
 * back exactly as they always did.
 */
export function durableStateMigratedForward(
  priorVersion: number | null,
  targetVersion: number | null,
): boolean {
  return priorVersion !== null && targetVersion !== null && targetVersion > priorVersion;
}

/** The operator-visible explanation for an unattainable rollback. */
export function durableStateRollForwardNotice(
  failure: DurableStateRollForwardRequired,
  targetReleasePath: string,
): string {
  return `!! ${failure.message}\n`
    + `!! rolling forward is the only recovery: deploy journal cleared, staged release `
    + `${targetReleasePath} kept`;
}

/**
 * Bun source transmitted with the macOS journal program. Reads the store's
 * SQLite `user_version` without opening a database — a 64-byte header read
 * takes no lock and cannot migrate the file it is inspecting.
 */
export const DURABLE_WORKER_STATE_PROBE_JS = `
function durableWorkerStateVersion() {
  if (!durableStatePath || !fs.existsSync(durableStatePath)) return null;
  const durableStat = fs.lstatSync(durableStatePath);
  if (!durableStat.isFile() || durableStat.isSymbolicLink() || durableStat.size < 64) return null;
  const header = Buffer.alloc(64);
  const durableHandle = fs.openSync(durableStatePath, "r");
  try {
    if (fs.readSync(durableHandle, header, 0, 64, 0) !== 64) return null;
  } finally { fs.closeSync(durableHandle); }
  if (header.subarray(0, 15).toString("latin1") !== "SQLite format 3") return null;
  return header.readUInt32BE(60);
}
`;

/**
 * POSIX shell equivalent for the Linux journal writers, which are shell
 * commands rather than a transmitted program. Prints the version, or nothing
 * when there is no readable store.
 */
export const DURABLE_WORKER_STATE_PROBE_SH =
  `durable_worker_state() { store="$1"; `
  + `if test ! -f "$store" || test -L "$store"; then return 0; fi; `
  + `if test "$(head -c 15 -- "$store" 2>/dev/null)" != 'SQLite format 3'; then return 0; fi; `
  + `hex=$(od -An -tx1 -j60 -N4 -- "$store" 2>/dev/null | tr -d ' \\n'); `
  + `case "$hex" in [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) `
  + `printf '%s' "$((0x$hex))";; esac; }; `;
