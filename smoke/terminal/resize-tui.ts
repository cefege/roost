#!/usr/bin/env bun

if (process.stdin.isTTY) {
  process.stdin.setRawMode(true);
  process.stdin.resume();
}
const ESC = "\x1b";
let restored = false;

function restore(): void {
  if (restored) return;
  restored = true;
  if (process.stdin.isTTY) process.stdin.setRawMode(false);
  process.stdout.write(`${ESC}[?1049l${ESC}[0m`);
}

function draw(): void {
  const columns = Math.max(process.stdout.columns || 80, 20);
  const rows = Math.max(process.stdout.rows || 24, 5);
  const title = ` Cell resize probe: ${columns}x${rows} `;
  const border = `+${"-".repeat(Math.max(columns - 2, title.length))}+`;
  const contentWidth = border.length - 2;
  const lines = [border, `|${title.padEnd(contentWidth)}|`, border];
  for (let index = 1; index <= 60; index++) {
    lines.push(`| CELLLINE-${index}${`  row ${index}`.padEnd(Math.max(contentWidth - ` CELLLINE-${index}`.length, 0))}|`);
  }
  lines.push(border, "Press q to return to shell.");
  process.stdout.write(`${ESC}[H${ESC}[2J${lines.join("\r\n")}`);
}

process.stdout.write(`${ESC}[?1049h${ESC}[?25l`);
draw();
process.on("SIGWINCH", draw);
process.on("SIGTERM", () => { restore(); process.exit(0); });
process.on("SIGINT", () => { restore(); process.exit(0); });
process.on("exit", restore);
process.stdin.on("data", (chunk) => {
  if (chunk.toString().includes("q")) {
    restore();
    process.exit(0);
  }
});
