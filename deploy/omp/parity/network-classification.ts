// Classifies network-guard trace rows. OMP's model registry probes implicit
// local engines and public model catalogs at startup; the guard denies those
// probes like any other unowned destination, but they are catalog discovery,
// not agent traffic escaping the owned provider fixture.
const modelDiscoveryFrame = /\/packages\/(?:catalog\/src\/(?:model-manager|discovery\/|provider-models\/)|coding-agent\/src\/config\/(?:model-discovery|model-registry|model-provider-discovery))/;
const guardFrame = /\/deploy\/omp\/parity\/network-guard\.ts/;

export const deniedDestinationPrefix = "Unexpected OMP parity network destination:";

/** The first frame outside the guard decides: a denied request whose caller is OMP's model discovery. */
export function isModelDiscoveryStack(stack: string): boolean {
  const caller = stack.split("\n").slice(1).find(frame => !guardFrame.test(frame));
  return caller !== undefined && modelDiscoveryFrame.test(caller);
}

/** A denied destination that counts against the zero-escape invariant. */
export function isEscapingRequest(row: { error?: unknown; kind?: unknown }): boolean {
  return typeof row.error === "string" && row.kind !== "model-discovery";
}

/** A denied startup model-catalog probe. */
export function isDeniedModelDiscovery(row: { error?: unknown; kind?: unknown }): boolean {
  return typeof row.error === "string" && row.kind === "model-discovery";
}
