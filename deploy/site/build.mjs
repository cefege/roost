// Builds the dependency-free marketing site into this checkout's target/site.
// Called with `bun deploy/site/build.mjs` before preview or publication.
// Legacy URLs hand off to the v3 contract instead of retaining v2 instructions.
import { cp, mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const source = fileURLToPath(new URL('./public/', import.meta.url));
const output = fileURLToPath(new URL('../../target/site/', import.meta.url));
await mkdir(output, { recursive: true });
// Copy into a fresh staging tree so deleted source assets cannot survive a build.
const staging = await mkdtemp(path.join(path.dirname(output), 'site-build-'));
await cp(source, staging, { recursive: true });
await cp(new URL('../../LICENSE', import.meta.url), path.join(staging, 'LICENSE'));

const assetNames = new Map();
for (const filename of ['fonts/nunito-latin.woff2', 'icon.svg', 'bird.svg', 'social.svg', 'og.png', 'apple-touch-icon.png', 'icon-192.png', 'site.css', 'site.js']) {
  let contents = await readFile(path.join(staging, filename));
  const { dir, name, ext } = path.parse(filename);
  if (ext === '.css' || ext === '.svg') {
    let text = contents.toString('utf8');
    for (const [original, hashed] of assetNames) text = text.replaceAll(original, hashed);
    contents = Buffer.from(text);
    await writeFile(path.join(staging, filename), contents);
  }
  const digest = createHash('sha256').update(contents).digest('hex').slice(0, 12);
  const hashed = path.join(dir, `${name}.${digest}${ext}`);
  await rename(path.join(staging, filename), path.join(staging, hashed));
  assetNames.set(`/${filename}`, `/${hashed}`);
}

const guide = 'https://github.com/cefege/roost/blob/v3/GETTING_STARTED.md';
const architecture = 'https://github.com/cefege/roost/blob/v3/ARCHITECTURE.md';
const redirects = {
  'install': '/#install',
  'docs': guide,
  'docs/install': `${guide}#install`,
  'docs/quickstart': `${guide}#install`,
  'docs/fleet': `${guide}#add-another-machine`,
  'docs/cli': 'https://github.com/cefege/roost/blob/v3/docs/phase6-cli-contract.md',
  'docs/mobile': `${guide}#pair-your-phone`,
  'docs/networking': `${guide}#start-locally-then-add-a-front-door-when-you-need-one`,
  'docs/security': `${architecture}#tenant-isolation`,
  'docs/terminal': `${architecture}#terminal-fidelity`,
  'docs/agents': `${architecture}#terminal-only-boundary`,
};
for (const comparison of ['', 'amux', 'claude-code-web', 'cmux', 'conductor', 'herdr', 'omnara', 'tmux', 'vibetunnel']) {
  redirects[comparison ? `alternatives/${comparison}-vs-roost` : 'alternatives'] = '/#why';
}
for (const [route, destination] of Object.entries(redirects)) {
  const directory = path.join(staging, route);
  await mkdir(directory, { recursive: true });
  await writeFile(path.join(directory, 'index.html'), `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0;url=${destination}"><meta name="robots" content="noindex"><link rel="canonical" href="${destination.startsWith('/') ? `https://roosttt.com${destination}` : destination}"><title>Roost — this page has moved</title><script src="/site.js"></script><link rel="stylesheet" href="/site.css"></head><body><main class="legal wrap"><h1>A new place to land.</h1><p>Roost’s current v3 information lives here: <a href="${destination}">continue to the updated page ↗</a>.</p></main></body></html>\n`);
}
await writeFile(path.join(staging, '404.html'), '<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Page not found — Roost</title><script src="/site.js"></script><link rel="stylesheet" href="/site.css"></head><body><main class="legal wrap"><p>404 · A little off course.</p><h1>Let’s get you home.</h1><p>This page isn’t here, but your new favourite terminal might be.</p><a class="button" href="/">Back to Roost ↗</a></main></body></html>\n');
await writeFile(path.join(staging, 'robots.txt'), 'User-agent: *\nAllow: /\nSitemap: https://roosttt.com/sitemap.xml\n');
const sitemap = '<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">' + ['', 'privacy/', 'terms/'].map(route => `<url><loc>https://roosttt.com/${route}</loc></url>`).join('') + '</urlset>\n';
await writeFile(path.join(staging, 'sitemap.xml'), sitemap);
await writeFile(path.join(staging, 'sitemap-index.xml'), '<?xml version="1.0" encoding="UTF-8"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><sitemap><loc>https://roosttt.com/sitemap.xml</loc></sitemap></sitemapindex>\n');

async function rewriteAssetReferences(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) await rewriteAssetReferences(filename);
    else if (entry.name.endsWith('.html')) {
      let html = await readFile(filename, 'utf8');
      for (const [original, hashed] of assetNames) html = html.replaceAll(original, hashed);
      await writeFile(filename, html);
    }
  }
}
await rewriteAssetReferences(staging);
await rm(output, { recursive: true, force: true });
await rename(staging, output);
console.log(`Built marketing site: ${output}`);
