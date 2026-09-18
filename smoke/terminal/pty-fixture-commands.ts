// Command vocabulary for the portable PTY fixture: the field validators every
// op shares and the mapping from an arming command to the input trigger the
// fixture then waits for. Pure — pty-fixture.ts owns the IO, the counters and
// the work lanes, and pty-fixture-echo.ts reuses the validators.

export type ArmedInput =
  | { kind: "legacy-key"; nonce: string }
  | { kind: "cursor-move"; nonce: string }
  | { kind: "line-overwrite"; nonce: string; bytes: number }
  | { kind: "alt-redraw"; nonce: string; trigger: "key" | "line"; bytes: number };

export function requiredString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0 || value.length > 4_096) {
    throw new Error(`${field} must be a non-empty string no longer than 4096 characters`);
  }
  return value;
}

export function boundedInteger(
  value: unknown, field: string, minimum: number, maximum: number,
): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw new Error(`${field} must be an integer in [${minimum}, ${maximum}]`);
  }
  return value as number;
}

/** The trigger this command arms, or null when it arms nothing. */
export function armedInputForCommand(raw: unknown): ArmedInput | null {
  if (!raw || typeof raw !== "object" || !("op" in raw) || typeof raw.op !== "string") return null;
  const nonce = "nonce" in raw ? raw.nonce : undefined;
  switch (raw.op) {
    case "ARM_KEY":
      return { kind: "legacy-key", nonce: requiredString(nonce, "nonce") };
    case "ARM_CURSOR_MOVE":
      return { kind: "cursor-move", nonce: requiredString(nonce, "nonce") };
    case "ARM_LINE_OVERWRITE":
      return { kind: "line-overwrite", nonce: requiredString(nonce, "nonce"), bytes: 0 };
    case "ARM_ALT_REDRAW": {
      const trigger = "trigger" in raw ? raw.trigger : undefined;
      if (trigger !== "key" && trigger !== "line") {
        throw new Error("trigger must be key or line");
      }
      return {
        kind: "alt-redraw",
        nonce: requiredString(nonce, "nonce"),
        trigger,
        bytes: 0,
      };
    }
    default:
      return null;
  }
}
