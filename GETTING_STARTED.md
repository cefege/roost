<!-- AUDIENCE: human -->
# Getting started with Roost

Roost runs across your Macs over your own network — [Tailscale](https://tailscale.com)
is the tested, recommended setup. One Mac runs the coordinator + a worker; your
phone and other devices connect to it over your tailnet.

## Prerequisite: Tailscale

Roost needs Tailscale running on this Mac (it's the network everything talks
over).

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

## Pair your phone

Your phone needs to be on the same tailnet (the Tailscale app, signed in).
Then, in Roost on your Mac:

**Settings → Pair a device → scan the QR with your phone's camera.**

The phone opens Roost and signs itself in — nothing to type.

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
