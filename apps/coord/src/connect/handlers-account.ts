// The coordinator router consumes one account-handler facade while owner
// activation and password reset keep separate transaction boundaries. Their
// original RPC ordering remains stable when this object enters router.service().

import type { ServiceImpl } from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { makeOwnerActivationHandlers } from "./handlers-owner-activation.ts";
import { makePasswordResetHandlers } from "./handlers-password-reset.ts";
import type { ConnectDeps } from "./router.ts";

type AccountMethods =
  | "authOwnerActivate"
  | "authPasswordResetRequest"
  | "authPasswordResetRedeem";

export type AccountHandlers = Pick<ServiceImpl<typeof CoordinatorService>, AccountMethods>;

export function makeAccountHandlers(deps: ConnectDeps): AccountHandlers {
  return {
    ...makeOwnerActivationHandlers(deps),
    ...makePasswordResetHandlers(deps),
  };
}
