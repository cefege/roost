import * as fs from "node:fs/promises";
import * as path from "node:path";
import type { BuiltinRequirements } from "./builtin-requirements";
import type { BuiltinSeedContext, BuiltinSeedManifest } from "./builtin-seed";
import { ownedBuiltinPath, writeOwnedBuiltinFile } from "./builtin-seed";

async function git(directory: string, args: string[]): Promise<void> {
  const child = Bun.spawn(["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", ...args], {
    cwd: directory, env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", GIT_TERMINAL_PROMPT: "0" },
    stdout: "pipe", stderr: "pipe", stdin: "ignore",
  });
  const [status, error] = await Promise.all([child.exited, new Response(child.stderr).text()]);
  if (status !== 0) throw new Error(`Owned git fixture ${args[0]} failed (${status}): ${error}`);
}

async function initializeGit(directory: string, branch: string, files: Record<string, string>): Promise<void> {
  await fs.mkdir(directory, { recursive: true });
  await git(directory, ["init", "--initial-branch", branch]);
  await git(directory, ["config", "user.name", "Parity Fixture"]);
  await git(directory, ["config", "user.email", "parity@example.invalid"]);
  for (const [name, content] of Object.entries(files)) await writeOwnedBuiltinFile(directory, name, content);
  await git(directory, ["add", "."]);
  await git(directory, ["commit", "--no-gpg-sign", "-m", "Seed isolated builtin parity workspace"]);
}

async function seedMarketplace(context: BuiltinSeedContext): Promise<void> {
  await initializeGit(path.join(context.workspace, "parity-marketplace"), "parity-market", {
    ".claude-plugin/marketplace.json": JSON.stringify({ name: "parity-market", owner: { name: "Parity Fixture" },
      plugins: [{ name: "parity-plugin", version: "1.0.0", description: "Owned parity plugin", source: "./parity-plugin" }] }),
    "parity-plugin/.claude-plugin/plugin.json": JSON.stringify({ name: "parity-plugin", version: "1.0.0", description: "Owned inert parity plugin" }),
    "parity-plugin/commands/parity-plugin.md": "---\ndescription: Owned parity fixture command\n---\nParity plugin content.\n",
  });
}

async function seedSecurity(context: BuiltinSeedContext): Promise<void> {
  const { SecurityStore } = await context.load<typeof import("@omp-source/packages/coding-agent/src/security/store")>("packages/coding-agent/src/security/store.ts");
  const { importSarif } = await context.load<typeof import("@omp-source/packages/coding-agent/src/security/importers/sarif")>("packages/coding-agent/src/security/importers/sarif.ts");
  const { exportSecurityBundleToSarif } = await context.load<typeof import("@omp-source/packages/coding-agent/src/security/sarif")>("packages/coding-agent/src/security/sarif.ts");
  const store = await SecurityStore.openForCwd(context.workspace);
  const bundle = await importSarif({ version: "2.1.0", runs: [{ tool: { driver: { name: "parity", rules: [{ id: "PARITY001", shortDescription: { text: "Parity finding" } }] } },
    results: [{ ruleId: "PARITY001", message: { text: "Parity finding" }, level: "warning", locations: [{ physicalLocation: { artifactLocation: { uri: "parity.txt" }, region: { startLine: 1 } } }] }] }] }, { repositoryRoot: store.repositoryRoot });
  bundle.scan.id = "secscan_parity";
  bundle.scan.findingIds = ["finding_parity"];
  if (bundle.findings.length !== 1) throw new Error("Real SARIF importer did not produce the seed finding");
  bundle.findings[0].id = "finding_parity";
  bundle.findings[0].scanId = bundle.scan.id;
  bundle.report = "Parity report\n";
  bundle.sarif = exportSecurityBundleToSarif(bundle);
  await store.putBundle(bundle);
}

async function seedExternalEditor(requirement: BuiltinRequirements, context: BuiltinSeedContext, manifest: BuiltinSeedManifest): Promise<void> {
  const editor = requirement.host.externalEditor;
  if (!editor) return;
  const executable = await writeOwnedBuiltinFile(context.home, ".parity/bin/parity-editor", `#!/usr/bin/env bun\nimport * as fs from "node:fs/promises";\nconst target = process.argv.at(-1);\nif (!target || target === process.argv[1]) throw new Error("External editor target missing");\nawait fs.writeFile(target, ${JSON.stringify(editor.writes)});\nprocess.exitCode = ${editor.exitCode};\n`);
  await fs.chmod(executable, 0o700);
  manifest.environment.EDITOR = executable;
  manifest.environment.VISUAL = executable;
}

export async function seedBuiltinPrerequisites(requirement: BuiltinRequirements, context: BuiltinSeedContext, manifest: BuiltinSeedManifest): Promise<void> {
  await fs.mkdir(path.join(context.workspace, "parity-destination"), { recursive: true });
  if (requirement.git) {
    await initializeGit(context.workspace, requirement.git.branch, requirement.git.commitFiles);
    for (const [name, content] of Object.entries(requirement.git.dirtyFiles)) await writeOwnedBuiltinFile(context.workspace, name, content);
  }
  for (const directory of requirement.directories ?? []) await fs.mkdir(ownedBuiltinPath(context.workspace, directory), { recursive: true });
  if (requirement.mcp) {
    const { getMCPConfigPath } = await context.load<typeof import("@omp-source/packages/utils/src/dirs")>("packages/utils/src/dirs.ts");
    const { addMCPServer } = await context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/config-writer")>("packages/coding-agent/src/mcp/config-writer.ts");
    await addMCPServer(getMCPConfigPath(requirement.mcp.scope, context.workspace), requirement.mcp.name, { command: requirement.mcp.command, enabled: requirement.mcp.enabled });
  }
  if (requirement.captures.some(capture => capture.channel === "builtin.approvals")) {
    const approval = await context.load<typeof import("@omp-source/packages/coding-agent/src/tools/approval-cache")>("packages/coding-agent/src/tools/approval-cache.ts");
    const agentDir = manifest.environment.PI_CODING_AGENT_DIR;
    if (!agentDir) throw new Error("Missing owned agent directory for approval seed");
    approval.getApprovalGrants(agentDir).record(approval.approvalGrantKey("read", ownedBuiltinPath(context.workspace, "parity.txt")), "read", "global");
  }
  await seedExternalEditor(requirement, context, manifest);
  for (const prerequisite of requirement.prerequisites) {
    if (prerequisite.startsWith("Seed SecurityStore through its real putBundle API")) await seedSecurity(context);
    else if (prerequisite.startsWith("Create real local git repository ./parity-marketplace")) await seedMarketplace(context);
    else if (prerequisite.startsWith("Seed only the owned Smithery credential store")) {
      const { saveSmitheryApiKey } = await context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/smithery-auth")>("packages/coding-agent/src/mcp/smithery-auth.ts");
      await saveSmitheryApiKey("parity-local-key");
    } else if (prerequisite.startsWith("Launch actual session without eval/task tools")) manifest.launch.omitTools = ["eval", "task"];
    else if (prerequisite.startsWith("Rule provider tape must return valid TTSR Markdown")) {
      if (requirement.session.seed === "empty" || requirement.models.turns.length === 0) throw new Error("Rule-generation fixture lacks seeded assistant history or real provider tape");
    } else if (prerequisite.startsWith("Empty owned Codex auth; provide silent loopback peer SDP")) {
      if (requirement.host.audio !== "silent-loopback-no-device" || requirement.host.oauth !== "empty-owned-store") throw new Error("Live prerequisite lacks isolated audio/OAuth host contract");
    } else if (prerequisite.startsWith("Ubuntu native ALSA controlled loopback device and real browser virtual-WAV microphone")) {
      if (requirement.host.audio !== "virtual-loopback-wave" || requirement.host.oauth !== "owned-live-fixture" || !manifest.services.some(service => service.kind === "live")) throw new Error("Live prerequisite lacks its declared controlled audio/OAuth service");
      const audioConfiguration = manifest.environment.ALSA_CONFIG_PATH;
      if (!audioConfiguration) throw new Error("Live prerequisite lacks a prepared owned ALSA configuration");
      ownedBuiltinPath(context.home, path.relative(context.home, audioConfiguration));
    } else if (prerequisite.startsWith("Scrub multiplexer identification environment")) {
      const { TERMINAL_MULTIPLEXER_ENV_KEYS } = await context.load<typeof import("@omp-source/packages/tui/src/terminal-multiplexer")>("packages/tui/src/terminal-multiplexer.ts");
      manifest.unsetEnvironment.push(...TERMINAL_MULTIPLEXER_ENV_KEYS.filter(key => key !== "TERM"));
      manifest.environment.TERM = "xterm-256color";
    } else if (prerequisite.startsWith("Seed only actual project MCP config via canonical addMCPServer")) {
      if (!manifest.services.some(service => service.kind === "mcp-http")) throw new Error("MCP OAuth prerequisite lacks its owned Streamable HTTP service, which writes the project config through addMCPServer");
    } else if (prerequisite.startsWith("Follow only the actual source onAuth host-open request")) {
      if (!manifest.services.some(service => service.kind === "mcp-http")) throw new Error("MCP OAuth prerequisite lacks its declared owned authorization service");
    } else if (prerequisite.startsWith("Scrub SMITHERY_API_KEY")) {
      manifest.unsetEnvironment.push("SMITHERY_API_KEY");
      if (!manifest.services.some(service => service.kind === "smithery")) throw new Error("Smithery prerequisite lacks its owned registry service");
    } else if (prerequisite.startsWith("Map only fixed registry.smithery.ai paths")) {
      if (!manifest.services.some(service => service.kind === "smithery")) throw new Error("Smithery prerequisite lacks its owned registry service");
    } else if (prerequisite.startsWith("Real /restart retains actual same owned HOME/session/MCP config")) {
      if (!requirement.lifecycle.superviseRealRestart || !requirement.lifecycle.resumeSameSession) throw new Error("Restart prerequisite lacks real supervised restart with the same session");
    } else if (prerequisite.startsWith("Reuse the actual source packages/collab-web/scripts/local-relay.ts startLocalRelay")) {
      if (!manifest.services.some(service => service.kind === "collab-relay")) throw new Error("Collaboration prerequisite lacks its owned canonical relay service");
    } else if (prerequisite.startsWith("Positive guest cases require a SECOND REAL owned source interactive TUI process") || prerequisite.startsWith("On cleanup send real peer /collab stop then /exit")) {
      if (requirement.collabWorkflow?.peer) throw new Error("Collaboration peer supervisor is not implemented for guest workflow cases");
    } else throw new Error(`Unimplemented builtin prerequisite: ${prerequisite}`);
  }
}
