import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { createRegistry, defineExtension, section } from "@earendil-works/pi-durable";
import { CodingTools } from "@earendil-works/pi-durable/tools";
import { RoostTarget } from "./target-doc.ts";

export function createAgentRegistry() {
  const registry = createRegistry();
  registry.install(CodingTools);
  registry.install(defineExtension({
    name: "roost",
    sections: [section("roost", async (input, context) => {
      const target = await input.read.snapshot(RoostTarget, input.conversationId, context);
      if (!target?.cwd || !target.worker_label || !target.worker_os) return undefined;
      return `You are Roost's built-in coding agent. Your tools run on the machine "${target.worker_label}" (${target.worker_os}), starting in ${target.cwd}. That folder is the project this conversation was opened for; work elsewhere on the machine only when the task needs it.`;
    })],
  }));
  return registry;
}
