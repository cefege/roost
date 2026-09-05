<!-- AUDIENCE: human -->
# Getting started with Roost

The v0.5.0 self-hosted coordinator/worker runtime is released for macOS
arm64/x64 and Linux arm64/x64. Production fleet rollout remains pending.
Quickstart supports two production modes:
automatic Tailscale Serve and operator-managed direct HTTPS. Direct
coordinator quickstart is Tailscale-free; the current extra-worker installer
still requires a running Tailscale daemon and is called out below.

**Only coordinator and worker machines need a supported host OS.** Everything
you browse *from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android
phone, an iPad, an Android tablet, whatever — needs nothing but a modern
browser (optionally added to the home screen as a PWA).

## Choose a coordinator network mode

### Automatic: Tailscale Serve

Run `roost quickstart` without any endpoint flags. Quickstart discovers the
coordinator's tailnet name, keeps the coordinator's plaintext listener on
loopback, provisions HTTPS on port 4102, and configures Tailscale Serve.
Coordinator and worker machines, and browsers using this route, join the same
tailnet.

For this convenience mode:

1. On macOS, either install the open-source CLI daemon with
   `brew install tailscale`, or install the GUI app and follow its prompts.
2. For the Homebrew daemon, run
   `sudo tailscaled install-system-daemon && sudo tailscale up`. It does not
   use the macOS network extension. The GUI app does, so approve that extension
   when its UI asks.
3. On Linux, add Tailscale's repository, then run
   `sudo systemctl enable --now tailscaled && sudo tailscale up` and
   `sudo tailscale set --operator=$USER`.

### Direct: your trusted HTTPS endpoint

Direct coordinator quickstart does not resolve or call Tailscale. You provide
DNS or another stable hostname, routing and firewall policy, and the
certificate files. Every browser and worker connection using that origin must
be able to reach it and trust its complete certificate chain; a self-signed or
privately issued certificate works only after its CA is trusted on every
client.

The three flags are one group: supply all of `--coordinator-url`, `--tls-cert`,
and `--tls-key`, or supply none and use automatic mode. Ambient `ROOST_*`
endpoint variables do not turn a no-flag invocation into direct mode.

Direct inputs follow these rules:

- `--coordinator-url` is an HTTPS origin with an explicit numeric port from 1
  through 65535, even for `:443`. It has no username/password, query, fragment,
  or path other than an optional single `/`.
- Certificate and key paths are absolute, readable, non-symlink regular files.
  They must remain distinct after lexical normalization and must resolve to
  different files; aliases and hard links to one file are rejected.
- The certificate covers the URL hostname and is currently valid, and its chain
  terminates at a CA trusted by every browser and worker.

Quickstart normalizes the URL to its HTTPS origin, while retaining the explicit
input port for the listener. For example, `https://roost.example.com:443/`
persists the public origin as `https://roost.example.com` and binds port 443.

## Install + run

On macOS arm64/x64 or Linux arm64/x64, install the published binary. The
installer verifies it against the adjacent GitHub Release SHA-256 sidecar:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
```

Then choose exactly one quickstart form. Automatic Tailscale mode:

```sh
"$HOME/.local/bin/roost" quickstart
```

Direct HTTPS, here using arbitrary port 8443:

```sh
"$HOME/.local/bin/roost" quickstart \
  --coordinator-url "https://roost.example.com:8443" \
  --tls-cert "$HOME/.config/roost/tls/fullchain.pem" \
  --tls-key "$HOME/.config/roost/tls/privkey.pem"
```

That direct invocation installs the coordinator service with this endpoint
contract (using the normalized absolute certificate paths):

```text
ROOST_FRONTED=0
ROOST_COORDINATOR_BIND=0.0.0.0:8443
ROOST_COORDINATOR_PUBLIC_URL=https://roost.example.com:8443
ROOST_TLS_CERT_PATH=/home/<user>/.config/roost/tls/fullchain.pem
ROOST_TLS_KEY_PATH=/home/<user>/.config/roost/tls/privkey.pem
```

`ROOST_TAILNET_HTTPS_PORT` and the loopback/front-proxy settings belong only to
automatic mode; a direct coordinator quickstart neither persists them nor
invokes Tailscale.

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

In self-hosted mode, coordinator startup creates and thereafter validates the
single local tenant automatically: the internal `local@roost.invalid` account,
its `personal` organization, and the `default` dashboard. Existing coherent
single-tenant databases keep their IDs and names. There is no separate
organization bootstrap command to run before quickstart or after an upgrade.

### First browser enrollment

Quickstart mints a one-shot browser grant in a `#pair` URL fragment and passes
that URL directly to the platform browser opener. Quickstart never prints or
logs the grant. URL fragments are not sent in the HTTP request or an HTTP
`Referer`, so the coordinator and intermediaries do not receive the secret as
URL metadata; the loaded Roost app redeems it.

Do not try to copy a pairing secret from terminal output, shell history, logs,
or screenshots. If the platform opener fails, arrange a working local browser
opener and rerun quickstart, or use **Settings → Pair a device** from an already
authorized browser. Later devices should always use that Settings pairing flow.

## Managed deployment qualification (not publicly launched)

v0.5.0 includes a qualified managed implementation, not a public managed
service. Production does not publish the managed coordinator image or activate
the shared dashboard origin, so the public release cannot be used to provision
a managed account.

The qualified Linux profile has a root-owned operator/provisioner plane and
one exact-spec non-root coordinator container per account, created from a
digest-pinned immutable image. Each account has separate writable state, keys,
worker credentials, and an opaque route key. The profile's Caddy configuration
routes `/_roost/t/<route-key>/…` on the shared dashboard origin to exactly one
container over the named Docker `web` network; no coordinator port is
published on the host. The container has no shell, package manager, SSH,
rsync, Docker socket, source checkout, host home, or customer worker process.

Managed accounts are operator-created through the `roost saas` commands.
Production email signup and Google auth remain disabled. Owner activation is a
held, expiring email flow: routing and resolver proofs complete before mail is
released, and reconciliation resumes the last safe state without deleting
tenant data.

Server authorization derives dashboard scope from the authenticated account
membership rather than trusting a browser-supplied dashboard id. RPC, Sync,
worker routing, and browser dashboard cutover retain that same scope.

The mandatory `test:managed` profile runs four E2E files with five top-level
cases: browser activation/login/reset, two-account container and route
isolation, encrypted backup/restore, and dormant email/Google signup. Passing
that profile is the qualification gate; it is not evidence that the service,
image, domain, containers, or signup path is live.

## Optional: Cloudflare browser access for automatic mode

Cloudflare browser access adds a public browser endpoint to an already working
automatic Tailscale installation:

- Cloudflare Access authenticates the human reaching the browser endpoint.
- Roost pairing still authorizes that browser as a Roost device. An Access
  login does not replace pairing.
- Tailscale remains the private coordinator/worker network.
- Only the coordinator runs `cloudflared`; browser devices and workers do not
  install it.

| | Default Tailscale path | Cloudflare browser access |
|---|---|---|
| **Browser device software** | Tailscale app | Ordinary browser only |
| **Coordinator/worker network** | Tailscale | Tailscale |
| **Public DNS/domain** | None | Cloudflare-managed domain required |
| **Cloudflare setup** | None | Tunnel plus self-hosted Access application |
| **Best reason to choose it** | Minimum setup | Browser access from unmanaged devices |
| **Tradeoff** | Browser must join the tailnet | Manual Cloudflare administration and an extra internet-facing dependency |

Follow these steps on the coordinator.

### 1. Check the prerequisites

You need:

- a working Roost coordinator and workers on Tailscale;
- a domain whose DNS is managed by Cloudflare;
- a Cloudflare Zero Trust team with a login method that works for the
  operator's email (otherwise, first [configure an identity
  provider](https://developers.cloudflare.com/cloudflare-one/integrations/identity-providers/));
- administrator access to install a `cloudflared` service on the coordinator;
- the [outbound connectivity required by Cloudflare
  Tunnel](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/troubleshoot-tunnels/connectivity-prechecks/).

This manual flow uses `cloudflared tunnel login`; it does not require a
Cloudflare API token.

### 2. Install `cloudflared` and create a locally-managed tunnel

Install `cloudflared` on the coordinator:

```sh
# macOS
brew install cloudflared

# Debian / Ubuntu
sudo mkdir -p --mode=0755 /usr/share/keyrings
curl -fsSL https://pkg.cloudflare.com/cloudflare-main.gpg \
  | sudo tee /usr/share/keyrings/cloudflare-main.gpg >/dev/null
echo 'deb [signed-by=/usr/share/keyrings/cloudflare-main.gpg] https://pkg.cloudflare.com/cloudflared any main' \
  | sudo tee /etc/apt/sources.list.d/cloudflared.list
sudo apt-get update && sudo apt-get install cloudflared

# RHEL / CentOS Stream / Fedora / Amazon Linux
curl -fsSL https://pkg.cloudflare.com/cloudflared.repo \
  | sudo tee /etc/yum.repos.d/cloudflared.repo
sudo dnf install cloudflared
```

Other distributions and architectures: [Cloudflare's package
index](https://pkg.cloudflare.com) or the [release
binaries](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/downloads/).
`roost expose` refuses to run when `cloudflared` is not on `PATH`.

Then follow Cloudflare's [locally-managed tunnel
guide](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/local-management/create-local-tunnel/)
and run:

```sh
cloudflared tunnel login
cloudflared tunnel create roost
```

Copy the tunnel UUID and the absolute credentials-file path printed by
`cloudflared tunnel create`.

### 3. Configure tunnel ingress

Create `$HOME/.cloudflared/config.yml`:

```yaml
tunnel: <TUNNEL-UUID>
credentials-file: /absolute/path/printed/by/cloudflared/<TUNNEL-UUID>.json
ingress:
  - hostname: roost.example.com
    service: http://127.0.0.1:4104
  - service: http_status:404
```

Replace each angle-bracketed placeholder and replace `roost.example.com` with
your real hostname. `roost expose` rejects a missing config, invalid ingress, a
path-scoped first rule, a different hostname or first service, or routes that
do not resolve to the loopback browser listener at `127.0.0.1:4104`.

### 4. Route the hostname

```sh
cloudflared tunnel route dns roost roost.example.com
```

Replace `roost.example.com` with the same hostname used in the config.

### 5. [Create the Access application](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/)

In **Cloudflare Zero Trust → Access controls → Applications**, create a
**Self-hosted** application for exactly `roost.example.com`, replacing the
example with your hostname. Add an **Allow** policy containing the operator's
email. Copy:

- the team domain in the exact form `<team>.cloudflareaccess.com`;
- **Additional settings → [Application Audience (AUD)
  Tag](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/)**,
  which must be exactly 64 lowercase hexadecimal characters.

### 6. Configure Roost

Run the existing command with the same bare hostname:

```sh
roost expose roost.example.com \
  --team <team>.cloudflareaccess.com \
  --aud <64-lowercase-hex>
```

For a non-default config, add `--config <path>`. A relative path is resolved
against the current working directory before validation.

### 7. Install the tunnel service

Run the service-install command printed by `roost expose`. With the default
config, it is:

```sh
sudo cloudflared --config "$HOME/.cloudflared/config.yml" service install
```

The explicit `--config` is what makes this work on Linux: under `sudo`, the
service's `$HOME` is `/root`, so `cloudflared` would not find the config you
just wrote. Your own shell expands `$HOME` before `sudo` runs, so the path above
is the right one.

macOS launchd starts the service on install. On Linux, start and enable it
yourself:

```sh
sudo systemctl enable --now cloudflared
sudo systemctl status cloudflared
```

Roost does not install, update, or own this Cloudflare service; it only prints
the command.

### 8. Verify access and pair the browser

While logged out of Cloudflare Access, run:

```sh
curl -i -X POST https://roost.example.com/roost.v1.CoordinatorService/MiscHealth
```

Expect a Cloudflare Access login redirect or challenge. Origin HTTP 200 means
Access is absent; HTTP 502 or 530 means tunnel routing is wrong.

Then open `https://roost.example.com`, complete the Access login, and pair that
browser through Roost. A successful Access login without Roost pairing is not
sufficient.

### What this does not change

- Workers still join through the Tailscale coordinator URL and require
  Tailscale.
- `roost deploy`, VNC/Screen Sharing, Finder/SMB, SSH/rsync, and
  development-port links continue using direct or tailnet reachability.
- The Cloudflare browser surface intentionally denies worker WebSockets,
  worker bootstrap, database export, internal handoff, and coordinator-move
  RPCs.
- Cloudflare availability and the configured Access policy become dependencies
  for the public browser URL. The private Tailscale path remains available.

### Disable public access

Stop and remove the tunnel service:

```sh
sudo systemctl disable --now cloudflared   # Linux only
sudo cloudflared service uninstall
```

Then delete the corresponding Access application, DNS record, and tunnel in
the Cloudflare dashboard. Stopping the tunnel removes public reachability; it
does not stop coordinator/worker traffic over Tailscale.

> **Lifecycle limitation:** There is no `roost unexpose` command.
> `roost expose` persists `ROOST_WEB_PUBLIC_URL` and the Access settings in the
> coordinator service. Deleting the Cloudflare resources therefore does not
> fully restore local pairing links or remove the saved public URL. Do not
> hand-edit launchd or systemd files. To perform a full local reset, reinstall
> or reconfigure the coordinator.

## Pair your phone

Choose one route:

- **Default Tailscale route:** Install the Tailscale app on the phone and sign
  in to the same tailnet. In Roost on your computer, choose **Settings → Pair a
  device**, then scan the QR with the phone's camera.
- **Direct HTTPS route:** Make the configured HTTPS origin reachable from the
  phone and ensure its certificate chain is trusted there. In Roost on an
  authorized browser, choose **Settings → Pair a device**, then scan the QR.
- **Cloudflare route:** After completing the optional Cloudflare setup above,
  open your Cloudflare hostname on the phone without installing Tailscale.
  Complete the Cloudflare Access login, then complete Roost QR/device pairing.

Every route requires Roost pairing. Network reachability or a Cloudflare Access
login alone does not authorize the phone as a Roost device.

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

v0.5.0 enrolls macOS or Linux workers. In either coordinator mode,
**Settings → Machines → Add machine** derives the installed public HTTPS
origin and creates a one-shot pull command. On an automatic-mode coordinator,
the equivalent CLI generators are `roost add-machine --platform macos` and
`roost add-machine --platform linux`. No coordinator SSH or push is involved.

Paste the generated command on the worker:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coordinator-host>:<port>" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

`join.sh` currently requires a running Tailscale daemon even when
`ROOST_COORDINATOR_URL` is the direct HTTPS origin. In direct mode, the worker
must also reach that origin and trust its certificate chain. The CLI generator
is automatic-mode-only and always uses the coordinator MagicDNS name on port
4102. v0.5.0 therefore has no Tailscale-free direct extra-worker enrollment
flow.

The machine appears in **Settings → Machines** within a few seconds. macOS
uses launchd and Linux uses `systemd --user`. The server-side bootstrap token
is one-shot and expires after 24 hours.

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

## Check current health and recent anomalies

```sh
roost status
roost doctor --since 1h
```

`roost status` reports the selected network mode, required Tailscale/Serve or
direct-certificate state, local coordinator and worker services, coordinator
health and tagged SHA, and remote worker age/build observations. Its exit
status gates required Tailscale, the two local services, and coordinator
reachability; inspect the fleet rows rather than treating that exit status as
proof that every remote worker converged. `roost doctor --since <window>`
summarizes local logs from that window and reports anomalies such as uncaught
errors, sequence gaps, queue overflows, degraded keepers, and failed
backups/readiness.

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
   ROOST_COORD_URL="https://<host>:<port>" \
     bun test smoke/api_smoke.test.ts
   ```
5. Restart the coordinator and local worker. Require a new coordinator boot
   timestamp, all workers online on the expected build, and the pre-restart PTY
   to paint a new marker. Reject new uncaught errors, sequence gaps, queue
   overflows, stale keepers, or failed backup/readiness events.
6. If automatic-mode Cloudflare access is enabled, require an unauthenticated
   public `MiscHealth` POST to receive the Access challenge rather than origin
   200, while the authorized browser and private Tailscale URL remain healthy.

## Logs

```sh
roost logs coord     # coordinator logs
roost logs worker    # worker logs
```
