// The coordinator-link callbacks that carry a relayed Mecatl exchange to this
// machine's daemon. Spread into the dependency object built by
// coord-link-deps.ts, which owns the forward ref these read. Kept separate so
// the relay's transport seam does not grow the link's dependency module.

import type { CoordLinkDeps } from "../transport/coord-link-types.ts";
import type { CoordLinkRefs } from "../coord-link-deps.ts";

export type { MecatlRelay } from "./relay.ts";

/** A machine with no Mecatl runtime binds no relay. The coordinator's own
 *  request deadline turns that silence into the pane's unavailable state, so
 *  there is no second refusal protocol here. */
export function mecatlRelayHandlers(
  refs: Pick<CoordLinkRefs, "mecatlRelay">,
): Pick<CoordLinkDeps, "onMecatlRelayRequest" | "onMecatlRelayCancel"> {
  return {
    onMecatlRelayRequest: (request) => refs.mecatlRelay?.handleRequest(request),
    onMecatlRelayCancel: (request) => refs.mecatlRelay?.handleCancel(request.requestId),
  };
}
