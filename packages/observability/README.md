<!-- AUDIENCE: claude -->
<!-- Portable structured logging, diagnostics, and trace identity; explicit subpaths only. -->
<!-- This package depends on zod and no application, host runtime, or browser framework. -->

# @roost/observability

Portable logging, diagnostics, and trace identity. Coord, worker, and other callers import the explicit owner; no app or host runtime is imported here.

## Import rule: subpath-only

`import { X } from "@roost/observability"` does not resolve. Use `@roost/observability/log`, `@roost/observability/diag`, or `@roost/observability/trace`.

| Subpath | Supplies |
| --- | --- |
| `@roost/observability/log` | Structured `log.debug/info/warn/error` sink with level gating and structured fields. |
| `@roost/observability/diag` | Opt-in diagnostic firehose and signal helpers. |
| `@roost/observability/trace` | `TraceId`, `newTraceId()`, and the trace header contract. |

## Module map

| Directory | Owns | Must not own |
| --- | --- | --- |
| `packages/observability/src/` | `log.ts`, `diag.ts`, and `trace.ts`; portable log records, trace identity, and diagnostic signal vocabulary. | Application state, transport policy, protocol schemas, or host filesystem/DB writes. |
| `packages/observability/tests/` | Logger, diagnostic, and trace behavior tests. | App integration fixtures or console-output tests for the CLI. |

## Invariants

- `TraceId` is branded and generated once here; the trace header and log correlation use that identity. Do not duplicate a trace schema in an app.
- Log records are one structured line per event with `ts`, `level`, `target`, and optional trace/fields. `ROOST_LOG_LEVEL` gates debug output; it does not change record shape.
- `diag()` is opt-in firehose telemetry. It must not carry terminal content, secrets, pairing tokens, or private conversation references.
- The package depends only on `zod`; it must not import `node:*`, `bun`, an app, or a package with a host runtime.
- Coord and worker use `log` for state transitions; the CLI may print directly because stdout is its product surface.

## Test

`bun test packages/observability/tests/` runs the package tests.
