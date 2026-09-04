/**
 * Exposes the hidden root-side SaaS provisioner command entry point.
 * The compiled CLI dispatches here only for the exact internal serve action.
 * Rejecting every other argument keeps privileged startup outside the public command surface.
 */

import { serveProvisioner } from "./runtime.ts";

export async function saasProvisioner(args: string[]): Promise<void> {
  if (args.length !== 1 || args[0] !== "serve") throw new Error("internal SaaS provisioner dispatch refused");
  await serveProvisioner();
}
