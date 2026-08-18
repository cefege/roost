#!/usr/bin/env bun
// Render the site's static raster assets from vector sources:
//   public/og.png              1200x630 social card (inline SVG template below)
//   public/icon-192.png        PWA / rich-link icon  (from public/icon.svg)
//   public/apple-touch-icon.png  iOS home screen 180 (from public/icon.svg)
//
// Mirrors scripts/gen-icons.ts at the repo root, but resolves every path from
// import.meta.dir so `bun scripts/gen-og.ts` works from any cwd (package.json
// runs it as the first half of `build`). Text is drawn with plain <text> in the
// system sans stack — resvg loads system fonts; the site itself ships no
// webfonts and this must not become the exception.
import { Resvg } from "@resvg/resvg-js";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const PUB = join(import.meta.dir, "..", "public");
const FONT = "DejaVu Sans, Noto Sans, Liberation Sans, sans-serif";

/** The icon.svg glyph, transplanted into the card at `size` px from (x, y). */
function glyph(x: number, y: number, size: number): string {
  const s = size / 24;
  return `<g transform="translate(${x} ${y}) scale(${s})">
    <rect width="24" height="24" rx="5" fill="#141113" stroke="#524345" stroke-width="0.5"/>
    <g fill="#db7556">
      <ellipse cx="11.5" cy="12.5" rx="6.2" ry="6.4"/>
      <circle cx="14.5" cy="8" r="3.6"/>
      <polygon points="17.5,6.4 19.9,8.8 17.5,11.2"/>
      <polygon points="8.5,10.5 4.0,18.6 11.0,19.0"/>
      <rect x="4.8" y="19" width="14.4" height="1.5" rx="0.75"/>
    </g>
  </g>`;
}

const OG_CARD = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
  <defs>
    <radialGradient id="glow" cx="0.14" cy="0" r="0.85">
      <stop offset="0" stop-color="#db7556" stop-opacity="0.22"/>
      <stop offset="1" stop-color="#db7556" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="1200" height="630" fill="#0d0d0d"/>
  <rect width="1200" height="630" fill="url(#glow)"/>
  ${glyph(80, 96, 112)}
  <text x="220" y="186" font-family="${FONT}" font-size="88" font-weight="700" fill="#f1dfde" letter-spacing="-2">Roost</text>
  <rect x="80" y="256" width="88" height="4" rx="2" fill="#db7556"/>
  <text x="80" y="360" font-family="${FONT}" font-size="62" font-weight="600" fill="#f1dfde">One control panel for</text>
  <text x="80" y="436" font-family="${FONT}" font-size="62" font-weight="600" fill="#f1dfde">every machine you own.</text>
  <text x="80" y="514" font-family="${FONT}" font-size="28" fill="#a8908d">self-hosted \u00b7 macOS \u00b7 Linux \u00b7 Windows</text>
  <text x="1120" y="514" text-anchor="end" font-family="${FONT}" font-size="24" fill="#a8908d">github.com/cefege/roost</text>
  <rect y="626" width="1200" height="4" fill="#db7556"/>
</svg>`;

function emit(svg: string, width: number, out: string): void {
  const png = new Resvg(svg, {
    fitTo: { mode: "width", value: width },
    font: { loadSystemFonts: true, defaultFontFamily: "DejaVu Sans" },
  })
    .render()
    .asPng();
  const path = join(PUB, out);
  writeFileSync(path, png);
  console.log(`gen-og: ${out} (${statSync(path).size} bytes)`);
}

const iconSvg = readFileSync(join(PUB, "icon.svg"), "utf-8");

emit(OG_CARD, 1200, "og.png");
emit(iconSvg, 192, "icon-192.png");
emit(iconSvg, 180, "apple-touch-icon.png");
