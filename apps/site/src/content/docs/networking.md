---
title: "Networking"
description: "One loopback coordinator listener, the front door you choose, and the paths that stay private."
order: 7
section: "Reference"
---

## Start local; declare an HTTPS origin only for expansion

Fresh `roost quickstart` creates a loopback-only coordinator and local worker:

```text
browser and local worker  →  http://127.0.0.1:4103
```

The persistent service profile is
`ROOST_COORDINATOR_BIND=127.0.0.1:4103`, `ROOST_TRUST_PROXY=0`,
`ROOST_WEB_PUBLIC_URL=`, `ROOST_COORDINATOR_PUBLIC_URL=`,
`ROOST_CORS_ALLOWED_ORIGINS=http://127.0.0.1:4103`, and
`ROOST_SKIP_ENV_LOCAL=1`. No domain, HTTPS proxy, VPN, or external address is
needed for that first machine. `roost status` correctly reports local-only
access as unconfigured remote access, not a fault.

To make Roost available beyond that machine, choose an operator-managed HTTPS
origin and run this on the coordinator host **before** exposing the listener:

```sh
roost quickstart --coordinator-url https://roost.example.com
```

`--coordinator-url` must be an absolute `https:` origin with no path, query, or
fragment; an explicit port is optional. Promotion changes the coordinator
endpoint profile and restarts only the coordinator, retaining the local worker,
keeper, PTYs, and state. Configure the front door afterwards:

```text
your HTTPS front door  →  installed 127.0.0.1:4103 bind
```

It must overwrite `X-Forwarded-For`. Roost does not provision certificates,
DNS, tunnels, VPNs, SSH, or target-machine reachability; a declared origin is
not a reachability proof.

## Three front doors

Any reverse proxy works. These three cover most installs, and Roost implements
none of them — copy-paste configuration lives in
[Install](/docs/install/) and the repository's `GETTING_STARTED.md`.

| | Caddy with your domain | Cloudflare tunnel | `tailscale serve` |
|---|---|---|---|
| Public DNS/domain | yours | yours, on Cloudflare | none |
| Inbound ports open | 80/443 | none | none |
| Certificate on the box | Caddy manages it | none | Tailscale manages it |
| Browser device software | ordinary browser | ordinary browser | Tailscale client |
| Best reason to choose it | plain public hosting | works behind NAT | no domain, no exposure |

Workers dial the coordinator origin outbound and never listen for inbound
connections. The first local worker dials its loopback coordinator. Remote
enrollment requires a declared, non-loopback HTTPS origin; use
`ROOST_COORDINATOR_PUBLIC_URL` only when workers need a different HTTPS door
from browsers.

## Overwrite `X-Forwarded-For`, never append

The coordinator reads the **first** entry of `X-Forwarded-For` as the caller
address, and that address decides whether a request counts as on-host. Your
front door must therefore replace the header with its own client address.
A proxy that appends — `cloudflared` appends the visitor address after any
client-supplied chain — lets a caller prepend an address of its choosing and
claim to be on-host.

Prove it from a machine that is not the coordinator:

```sh
curl -si https://roost.example.com/api/db-export | head -1
```

`403` is correct, and `404` is equally correct once you deny the path at the
front door as below. Only `200` is a failure: it means the caller address is
not arriving intact and the whole database is downloadable.

## Paths that stay private

Deny two prefixes at the front door: `/internal/*`, a namespace the coordinator
reserves for private on-host routes, and `/api/db-export` (the whole database).
The coordinator also refuses `/api/db-export` for any caller it does not resolve
as on-host, so the edge rule is defence in depth.

`/ws/coord-worker/*` — the worker link — passes by default: in a standard
install workers dial the same origin browsers use. Deny it only once workers
have their own declared origin (`ROOST_COORDINATOR_PUBLIC_URL` on the
coordinator, matching `ROOST_COORDINATOR_URL` on each worker). Denying it while
workers still dial the public origin strands every one of them on a 404
transport.

## No network position grants authority

Being on the tailnet, on a VPN, or on loopback is transport reachability, never
authorization. Every browser redeems a scoped one-shot grant or completes the
approved-and-confirmed pairing ceremony; every worker redeems a scoped worker
grant. A login your front door performs is authentication of a human, not
authorization of a device.

The optional HTTPS front door transports pairing but cannot complete it. A
trusted browser's approval reveals one six-digit code; only the requesting
browser's matching token-bound confirmation grants access. Old ceremony tabs
fail closed with `pairing client must reload`, and the version-1 migration
expires legacy pending requests.

WireGuard, Headscale, ZeroTier, and other private overlays make the front door
reachable and change nothing else; Roost neither configures nor exercises them.

## Next

- [Install](/docs/install/) — the one install shape and the front-door recipes
- [Fleet](/docs/fleet/) — why workers only ever dial outbound
- [Security](/docs/security/) — pairing, device keys, and revocation
- [The CLI](/docs/cli/) — `status`, `doctor`, and the rest
