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

## Publish to roosttt.com

Production uses two independent origins:

- `https://roosttt.com` (and its `www` redirect) is the fully static Astro
  marketing and documentation site. Every apex path stays on the static origin.
- `https://dashboard.roosttt.com` is the managed application. Cloudflared
  forwards the entire hostname, without path matchers or static fallbacks,
  directly to the coordinator public listener at `127.0.0.1:4104`.

The apex never proxies SPA routes, RPCs, WebSockets, or coordinator assets.
Likewise, the dashboard hostname never serves files from the Astro publish
directory.

### Build and publish order

Build both current browser surfaces before changing any origin routing:

```sh
bun run --cwd apps/web build
ROOST_SITE_ORIGIN=https://roosttt.com bun run --cwd apps/site publish
bash apps/coord/scripts/install.sh install
```

`publish` builds Astro and runs
`rsync -a --delete dist/ /srv/roost-site/www/` (override the target with
`ROOST_SITE_PUBLISH_ROOT`). The read-only Caddy mount reflects the published
files without a container restart. The web build must precede coordinator
installation so the installed coordinator embeds the current managed SPA.

Install and run the coordinator as its own persistent service with a dedicated
service account, data directory, signing key, SQLite database, and backup
schedule. Its production environment must include:

```sh
ROOST_SAAS_MODE=1
ROOST_PUBLIC_BIND=127.0.0.1:4104
ROOST_WEB_PUBLIC_URL=https://dashboard.roosttt.com
ROOST_TRUST_PROXY=1
```

Do not set the self-hosted Cloudflare Access variables in managed mode. Email
delivery is disabled by default. To enable it, configure all four of
`ROOST_RESEND_ENDPOINT`, `ROOST_RESEND_API_KEY`, `ROOST_EMAIL_FROM`, and
`ROOST_EMAIL_OUTBOX_KEY`; never configure a partial group.

Provision the initial database once. Supply the password only through
piped stdin or the temporary `ROOST_OWNER_BOOTSTRAP_PASSWORD` environment
variable. Never put it in argv, a service definition, this repository, or an
edge configuration:

```sh
printf '%s\n' "$ROOST_OWNER_BOOTSTRAP_PASSWORD" \
  | roost organizations bootstrap-owner \
      --email <owner-email> --organization roost --dashboard personal
unset ROOST_OWNER_BOOTSTRAP_PASSWORD
```

Only after the Astro publish and coordinator installation are current, apply
the edge topology:

| Host | Caddy behavior | Cloudflare tunnel ingress |
|---|---|---|
| `roosttt.com` | Serve `/srv/roost-site` as static files; never proxy coordinator paths | Forward the apex hostname to Caddy at `http://127.0.0.1:80` |
| `www.roosttt.com` | Redirect to `https://roosttt.com` | Forward the `www` hostname to Caddy at `http://127.0.0.1:80` |
| `dashboard.roosttt.com` | Not routed through Caddy | Forward the whole dashboard hostname directly to `http://127.0.0.1:4104` |

The static site mount remains
`/srv/roost-site/www:/srv/roost-site:ro`. Cloudflare terminates public TLS.
Configure all three DNS records in the `roosttt.com` zone as proxied records
targeting the existing tunnel. Keep the existing apex and `www` tunnel ingress
rules pointed at Caddy, and add a separate whole-host dashboard ingress rule
pointed directly at the coordinator listener. Do not copy tunnel credentials,
API tokens, coordinator keys, or account passwords into the repository or
service documentation.

The dashboard cloudflared ingress rule must forward the whole hostname. Do not
enumerate `/login`, `/app`, deep links, RPC methods, `/assets`, Sync, or worker
WebSocket paths; all current and future paths on that hostname go directly to
the managed public listener. Keep the apex Caddy block a static file server with
no coordinator proxy handlers, and do not add a dashboard Caddy block.

No Caddy change or reload is needed for the dashboard hostname. After updating
tunnel ingress, restart cloudflared with the installed unit's supported restart
procedure; do not send it `SIGHUP`.

### Readiness

First confirm that representative marketing paths still come from the static
apex:

```sh
curl -sI https://roosttt.com/
curl -s https://roosttt.com/robots.txt
curl -sI https://roosttt.com/alternatives/cmux-vs-roost/
```

Then verify the managed identity/login boundary on the dashboard hostname:

```sh
curl -sS -X POST -H 'content-type: application/json' --data '{}' \
  https://dashboard.roosttt.com/roost.v1.CoordinatorService/AuthCoordIdentity
curl -i -X POST -H 'content-type: application/json' --data '{}' \
  https://dashboard.roosttt.com/roost.v1.CoordinatorService/AuthPasswordLogin
```

The identity response must report managed mode. The empty login must reach the
Roost handler and return its credential failure. A Cloudflare Access
interstitial, static-site response, or 404 from the apex configuration means
the dashboard whole-host origin is wired incorrectly.

Changing the static public hostname requires setting `ROOST_SITE_ORIGIN` to the
new origin and rebuilding; canonical URLs, OG image URLs, `robots.txt`, and the
sitemap are baked in at build time. Changing the managed public hostname
requires updating `ROOST_WEB_PUBLIC_URL` and the whole-host edge/DNS routing;
it does not change the Astro publish.
