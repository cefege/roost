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

The coordinator listens on `127.0.0.1:4103` and whatever fronts it owns that
mapping. Check the existing Serve configuration first, then add the site
mapping only — do not touch the coordinator's:

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
coordinator origin:

```sh
ROOST_SITE_ORIGIN=https://roosttt.com bun run --cwd apps/site publish
```

`apps/site/package.json::publish` builds the site, then
`apps/site/scripts/publish.ts` runs
`rsync -a --delete dist/ /srv/roost-site/www/`. Override the destination with
`ROOST_SITE_PUBLISH_ROOT`. The edge Caddy container mounts the destination
read-only, so publishing does not restart a coordinator or edge service.

The apex and `www` origins remain static-only. They do not proxy SPA routes,
Connect RPC, Sync, or worker WebSockets. A Roost dashboard is reached through
the operator's own front door, never through this site.
