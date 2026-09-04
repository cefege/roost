// The coordinator router needs one stable auth-handler facade while security
// domains stay small enough to review independently. It preserves the original
// bootstrap, pairing, then device-method ordering in the router service object.

import type { ServiceImpl } from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { makeAuthBootstrapHandlers } from "./handlers-auth-bootstrap.ts";
import { makeDeviceHandlers } from "./handlers-devices.ts";
import { makePairingHandlers } from "./handlers-pairing.ts";
import type { ConnectDeps } from "./router.ts";

type AuthMethods =
  | "authCoordIdentity"
  | "authDashboardAccess"
  | "authMintBootstrap"
  | "authRedeemWorker"
  | "authRedeemBrowser"
  | "authLogout"
  | "pairCreate"
  | "pairPoll"
  | "pairList"
  | "pairApprove"
  | "pairDeny"
  | "devicesList"
  | "devicesRevoke"
  | "devicesRotateCurrent";

export function makeAuthHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AuthMethods> {
  return {
    ...makeAuthBootstrapHandlers(deps),
    ...makePairingHandlers(deps),
    ...makeDeviceHandlers(deps),
  };
}
