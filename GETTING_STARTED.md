<!-- AUDIENCE: human -->
# Getting started with Roost

The v0.5.0 self-hosted coordinator/worker runtime is released for macOS
arm64/x64 and Linux arm64/x64. Production fleet rollout remains pending.

**Only coordinator and worker machines need a supported host OS.** Everything
you browse *from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android
phone, an iPad, an Android tablet, whatever — needs nothing but a modern
browser (optionally added to the home screen as a PWA).

## One coordinator HTTP/TLS front-door shape

There is exactly one coordinator HTTP/TLS front-door contract:

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

This one-front-door statement applies to coordinator HTTP/TLS only. Peer
transport does not add a worker HTTP/TLS front door: it can create an
authenticated worker UDP endpoint only after coordinator admission. The
coordinator serves the SPA from its own binary, so nothing else has to host the
dashboard.

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
health on its loopback HTTP bind, not direct-peer connectivity, and points the
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

## Three coordinator HTTP/TLS front-door recipes

Pick one. Roost implements none of them: each is ordinary configuration for
software you already know how to operate, and each ends with the same two
coordinator HTTP/TLS facts — the loopback bind and the `ROOST_WEB_PUBLIC_URL`
it is told.

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

`roost push` first commits the local POSIX coordinator through participant-free
journal V4. It requires a clean complete Git commit and proof that the commit is
on the configured upstream unless `--no-git` was explicitly chosen. Zero online
workers is valid; `--targets` may name a subset; `--no-web` retains the current
coordinator SPA.

Coordinator activation stages an immutable release, stops the prior service,
then snapshots the database. The target boots with durable mutation ingress held
and reports `update_ready=false` plus its transaction ID. After target identity,
SPA stamp, service definition and startup policy are proved, the CLI checkpoints
`finalizing`; the target releases the write gate and reports ready. Rollback
checkpoints `prior-restored` before the prior service can admit writes, so
recovery never restores the database twice.

After coordinator commit and lock release, `push` submits one durable job per
selected worker. Each host owns its platform journal and lease independently:
one activation failure cannot roll back a successful worker or the coordinator.
Offline, keeper-blocked, missing-runtime and paused-Windows hosts are deferred,
not forced. Exit 8 means an attempted worker failed; output separately counts
updated, already-current, deferred and failed machines.

The coordinator reevaluates routable workers every 30 seconds and retained
waiting/recovery records on their persisted schedule. A returning machine
automatically catches up to the running coordinator SHA. Settings → Machines
shows the same durable operation in every browser and keeps Update/Retry plus a
copyable structured report; Refresh status never reloads the page or starts a
deployment.

The fleet is therefore NOT guaranteed to be one version between a push and a
deferred machine's return. What that window costs is wire compatibility between
a new coordinator and an old worker — the cell-frame and `SessionEvent` shapes
in `apps/shared/src/wire/` — so a change to those shapes must stay backward
compatible for one release, or the deferred machine must be updated before the
shape change ships.

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

## Automatic terminal transport

Roost starts coordinator Sync immediately and automatically elects one carrier
per terminal session, in this order:

1. Same-worker loopback, when the browser can reach the selected worker's local
   door.
2. A qualified, coordinator-admitted, authenticated WebRTC peer.
3. Coordinator Sync fallback.

There is no browser setting to force a carrier. Sync remains the metadata,
authorization/control, signaling, and fallback plane; direct transport carries
only the worker-authoritative terminal view, cells, input, and history. It is
not an SSH transport, a browser-side terminal renderer, a worker HTTPS service,
or a replacement for coordinator control.

### Same-worker loopback

Every worker serves the SPA on its own loopback door, default
`ROOST_WORKER_LOCAL_UI_BIND=127.0.0.1:4104`. A browser on that machine can open
`http://127.0.0.1:4104` and talk to that worker's PTYs directly. The door
refuses any non-loopback bind, answers only loopback names
(`127.0.0.1`, `localhost`, `[::1]`), refuses other `Host` values, and advertises
only the coordinator URL and worker fingerprint at `/api/local-bootstrap`.
`ROOST_WEB_DIST_PATH` overrides the SPA it serves for source runs.

When a coordinator-served page has a worker on the browser machine, its first
live terminal pane probes `http://127.0.0.1:4104` once. If the worker answers,
that worker's sessions prefer loopback and their pane tabs show the direct
marker. Chromium can require a one-time local-network permission. Firefox and
Safari block a plaintext-loopback request from an HTTPS page; those browsers
continue with qualified WebRTC or Sync without a missing terminal. The door
admits the coordinator origin; if the browser front door differs from the
worker's coordinator URL, list it in the worker's
`ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS` (comma-separated).

Changing the local door port changes the pre-allowlisted
`http://127.0.0.1:4104` origin. Add the new origin to the coordinator's
`ROOST_CORS_ALLOWED_ORIGINS` or its cross-origin RPCs and Sync socket are
refused. A coordinator-served page does not learn a moved local port, so point
its one-shot probe at it with
`localStorage.setItem("roost.localWorkerOrigin", "http://127.0.0.1:<port>")`.

### WebRTC peer configuration

These settings are read when the relevant service starts. Values other than
the stated exact forms fail startup instead of being treated as truthy.

| Service | Variable | Default and effect |
|---|---|---|
| Coordinator | `ROOST_TERMINAL_PEER_ENABLED` | `1` when unset; accepts only `0` or `1`. `0` disables WebRTC peer admission and negotiation fleet-wide. |
| Coordinator | `ROOST_TERMINAL_PEER_STUN_URLS` | Unset: `stun:stun.cloudflare.com:3478`. An explicit empty value disables external STUN discovery. Custom values are constrained below. |
| Worker | `ROOST_TERMINAL_PEER_ENABLED` | `1` when unset on macOS/Linux; `0` when unset on Windows. Accepts only `0` or `1`; Windows rejects explicit `1`. `0` keeps this worker on loopback/Sync only. |
| Worker | `ROOST_TERMINAL_PEER_BIND_ADDRESS` | Unset: ICE may gather supported interfaces. Otherwise one literal unicast IPv4 or IPv6 address; hostnames are rejected. |
| Worker | `ROOST_TERMINAL_PEER_PORT_RANGE` | Unset: ICE uses ephemeral UDP ports. Otherwise `min-max`, inclusive decimal ports from `1024` through `65535`. |

Set `ROOST_TERMINAL_PEER_ENABLED=0` on the coordinator to disable new WebRTC
peers across the fleet, or on an individual worker to disable its peer
capability. Both choices retain same-worker loopback and Sync. Set
`ROOST_TERMINAL_PEER_STUN_URLS=` on the coordinator to retain WebRTC host
candidates while disabling external address discovery.

The STUN list is coordinator-owned and sent unchanged to both peer endpoints;
a browser or worker request cannot choose it. A nonempty list contains one to
four distinct comma-separated `stun:` UDP URLs. Each URL is a DNS name, IPv4,
or bracketed IPv6 address with an optional valid port; whitespace, control
characters, duplicates, credentials, paths, queries, fragments, `turn:`,
`turns:`, and `stuns:` are rejected. Roost provides no TURN service or
credentials. STUN observes address-discovery traffic, not terminal contents or
Roost grants; an unavailable STUN server leaves host candidates, loopback, and
Sync usable.

### UDP, privacy, and fallback boundaries

A worker opens no peer UDP socket until a coordinator-authenticated, current
grant admits an offer. WebRTC terminal data is encrypted, but ICE necessarily
shares candidate route IP/port metadata with the authenticated peer, and a
STUN service observes its address-discovery traffic. Browser privacy policy can
hide candidates or deny usable UDP paths; Roost requests no camera or
microphone permission.

Roost does not open firewalls, forward ports, install or manage Tailscale, or
guarantee NAT traversal. `ROOST_TERMINAL_PEER_PORT_RANGE` and
`ROOST_TERMINAL_PEER_BIND_ADDRESS` let an operator fit existing UDP policy, but
blocked UDP, browser policy, NAT, or ICE failure simply leaves the session on
Sync. A Tailscale or other VPN interface may provide a usable route; it is
neither required nor evidence that WebRTC will connect.

An established healthy direct route can continue painting and accepting input
during a coordinator outage while its authorization remains valid. It is not
permanent: new negotiation, grant renewal, Sync fallback, and fleet controls
need coordinator reachability. The UI says
`Coordinator unreachable — direct terminals may remain available; fleet controls unavailable`
only for currently live direct routes. If the direct route ends before the
coordinator returns, the terminal remains unavailable rather than receiving a
replayed input; Sync repairs the route after the coordinator returns.

### Route state and diagnostics

An elected loopback pane is labeled **Direct on this device**; an elected WebRTC
pane is labeled **Direct peer connection**. A candidate is not displayed as an
active direct route. Use the terminal context menu's **Capture terminal
diagnostic** action to inspect route metadata: active/candidate carrier kind,
worker epoch, opaque peer ID, candidate type, peer phase, probe age, control
RTT, buffered bytes, fallback reason, and pending-input count.

Route metadata excludes SDP, ICE candidate addresses, grant material, and
credentials. Terminal diagnostic capture has its separate terminal-content
consent and warning; handle the full capture according to that warning.

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
The `spa:` line is a page request against the coordinator's own listener, not a
guess from configuration: `served` with the `ROOST_WEB_DIST_PATH` the installed
service stamped, or `MISSING` when that listener answers no page — the state
where RPCs and terminals keep working while every URL is a 404. The remedy
names which cause applies (a stamped dist with no `index.html`, or one that
exists but is unused). Which build the coordinator actually picked is its own
startup line: `spa_source`, or `spa_source_missing` when it has neither a disk
dist nor an embedded build.
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

Use one release commit and one fleet transaction:

1. Qualify the four public host targets—macOS arm64/x64 and Linux arm64/x64—
   from the same source commit. Each published binary must match its GitHub
   Release SHA-256 sidecar.
2. Run the hermetic real-flow tier on that commit:
   `bun run test:terminal` (real coordinator, worker, keeper, PTY, and browser
   through `smoke/terminal/stack.ts`). That tier is the gate; a live canary
   only observes a deployment.
3. From the clean pushed checkout, run
   `bun apps/roost-cli/src/main.ts push`. The coordinator commits independently,
   then each macOS/Linux worker settles through its own durable job. A failed
   host leaves successful machines committed; an offline or safety-blocked host
   is named as deferred and automatically catches up when safe. Confirm required
   machines reach the new SHA in `roost status` or Settings → Machines.
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
