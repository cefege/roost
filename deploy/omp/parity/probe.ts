import { appendFileSync, existsSync, lstatSync, mkdirSync, realpathSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import type { AgentSession } from "@omp-source/packages/coding-agent/src/session/agent-session";
import type { ToolSession } from "@omp-source/packages/coding-agent/src/tools";
import type { InteractiveMode } from "@omp-source/packages/coding-agent/src/modes/interactive-mode";
import type { InteractiveModeContext } from "@omp-source/packages/coding-agent/src/modes/types";
import type { BtwController } from "@omp-source/packages/coding-agent/src/modes/controllers/btw-controller";
import type { SessionRecorder } from "@omp-source/packages/coding-agent/src/stream/recording";

export interface ProbeLocation {
  source: string;
  line: number;
  endLine?: number;
  ownerLine?: number;
  ownerEndLine?: number;
  component: string;
  method: string;
  expression?: string;
}

export interface ProbeVisit extends ProbeLocation {
  type: "visit";
  pid: number;
  sequence: number;
  generation?: string;
  adapter?: "terminal" | "browser";
  receiverClass?: string;
  receiverComponent?: string;
  receiverNativeId?: string;
  arguments?: unknown[];
}

export interface WireObservation {
  type: "wire";
  direction: "out" | "in";
  verb: string;
  body: unknown;
  source: string;
  line: number;
  pid: number;
  sequence: number;
  generation?: string;
  adapter?: "terminal" | "browser";
  /** "relay": synthesized by the ACP relay, never sent by the browser. */
  origin?: "relay";
}

export interface ProbeDocument extends Omit<WireObservation, "type" | "direction" | "verb" | "body"> {
  type: "document";
  surface: string;
  surfaceSequence: number;
  document: unknown;
  backend: string;
  mirrorOrigin: ProbeLocation;
  mirrorRequested?: boolean;
}

export interface InteractiveState { paused: boolean; loopEnabled: boolean; todoExpanded: boolean }
export interface RecordingState { active: boolean; lastPaintAt: number; path: string }
export interface ProbeOptions { artifactRoot?: string; effectsPath?: string; tracePath?: string; adapter?: "terminal" | "browser" }

function within(root: string, file: string): boolean {
  const path = relative(root, file);
  return path !== ".." && !path.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) && !isAbsolute(path);
}

/** Owns references and append-only observations in one isolated fixture process. */
export class ParityProbe {
  private readonly artifactRoot: string;
  private readonly effectsPath?: string;
  private readonly tracePath?: string;
  private readonly checkedPaths = new Set<string>();
  private readonly componentClasses = new WeakMap<Function, string>();
  private readonly componentObjects = new WeakMap<object, string>();
  private readonly generation = randomUUID();
  private readonly adapter?: string;
  private session?: AgentSession;
  private readonly toolSessions = new WeakMap<AgentSession, ToolSession>();
  private readonly conditionStates = new Map<string, boolean>();
  private mode?: InteractiveMode;
  private btw?: BtwController;
  private recorder?: SessionRecorder;
  private readInteractive?: () => InteractiveState;
  private readRecording?: () => RecordingState;
  private sequence = 0;
  private pluginReloads = 0;
  private committedForcedTool?: string;
  private readonly mirroredBackends = new WeakMap<object, { id: string; origin: ProbeLocation; requested?: boolean }>();

  constructor(options: ProbeOptions = {}) {
    this.artifactRoot = resolve(options.artifactRoot ?? join(import.meta.dir, "../../../target/omp-parity"));
    this.effectsPath = options.effectsPath ?? process.env.OMP_PARITY_EFFECTS;
    this.tracePath = options.tracePath ?? process.env.OMP_PARITY_PROBE_TRACE;
    this.adapter = options.adapter ?? process.env.OMP_PARITY_ADAPTER;
  }

  requireInstrumentation(): void {
    this.checkedFile(this.tracePath, "OMP_PARITY_PROBE_TRACE");
    this.checkedFile(this.effectsPath, "OMP_PARITY_EFFECTS");
    if (this.adapter !== "terminal" && this.adapter !== "browser") throw new Error("OMP_PARITY_ADAPTER must identify the actual terminal or browser CLI child.");
  }

  requireSession(): AgentSession {
    if (!this.session) throw new Error("The actual SDK AgentSession has not completed construction.");
    return this.session;
  }

  requireToolSession(): ToolSession {
    const session = this.toolSessions.get(this.requireSession());
    if (!session) throw new Error("The actual SDK AgentSession has no configured ToolSession.");
    return session;
  }

  observeToolSession(session: AgentSession, toolSession: ToolSession | undefined, origin: ProbeLocation): void {
    if (toolSession) this.toolSessions.set(session, toolSession);
    this.trace({ type: "tool-session", configured: toolSession !== undefined, ...origin });
  }

  conditionOutcome(setting: string): boolean {
    if (!this.conditionStates.has(setting)) throw new Error(`Canonical condition has not evaluated ${setting}.`);
    return this.conditionStates.get(setting)!;
  }

  observeCondition<Value>(value: Value, setting: string, origin: ProbeLocation): Value {
    if (typeof value === "boolean") this.conditionStates.set(setting, value);
    this.trace({ type: "condition", setting, outcome: value, ...origin });
    return value;
  }

  requireInteractiveMode(): InteractiveMode {
    if (!this.mode) throw new Error("The actual InteractiveMode has not completed construction.");
    return this.mode;
  }

  requireInteractiveContext(): InteractiveModeContext { return this.requireInteractiveMode(); }

  requireBtwController(): BtwController {
    if (!this.btw) throw new Error("The actual BtwController has not completed construction.");
    return this.btw;
  }

  requireRecorder(): SessionRecorder {
    if (!this.recorder) throw new Error("The actual SessionRecorder has not completed construction.");
    return this.recorder;
  }

  interactiveState(): InteractiveState {
    if (!this.readInteractive) throw new Error("Actual interactive state is not yet observable.");
    return this.readInteractive();
  }

  recordingState(): RecordingState {
    if (!this.readRecording) throw new Error("No actual SessionRecorder has been observed.");
    return this.readRecording();
  }

  get pluginReloadsCompleted(): number { return this.pluginReloads; }

  forcedToolChoice(): string | undefined {
    return this.requireSession().toolChoiceQueue.inspect().includes("user-force") ? this.committedForcedTool : undefined;
  }

  completedForcedToolChoice(session: AgentSession, toolName: string, origin: ProbeLocation): void {
    if (session === this.session) this.committedForcedTool = toolName;
    this.trace({ type: "transition", transition: "forced-tool-committed", toolName, ...origin });
  }

  record(channel: string, value: unknown): void {
    if (typeof channel !== "string" || channel.length === 0) throw new Error("Parity effect channel must not be empty.");
    this.append(this.checkedFile(this.effectsPath, "OMP_PARITY_EFFECTS"), { channel, value });
  }

  observeSession(session: AgentSession, origin: ProbeLocation): void {
    this.session ??= session;
    this.trace({ type: "session", ...origin });
  }

  observeInteractiveMode(mode: InteractiveMode, state: () => InteractiveState, origin: ProbeLocation): void {
    this.mode = mode;
    this.readInteractive = state;
    this.trace({ type: "controller", ...origin });
  }

  observeBtwController(controller: BtwController, origin: ProbeLocation): void {
    this.btw = controller;
    this.trace({ type: "controller", ...origin });
  }

  observeRecorder(recorder: SessionRecorder, state: () => RecordingState, origin: ProbeLocation): void {
    this.recorder = recorder;
    this.readRecording = state;
    this.trace({ type: "controller", ...origin });
  }

  completedPluginReload(origin: ProbeLocation): void {
    this.pluginReloads++;
    this.trace({ type: "transition", transition: "plugin-reload-completed", count: this.pluginReloads, ...origin });
  }

  completedSettingWrite(setting: string, layer: string, origin: ProbeLocation): void {
    this.trace({ type: "visit", ...origin, arguments: [{ setting, layer }] });
  }

  returnedValue<Value>(value: Value, origin: ProbeLocation, receiver?: unknown): Value {
    this.trace({ type: "visit", ...origin, ...this.receiverMetadata(receiver), arguments: [{ result: value === null ? "null" : value === undefined ? "undefined" : "value" }] });
    return value;
  }

  observedValue<Value>(value: Value, origin: ProbeLocation, receiver?: unknown): Value {
    this.trace({ type: "visit", ...origin, ...this.receiverMetadata(receiver) });
    return value;
  }

  observeComponentClass(constructor: Function, identity: string): void {
    this.componentClasses.set(constructor, identity);
  }

  observeComponentObject<Value extends object>(value: Value, identity: string): Value {
    this.componentObjects.set(value, identity);
    return value;
  }

  visit(origin: ProbeLocation, receiver?: unknown, firstArgument?: unknown): void {
    let parsedCommand: Record<string, string> | undefined;
    if (firstArgument !== null && typeof firstArgument === "object") {
      const fields = Object.getOwnPropertyDescriptors(firstArgument);
      if (["name", "text", "args"].every(key => typeof fields[key]?.value === "string")) {
        parsedCommand = Object.fromEntries(["name", "text", "args"].map(key => [key, fields[key]!.value as string]));
      }
    }
    this.trace({ type: "visit", ...origin, ...this.receiverMetadata(receiver), ...(parsedCommand ? { arguments: [parsedCommand] } : {}) });
  }

  featureArgument<Value>(value: Value, origin: ProbeLocation): Value {
    this.trace({ type: "visit", ...origin, method: "feature", arguments: [value] });
    return value;
  }

  enableNativeMirror(backend: object, requested: boolean | undefined, origin: ProbeLocation): true {
    this.mirroredBackends.set(backend, { id: randomUUID(), origin, requested });
    this.trace({ type: "mirror", enabled: true, requested, ...origin });
    return true;
  }

  nativeDocument(backend: object, surface: string, surfaceSequence: number, document: unknown, origin: ProbeLocation): void {
    const mirror = this.mirroredBackends.get(backend);
    if (!mirror) throw new Error("Native document observation requires the actual debug-mirror construction.");
    if (document === undefined) return;
    this.trace({ type: "document", surface, surfaceSequence, document, backend: mirror.id, mirrorOrigin: mirror.origin,
      mirrorRequested: mirror.requested, source: origin.source, line: origin.line });
  }

  completedNativeEvent<Value>(value: Value, event: unknown, origin: ProbeLocation): Value {
    this.trace({ type: "native-event", event, ...origin });
    return value;
  }

  completedNativeReference<Value>(value: Value, semantic: unknown, wireId: string, origin: ProbeLocation): Value {
    const selectedAfter = value !== null && typeof value === "object" ? Object.getOwnPropertyDescriptor(value, "selected")?.value : undefined;
    this.trace({ type: "reference", semantic, wireId, selectedAfter, ...origin });
    return value;
  }

  private relayBody: string | undefined;

  wire(direction: "out" | "in", message: { verb: string; body: unknown }, origin: ProbeLocation): void {
    // The terminal encodes only o/f/x/q/b/t (NativeTransportMessage.verb); the sole encodeTspJson call with r or e is the
    // ACP relay synthesizing a reply, so such an "out" encode is a relay-origin inbound frame.
    if (direction === "out" && (message.verb === "r" || message.verb === "e")) {
      this.relayBody = JSON.stringify(message.body);
      this.trace({ type: "wire", direction: "in", verb: message.verb, body: message.body, source: origin.source, line: origin.line, origin: "relay" });
      return;
    }
    let relayed = false;
    if (direction === "in" && (message.verb === "r" || message.verb === "e")) {
      relayed = this.relayBody !== undefined && JSON.stringify(message.body) === this.relayBody;
      if (!relayed) this.relayBody = undefined;
    }
    this.trace({ type: "wire", direction, verb: message.verb, body: message.body, source: origin.source, line: origin.line, ...(relayed ? { origin: "relay" as const } : {}) });
  }

  decodedMessage<Value>(message: Value, origin: ProbeLocation): Value {
    if (message !== null && typeof message === "object" && "verb" in message) {
      if (message.verb === "r" && "reply" in message) this.wire("in", { verb: "r", body: message.reply }, origin);
      if (message.verb === "e" && "event" in message) this.wire("in", { verb: "e", body: message.event }, origin);
    }
    return message;
  }

  private receiverMetadata(receiver: unknown): Pick<ProbeVisit, "receiverClass" | "receiverComponent" | "receiverNativeId"> {
    if (receiver === null || typeof receiver !== "object" && typeof receiver !== "function") return {};
    const nativeId = this.reconciledId(receiver);
    const direct = this.componentObjects.get(receiver);
    if (direct) return { receiverComponent: direct, ...nativeId };
    const prototype = Object.getPrototypeOf(receiver);
    const constructor = typeof receiver === "function" ? receiver : prototype && Object.getOwnPropertyDescriptor(prototype, "constructor")?.value;
    if (typeof constructor !== "function") return nativeId;
    const name = Object.getOwnPropertyDescriptor(constructor, "name")?.value;
    const identity = this.componentClasses.get(constructor);
    return { ...(typeof name === "string" ? { receiverClass: name } : {}), ...(identity ? { receiverComponent: identity } : {}), ...nativeId };
  }

  /** The id the native reconciler tagged on a component object (`Symbol("native.id")`), which prefixes every node id it owns. */
  private reconciledId(receiver: object): Pick<ProbeVisit, "receiverNativeId"> {
    for (const symbol of Object.getOwnPropertySymbols(receiver)) {
      if (symbol.description !== "native.id") continue;
      const value = Object.getOwnPropertyDescriptor(receiver, symbol)?.value;
      if (typeof value === "string") return { receiverNativeId: value };
    }
    return {};
  }

  private checkedFile(file: string | undefined, environment: string): string {
    if (!file) throw new Error(`${environment} is required for actual parity observations.`);
    const absolute = resolve(file);
    if (!within(this.artifactRoot, absolute) || absolute === this.artifactRoot) {
      throw new Error(`${environment} must be an owned file under ${this.artifactRoot}.`);
    }
    if (!this.checkedPaths.has(absolute)) {
      mkdirSync(dirname(absolute), { recursive: true });
      if (!within(realpathSync(this.artifactRoot), realpathSync(dirname(absolute)))
        || existsSync(absolute) && (!lstatSync(absolute).isFile() || lstatSync(absolute).isSymbolicLink())) {
        throw new Error(`${environment} cannot point outside the owned artifact directory.`);
      }
      this.checkedPaths.add(absolute);
    }
    return absolute;
  }

  private trace(row: Record<string, unknown>): void {
    this.append(this.checkedFile(this.tracePath, "OMP_PARITY_PROBE_TRACE"), { ...row, pid: process.pid, generation: this.generation, adapter: this.adapter, sequence: ++this.sequence });
  }

  private append(file: string, row: Record<string, unknown>): void {
    const serialized = JSON.stringify(row);
    appendFileSync(file, `${serialized}\n`);
  }
}

// OMP re-evaluates an extension's module graph under a cache-busting specifier, so a module-level
// instance would split the CLI's observations (session binding, trace sequence) from the extension's.
const probeKey = Symbol.for("roost.omp-parity.probe");
const registry = globalThis as typeof globalThis & { [probeKey]?: ParityProbe };
export const probe: ParityProbe = registry[probeKey] ??= new ParityProbe();
