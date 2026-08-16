import { mkdir, readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import {
  applyPrivateDacl,
  durableRemove,
  durableWriteFile,
} from "@roost/shared/durability";
import { supportedHostPlatform } from "@roost/shared/platform";
import {
  OMP_AGENT_INTEGRATION,
  PI_AGENT_INTEGRATION,
} from "./integration-assets.generated.ts";

const OMP_INSTALL_NAME = "roost-omp-agent-state.ts";
const PI_INSTALL_NAME = "roost-pi-agent-state.ts";

function expandHome(value: string, home: string): string {
  if (value === "~") return home;
  if (value.startsWith("~/") || value.startsWith("~\\")) return join(home, value.slice(2));
  return value;
}

export function resolvePiExtensionDir(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): string {
  const configured = env.PI_CODING_AGENT_DIR?.trim();
  const agentDir = configured ? expandHome(configured, home) : join(home, ".pi", "agent");
  return join(agentDir, "extensions");
}

export function resolveOmpExtensionDir(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): string {
  const sharedAgentDir = env.PI_CODING_AGENT_DIR?.trim();
  if (sharedAgentDir) return join(expandHome(sharedAgentDir, home), "extensions");
  const configured = expandHome(env.PI_CONFIG_DIR?.trim() || ".omp", home);
  const configDir = isAbsolute(configured) ? configured : join(home, configured);
  return join(configDir, "agent", "extensions");
}

async function installOwnedAsset(
  directory: string,
  filename: string,
  integrationId: "omp" | "pi",
  content: string,
): Promise<string> {
  const platform = supportedHostPlatform();
  await mkdir(directory, { recursive: true, mode: 0o700 });
  const target = join(directory, filename);
  let existing: string | null = null;
  try { existing = await readFile(target, "utf8"); }
  catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    if (code !== "ENOENT") throw error;
  }
  if (existing === content) {
    if (platform === "win32") {
      await applyPrivateDacl(target, { platform, mode: 0o600, privateDacl: true });
    }
    return target;
  }
  if (existing !== null && !existing.includes(`ROOST_INTEGRATION_ID=${integrationId}`)) {
    throw new Error(`refusing to overwrite non-Roost extension: ${target}`);
  }
  await durableWriteFile(target, content, {
    platform,
    mode: 0o600,
    privateDacl: true,
  });
  return target;
}

export interface InstalledAgentIntegrations {
  omp: string;
  pi: string;
}

async function integrationAssets(): Promise<{ omp: string; pi: string }> {
  if (OMP_AGENT_INTEGRATION && PI_AGENT_INTEGRATION) {
    return { omp: OMP_AGENT_INTEGRATION, pi: PI_AGENT_INTEGRATION };
  }
  const [omp, pi] = await Promise.all([
    Bun.file(new URL("./integrations/omp/roost-agent-state.ts", import.meta.url)).text(),
    Bun.file(new URL("./integrations/pi/roost-agent-state.ts", import.meta.url)).text(),
  ]);
  return { omp, pi };
}

async function retireOwnedOmpExtension(directory: string): Promise<void> {
  const target = join(directory, "roost-omp-" + "session-api.ts");
  let content: string;
  try {
    content = await readFile(target, "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  if (!content.includes("ROOST_INTEGRATION_ID=omp")) return;
  await durableRemove(target, { platform: supportedHostPlatform() });
}

export async function installAgentIntegrations(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): Promise<InstalledAgentIntegrations> {
  const ompDirectory = resolveOmpExtensionDir(env, home);
  await retireOwnedOmpExtension(ompDirectory);
  const assets = await integrationAssets();
  const [omp, pi] = await Promise.all([
    installOwnedAsset(ompDirectory, OMP_INSTALL_NAME, "omp", assets.omp),
    installOwnedAsset(resolvePiExtensionDir(env, home), PI_INSTALL_NAME, "pi", assets.pi),
  ]);
  return { omp, pi };
}
