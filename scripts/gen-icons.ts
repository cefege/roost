#!/usr/bin/env bun
// Render the Roost PWA raster icons from the SVG sources in apps/web/public.
// Sources: icon.svg (any-purpose) + icon-maskable.svg (safe-zone padded).
// Run after editing either SVG. Outputs PNGs into apps/web/public/ — vite build
// then copies them into dist, and gen-embed.ts bakes dist into the binary.
import { Resvg } from "@resvg/resvg-js";
import { readFileSync, writeFileSync } from "node:fs";

const PUB = "apps/web/public";
function render(src: string, size: number, out: string) {
  const svg = readFileSync(`${PUB}/${src}`, "utf-8");
  const png = new Resvg(svg, { fitTo: { mode: "width", value: size } }).render().asPng();
  writeFileSync(`${PUB}/${out}`, png);
  console.log(`gen-icons: ${out} (${size}x${size})`);
}

render("icon.svg", 32, "icon-32.png");           // tiny favicon fidelity
render("icon.svg", 192, "icon-192.png");          // PWA any
render("icon.svg", 512, "icon-512.png");          // PWA any / splash
render("icon.svg", 180, "apple-touch-icon.png");  // iOS home screen
render("icon-maskable.svg", 512, "icon-maskable-512.png"); // Android adaptive
