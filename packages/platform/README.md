<!-- AUDIENCE: claude -->
<!-- Browser-safe host-platform conventions with no Roost package dependency. -->
<!-- Explicit subpaths only; host filesystem and process operations stay outside this package. -->

# @roost/platform

Small host-platform conventions usable from browser, Node, or Bun. It owns supported-platform naming, lexical native-path normalization, POSIX shell quoting, worker service environment parsing, and machine enrollment command construction.

## Import rule: subpath-only

`import { X } from "@roost/platform"` does not resolve. Use the explicit export subpath: `@roost/platform/platform`, `@roost/platform/native-path`, `@roost/platform/shell-quote`, `@roost/platform/worker-service-env`, or `@roost/platform/machine-join-command`.

## Module map

| Directory | Owns | Must not own |
| --- | --- | --- |
| `packages/platform/src/` | `platform.ts`, `native-path.ts`, `shell-quote.ts`, `worker-service-env.ts`, and `machine-join-command.ts`. | Node builtins, filesystem mutation, app configuration, protocol schemas, or process spawning. |
| `packages/platform/tests/` | Shell quoting, path identity, and platform helper tests. | Host service or filesystem integration tests. |

## Invariants

- `native-path.ts` imports zero Node builtins so browser bundles can use it. It is lexical validation/normalization, not worker-native `node:path` behavior.
- `platform.ts` rejects unsupported host platforms rather than silently falling through. Keep the supported list and names in one place.
- `shell-quote.ts` owns the canonical POSIX single-quote encoding. Do not fork shell escaping in deploy or service code.
- `worker-service-env.ts` owns worker service environment parsing; `machine-join-command.ts` owns the enrollment command shape. Neither starts a process or contacts a coordinator.
- This package has no dependency on `@roost/host`, `@roost/protocol`, an app, `node:*`, or `bun`.

## Test

`bun test packages/platform/tests/` runs the package tests.
