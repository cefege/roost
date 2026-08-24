<!-- AUDIENCE: human -->
# Getting started with Roost

Roost runs across macOS, Linux, and Windows x64 machines. The supported
automated production topology is [Tailscale](https://tailscale.com): one machine
runs the coordinator plus a worker, and browsers and other workers reach it
through the tailnet. WireGuard, Headscale, ZeroTier, other VPNs, and a plain LAN
are manual, unverified alternatives rather than equivalent installer paths.

**Only coordinator and worker machines need a supported host OS.** Everything
you browse *from* — a Mac, a Windows PC, a Linux desktop, an iPhone, an Android
phone, an iPad, an Android tablet, whatever — needs nothing but a browser
(optionally added to the home screen as a PWA).

## Prerequisite: Tailscale

Roost's supported setup requires Tailscale on the coordinator and every worker
machine; it is both the private transport and trusted enrollment boundary.

1. Install it: `brew install tailscale` on macOS (or use the Mac App Store);
   follow <https://tailscale.com/download/linux> on Linux; or install it from
   <https://tailscale.com/download/windows> on Windows.
2. Start it: `tailscale up` (on Linux: `sudo systemctl enable --now tailscaled`
   first).
3. On macOS, approve the Tailscale **network extension** when the system prompts
   you in System Settings. (This one step can't be automated.)

## Install + run

Install the verified release, then run the guided production setup.

On macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install-binary.sh | bash
"$HOME/.local/bin/roost" quickstart
```

> **Windows releases are paused.** The Windows CI/release tier is disabled while
> its gates are repaired, so releases after `v0.3.2` publish no
> `install-binary.ps1`, `join.ps1`, or `roost-windows-x64.zip`. The steps below
> are unchanged and correct, but they cannot complete against
> `releases/latest` today.

On Windows x64, open an elevated PowerShell 5.1+ session. Supply the trusted
SHA-256 fingerprint of the release-publisher leaf certificate through a channel
independent of the downloaded release manifest:

```powershell
$publisher = "<trusted publisher certificate SHA-256>".ToLowerInvariant()
if ($publisher -notmatch '^[0-9a-f]{64}$') { throw 'Invalid publisher SHA-256' }
$release = Invoke-RestMethod 'https://api.github.com/repos/cefege/roost/releases/latest'
$base = "https://github.com/cefege/roost/releases/download/$($release.tag_name)"
$programData = [IO.Path]::GetFullPath($env:ProgramData)
if (((Get-Item -LiteralPath $programData -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
  throw 'Unsafe ProgramData directory'
}
$owner = ([Security.Principal.NTAccount](Get-Acl -LiteralPath $programData).Owner).Translate([Security.Principal.SecurityIdentifier]).Value
if ($owner -notin @('S-1-5-18', 'S-1-5-32-544')) { throw 'ProgramData owner is not trusted' }
$staging = Join-Path $programData ('RoostBootstrap-' + [Guid]::NewGuid().ToString('N'))
$security = [Security.AccessControl.DirectorySecurity]::new()
$security.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
[IO.Directory]::CreateDirectory($staging, $security) | Out-Null
& icacls.exe $staging '/setintegritylevel' '(OI)(CI)H' | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot apply high-integrity staging policy' }
$installer = Join-Path $staging 'install-binary.ps1'
try {
  Invoke-WebRequest -UseBasicParsing "$base/install-binary.ps1" -OutFile $installer
  $signature = Get-AuthenticodeSignature -LiteralPath $installer
  if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate) {
    throw 'Roost installer has no valid Authenticode signature and trusted timestamp'
  }
  $sha256 = [Security.Cryptography.SHA256]::Create()
  try { $actual = -join ($sha256.ComputeHash($signature.SignerCertificate.RawData) | ForEach-Object { $_.ToString('x2') }) }
  finally { $sha256.Dispose() }
  if ($actual -cne $publisher) { throw "Roost installer publisher mismatch: $actual" }
  Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned -Force
  & $installer -HostRole coordinator -PublisherSha256 $publisher -ReleaseBaseUrl $base
} finally {
  Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue
}
```

The Windows installer defaults to a dedicated local `roost-operator` identity,
creates it with a cryptographically random password when absent, and rejects
administrator identities. It prompts for a password only when reusing an
existing account. It denies interactive logon, verifies the
detached manifest signature, ZIP digest, per-file digests, Authenticode publisher,
and trusted timestamps, then installs the coordinator, worker, keeper, and updater
as Windows SCM services. `quickstart` configures the coordinator, local worker,
and browser pairing; no enrollment token is copied.

The source/development path is separate and intended for macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That command installs Bun and a checkout which tracks `main`; it is not a
pinned production release.

## Optional: browser access through Cloudflare

Cloudflare browser access adds a public browser endpoint to an already working
Roost installation:

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
- **Cloudflare route:** After completing the optional Cloudflare setup above,
  open your Cloudflare hostname on the phone without installing Tailscale.
  Complete the Cloudflare Access login, then complete Roost QR/device pairing.

Both routes require Roost pairing. Cloudflare Access login alone does not
authorize the phone as a Roost device.

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

Extra machines join by pulling; no SSH and no push from the coordinator. On the
coordinator, open **Settings → Machines → Add machine**, or run one of
`roost add-machine --platform macos`, `roost add-machine --platform linux`,
or `roost add-machine --platform windows`. Each creates a one-shot token and
the platform-specific enrollment command.

On a macOS or Linux worker, paste the generated command:

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coord>.<tailnet>.ts.net:4102" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

For a Windows x64 worker, choose **Windows** in the dialog and enter the
trusted release-publisher certificate SHA-256, or run:

```sh
roost add-machine --platform windows --publisher-sha256 "<trusted publisher certificate SHA-256>"
```

Paste the generated command into elevated PowerShell 5.1+. Before executing
downloaded code, that command verifies the release `join.ps1` Authenticode
chain, trusted timestamp, and exact leaf-certificate pin. The signed join script
then keeps the one-shot token in memory only while enrolling the worker and
installing its restricted SCM services.

The machine appears in **Settings → Machines** within a few seconds. macOS uses
launchd, Linux uses `systemd --user`, and Windows uses SCM. The server-side
bootstrap token is one-shot and expires after 24 hours.

To update the whole registered fleet from a clean Roost checkout, push the
commit first and run:

```sh
bun apps/roost-cli/src/main.ts push
```

`roost push` discovers every registered worker, preflights the exact published
Windows manifest when the fleet contains Windows, upgrades and proves the
coordinator first, then deploys the exact clean commit to macOS and Linux and
sends authenticated Windows workers through the signed updater service. It
waits for a fresh post-update heartbeat reporting the expected worker build and
a current keeper from every target. Each host uses a journaled activation with
health proof and automatic rollback; the command continues to independent
hosts but never prints completion when any host failed, stayed stale, or
reported another build. Use `--targets=host1,host2` to narrow the rollout and
`--no-web` only when the existing coordinator SPA should be retained.

To update one registered Windows worker, run `roost deploy <host>` from the
coordinator; it uses the same authenticated, signed, journaled transaction.

On an installed Windows host, `roost update` queues the signed release through
the restricted updater service without a UAC prompt, then exits so the service
can replace the stable launcher. The updater persists every phase and completes
or rolls back independently. Only the initial install or the one-time legacy
migration below needs elevation.

Windows installations created before the stable launcher/updater topology need
one final run of the signed elevated installer before remote updates can use
that topology. The updater detects this state and returns
`migration-required` before mutation; it never attempts an unprivileged,
non-rollback-safe SCM migration.
One-host POSIX deployment is source-based:
`bun apps/roost-cli/src/main.ts deploy <host>` stages that exact pushed commit
over SSH. Source deployments intentionally refuse to run from the standalone
release binary because it does not contain a Git checkout.

## Check current health and recent anomalies

```sh
roost status
roost doctor --since 1h
```

`roost status` is the current service/network/fleet gate: Tailscale state,
coordinator and worker services, coordinator health and tagged SHA, worker
freshness, and whether TLS is provided by Tailscale Serve or a direct
certificate. `roost doctor --since <window>` is different: it summarizes local
logs from that time window and reports anomaly counts such as uncaught errors,
sequence gaps, queue overflows, degraded keepers, and failed backups/readiness.

## Backups and rollback scope

The coordinator creates a verified SQLite snapshot before applying pending
migrations to an existing database and on the scheduled backup interval. It
integrity-checks the standalone snapshot before compressing it and retains the
14 newest `coord_v2.<timestamp>.db.gz` archives in the coordinator data
directory's `backups/` folder.

These archives are same-host rollback material. They do not survive loss of the
coordinator disk and are not off-host disaster recovery; copy them to storage
with an independent failure domain if host-loss recovery is required.

Windows update journals and retained version directories are same-host rollback
material too. Each update checkpoints the prior service definitions, active
role vector, health identity, and current manifest before switching versions;
startup replays unfinished rollback and progress after worker or coordinator
restarts. As with coordinator database snapshots, copy required recovery
material off-host if disk-loss recovery matters.


## Release rollout and canaries

Use one tagged release and one clean command:

1. Publish the release. The canonical macOS, Linux, and Windows x64 assets must
   identify the same source commit; the Windows manifest, checksum sidecar,
   detached signature, package, publisher certificate, and `shawl.exe` must all
   pass the release workflow before publication.
2. From that clean pushed checkout, run
   `bun apps/roost-cli/src/main.ts push`. The command upgrades and proves the
   coordinator first, then updates every registered worker, continues past an
   independent host failure, and reports a final failure unless every target
   converges to the expected build.
3. Run the live API canary:
   ```sh
   ROOST_COORD_URL=https://<coord>.<tailnet>.ts.net:4102 \
     bun test smoke/api_smoke.test.ts
   ```
4. Run the hermetic real-flow tier on the release commit — `bun run test:terminal`
   (real coord + worker + keeper + PTY + browser, `smoke/terminal/stack.ts`).
   That tier is the gate; the live steps here only observe the deployment.
5. Restart the coordinator and local worker. Require a new coordinator boot
   timestamp, all workers online on the expected build, and the pre-restart PTY
   to paint a new marker. Reject new uncaught errors, sequence gaps, queue
   overflows, stale keepers, or failed backup/readiness events.
6. If Cloudflare access is enabled, require an unauthenticated public
   `MiscHealth` POST to receive the Access challenge rather than origin 200,
   while the authorized browser and private Tailscale URL both remain healthy.

## Logs

```sh
roost logs coord     # coordinator logs
roost logs worker    # worker logs
```
