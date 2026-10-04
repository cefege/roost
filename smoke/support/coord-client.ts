// Connect clients for the coordinator the harness launched: an authenticated
// one that presents a freshly minted bearer on every request, and the
// unauthenticated one the pairing ceremony starts from. Called by the terminal
// stack and the pairing specs; depends on the generated `CoordinatorService`.

import { createClient, type Client } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-node";
import { CoordinatorService } from "../gen/roost/v1/coordinator_pb.ts";

/** A coordinator client every call of which carries the harness's bearer. */
export type AuthorizedApiClient = Client<typeof CoordinatorService>;

export function createCoordClient(options: { baseUrl: string; getJwt: () => Promise<string> }): AuthorizedApiClient {
  return createClient(CoordinatorService, createConnectTransport({
    baseUrl: options.baseUrl,
    httpVersion: "1.1",
    useBinaryFormat: false,
    interceptors: [
      (next) => async (request) => {
        request.header.set("authorization", `Bearer ${await options.getJwt()}`);
        return next(request);
      },
    ],
  }));
}

export function createUnauthenticatedCoordClient(baseUrl: string): Client<typeof CoordinatorService> {
  return createClient(CoordinatorService, createConnectTransport({
    baseUrl,
    httpVersion: "1.1",
    useBinaryFormat: false,
  }));
}
