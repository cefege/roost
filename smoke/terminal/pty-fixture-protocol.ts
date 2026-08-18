export const PTY_FIXTURE_PROTOCOL = "ROOST-PTY/1 ";
export const PTY_FIXTURE_READY = "ROOST_PTY_READY/1";

export type PtyFixtureCommand =
  | { op: "ARM_KEY"; nonce: string }
  | { op: "ARM_CURSOR_MOVE"; nonce: string }
  | { op: "ARM_LINE_OVERWRITE"; nonce: string }
  | { op: "ARM_ALT_REDRAW"; nonce: string; trigger: "key" | "line" }
  | { op: "EMIT"; text: string; newline?: boolean; delayMs?: number }
  | { op: "FLOOD"; prefix: string; count: number; start?: number }
  | { op: "REPORT_SIZE"; nonce: string }
  | { op: "ALT_SCREEN"; active: boolean; prefix?: string; count?: number; nonce?: string }
  | { op: "SPAWN_TREE"; nonce: string; depth?: number }
  | { op: "EXIT" };

/** Length is UTF-8 bytes, not JS code units, so Unicode payloads stay framed. */
export function encodePtyFixtureCommand(command: PtyFixtureCommand): string {
  const payload = JSON.stringify(command);
  const byteLength = new TextEncoder().encode(payload).byteLength;
  return `${PTY_FIXTURE_PROTOCOL}${byteLength}\n${payload}`;
}
