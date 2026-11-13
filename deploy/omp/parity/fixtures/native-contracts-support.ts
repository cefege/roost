import type { NativeAssertion, NativeContract, WireReference } from "../coverage-native";

export const pickerNode = '[data-omp-node$="^picker"]';
export const overlayScope = '[data-omp-node="layer.ov-l"]';

export const fileState = (path: string, pointer: string, expected: unknown, name = `canonical controller ${pointer}`): NativeAssertion =>
  ({ name, browser: { type: "file", path, pointer }, expected });
export const effectState = (channel: string, pointer: string, expected: unknown, name = `canonical effect ${channel}${pointer}`): NativeAssertion =>
  ({ name, browser: { type: "effect", channel, pointer }, expected });
export const domText = (selector: string, expected: string, name = `actual HTML text ${selector}`): NativeAssertion =>
  ({ name, browser: { type: "dom", selector, read: "text" }, expected });

export function eventContract(ev: string, assertions: NativeAssertion[], match: Record<string, unknown>, selector?: string): NativeContract {
  return { entry: `event:${ev}`, request: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": ev, ...match }, ...(selector ? { target: { selector } } : {}) }, assertions };
}

export function eventProperty(ev: string, field: string, match: Record<string, unknown>, value: unknown, assertions: NativeAssertion[], pointer = `/${field}`): NativeContract {
  return { entry: `property:TspEvent.${field}`, request: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": ev, ...match }, pointer }, value, assertions };
}

export function frameContract(entry: string, assertions: NativeAssertion[], witness: "request" | "response" = "request", pointer?: string, match?: Record<string, unknown>, value?: unknown): NativeContract {
  const request: WireReference = { verb: "f", direction: "out", occurrence: "last", ...(match ? { match } : {}), ...(witness === "request" && pointer ? { pointer } : {}) };
  return {
    entry, witness,
    request,
    response: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "ack" }, ...(witness === "response" && pointer ? { pointer } : {}) },
    correlate: [{ request: "/sf", response: "/sf" }, { request: "/s", response: "/s" }],
    ...(value === undefined ? {} : { value }),
    assertions,
  };
}
