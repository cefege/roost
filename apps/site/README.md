# `@roost/site`

The Roost marketing site: landing page, docs, and the `/alternatives/` comparison hub.
Astro 5, `output: "static"` — plain `.astro` components and CSS custom properties, no
UI framework, no analytics, no external fonts. The only client-side JavaScript on the
whole site is the copy button in `src/components/CtaBand.astro`.

## Develop

```sh
bun run --cwd apps/site dev        # astro dev on http://localhost:4181
bun run --cwd apps/site typecheck  # astro check
```

## Build

```sh
bun run --cwd apps/site build      # scripts/gen-og.ts, then astro build -> apps/site/dist
bun run --cwd apps/site preview    # astro preview of the built output
bun apps/site/scripts/check-links.ts  # verify that root-relative links resolve
```

`dist/` is generated output and is not committed.

## Serve

`serve.ts` is a dependency-free Bun static server for `apps/site/dist`. It serves
`index.html` for directory paths, falls back to `dist/404.html` with status 404,
rejects any path that escapes `dist` with 403, and sets
`Cache-Control: public, max-age=31536000, immutable` for `/_astro/*` and
`public, max-age=300` for everything else. One log line per request.

```sh
bun run --cwd apps/site serve      # http://127.0.0.1:4180
```

Environment:

| variable | default | meaning |
| --- | --- | --- |
| `ROOST_SITE_PORT` | `4180` | listen port |
| `ROOST_SITE_HOST` | `127.0.0.1` | listen address (`0.0.0.0` to accept tailnet/LAN traffic directly) |
| `ROOST_SITE_ORIGIN` | `https://roosttt.com` | build-time canonical origin for `<link rel="canonical">`, OG URLs, and the sitemap |

## Run as a service (`systemd --user`)

`~/.config/systemd/user/roost-site.service`:

```ini
[Unit]
Description=Roost marketing site
After=network-online.target

[Service]
Type=simple
WorkingDirectory=/home/almalinux/repos/roost/apps/site
ExecStart=/home/almalinux/.bun/bin/bun serve.ts
Environment=ROOST_SITE_PORT=4180
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload && systemctl --user enable --now roost-site.service
systemctl --user status roost-site.service
journalctl --user -u roost-site.service -f
```

Linger is already enabled for `almalinux`, so the unit survives logout.

## Expose on the tailnet

The coordinator already owns the `:4102 -> 127.0.0.1:4103` mapping. Check it first, then
add the site mapping only — do not touch the coordinator's:

```sh
tailscale serve status
tailscale serve --bg --https=4443 http://127.0.0.1:4180
```

The site is then reachable at `https://ovh1-8c32g.tail67850e.ts.net:4443/`. Set
`ROOST_SITE_ORIGIN` to that origin for a tailnet-only preview build.

**Fallback** if `tailscale serve --https=4443` is refused (no HTTPS certs provisioned):
bind the server to the tailnet interface instead and point the origin at it —

```sh
ROOST_SITE_HOST=0.0.0.0 bun run --cwd apps/site serve
# reachable at http://ovh1-8c32g.tail67850e.ts.net:4180
```

...adding `Environment=ROOST_SITE_HOST=0.0.0.0` to the unit, and rebuilding with
`ROOST_SITE_ORIGIN=http://ovh1-8c32g.tail67850e.ts.net:4180` so canonical and OG links
match reality.

## Publish `roosttt.com`

`roosttt.com` is the static Astro origin. Publish it independently of every
coordinator or managed-service route:

```sh
ROOST_SITE_ORIGIN=https://roosttt.com bun run --cwd apps/site publish
```

`apps/site/package.json::publish` builds the site, then
`apps/site/scripts/publish.ts` runs
`rsync -a --delete dist/ /srv/roost-site/www/`. Override the destination with
`ROOST_SITE_PUBLISH_ROOT`. The edge Caddy container mounts the destination
read-only, so publishing does not restart a coordinator or edge service.

The apex and `www` origins remain static-only. They do not proxy SPA routes,
Connect RPC, Sync, worker WebSockets, tenant resolution, or managed
authentication.

## Managed deployment status

**Managed hosting is qualified but not launched in `v0.5.0`.** No production
managed coordinator containers, `dashboard.roosttt.com` Cloudflare route, or
managed image publication are active. The assets below define a future
operator deployment; they are not instructions to activate it as part of the
site publish.

Public signup is off. `assets/linux/systemd/roost-saas-auth.service` sets
`ROOST_SIGNUP_ENABLED=0` and `ROOST_GOOGLE_ENABLED=0`. Accounts are created by
an operator with `roost saas account-create --email <address>` after a
separately authorized managed launch. Do not expose `/signup`, enable either
gate, publish `Dockerfile.coord`, or add the dashboard tunnel route for the
`v0.5.0` site release.

## Qualified managed topology (not launched)

The managed design is one coordinator container and one persisted instance
layout per account, not one coordinator shared by every account.

| Boundary | Source owner | Contract |
|---|---|---|
| Public edge | `assets/linux/cloudflared/config.yml.example` | The future `dashboard.roosttt.com` hostname terminates at the edge Caddy origin. The example has one whole-host ingress and a terminal `http_status:404` rule. |
| Route generation | `apps/roost-cli/src/saas/caddy.ts` | `CaddyTenantRouter` writes `roost-tenants.caddy`. A 64-lowercase-hex route key selects `/_roost/t/<route-key>/*`; `handle_path` strips the prefix and proxies only to the matching account container. |
| Account resolver | `apps/roost-cli/src/saas/resolver.ts` | `POST /__roost/tenant/resolve` maps normalized email to the account route key in `/srv/data/roost/control.db`. Unknown emails receive a keyed synthetic route, so the response does not disclose account existence. |
| Browser routing | `apps/web/src/auth/tenant-routing.ts` | The browser persists the route key as a routing hint and sends coordinator traffic through `/_roost/t/<route-key>`. The route key is not authorization. |
| Account runtime | `apps/roost-cli/src/saas/docker.ts` and `apps/roost-cli/src/saas/docker-container-contract.ts` | `ManagedInstanceRuntime` creates and adopts only an exact-spec container labeled with its account and coordinator IDs and pinned to an immutable image digest. |
| Per-account state | `apps/roost-cli/src/saas/layout.ts` and `apps/roost-cli/src/saas/registry-validation.ts` | Each coordinator uses `/srv/data/roost/instances/<coordinator-id>/data` plus account-specific secrets, verifier material, manifest, database, logs, and authorized keys. |
| Operator control | `apps/roost-cli/src/saas/index.ts` | Account lifecycle, route reconciliation, encrypted backup, and immutable-image rollout are explicit operator commands. |
| Service isolation | `assets/linux/systemd/` and `assets/linux/nftables/roost-saas-origin-isolation.nft` | Separate auth, provisioner, resolver, reconcile, backup, bridge, and origin-firewall units keep public request handling away from Docker and privileged host state. |

Request flow:

1. The future Cloudflare tunnel forwards the dashboard hostname to edge Caddy.
2. Login submits email to `/__roost/tenant/resolve`; the resolver reads the
   control registry and returns a route key without confirming account
   existence.
3. The browser sends subsequent RPC and WebSocket traffic through
   `/_roost/t/<route-key>`.
4. Generated Caddy configuration maps that route key to exactly one
   per-account container and strips the routing prefix before proxying.
5. The coordinator authenticates the account/device and applies its persisted
   dashboard scope; route selection alone grants no access.

The operator command surface implemented by `apps/roost-cli/src/saas/index.ts`
is:

```sh
roost saas account-create --email <address>
roost saas account-resend --email <address>
roost saas account-disable --email <address> --yes
roost saas account-enable --email <address>
roost saas accounts
roost saas reconcile
roost saas backup
roost saas backup --email <address>
roost saas rollout --image <sha256:digest>
```

`account-create` is the managed account-entry policy: it reserves the
account and route key, creates the dedicated container and instance layout,
reconciles the Caddy route, proves direct and routed identity, and delivers
owner activation. `account-resend`, `account-disable`, and `account-enable`
operate on that same account-owned coordinator. `reconcile` repairs runtime
and route drift; `backup` encrypts coordinator backups; `rollout` accepts only
an immutable `sha256:` image digest.

The service-owned commands are `roost saas resolver`,
`roost __saas-auth serve`, and `roost __saas-provisioner serve`; their unit
files are `assets/linux/systemd/roost-saas-resolver.service`,
`roost-saas-auth.service`, and `roost-saas-provisioner.service`. They are
internal topology, not manual public-launch steps.
