import { appendFile, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { readProfile } from "./profile-manifest";

const workspace = process.env.OMP_PARITY_WORKSPACE;
if (!workspace) throw new Error("Host command requires the owned profile environment");
const profile = await readProfile(workspace);
if (process.env.HOME !== profile.home) throw new Error("Host command escaped owned HOME");
const trace = join(profile.home, ".parity/host.jsonl");
const mode = process.argv[2];
const clipboard = join(profile.home, ".parity/clipboard.txt");
if (mode === "copy") {
  const text = await Bun.stdin.text();
  await writeFile(clipboard, text);
  await appendFile(trace, `${JSON.stringify({ channel: "builtin.clipboard", value: text })}\n`);
} else if (mode === "paste") {
  if (await Bun.file(clipboard).exists()) process.stdout.write(await readFile(clipboard));
} else if (mode === "open") {
  const target = process.argv.at(-1);
  if (!target || target === "open") throw new Error("Actual opener did not supply a URL or path");
  await appendFile(trace, `${JSON.stringify({ channel: "builtin.host-open-url", value: target })}\n`);
} else throw new Error(`Unknown owned OS command boundary ${mode}`);
