import { appendFile, realpath } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { assertOwned, readProfile } from "./profile-manifest";
import { profileModelSelection } from "./profile-models";

const input = process.argv.slice(2);
const boundary = input.indexOf("--");
if (boundary < 0) throw new Error("Owned launcher argument boundary is required");
const configuration = input.slice(0, boundary);
function option(name: string): string {
  const index = configuration.indexOf(name);
  if (index < 0 || !configuration[index + 1]) throw new Error(`Missing launcher ${name}`);
  return configuration[index + 1]!;
}
const repoRoot = await realpath(option("--repo-root"));
const sourceRoot = await realpath(option("--source-root"));
const runRoot = await realpath(option("--run-root"));
const args = input.slice(boundary + 1);
let workspace = process.cwd();
for (let index = 0; index < args.length; index++) {
  if (args[index] === "--cwd") {
    if (!args[index + 1]) throw new Error("--cwd requires a real owned workspace");
    workspace = resolve(args[++index]!);
  } else if (args[index]?.startsWith("--cwd=")) workspace = resolve(args[index]!.slice("--cwd=".length));
}
workspace = await realpath(workspace);
assertOwned(runRoot, workspace);
const profile = await readProfile(workspace);
if (profile.repoRoot !== repoRoot || profile.sourceRoot !== sourceRoot) throw new Error("Launcher profile does not match its owned repository/source checkout");
const environment = { ...profile.environment };
const catalogStatus = process.env.ROOST_OMP_CATALOG_STATUS;
// Worker data is reached through a short socket-safe link; ownership is judged on the real directory.
const ownedWorkerPath = async (path: string) => {
  const real = join(await realpath(dirname(resolve(workspace, path))), basename(path));
  assertOwned(runRoot, real);
  return real;
};
if (catalogStatus) environment.ROOST_OMP_CATALOG_STATUS = await ownedWorkerPath(catalogStatus);
const parityRoot = join(repoRoot, "deploy/omp/parity");
// OMP refuses `--trusted-extension` beside `--extension`. The worker launches
// ACP with `--extension <catalog>`, so each product extension joins the exact
// trusted allowlist; both legs then load extensions the same way.
const productArgs: string[] = [];
const productExtensions: string[] = [];
for (let index = 0; index < args.length; index++) {
  const argument = args[index]!;
  if (argument === "--extension" || argument === "-e") {
    const value = args[++index];
    if (!value) throw new Error(`${argument} requires a module path`);
    productExtensions.push(await ownedWorkerPath(value));
  } else if (argument.startsWith("--extension=")) productExtensions.push(await ownedWorkerPath(argument.slice("--extension=".length)));
  else if (argument === "--hook" || argument.startsWith("--hook=")) throw new Error("Parity launches do not accept product hooks");
  else productArgs.push(argument);
}
const ownedArgs = [...productExtensions, join(parityRoot, "fixture-extension.ts")].flatMap(path => ["--trusted-extension", path]);
const model = profileModelSelection(profile);
const hasProviderArgument = args.some(argument => argument === "--provider" || argument.startsWith("--provider="));
const hasModelArgument = args.some(argument => argument === "--model" || argument.startsWith("--model="));
if (!hasProviderArgument && !hasModelArgument) ownedArgs.push("--provider", model.provider);
if (!hasModelArgument && !hasProviderArgument) ownedArgs.push("--model", model.defaultId);
if (profile.launch.memorySession) ownedArgs.push("--no-session");
else if (profile.launch.sessionPath) ownedArgs.push("--session", profile.launch.sessionPath);
else throw new Error("Persisted parity session has no real SessionManager path");
if (profile.launch.tools) ownedArgs.push("--tools", profile.launch.tools.join(","));
const command = [process.execPath, "--tsconfig-override", join(parityRoot, "tsconfig.json"), "--preload", join(parityRoot, "profile-start.ts"), "--preload", join(parityRoot, "preload.ts"), join(sourceRoot, "packages/coding-agent/src/cli.ts"), ...productArgs, ...ownedArgs];
environment.OMP_PARITY_LAUNCHER_PID = String(process.pid);
const child = Bun.spawn(command, { cwd: workspace, env: environment, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
const forward = (signal: "SIGINT" | "SIGTERM" | "SIGHUP") => { try { child.kill(signal); } catch { /* The real child may already have exited. */ } };
const interrupt = () => forward("SIGINT");
const terminate = () => forward("SIGTERM");
const hangup = () => forward("SIGHUP");
process.on("SIGINT", interrupt);
process.on("SIGTERM", terminate);
process.on("SIGHUP", hangup);
const status = await child.exited;
process.off("SIGINT", interrupt);
process.off("SIGTERM", terminate);
process.off("SIGHUP", hangup);
const generationPath = join(profile.home, ".parity/process-generation.json");
const generation = await Bun.file(generationPath).exists() ? await Bun.file(generationPath).json() as { generation: number } : undefined;
if (!generation) throw new Error("Actual CLI startup was not observed by the owned process preload");
await appendFile(environment.OMP_PARITY_PROCESS_TRACE!, `${JSON.stringify({ channel: "builtin.process", value: { exited: true, restarted: generation.generation > 1, generation: generation.generation, exitCode: status, signal: child.signalCode, pid: child.pid } })}\n`);
process.exit(status);
