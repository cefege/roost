// Throwaway: drive the REAL v2 integration transport against the Rust server.
import { createAgentReporter, reportAgentReference } from "/home/almalinux/repos/roost-v3-worker/apps/worker/src/agents/report-transport.ts";
const config = { endpoint: process.env.SMOKE_ENDPOINT!, capability: process.env.SMOKE_CAPABILITY!, sessionId: process.env.SMOKE_SESSION! };
const report = createAgentReporter(config);
report("blocked", "waiting on approval");
await new Promise((resolve) => setTimeout(resolve, 700));
const outcome = await reportAgentReference(config, { kind: "path", value: "/tmp/smoke-conversation.jsonl" });
console.log("SMOKE_REFERENCE " + JSON.stringify(outcome));
