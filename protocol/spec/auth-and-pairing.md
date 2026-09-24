<!-- Auth contract: non-extractable Ed25519 device keys, coordinator JWT verification, and pairing ceremony. -->
<!-- Pairing grants authority only after token-bound confirmation; URLs never carry accepted query credentials. -->
<!-- @roost/protocol/pairing owns portable ceremony entropy and canonical validation. -->

# Authentication and pairing

## Purpose

Roost authenticates devices and workers by public Ed25519 keys. A browser keeps its private key non-extractable, signs short-lived coordinator JWTs locally, and sends the bearer in `Authorization` or the second WebSocket subprotocol. Pairing is the explicit ceremony that adds a browser key to the account; approval alone grants no authority.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| JWT header/payload | `apps/web/src/client/auth/web-key.ts:113-125` | `alg=EdDSA`, `typ=JWT`, `kid=<fingerprint>`; payload repeats fingerprint as `sub`, audience `roost-coordinator`, and `iat`/`exp`. |
| `PairCreateRequest/Response`, `PairPollRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:718-734` | Anonymous requester creates a token-bound request and polls status. |
| `PairListRequest/Response`, `PairApproveRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:736-746` | Authorized approver lists pending requests and binds a verification code. |
| `PairConfirmRequest/Response`, `PairDenyRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:748-757` | Requester confirms with ID/token/code; approver denies. |
| `PairApprovalStatusRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:759-765` | Secret-free progress for the exact approver. |
| `AuthMintBootstrapRequest/Response`, `AuthRedeemWorkerRequest/Response`, `AuthRedeemBrowserRequest/Response` | `protocol/proto/roost/v1/coordinator.proto:621-642` | Authenticated one-shot bootstrap mint and public redemption into a persisted principal. |
| `DevicesList/Revoke/RotateCurrent` messages | `protocol/proto/roost/v1/coordinator.proto:922-930` | Device inventory and key lifecycle. |

## State machine
1. Browser WebCrypto generates Ed25519 with `extractable=false`. IndexedDB owns atomic key lifecycle. The fingerprint is lowercase SHA-256 hex of the raw 32-byte public key.
2. For each protected request, browser signs `{sub:kid,aud:"roost-coordinator",iat,exp}`. Connect sends `Authorization: Bearer <jwt>` and `x-roost-tab-id`; WebSockets send the JWT as their second subprotocol. The coordinator verifies signature, algorithm, audience, `sub===kid`, time bounds, and current key generation, then resolves exactly one account-device, worker, or legacy-self-hosted principal.
3. `PairCreate` validates ceremony version, Ed25519 public key, 16-byte request ID, and 32-byte requester token. If Cloudflare Access is enabled for a non-on-host request, valid front-door identity is required. It creates an exact-idempotent, expiring `pending` row and publishes only secret-free metadata.
4. `PairPoll` requires the matching requester-token digest. An authorized/on-host approver lists and approves with a six-digit code, moving `pending → verification_required`.
5. `PairConfirm` alone revalidates requester token, version, expiry, requester-key availability, approver/account continuity, and key collisions. It then transactionally creates the authorized key/account-device association and moves `verification_required → completed`. Wrong nonterminal codes return `{ok:false}` and increment the bounded attempt count; the terminal transition is `verification_failed`.
6. `PairApprovalStatus` is exact-approver-only and secret-free. `PairDeny` moves either live state to `denied`. Expiry retention terminalizes live rows, clears code digests, then removes terminal rows later.
7. URL bootstrap accepts only one nonempty fragment `pair` value. Startup captures it into tab/session-scoped storage and synchronously scrubs query/fragment credentials before network-facing modules load. Query-shaped credentials are scrubbed but never accepted.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `JWT_LIFETIME_SECS` | `300 s` | `apps/web/src/client/auth/web-key.ts:23` |
| `JWT_CACHE_TTL_MS` | `240,000 ms` | `apps/web/src/client/auth/web-key.ts:24` |
| `PAIRING_CEREMONY_VERSION` | `1` | `packages/protocol/src/pairing.ts:5` |
| `PAIR_VERIFICATION_CODE_LENGTH` | `6` decimal digits | `packages/protocol/src/pairing.ts:6` |
| Pair request ID | `16 bytes / 32 lowercase hex` | `packages/protocol/src/pairing.ts:37` |
| Requester token | `32 bytes / 64 lowercase hex` | `packages/protocol/src/pairing.ts:45` |
| `PAIR_REQUEST_TTL_MS` | `600,000 ms` | `apps/coord/src/auth/handlers-pairing.ts:52` |
| `MAX_PENDING_PAIR_REQUESTS` | `32` | `apps/coord/src/auth/pairing-account.ts:16` |
| `PAIR_VERIFICATION_ATTEMPT_LIMIT` | `5` | `apps/coord/src/auth/pairing-secrets.ts:23` |
| Bootstrap token | `roost_bt_` + `48` lowercase hex; `86,400,000 ms` TTL | `apps/coord/src/auth/bootstrap-tokens.ts` |

## Errors

- A protected handler without a browser principal returns Connect `Unauthenticated: authentication required` with `x-roost-auth-layer=device` (`apps/coord/src/auth/auth-interceptor.ts:256-277`).
- Invalid request ID/token/key or malformed verification code returns `InvalidArgument`; ceremony mismatch returns `FailedPrecondition: pairing client must reload`.
- Missing front-door assertion returns `Unauthenticated: pairing requires front-door sign-in`. Poll/confirm deliberately use generic `NotFound` for unknown or foreign request/token evidence.
- Pair conflicts use `AlreadyExists`; revoked keys/approver continuity use `PermissionDenied`; non-pending/expired/terminal transitions use `FailedPrecondition`; the 32-request ceiling uses `ResourceExhausted`.
- Long-lived authenticated sockets close with code `4003`, reason `reauth required`, at the verified token deadline. Key-generation revocation closes with `4001` (`apps/coord/src/auth/ws-auth-deadline.ts`).

## Reference implementation

- Browser identity/signing: `apps/web/src/client/auth/web-key.ts`, `apps/web/src/client/auth/web-key-storage.ts`
- Fragment boundary: `apps/web/src/client/auth/fragment-credential.ts`
- Portable ceremony: `packages/protocol/src/pairing.ts`
- Coordinator verification/principal: `apps/coord/src/auth/jwt.ts`, `apps/coord/src/auth/auth-principal.ts`, `apps/coord/src/auth/auth-interceptor.ts`
- Pairing: `apps/coord/src/auth/handlers-pairing.ts`, `apps/coord/src/auth/pairing-account.ts`, `apps/coord/src/auth/pairing-confirmation.ts`
- Bootstrap/device lifecycle: `apps/coord/src/auth/handlers-auth-bootstrap.ts`, `apps/coord/src/auth/handlers-devices.ts`
