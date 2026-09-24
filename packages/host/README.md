<!-- AUDIENCE: claude -->
<!-- Bun/Node host runtime: configuration, paths, local endpoints, service health, and paused Windows helpers. -->
<!-- Explicit subpaths only; applications and protocol specs own behavior outside this runtime package. -->

# @roost/host

Portable host-runtime helpers used by coord, worker, and the CLI: configuration, paths, local endpoints, durability, service health, SPA embedding, and Windows helper surfaces. It may use Node/Bun builtins; it must not import an application or redefine protocol state machines.

## Import rule: subpath-only

`import { X } from "@roost/host"` does not resolve. Import the explicit owner from `packages/host/package.json`; representative subpaths are `@roost/host/config`, `@roost/host/paths`, `@roost/host/local-endpoint`, `@roost/host/service-health`, `@roost/host/spa`, `@roost/host/machine-transaction`, `@roost/host/windows-helper`, and `@roost/host/windows/*`.

## Module map

| Directory | Owns | Must not own |
| --- | --- | --- |
| `packages/host/src/` | Coordinator/worker host config, paths, durability, local endpoints, HTTP security, service health, SPA/web embeds, tailnet, build identity, JWT base helpers, and machine transaction locking. | Browser UI, worker PTY/session logic, coordinator handlers, or protocol schema definitions. |
| `packages/host/src/windows/` | Paused Windows service definitions/S/security/manager/SCM, update broker/runtime/journal/rollback/assets, release manifest, path safety, and identity helpers. | POSIX service policy, worker terminal implementation, or a current Windows support claim. |
| `packages/host/tests/` | Config, SPA, paths, machine transaction, and Windows update/service seam tests. | Coord/worker end-to-end tests or browser tests. |

## Invariants

- `config.ts` loads and normalizes coordinator environment policy; declarative config shape stays in `coord-config-schema.ts`. Do not move worker configuration into this package.
- `paths.ts`, `local-endpoint.ts`, and `durability.ts` are the single owners for host paths, local endpoint security, and atomic durable writes. Their callers must not recreate those policies.
- `service-health.ts` and its internal protocol module are reached through the explicit service-health subpath; the protocol module has no independent public export.
- `spa.ts` and generated web embed modules serve/build assets only. They do not own frontend behavior.
- `windows/` is an explicit paused compatibility surface. Dynamic import and identity checks keep native Windows helpers out of POSIX paths; CI cannot exercise this directory on Linux/macOS.
- `@roost/host` may depend on `@roost/protocol`, `@roost/platform`, and `@roost/observability`; it must not import `apps/coord`, `apps/worker`, or `apps/roost-cli` internals.
- Protocol endpoint limits and carrier rules are authoritative in `protocol/spec`; this package only supplies host runtime seams.

## Test

`bun test packages/host/tests/` runs the package tests.
