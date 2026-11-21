#!/usr/bin/env bash
# Publishes only the public marketing site, never the product coordinator.
# Run from any directory; needs Bun, rsync, scp, and the ovh1 SSH alias.
# Archives the served files; changed Caddy config is backed up and hot-reloaded.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bun "$repo_root/deploy/site/build.mjs"
ssh -o BatchMode=yes ovh1 'set -eu; backup=$(mktemp /srv/roost-site/website-backup-XXXXXXXX.tar.gz); tar -czf "$backup" -C /srv/roost-site/www .; printf "Previous website archived at %s\n" "$backup"'
rsync -rlptz --delay-updates --delete-delay "$repo_root/target/site/" ovh1:/srv/roost-site/www/
scp "$repo_root/deploy/site/roost.caddy" ovh1:/srv/roost-site/roost.caddy.next
ssh -o BatchMode=yes ovh1 'bash -se' <<'REMOTE'
config=/srv/infra/edge/conf.d/roost.caddy
staged=/srv/roost-site/roost.caddy.next
trap 'rm -f "$staged"' EXIT
if ! cmp -s "$staged" "$config"; then
  backup=$(mktemp /srv/roost-site/caddy-backup-XXXXXXXX)
  cp "$config" "$backup"
  cp "$staged" "$config"
  if ! docker exec caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile; then
    cp "$backup" "$config"
    printf 'Caddy rejected the config; previous on-disk config restored.\n' >&2
    exit 1
  fi
  printf 'Marketing config reloaded; previous config archived at %s\n' "$backup"
fi
REMOTE
printf 'Published https://roosttt.com/ — verify the public page in a browser.\n'
