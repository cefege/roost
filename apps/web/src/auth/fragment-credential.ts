import type { CoordinatorRelocationFragment } from "./coordinator-relocation.ts";

export type FragmentCredential =
  | { kind: "none" }
  | { kind: "invalid" }
  | { kind: "pair"; token: string }
  | ({ kind: "relocation" } & CoordinatorRelocationFragment);

export function parseFragmentCredential(hash: string): FragmentCredential {
  const params = new URLSearchParams(hash.startsWith("#") ? hash.slice(1) : hash);
  const pairs = params.getAll("pair");
  const moves = params.getAll("move");
  const handoffs = params.getAll("handoff");
  const hasPair = params.has("pair");
  const hasMove = params.has("move");
  const hasHandoff = params.has("handoff");
  if (!hasPair && !hasMove && !hasHandoff) return { kind: "none" };
  if (
    hasPair
    && !hasMove
    && !hasHandoff
    && pairs.length === 1
    && Boolean(pairs[0])
  ) return { kind: "pair", token: pairs[0]! };
  if (
    !hasPair
    && hasMove
    && hasHandoff
    && moves.length === 1
    && handoffs.length === 1
    && Boolean(moves[0])
    && Boolean(handoffs[0])
  ) {
    return { kind: "relocation", token: moves[0]!, handoffId: handoffs[0]! };
  }
  return { kind: "invalid" };
}

export function credentialFreeUrl(pathname: string, search: string, hash: string): string {
  const params = new URLSearchParams(hash.startsWith("#") ? hash.slice(1) : hash);
  params.delete("pair");
  params.delete("move");
  params.delete("handoff");
  const fragment = params.toString();
  return `${pathname}${search}${fragment ? `#${fragment}` : ""}`;
}
