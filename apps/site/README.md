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
bun apps/site/scripts/check-links.ts  # every root-relative href resolves in dist
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
| `ROOST_SITE_ORIGIN` | `https://ovh1-8c32g.tail67850e.ts.net:4443` | build-time canonical origin for `<link rel="canonical">`, OG URLs, and the sitemap |

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

The site is then reachable at `https://ovh1-8c32g.tail67850e.ts.net:4443/`, which is the
default `ROOST_SITE_ORIGIN`.

**Fallback** if `tailscale serve --https=4443` is refused (no HTTPS certs provisioned):
bind the server to the tailnet interface instead and point the origin at it —

```sh
ROOST_SITE_HOST=0.0.0.0 bun run --cwd apps/site serve
# reachable at http://ovh1-8c32g.tail67850e.ts.net:4180
```

...adding `Environment=ROOST_SITE_HOST=0.0.0.0` to the unit, and rebuilding with
`ROOST_SITE_ORIGIN=http://ovh1-8c32g.tail67850e.ts.net:4180` so canonical and OG links
match reality.

## Public hosting (later)

No public domain is owned yet. `cloudflared` is already installed at
`~/.local/bin/cloudflared`:

```sh
cloudflared tunnel --url http://127.0.0.1:4180   # temporary trycloudflare.com URL
```

Once a domain exists, replace the quick tunnel with a named tunnel routing that hostname
to `http://127.0.0.1:4180`.

**Changing the public hostname means setting `ROOST_SITE_ORIGIN` to the new origin and
rebuilding** — canonical URLs, OG image URLs, `robots.txt`, and the sitemap are all baked
in at build time.
