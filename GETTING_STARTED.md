<!-- AUDIENCE: human -->
# Getting started with Roost

Roost runs across your Macs over your own network — [Tailscale](https://tailscale.com)
is the tested, recommended setup. One Mac runs the coordinator + a worker; your
phone and other devices connect to it over your tailnet.

## Prerequisite: Tailscale

Roost needs Tailscale running on the coordinator and every worker Mac (it's the
private network they use to communicate).

1. Install it: `brew install tailscale` (or the Mac App Store app).
2. Start it: `tailscale up`.
3. Approve the Tailscale **network extension** when macOS prompts you in
   System Settings. (This one step can't be automated.)

## Install + run

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/install.sh | bash
```

That installs Bun (if needed), gets the code, starts the coordinator and a
local worker, and opens Roost in your browser already signed in. No tokens to
copy.

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

### 2. Create a locally-managed tunnel

Follow Cloudflare's [locally-managed tunnel
guide](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/local-management/create-local-tunnel/)
and run:

```sh
brew install cloudflared
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

Stop the tunnel service:

```sh
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
  in to the same tailnet. In Roost on your Mac, choose **Settings → Pair a
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

## Add another Mac

Extra Macs join by pulling — no SSH, no push from the coordinator. On the
coordinator, open **Settings → Machines → Add machine** (or run
`roost add-mac`). Copy the generated one-liner, then paste it in a terminal on
the new Mac (Tailscale must be running there):

```sh
curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | \
  ROOST_COORDINATOR_URL="https://<coord>.<tailnet>.ts.net:4102" \
  ROOST_BOOTSTRAP_TOKEN="roost_bt_…" bash
```

The new Mac installs itself and registers with the coordinator — it appears in
**Settings → Machines** within a few seconds. The token is one-shot and
expires in 24 hours.

To **update** a Mac that's already joined, use `roost deploy <host>` (or the
"Deploy" button on its drift badge) — that's the push path for pushing new
code to existing workers.

## Check on it

```sh
roost status
```

Shows whether Tailscale, the coordinator, the worker, and TLS are all
healthy, with the fix for anything that's down. (`roost doctor` is the same
command.)

## Logs

```sh
roost logs coord     # coordinator logs
roost logs worker    # worker logs
```
