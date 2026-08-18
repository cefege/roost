// Default-agent (launch-button command) RPC handlers — app_settings-backed,
// universal across devices. Spread into router.ts's SINGLE router.service()
// literal — never registered with a router.service() call of its own, which
// would shadow every other domain with unimplemented-throws. Closes over
// ConnectDeps only (deps.db); no shared router-local state.

import type { ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { CoordinatorService, AgentConfigSchema } from "@roost/shared/proto/coordinator_pb";
import { requireAuth } from "./auth-interceptor.ts";
import { getAgentConfig, setAgentConfig } from "../agent-config.ts";
import type { ConnectDeps } from "./router.ts";

type AgentConfigMethods = "agentConfigGet" | "agentConfigSet";

export function makeAgentConfigHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AgentConfigMethods> {
  return {
    async agentConfigGet(_req, ctx) {
      requireAuth(ctx.values);
      return create(AgentConfigSchema, await getAgentConfig(deps.db));
    },
    async agentConfigSet(req, ctx) {
      requireAuth(ctx.values);
      return create(AgentConfigSchema, await setAgentConfig(deps.db, {
        selected: req.selected,
        customCommand: req.customCommand,
        autoLaunch: req.autoLaunch ?? false,
      }));
    },
  };
}
