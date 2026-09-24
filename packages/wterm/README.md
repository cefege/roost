<!-- AUDIENCE: claude -->
<!-- Patched wterm WASM loader and worker-side core factory; artifact and checksum stay co-located. -->
<!-- Explicit subpaths only; no application or browser UI ownership. -->

# @roost/wterm

The patched wterm WASM engine, loader, and headless `TerminalCore` factory. The WASM and SHA-256 sidecar live beside this package's loader so source and deployed layouts resolve the same artifact.

## Import rule: subpath-only

`import { X } from "@roost/wterm"` does not resolve. Use `@roost/wterm/wterm-wasm` for the artifact path/digest and `@roost/wterm/wterm-core-factory` for the worker-side core factory.

## Module map

| Directory | Owns | Must not own |
| --- | --- | --- |
| `packages/wterm/src/` | `wterm-wasm.ts`, `wterm-core-factory.ts`, and generated WASM embed bindings; patched core loading and server-side factory. | PTY process hosting, browser rendering, protocol schemas, or app state. |
| `packages/wterm/wasm/` | `wterm-roost.wasm` and `wterm-roost.wasm.sha256`, the committed patched artifact and digest. | Generated source bindings or runtime fallback policy. |
| `packages/wterm/tests/` | Core load, mouse/resize/unhandled-sequence, and differential trace-oracle tests. | Worker process or browser end-to-end tests. |

## Invariants

- The loader refuses bytes that do not match the committed SHA-256 sidecar. There is no stock-WASM fallback: the patched artifact raises scrollback and alternate-screen behavior that the product depends on.
- `wterm-core-factory.ts` is server-side and must not be imported by the web app or coordinator. The worker owns PTY/core lifecycle.
- The artifact and digest remain co-located with the loader. Deploy paths must ship the whole `packages/` tree; never add a second artifact location.
- This package may depend on `@roost/protocol` and `@roost/observability` for types/diagnostics, but it must not import an app, `@roost/host`, or a browser UI framework.

## Test

`bun test packages/wterm/tests/` runs the core, artifact, and trace-oracle suites.
