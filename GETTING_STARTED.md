<!-- AUDIENCE: human -->
# Getting started with Roost

The v0.5.0 self-hosted coordinator/worker runtime is released for macOS
arm64/x64 and Linux arm64/x64. Production fleet rollout remains pending.

**Only coordinator and worker machines need a supported host OS.** Everything
you browse *from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android
phone, an iPad, an Android tablet, whatever — needs nothing but a modern
browser (optionally added to the home screen as a PWA).

## One deployment shape

There is exactly one contract:

- The coordinator listens on loopback, in plaintext:
  `ROOST_COORDINATOR_BIND=127.0.0.1:4103`.
- You put a front door in front of it — Caddy, nginx, a Cloudflare tunnel,
  `tailscale serve`, anything that terminates TLS and proxies HTTP.
- That front door is a trusted proxy, so the coordinator reads its
  `X-Forwarded-For`: `ROOST_TRUST_PROXY=1`.
- You tell the coordinator the resulting public origin:
  `ROOST_WEB_PUBLIC_URL=https://roost.example.com`.

Roost owns no TLS, no DNS, no tunnel, and no certificate renewal, and it never
invents a hostname for you. The origin you declare seeds the SPA's CSP
`connect-src` and the Sync WebSocket origin allowlist, so it must be exactly
the origin a browser addresses — scheme, host, and non-default port included.

The coordinator serves the SPA from its own binary, so nothing else has to host
the dashboard.

## Install + run

On macOS arm64/x64 or Linux arm64/x64, install the published binary. The
installer verifies it against the adjacent GitHub Release SHA-256 sidecar:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
```

Then run quickstart. It takes one endpoint flag, and requires it:

```sh
"$HOME/.local/bin/roost" quickstart --coordinator-url "https://roost.example.com"
```

`--coordinator-url` is an absolute `https:` origin with no userinfo, query, or
fragment and no path beyond `/`. An explicit port is optional:
`https://roost.example.com` and `https://roost.example.com:8443` are both
accepted. Quickstart builds the SPA, installs the coordinator service bound to
loopback, deploys a worker on the same machine, waits for health, prints a
status readout, and opens an already-authorized browser. It proves coordinator
health on the loopback bind — the only listener Roost owns — and points the
local worker at the origin you declared, so a successful worker registration
also proves your front door passes worker traffic.

The installed coordinator service carries this endpoint contract:

```text
ROOST_COORDINATOR_BIND=127.0.0.1:4103
ROOST_TRUST_PROXY=1
ROOST_WEB_PUBLIC_URL=https://roost.example.com
```

`apps/coord/scripts/install.sh` takes the bind port from
`ROOST_COORD_LOOPBACK_PORT` (default 4103) and persists the resolved
`ROOST_COORDINATOR_BIND`, so the service definition states the listener once.

`ROOST_COORDINATOR_PUBLIC_URL` is optional and separate: it is the coordinator's
own identity origin, for installs where workers dial a different door than the
browsers do. Leave it unset and workers use the browser front door. The URL a
worker dials resolves as `ROOST_COORDINATOR_URL` →
`ROOST_COORDINATOR_PUBLIC_URL` → `ROOST_WEB_PUBLIC_URL`; with none of the three
set, enrollment refuses rather than guessing an origin.

> **Windows host releases are paused.** v0.5.0 publishes no Windows
> coordinator, worker, installer, join script, or package. Windows remains
> supported as a browser client, but there is no supported Windows host
> install, enrollment, or update procedure in this release.

The source/development path is separate and intended for macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That command installs Bun and a checkout which tracks `main`; it is not a
pinned production release.

Coordinator startup creates and thereafter validates the single local tenant
automatically: the internal `local@roost.invalid` account, its `personal`
organization, and the `default` dashboard. Existing coherent single-tenant
databases keep their IDs and names. There is no separate organization bootstrap
command to run before quickstart or after an upgrade.

## Three front-door recipes

Pick one. Roost implements none of them: each is ordinary configuration for
software you already know how to operate, and each ends with the same two facts
— the coordinator's loopback bind and the `ROOST_WEB_PUBLIC_URL` it is told.

### Recipe 1 — Caddy with your own domain

You have a domain and a machine reachable on 80/443. Caddy obtains and renews
the certificate itself; nothing about it reaches Roost.

`/etc/caddy/Caddyfile`:

```caddy
roost.example.com {
	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {remote_host}
	}
}
```

```sh
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Workers dial this same origin in the standard install, so `/ws/coord-worker/*`
passes.

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4103` plaintext with
`ROOST_TRUST_PROXY=1`, and `ROOST_WEB_PUBLIC_URL=https://roost.example.com`.

### Recipe 2 — Cloudflare tunnel

No open inbound ports, works behind NAT, and no certificate on the box.
Install `cloudflared` on the coordinator host, then:

```sh
cloudflared tunnel login
cloudflared tunnel create roost
cloudflared tunnel route dns roost roost.example.com
```

`$HOME/.cloudflared/config.yml`:

```yaml
tunnel: <TUNNEL-UUID>
credentials-file: /absolute/path/printed/by/cloudflared/<TUNNEL-UUID>.json
ingress:
  - hostname: roost.example.com
    service: http://127.0.0.1:8080
  - service: http_status:404
```

`cloudflared` **appends** the visitor address to any client-supplied
`X-Forwarded-For`, so it must not be the last hop (see the caller-address note
below). Put a proxy between the tunnel and the coordinator that rewrites the
header from `CF-Connecting-IP` and denies the private paths:

```caddy
http://roost.example.com:8080 {
	@no_cf_ip not header CF-Connecting-IP *
	respond @no_cf_ip "not found" 404

	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {http.request.header.CF-Connecting-IP}
	}
}
```

Workers dial this same origin in the standard install, so `/ws/coord-worker/*`
passes.

Install the tunnel as a service with an explicit config path — under `sudo` the
service's `$HOME` is `/root`, so `cloudflared` would not otherwise find the file
you just wrote:

```sh
sudo cloudflared --config "$HOME/.cloudflared/config.yml" service install
sudo systemctl enable --now cloudflared   # Linux; launchd starts it on install
```

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4103` plaintext with
`ROOST_TRUST_PROXY=1`, and `ROOST_WEB_PUBLIC_URL=https://roost.example.com`.

### Recipe 2a — Cloudflare Access in front of the browser surfaces

This extends recipe 2 with Cloudflare Access on the same hostname. Create two
self-hosted Access applications. Cloudflare matches the most specific path
first:

- **Roost browser** — paths `/` (the SPA document and assets) and
  `/roost.v1.CoordinatorService/PairCreate`. Set the policy to **Allow**,
  selector **Emails**, with the owner's exact Cloudflare-verified email.
  Choose the session duration to taste.
- **Roost machines** — paths `/ws/` and `/roost.v1.CoordinatorService/`.
  Set the policy to **Bypass**, **Everyone**. These paths carry worker and CLI
  traffic, which already authenticates with a signed Ed25519 JWT from an
  authorized key; the SPA is never served from them.

`PairCreate` is protected while the other RPCs are not because no non-browser
client calls it (the CLI has no pairing verb). Gating that one RPC therefore
costs nothing and gives every pairing request a Cloudflare-verified email.

Add these coordinator settings alongside the existing `ROOST_TRUST_PROXY=1`:

```text
ROOST_CF_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
ROOST_CF_ACCESS_AUD=<Application Audience Tag from the browser app>
```

Set both variables or neither. A half-configured Access gate is an error.

The Caddy hop must keep forwarding `Cf-Access-Jwt-Assertion` and the visitor
location headers `CF-IPCountry`, `CF-Region`, and `CF-IPCity`.
`reverse_proxy` forwards them by default; do not add a `header_up -` rule that
strips them. Keep the existing `X-Forwarded-For` rewrite from
`CF-Connecting-IP`:

```caddy
reverse_proxy 127.0.0.1:4103 {
	header_up X-Forwarded-For {http.request.header.CF-Connecting-IP}
}
```

Enable Cloudflare's managed transform so city and region are available:
**Rules → Settings → Managed Transforms → Add visitor location headers**.
Without it, only `CF-IPCountry` arrives and the pairing card shows country
only.

The tailnet recipe (recipe 3) and loopback access are unaffected. On-host
requests are exempt, and the Access gate is inert when
`ROOST_CF_ACCESS_TEAM_DOMAIN` and `ROOST_CF_ACCESS_AUD` are unconfigured.



### Recipe 3 — `tailscale serve`

No domain, no public exposure, no certificate management: every device that
browses Roost joins your tailnet.

```sh
tailscale serve --bg --https=443 http://127.0.0.1:4103
tailscale serve status
```

The public origin is the machine's MagicDNS name, e.g.
`https://roost-host.tailnet-name.ts.net`. `tailscale serve` has no path
matcher, so if you need the private paths denied, keep a local proxy in front of
the coordinator as in recipe 1 and point Serve at that instead.

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4103` plaintext with
`ROOST_TRUST_PROXY=1`, and
`ROOST_WEB_PUBLIC_URL=https://roost-host.tailnet-name.ts.net`.

### The caller address must be overwritten, never appended

The coordinator reads the **first** entry of `X-Forwarded-For` as the caller
address, and uses it to decide whether a request is on-host. Your front door
must therefore *replace* that header with its own client address rather than
appending to a chain the client supplied — an appending proxy lets any client
prepend an address of its choosing and claim to be on-host.

One request proves it, from a machine that is not the coordinator:

```sh
curl -si https://roost.example.com/api/db-export | head -1
```

Expect `403` (or `404` if you deny the path at the front door). A `200` means
the caller address is not reaching the coordinator correctly: fix the front door
before going further.

### `/internal/*` and `/api/db-export` are private

`/internal/*` is a namespace the coordinator reserves for private on-host
routes; `/api/db-export` is its whole database snapshot. Deny both at the front
door, as both recipes above do.
`/api/db-export` additionally refuses any caller the coordinator does not
resolve as on-host, so the edge rule is defence in depth rather than the only
guard.

`/ws/coord-worker/*` — the worker link — **passes** by default, because in a
standard install the workers reach the coordinator through the same front door
the browsers use. A worker dials `ROOST_COORDINATOR_URL`, else
`ROOST_COORDINATOR_PUBLIC_URL`, else `ROOST_WEB_PUBLIC_URL`, and both the
coordinator's deploy path and `roost add-machine` resolve it in exactly that
order.

**Hardening variant — only after you have given workers their own origin.**
Declare that origin as `ROOST_COORDINATOR_PUBLIC_URL` on the coordinator — it
must be a separate HTTPS front door a worker can reach, such as a tailnet
`tailscale serve` URL or an HTTPS proxy on a VPN address, never the
coordinator's own plaintext loopback listener — and make each worker's
`ROOST_COORDINATOR_URL` name it. Then, and only
then, the browser-only door denies the worker path too. Both doors define the
same site address, so **choose exactly one** — never paste both.

Shared browser + worker door, the default, because workers dial this origin:

```caddy
roost.example.com {
	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {remote_host}
	}
}
```

Browser-only door, valid once workers dial `ROOST_COORDINATOR_PUBLIC_URL`
instead:

```caddy
roost.example.com {
	@private path /internal/* /api/db-export /ws/coord-worker/*
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {remote_host}
	}
}
```

Behind a Cloudflare tunnel the same distinction lands on the Caddy hop the
tunnel points at — keep its `CF-Connecting-IP` guard and header rewrite, and
choose the matcher the same way. Again, exactly one of the two.

Shared browser + worker door, behind `cloudflared`:

```caddy
http://roost.example.com:8080 {
	@no_cf_ip not header CF-Connecting-IP *
	respond @no_cf_ip "not found" 404

	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {http.request.header.CF-Connecting-IP}
	}
}
```

Browser-only door, behind `cloudflared`:

```caddy
http://roost.example.com:8080 {
	@no_cf_ip not header CF-Connecting-IP *
	respond @no_cf_ip "not found" 404

	@private path /internal/* /api/db-export /ws/coord-worker/*
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4103 {
		header_up X-Forwarded-For {http.request.header.CF-Connecting-IP}
	}
}
```

Denying the worker path while workers still dial the public origin strands
every one of them: their transport answers 404 and no session on that machine
is reachable.

## First browser enrollment

Quickstart mints a one-shot browser grant in a `#pair` URL fragment and passes
that URL directly to the platform browser opener. Quickstart never prints or
logs the grant. URL fragments are not sent in the HTTP request or an HTTP
`Referer`, so the coordinator and intermediaries do not receive the secret as
URL metadata; the loaded Roost app redeems it.

Do not try to copy a pairing secret from terminal output, shell history, logs,
or screenshots. If the platform opener fails, arrange a working local browser
opener and rerun quickstart, or use **Settings → Pair a device** from an already
authorized browser. Later devices should always use that Settings pairing flow.

## Pair your phone

Make the front door reachable from the phone, open it, then choose **Settings →
Pair a device** in Roost on an already-authorized browser and scan the QR with
the phone's camera. On a tailnet front door, install the Tailscale app on the
phone and sign in to the same tailnet first.

Pairing is what authorizes a device. Network reachability, a VPN membership, or
a login your front door performs on its own does not authorize a phone as a
Roost device.

## Turn on agent notifications

Terminals running a coding agent show their state (working / needs input /
done) with no setup: the sidebar row, tab, and folder rollup update themselves,
and a background agent that stops for input or finishes raises an in-app toast
plus an unseen count in the browser tab title.

OS notifications — the kind that reach you when Roost isn't the tab you're
looking at — need one explicit grant per device, because the browser only asks
on a real click:

1. Open Roost on that device and go to **Settings → Notifications**.
2. Turn on **Desktop notifications** and accept the browser permission prompt.
   On iPhone/iPad, add Roost to the home screen first and open it from there —
   Safari only allows notifications for installed web apps.
3. Optionally turn on a sound for "needs input" and/or "finished".

Each device subscribes separately, and a device that is actively viewing the
session it is about does not get an OS notification for it. Tapping a
notification opens that session.

## Add another machine

v0.5.0 enrolls macOS or Linux workers. **Settings → Machines → Add machine**
creates a one-shot pull command; the CLI equivalents are
`roost add-machine --platform macos` and `roost add-machine --platform linux`.
The enrollment URL comes from the installed coordinator service definition,
overlaid by the ambient environment, in the order `ROOST_COORDINATOR_URL` →
`ROOST_COORDINATOR_PUBLIC_URL` → `ROOST_WEB_PUBLIC_URL`. With none of them set
the command refuses, naming all three, instead of inventing an origin. No
coordinator SSH or push is involved.

Paste the generated command on the worker:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://roost.example.com" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

The worker must reach that origin and trust its certificate chain, and the
origin must accept `/ws/coord-worker/*`. Give workers a private origin
(`ROOST_COORDINATOR_PUBLIC_URL` on the coordinator, and the matching
`ROOST_COORDINATOR_URL` here) when you would rather keep the worker link off
the public front door.

The machine appears in **Settings → Machines** within a few seconds. macOS
uses launchd and Linux uses `systemd --user`. The server-side bootstrap token
is one-shot and expires after 24 hours.

### OMP conversation restoration remains opt-in

Automatic OMP conversation restoration is disabled by default while the
official-OMP POSIX real-stack qualification is still outstanding. Unit and
integration coverage do not substitute for that qualification. On macOS or
Linux, an operator may explicitly add
`ROOST_AGENT_CONVERSATION_RESTORE=1` to the worker's first install or generated
join command. The setting accepts exactly `0` or `1`; an installed explicit
value is preserved by later source deployments, including an explicit `0`
opt-out. Windows rejects explicit `1` as unsupported.

The switch applies only when keeper adoption fails after involuntary PTY loss.
After the ordinary replacement shell is created and `respawned` is durably
admitted, the worker may type exactly one fixed resume command —
`omp --resume=<reference>`, canonically quoted — as one CR-terminated input
batch. Successful adoption sends no resume input, a duplicate reference is
skipped, and a rejected or ambiguous input is never retried.
The opaque reference remains private recovery metadata until the OMP
integration replaces or clears it; Roost still owns no agent conversation or
transcript.

## Update the fleet

To update a source-installed coordinator and its registered fleet from a clean
Roost checkout, run:

```sh
bun apps/roost-cli/src/main.ts push
```

`roost push` is one journaled transaction across the local POSIX coordinator
and the exact complete registered macOS/Linux worker fleet. It requires at
least one registered worker, a clean complete Git commit, and proof that the
commit is on the configured upstream (unless `--no-git` was explicitly
chosen). A registered Windows worker blocks the rollout. `--targets` may name
the exact complete registered set, but cannot narrow it; `--no-web` only
retains an existing coordinator SPA.

The command snapshots the live coordinator database, activates and proves the
target coordinator in a held state, then stages and proves every worker at the
same SHA with a current keeper and fresh heartbeat. Only then does it record
the durable finalization decision. Before that decision, any participant
failure rolls every worker back, restores and proves the prior coordinator and
database, and reports failure. After that decision, interrupted recovery can
only finish the target release.

One-host POSIX deployment remains a separate source operation:
`bun apps/roost-cli/src/main.ts deploy <host>` stages the exact pushed commit
over SSH. Source deployments intentionally refuse to run from the standalone
release binary because it does not contain a Git checkout.

A remote target's own identity is never taken from the shell running the
deploy. `ROOST_WORKER_LABEL` and `ROOST_REACHABLE_ADDR` come from the target's
installed service definition, or from `--label=<name>` /
`--reachable-addr=<fqdn>` on the command line; with neither present the target
derives its own hostname and reachable address. Exporting either variable while
deploying to a host that has no prior install refuses the deploy rather than
registering that host under this machine's name.

## Check current health and recent anomalies

```sh
roost status
roost doctor --since 1h
```

`roost status` reports the local coordinator and worker services, coordinator
health and tagged SHA against its own loopback bind, the configured public URL
and whether it answers `AuthCoordIdentity`, and remote worker age/build
observations. A public URL that is not configured prints as informational; a
configured one that does not answer fails the run and names the remedy — point
your front door at the coordinator's loopback bind. Its exit status gates both
local services, coordinator reachability, and that public-URL answer; inspect
the fleet rows rather than treating that exit status as proof that every remote
worker converged.
`roost doctor --since <window>` summarizes local logs from that window and
reports anomalies such as uncaught errors, sequence gaps, queue overflows,
degraded keepers, and failed backups/readiness.

During an ordinary worker or coordinator-link disconnect, keeper processes
continue owning the PTYs. Crash-safe lifecycle events replay before the worker
snapshot, and visible browsers redial, rehydrate, and rebaseline in place
without requiring a page reload. Keeper adoption retains a bounded 1 MiB raw
history window per channel; it is not an unlimited full-scrollback guarantee,
and continuity failures surface as doctor diagnostics rather than being
silently spliced.

## Backups and rollback scope

The coordinator creates a verified SQLite snapshot before applying pending
migrations to an existing database. It also backs up every 24 hours from
process start, including an immediate startup backup when none exists or the
newest is stale. It integrity-checks the standalone snapshot before
compressing it and retains the 14 newest
`coord_v2.<timestamp>.db.gz` archives in the coordinator data directory's
`backups/` folder.

These archives are same-host recovery material. They do not survive loss of
the coordinator disk, are not off-host disaster recovery, and are not the
automatic fleet-rollout rollback mechanism. Copy them to storage with an
independent failure domain and own the restore procedure when host-loss
recovery is required.

Atomic rollout creates a separate temporary gzip snapshot, records its digest
in the coordinator deploy journal, and verifies decompression and SQLite
integrity before an automatic restore. Successful fleet finalization removes
that transaction snapshot and journal.

## Release rollout and canaries

Use one release commit and one atomic fleet transaction:

1. Qualify the four public host targets—macOS arm64/x64 and Linux arm64/x64—
   from the same source commit. Each published binary must match its GitHub
   Release SHA-256 sidecar.
2. Run the hermetic real-flow tier on that commit:
   `bun run test:terminal` (real coordinator, worker, keeper, PTY, and browser
   through `smoke/terminal/stack.ts`). That tier is the gate; a live canary
   only observes a deployment.
3. From the clean pushed checkout, run
   `bun apps/roost-cli/src/main.ts push`. Require the exact registered
   macOS/Linux fleet to converge atomically: exhaustive staging/proof and
   global rollback before the durable decision, finish-only recovery after it.
4. Run the live API canary against the installed origin:
   ```sh
   ROOST_COORD_URL="https://roost.example.com" \
     bun test smoke/api_smoke.test.ts
   ```
5. Restart the coordinator and local worker. Require a new coordinator boot
   timestamp, all workers online on the expected build, and the pre-restart PTY
   to paint a new marker. Reject new uncaught errors, sequence gaps, queue
   overflows, stale keepers, or failed backup/readiness events.
6. Re-prove the front door: an unauthenticated `MiscHealth` POST through the
   public origin reaches the coordinator, and `/api/db-export` from off-host
   answers 403 or the front door's 404.

## Logs

```sh
roost logs coord     # coordinator logs
roost logs worker    # worker logs
```
