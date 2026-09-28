import { z } from "zod";
const AgentRuntimeState = z.enum(["working", "blocked", "idle"]);
const SessionId = z.string().regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i).brand<"SessionId">();
const CapabilitySchema = z.string().regex(/^[a-f0-9]{64}$/);
const REPORTED_STATE_UNKNOWN_REASON = 'state "unknown" is not reported; send active: false to withdraw the status';
const Reported = z.string().superRefine((value, context) => {
  if (AgentRuntimeState.safeParse(value).success) return;
  context.addIssue({ code: z.ZodIssueCode.custom, message: value === "unknown" ? REPORTED_STATE_UNKNOWN_REASON : `state must be one of ${AgentRuntimeState.options.join(", ")}` });
}).pipe(AgentRuntimeState);
const Status = z.object({ version: z.literal(1), method: z.literal("agent.report"), capability: CapabilitySchema,
  params: z.object({ session_id: SessionId, state: Reported, message: z.string().max(512).optional(), active: z.boolean() }).strict() }).strict();
const RefValue = z.object({ kind: z.enum(["id", "path"]), value: z.string() }).strict().transform((value, context) => {
  const ok = value.value.length > 0 && !/[\u0000-\u001f\u007f-\u009f]/.test(value.value);
  if (!ok) { context.addIssue({ code: z.ZodIssueCode.custom, message: "invalid agent conversation reference" }); return z.NEVER; }
  return value;
});
const Reference = z.object({ version: z.literal(1), method: z.literal("agent.reference"), capability: CapabilitySchema,
  params: z.object({ session_id: SessionId, reference: RefValue.nullable() }).strict() }).strict();
const Request = z.discriminatedUnion("method", [Status, Reference]);
const cap = "a".repeat(64), sid = "11111111-1111-4111-8111-111111111111";
const base = { version: 1, method: "agent.report", capability: cap, params: { session_id: sid, state: "working", active: true } };
const ref = { version: 1, method: "agent.reference", capability: cap, params: { session_id: sid, reference: { kind: "id", value: "x" } } };
const clone = (v: any) => JSON.parse(JSON.stringify(v));
const cases: any[] = [];
const add = (v: any) => cases.push(v);
add(5); add("s"); add(null); add([]); add({});
add({ ...base, method: "agent.nope" }); add({ ...base, method: 7 });
add({ ...base, version: 2 }); add({ ...base, version: "1" }); add({ ...base, version: 1.0 }); { const c = clone(base); delete c.version; add(c); }
add({ ...base, capability: "A".repeat(64) }); add({ ...base, capability: 5 }); { const c = clone(base); delete c.capability; add(c); }
add({ ...base, params: 5 }); add({ ...base, params: null }); { const c = clone(base); delete c.params; add(c); }
for (const [k, v] of [["session_id", "nope"], ["session_id", 5], ["state", "unknown"], ["state", "done"], ["state", 3], ["message", "x".repeat(513)], ["message", null], ["message", "é".repeat(512)], ["message", "😀".repeat(257)], ["active", "yes"], ["pid", 7]] as const) {
  const c = clone(base); c.params[k] = v; add(c);
}
{ const c = clone(base); delete c.params.active; add(c); }
{ const c = clone(base); delete c.params.state; add(c); }
{ const c = clone(base); c.params.state = "unknown"; c.extra = 1; add(c); }
{ const c = clone(base); c.extra = 1; add(c); }
{ const c = clone(base); c.version = 2; c.params.state = 5; add(c); }
add(ref); add({ ...ref, params: { session_id: sid, reference: null } });
for (const r of [undefined, 5, { kind: "x", value: "v" }, { kind: 5, value: "v" }, { value: "v" }, { kind: "id" }, { kind: "id", value: 5 }, { kind: "id", value: "" }, { kind: "id", value: "a\u0000b" }, { kind: "id", value: "v", extra: 1 }]) {
  const c = clone(ref); c.params.reference = r; if (r === undefined) delete c.params.reference; add(c);
}
const out = cases.map((value) => {
  const line = JSON.stringify(value);
  const parsed = Request.safeParse(value);
  return { line, ok: parsed.success, detail: parsed.success ? null : parsed.error.issues[0]?.message ?? null };
});
console.log(JSON.stringify(out, null, 1));
