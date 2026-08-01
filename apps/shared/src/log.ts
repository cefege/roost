// Structured logger. Emits one JSON line per log event:
//   {ts, level, trace_id?, target, ...fields}
// Bun has no built-in log rotation, so coord/worker write plain files and the
// installers wire up the host mechanism: macOS `newsyslog`/`log_rotate(8)` over
// ~/Library/Logs/Roost*/main.{out,err}.log, Linux a user `logrotate` config +
// `roost-logrotate.timer` over $XDG_STATE_HOME/Roost*/main.{out,err}.log.
// Linux rotation MUST be copytruncate: systemd's StandardOutput=append: holds
// the fd open, so a rename-based rotate would orphan the writer.
// R0.15.
//
// Minimum level gate: ROOST_LOG_LEVEL env (default "info").
// Suppresses log.debug calls (e.g. jwt.verified fires on every JWT
// verification — ~120k lines/day) unless explicitly set to "debug".
// Level ordering: debug=0, info=1, warn=2, error=3.

import type { TraceId } from "./wire/brand.ts";

export type LogLevel = "debug" | "info" | "warn" | "error";

interface LogFields {
  trace_id?: TraceId | string;
  [k: string]: unknown;
}

const LEVEL_ORDER: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3,
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const _proc = (globalThis as any).process;
const _envLevel = _proc?.env?.ROOST_LOG_LEVEL as string | undefined;
const MIN_LEVEL: LogLevel =
  _envLevel && _envLevel in LEVEL_ORDER ? (_envLevel as LogLevel) : "info";
const _minLevelNum = LEVEL_ORDER[MIN_LEVEL];

function emit(level: LogLevel, target: string, msg: string, fields?: LogFields): void {
  if (LEVEL_ORDER[level] < _minLevelNum) return;
  const line = JSON.stringify({
    ts: Date.now(),
    level,
    target,
    msg,
    ...(fields ?? {}),
  });
  if (level === "error" || level === "warn") {
    console.error(line);
  } else {
    console.log(line);
  }
}

export const log = {
  debug: (target: string, msg: string, fields?: LogFields) => emit("debug", target, msg, fields),
  info: (target: string, msg: string, fields?: LogFields) => emit("info", target, msg, fields),
  warn: (target: string, msg: string, fields?: LogFields) => emit("warn", target, msg, fields),
  error: (target: string, msg: string, fields?: LogFields) => emit("error", target, msg, fields),
};
