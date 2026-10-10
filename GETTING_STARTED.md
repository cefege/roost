<!-- AUDIENCE: human -->
# Getting started with Roost

Roost's coordinator and workers run on macOS arm64/x64 and Linux arm64/x64.
A Windows 11 x64 PC runs as a worker that joins an existing coordinator.

**Only coordinator and worker machines need a supported host OS.** Everything
you browse *from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android
phone, an iPad, an Android tablet, whatever — needs nothing but a modern
browser (optionally added to the home screen as a PWA).

## Install

On the Mac or Linux machine that will host Roost, run:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | bash
```

That one command:

1. Fetches the newest stable v3 release's `roost` and `roost-keeper` for this
   machine (the newest pre-release while no stable v3 exists, and says so) and
   checks each against the SHA-256 digest published beside it. A v3 `roost`
   already in `~/.local/bin` or on `PATH` is used instead.
2. Runs `roost quickstart`, which installs the coordinator and this machine's
   worker as user services (launchd on macOS, `systemd --user` on Linux),
   installs the web app published with the same release, waits for the
   coordinator to answer, and links `~/.local/bin/roost`.
3. Prints a status readout and opens your browser at `http://127.0.0.1:4113`,
   already paired.

You need no domain, HTTPS proxy, VPN, toolchain or other runtime. On Linux,
quickstart turns on linger for your account so the services outlive logout,
and refuses before writing anything when the account may not.

To see every file it would write first, with nothing changed:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | bash -s -- --dry-run
```

Rerunning the install is safe: it preserves the installed endpoint, database,
worker and keeper, reactivates the coordinator, and opens a new browser pairing.

On a machine with no desktop to open a browser on (a server reached over SSH),
run `roost add-browser` there and open the URL it prints wherever you have a
browser that can reach the coordinator.

Coordinator startup creates and thereafter validates the single local tenant
automatically: the internal `local@roost.invalid` account, its `personal`
organization, and the `default` dashboard. There is no separate organization
bootstrap command to run before quickstart or after an upgrade.

## Start locally, then add a front door when you need one

The fresh local coordinator service uses this profile:

```text
ROOST_COORDINATOR_BIND=127.0.0.1:4113
ROOST_TRUST_PROXY=0
ROOST_WEB_PUBLIC_URL=
ROOST_COORDINATOR_PUBLIC_URL=
ROOST_CORS_ALLOWED_ORIGINS=http://127.0.0.1:4113
```

The coordinator listens only on loopback and serves plaintext there. Its local
worker dials that same loopback origin. Browser pairing still authorizes the
browser; it is not a network-access grant.

When you want browsers or another worker to connect from elsewhere, choose an
operator-managed HTTPS address. Before exposing the running local listener, run
this on the coordinator machine:

```sh
roost quickstart --coordinator-url "https://roost.example.com"
```

`--coordinator-url` is an absolute `https:` origin with no userinfo, query, or
fragment and no path beyond `/`; an explicit port is allowed. This promotion
changes only the coordinator endpoint profile and restarts only the coordinator.
It preserves the installed worker, keeper, PTYs, database, and local browser
route. Configure your front door afterwards to forward to the installed
loopback bind and to **overwrite**, never append, `X-Forwarded-For`.

Roost owns no TLS, DNS, tunnel, VPN, certificate renewal, SSH session, or
target reachability. A valid HTTPS address is not proof that another machine
can reach it. The front door's declared browser origin seeds the SPA's CSP
`connect-src` and Sync WebSocket allowlist, so it must be exactly the browser
origin — scheme, host, and non-default port included.

`ROOST_COORDINATOR_PUBLIC_URL` remains optional and separate: use it only when
workers should dial a different HTTPS door from browsers. The worker URL
precedence is `ROOST_COORDINATOR_URL` → `ROOST_COORDINATOR_PUBLIC_URL` →
`ROOST_WEB_PUBLIC_URL`.

## Run the coordinator in a container or on Kubernetes

The coordinator also ships as an image, `ghcr.io/cefege/roost-coordinator`
(`edge` tracks the `v3` branch; release tags carry their version). Workers are
not containerized: they own real PTYs on real machines, and enroll against the
containerized coordinator exactly as against an installed one.

Set `ROOST_COORDINATOR_DATABASE_URL=postgres://user:password@host:5432/db`
and the coordinator keeps no durable state on its own disk: it runs on a
read-only root filesystem, can be rescheduled freely, and its backups are the
Postgres operator's. Without that variable it uses its SQLite file under
`/var/lib/roost`, which must then be a volume. Setting both it and
`ROOST_COORDINATOR_DB` is refused. Run exactly one coordinator: live terminal
fan-out is in-process.

An existing SQLite install moves onto Postgres with `roost db-to-postgres`:
stop the coordinator, then

```sh
export ROOST_COORDINATOR_DATABASE_URL=postgres://roost:…@db-host:5432/roost
roost db-to-postgres                  # --from defaults to this install's file
```

Both ends are migrated to this build's schema, every table is copied in one
transaction, identity sequences continue past the copied ids, and each table's
count is checked against the source. A target that already holds rows (a
coordinator booted against it first) is refused unless you pass `--replace`.
Then swap `ROOST_COORDINATOR_DB` for `ROOST_COORDINATOR_DATABASE_URL` in the
coordinator's environment. Paired browsers, enrolled workers and push
subscriptions all carry over, because they are rows.

The reverse, Postgres back to a SQLite file, is `roost db-to-sqlite`. It reads
the source in one consistent snapshot, so the Postgres coordinator may keep
running; stop the one that will use the file:

```sh
export ROOST_COORDINATOR_DATABASE_URL=postgres://roost:…@db-host:5432/roost
roost db-to-sqlite                    # --to defaults to this install's file
```

Then swap `ROOST_COORDINATOR_DATABASE_URL` for `ROOST_COORDINATOR_DB` in the
coordinator's environment.

On one machine, with Docker:

```sh
docker compose up -d --build          # Postgres + coordinator on 127.0.0.1:4113
docker compose exec coordinator roost add-browser
```

`roost add-browser` prints a one-shot `…/#pair=…` URL; open it in a browser
to pair. On Kubernetes, with the chart in `deploy/helm/roost-coordinator`:

```sh
helm install roost-coordinator deploy/helm/roost-coordinator -n roost --create-namespace \
  --set database.existingSecret=roost-db \
  --set publicUrl=https://roost.example.com \
  --set ingress.enabled=true --set ingress.className=nginx \
  --set ingress.host=roost.example.com
kubectl -n roost exec deploy/roost-coordinator -- roost add-browser
kubectl -n roost exec deploy/roost-coordinator -- roost add-machine --platform linux
```

`roost-db` is a Secret with the key `ROOST_COORDINATOR_DATABASE_URL`. Or run
the chart's own single Postgres: `--set postgres.enabled=true --set
postgres.password=…`, plus `--set postgres.backup.enabled=true` for a daily
`pg_dump` CronJob that keeps 14 archives on its own PVC (restore with
`pg_restore --clean --if-exists -d roost <archive>`). On a multi-node cluster
pin the coordinator, Postgres and its backups to one node with `nodeSelector`.
`--set replicas=0` stops the coordinator for maintenance such as a
`roost db-to-postgres --replace` through `kubectl port-forward`; the chart
refuses any count but 0 or 1. The pod is probed on `/readyz` (the database
answers and the process is not draining) and `/healthz`; on `SIGTERM` it
withdraws readiness and gives open connections 20 s before exiting.
`--set logs.persistence.enabled=true` keeps the coordinator's warn/error log
(`main.err.log`, rotated at 32 MiB with one previous file) on its own PVC
through `ROOST_LOG_FILE_DIR`, so `kubectl -n roost exec deploy/roost-coordinator
-- roost doctor --since 24h` reads across rollouts; doctor reads the audit log
from the Postgres the pod's `ROOST_COORDINATOR_DATABASE_URL` names.

The Rust agent harness runs in the coordinator; worker-side tools are dispatched
over the worker link. For a proxy or alternate provider endpoint, set
`ROOST_AGENT_ENDPOINT_OVERRIDES` to a JSON object mapping provider names to base
URLs, for example `{"anthropic":"https://gateway.example"}`.

When the front door is a TCP proxy on another machine rather than an ingress
(for example a Caddy host forwarding over a tailnet), expose the coordinator
with `service.type=NodePort`, `service.externalTrafficPolicy=Local` so the
pod sees the proxy's own address, `trustedProxyCidrs` set to that address,
and `networkPolicy.enabled=true` with `networkPolicy.fromCidrs` naming it, so
nothing else reaches the port.

Behind a load balancer or ingress the front door reaches the coordinator over
the network, not loopback, so declare the proxy networks:
`ROOST_TRUST_PROXY=1` with `ROOST_TRUSTED_PROXY_CIDRS=10.0.0.0/8,…` (the
chart's `trustedProxyCidrs`). `X-Forwarded-For` is then believed only from
peers inside those networks; from any other peer it is ignored. The front door
must still overwrite that header, never append to it.

## Three coordinator HTTP/TLS front-door recipes

After running the promotion command on the coordinator, choose one of these
operator-managed front doors. Roost implements none of them: each forwards to
the loopback bind, supplies HTTPS, and must overwrite `X-Forwarded-For`. The
declared `ROOST_WEB_PUBLIC_URL` is the browser origin it serves.

### Recipe 1 — Caddy with your own domain

You have a domain and a machine reachable on 80/443. Caddy obtains and renews
the certificate itself; nothing about it reaches Roost.

`/etc/caddy/Caddyfile`:

```caddy
roost.example.com {
	@private path /internal/* /api/db-export
	respond @private "not found" 404

	reverse_proxy 127.0.0.1:4113 {
		header_up X-Forwarded-For {remote_host}
	}
}
```

```sh
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Workers enrolled through this shared HTTPS door dial it, so
`/ws/coord-worker/*` passes.

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4113` plaintext with
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

	reverse_proxy 127.0.0.1:4113 {
		header_up X-Forwarded-For {http.request.header.CF-Connecting-IP}
	}
}
```

Workers enrolled through this shared HTTPS door dial it, so
`/ws/coord-worker/*` passes.

Install the tunnel as a service with an explicit config path — under `sudo` the
service's `$HOME` is `/root`, so `cloudflared` would not otherwise find the file
you just wrote:

```sh
sudo cloudflared --config "$HOME/.cloudflared/config.yml" service install
sudo systemctl enable --now cloudflared   # Linux; launchd starts it on install
```

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4113` plaintext with
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
reverse_proxy 127.0.0.1:4113 {
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
tailscale serve --bg --https=443 http://127.0.0.1:4113
tailscale serve status
```

The public origin is the machine's MagicDNS name, e.g.
`https://roost-host.tailnet-name.ts.net`. `tailscale serve` has no path
matcher, so if you need the private paths denied, keep a local proxy in front of
the coordinator as in recipe 1 and point Serve at that instead.

Coordinator: `ROOST_COORDINATOR_BIND=127.0.0.1:4113` plaintext with
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

	reverse_proxy 127.0.0.1:4113 {
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

	reverse_proxy 127.0.0.1:4113 {
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

	reverse_proxy 127.0.0.1:4113 {
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

	reverse_proxy 127.0.0.1:4113 {
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
opener and rerun quickstart, or pair from an already authorized browser in
**Settings → Devices**. Later devices should always use that Settings pairing
flow.

## Pair your phone

Phone pairing needs an HTTPS front door that the phone can reach. After
promotion and front-door configuration, open **Settings → Devices → Pair a
phone** on an already-paired browser and press **Show pairing code**: it mints
one browser grant and draws its `#pair=` link as a QR code, valid for one use
within 24 hours. Scan it with the phone's camera; the page opens and pairs with
nothing to type. A loopback-only install shows no code and says to declare the
front door first. `roost add-browser` draws the same QR beside the URL in a
terminal (`--no-qr` turns it off).

Without a camera, open the HTTPS origin on the phone and choose **Request
access**: an already-paired browser approves the request under **Approve a
browser** and shows a 6-digit code, which you type on the phone. On a tailnet
front door, install the Tailscale app on the phone and sign in to the same
tailnet first.

Pairing is what authorizes a device. Network reachability, a VPN membership, or
a login your front door performs on its own does not authorize a phone as a
Roost device.

## Turn on agent notifications

Terminals running a coding agent show their state (working / needs input /
done) with no setup: the sidebar row, tab, and folder rollup update themselves,
and a background agent that stops for input or finishes raises an in-app toast
plus an unseen count in the browser tab title.

**Settings → Notifications → Desktop** subscribes this browser to Web Push: the
switch asks for notification permission on the click, then registers the
service worker and subscription with the coordinator. A needs-input or done
transition then reaches you as an OS notification even when Roost is not the
tab you are looking at, except on a device already viewing that session;
clicking it opens the session. On iPhone and iPad, notifications reach only a
Roost installed with **Share → Add to Home Screen**. The two sound switches
play a short tone on the same alerts that raise a toast.

## Add another machine

Roost enrolls macOS and Linux workers from **Settings → Machines → Add machine**,
and Windows workers from `roost add-machine --platform windows` ([below](#a-windows-11-pc)).
The pane
first checks whether the coordinator advertises an HTTPS enrollment address. A
local-only Roost does not mint a command: choose an
operator-managed HTTPS address, run
`roost quickstart --coordinator-url https://<your-Roost-address>` on the
coordinator **before** exposing its listener, configure the front door to
forward to the installed loopback bind and overwrite `X-Forwarded-For`, then
select **Check again**.

Once the coordinator advertises a valid external origin, generate the one-shot
pull command and run it manually on the target through your usual terminal,
SSH session, or cloud console. Roost neither opens an SSH session nor configures
the target network. The target must reach the declared HTTPS address and trust
its certificate chain; generation does not prove that reachability. CLI
equivalents are `roost add-machine --platform macos` and
`roost add-machine --platform linux`.

The enrollment address uses the installed coordinator declaration:
`ROOST_COORDINATOR_URL` → `ROOST_COORDINATOR_PUBLIC_URL` →
`ROOST_WEB_PUBLIC_URL`. A missing, local, or invalid declaration refuses before
minting a token rather than selecting another origin.

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/v3/install.sh | \
  ROOST_COORDINATOR_URL="https://roost.example.com" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

This is the same `install.sh` as the first machine's: given the grant it runs
`roost join` instead of `roost quickstart`. Coordinators that print a
`…/v3/join.sh` URL reach the same script.

The public worker path `/ws/coord-worker/*` must pass on a shared front door.
Give workers a separately declared `ROOST_COORDINATOR_PUBLIC_URL` only when the
worker link should use a distinct HTTPS door.

The machine appears in **Settings → Machines** within a few seconds. macOS
uses launchd and Linux uses `systemd --user`. The server-side bootstrap token
is one-shot and expires after 24 hours.

### A Windows 11 PC

A Windows PC joins as a worker only; it never runs the coordinator, and
`roost quickstart` refuses there. On the coordinator host, mint its line:

```sh
roost add-machine --platform windows --label "Build PC"
```

Paste the printed line into a **non-elevated** PowerShell on the PC. It sets
`ROOST_COORDINATOR_URL` and `ROOST_BOOTSTRAP_TOKEN` in that session and runs
`install.ps1`:

```powershell
$env:ROOST_COORDINATOR_URL='https://roost.example.com'; $env:ROOST_BOOTSTRAP_TOKEN='roost_bt_…'; $env:ROOST_WORKER_LABEL='Build PC'; irm https://raw.githubusercontent.com/cefege/roost/v3/install.ps1 | iex
```

`install.ps1` picks the release the same way `install.sh` does (set
`$env:ROOST_RELEASE_CHANNEL='prerelease'` first to take a pre-release),
checks `roost-windows-x64.exe` and `roost-keeper-windows-x64.exe` against the
digests published beside them, and runs `roost join`. The join installs the
release under `%LOCALAPPDATA%\RoostWorkerV3\versions\<tag>`, writes the
launcher `%LOCALAPPDATA%\RoostWorkerV3\service\roost3-worker.cmd`, and
registers and starts the Scheduled Task `\Roost\roost3-worker`, which runs at
every logon of this user. Logs are
`%LOCALAPPDATA%\RoostWorkerV3\logs\main.out.log` and `main.err.log`; the
keeper's is `keeper.log` beside them. A `roost.cmd` shim in
`%LOCALAPPDATA%\RoostWorkerV3\bin` is added to the user `Path`.

Sessions open PowerShell 7 (`pwsh.exe`) when it is installed, else Windows
PowerShell, else `cmd.exe`; set a user environment variable `SHELL` to choose
another, and restart the worker. The task runs under this user's interactive
logon, so the worker is up only while the user is signed in; an unattended PC
needs Windows auto-logon. The same line also works from an OpenSSH session
into the PC.

Stopping the task ends only its console host: the `roost.exe worker` below
it keeps running. Restart the worker by ending that process too; the keeper
keeps the PTYs, as it does across any worker restart:

```powershell
Stop-ScheduledTask -TaskPath '\Roost\' -TaskName roost3-worker
Get-CimInstance Win32_Process -Filter "Name='roost.exe'" | Where-Object { $_.CommandLine -match '\sworker(\s|$)' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
Start-ScheduledTask -TaskPath '\Roost\' -TaskName roost3-worker
```

Windows sessions need Windows 11 (or Windows 10 1803+) for AF_UNIX sockets
and ConPTY. `roost update` does not replace binaries on Windows: upgrade by
running the same `install.ps1` line with a fresh grant, or with
`cargo xtask fleet install` for a fleet host.


## Update

### A self-hosted install: `roost update`

On each macOS or Linux machine installed from a release, run:

```sh
roost update
```

It takes no arguments. It first resolves any update a previous run left
interrupted, then picks the highest published v3 release on this machine's
channel, checks the binary against its `.sha256` sidecar before staging
it beside the installed one, admits the running keeper against the new
binary's keeper contract, and swaps the binary with a journaled atomic rename.
It then unpacks that release's web bundle into the release directory both
service definitions already point at. It prints `already the latest release`
or `no published release to update to` when there is nothing to do, and a
source build refuses to replace itself.

**Channels.** A stable build updates only to stable releases; a pre-release
build (`v3.x.y-rc.N`) also takes newer pre-releases. Only a strictly newer
version is installed, so an update never moves a machine sideways or back.
`ROOST_RELEASE_CHANNEL=stable` or `=prerelease` overrides the default, for
both `roost update` and the install script.

The running services keep the old binary until they restart:

```sh
systemctl --user restart roost3-coord roost3-worker            # Linux
launchctl kickstart -k gui/$(id -u)/com.roost.coordinator-v3    # macOS
launchctl kickstart -k gui/$(id -u)/com.roost.worker-v3
```

A machine running only a worker restarts only `roost3-worker` /
`com.roost.worker-v3`. The keeper keeps the PTYs across a worker restart. A
containerized coordinator updates by image tag instead (`helm upgrade` with
`--set image.tag=<tag>`; empty means the chart's `appVersion`).

### Rolling a fleet from a source checkout: `roost push`

To roll a source-installed coordinator and its registered fleet onto one
commit from a clean Roost checkout, run:

```sh
roost push
```

`roost push` takes no arguments. It is one journaled transaction across the
local POSIX coordinator and every registered macOS/Linux worker it can reach.
It requires at least one registered worker and a clean commit, which it proves
and publishes before mutating anything. A subset of the fleet is
`roost deploy <host>`, not a push flag.

The command snapshots the live coordinator database, activates and proves the
target coordinator in a held state, then stages and proves every **participant**
worker at the same SHA with a current keeper and fresh heartbeat. Only then does
it record the durable finalization decision. Before that decision, any
participant failure rolls every participant back, restores and proves the prior
coordinator and database, and reports failure. After that decision, interrupted
recovery can only finish the target release.

A registered worker that is unreachable, stale, or not on the coordinator's
prior SHA is **deferred**, not a refusal: the push converges the machines it can
reach and names the rest. The fleet's desired release is the running
coordinator's own SHA, so a deferred machine is simply behind it, and the
coordinator starts that machine's catch-up deploy itself the next time the
worker attaches. `roost status` and Settings → Machines show each machine as up
to date, update available, updating, or update pending while offline.

A machine that returns is converged by the coordinator's own catch-up deploy, or
immediately with `roost deploy <host>`. Re-running `roost push` will NOT pick it
up: every per-host rollout proves the installed service against the rollout's
single prior SHA and refuses anything else, so a machine sitting on an older
commit is structurally a deferral for as long as the fleet has moved on.

The fleet is therefore NOT guaranteed to be one version between a push and a
deferred machine's return. What that window costs is wire compatibility between
a new coordinator and an old worker — the cell-frame and `SessionEvent` shapes
in `protocol/proto/roost/v1/` — so a change to those shapes must stay backward
compatible for one release, or the deferred machine must be updated before the
shape change ships.

One-host POSIX deployment is `roost deploy <host>`: it stages this checkout's
pushed commit over SSH, or with `--release <tag>` that release's published
binaries and web bundle. Building from source refuses to run from the
standalone release binary because it does not contain a Git checkout.

A remote target's own identity is never taken from the shell running the
deploy. `ROOST_WORKER_LABEL` and `ROOST_REACHABLE_ADDR` come from the target's
installed service definition, or from `--label=<name>` /
`--reachable-addr=<fqdn>` on the command line; with neither present the target
derives its own hostname and reachable address. Exporting either variable while
deploying to a host that has no prior install refuses the deploy rather than
registering that host under this machine's name.

Our own fleet's maintainer flow is: push a `v3.*` tag, let `release.yml` build
it, then `cargo xtask fleet install --version <tag>`, which downloads that
release and installs it on the hosts in `xtask/fleet.json`; see `CLAUDE.md`
§ Release to the fleet.

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
`ROOST_WORKER_LOCAL_UI_BIND=127.0.0.1:4114`. A browser on that machine can open
`http://127.0.0.1:4114` and talk to that worker's PTYs directly. The door
refuses any non-loopback bind, answers only loopback names
(`127.0.0.1`, `localhost`, `[::1]`), refuses other `Host` values, and advertises
only the coordinator URL and worker fingerprint at `/api/local-bootstrap`.
`ROOST_WEB_DIST_PATH` overrides the SPA it serves for source runs.

When a coordinator-served page has a worker on the browser machine, its first
live terminal pane probes `http://127.0.0.1:4114` once. If the worker answers,
that worker's sessions prefer loopback and their pane tabs show the direct
marker. Chromium can require a one-time local-network permission. Firefox and
Safari block a plaintext-loopback request from an HTTPS page; those browsers
continue with qualified WebRTC or Sync without a missing terminal. The door
admits the coordinator origin; if the browser front door differs from the
worker's coordinator URL, list it in the worker's
`ROOST_WORKER_LOCAL_UI_ALLOWED_ORIGINS` (comma-separated).

Changing the local door port changes the pre-allowlisted
`http://127.0.0.1:4114` origin. Add the new origin to the coordinator's
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
exists but is unused). Which dist the coordinator actually picked is its own
startup line: `spa source: disk` with `web_dist_path`, or
`spa source missing: every page request answers 404`.
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

With a SQLite database, the coordinator creates a verified snapshot before
applying pending migrations to an existing database. It also backs up every 24
hours from process start, including an immediate startup backup when none
exists or the newest is stale. With `ROOST_COORDINATOR_DATABASE_URL` it takes
no backups and `/api/db-export` answers 404: the Postgres server's own backups
are the recovery material. The SQLite coordinator integrity-checks the
standalone snapshot before compressing it and retains the 14 newest
`coord_v2.<timestamp>.db.gz` archives in the coordinator data directory's
`backups/` folder.

These archives are same-host recovery material. They do not survive loss of
the coordinator disk, are not off-host disaster recovery, and are not the
automatic fleet-rollout rollback mechanism. Copy them to storage with an
independent failure domain and own the restore procedure when host-loss
recovery is required.

`roost push` copies the coordinator's SQLite database, write-ahead log and
shared-memory file into `coordinator-rollback-<rollout id>/` in the
coordinator's service directory before it touches anything, restores from that
copy on rollback, and removes it once the transaction settles.

## Release rollout and canaries

Use one release tag and one fleet transaction:

1. Qualify the four public host targets—macOS arm64/x64 and Linux arm64/x64—
   from the same source commit. Each published binary must match its GitHub
   Release SHA-256 sidecar.
2. Update every machine: `roost update` plus a service restart on each, or
   `roost push` from the clean pushed checkout. A machine that was offline
   during a push is named as deferred and catches up on its next attach; for a
   release you care about, confirm it reaches the new SHA in `roost status`
   before declaring the rollout done.
3. Require a new coordinator boot, all workers online on the expected build,
   and a pre-restart PTY to paint a new marker. Reject new anomalies in
   `roost doctor --since 1h`.
4. Re-prove the front door: an unauthenticated `MiscHealth` POST through the
   public origin reaches the coordinator, and `/api/db-export` from off-host
   answers 403 or the front door's 404.

## Logs

```sh
roost logs coord             # coordinator stdout then stderr, last lines
roost logs worker -n 500     # worker; -n/--tail sets the line count
```
