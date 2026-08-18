import type { APIContext } from "astro";

/**
 * Static robots.txt. The sitemap URL has to be absolute, so it is built from
 * the configured origin (`site` in astro.config.mjs, overridable via
 * ROOST_SITE_ORIGIN) rather than hardcoded — change the hostname in one place
 * and rebuild.
 */
export function GET(context: APIContext): Response {
  const site = context.site;
  if (!site) throw new Error("robots.txt: astro.config.mjs must set `site`");
  const body = `User-agent: *\nAllow: /\nSitemap: ${new URL("sitemap-index.xml", site).href}\n`;
  return new Response(body, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
