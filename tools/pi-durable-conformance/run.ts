import { DatabaseSync } from "node:sqlite";
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { BACKGROUND_CONTEXT } from "../../target/durable-integration.RMAHq5/pi/packages/chord/src/context/index.ts";
import { createModels } from "../../target/durable-integration.RMAHq5/pi/packages/ai/src/models.ts";
import { fauxAssistantMessage, fauxProvider, fauxText, fauxToolCall, type FauxResponseStep } from "../../target/durable-integration.RMAHq5/pi/packages/ai/src/providers/faux.ts";
import { defineDoc, defineTask, defineTool, defineExtension, Harness, InboxDoc, LiveDoc, MemoryStorage, createRegistry } from "../../target/durable-integration.RMAHq5/pi/packages/durable/src/index.ts";
import { Type } from "../../target/durable-integration.RMAHq5/pi/node_modules/typebox/build/index.mjs";
import { openNodeSqliteStorage } from "../../target/durable-integration.RMAHq5/pi/packages/durable/src/storage/sqlite/node.ts";
import { once } from "node:events";
import { completed, deferred, eventually } from "../../target/durable-integration.RMAHq5/pi/packages/durable/test/task-support.ts";
import { allEntries, chatSetup, openChat, waitFor } from "../../target/durable-integration.RMAHq5/pi/packages/durable/test/chat-support.ts";

const here = resolve(fileURLToPath(new URL(".", import.meta.url)));
const root = resolve(here, "../..");
const fixtureDirectory = resolve(root, "protocol/conformance/pi-durable");
const upstream = resolve(root, "target/durable-integration.RMAHq5/pi");
const pinnedCommit = "d5b3de22d0e24a3e9c38a9410d3329b099717283";
const actualCommit = execFileSync("git", ["-C", upstream, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
if (actualCommit !== pinnedCommit) throw new Error(`Expected Pi source ${pinnedCommit}, found ${actualCommit}`);
const sourceChanges = execFileSync("git", [
	"-C", upstream, "status", "--porcelain", "--",
	"package.json", "package-lock.json", "packages/ai", "packages/chord", "packages/durable",
], { encoding: "utf8" }).trim();
if (sourceChanges !== "") throw new Error(`Pinned Pi oracle source has local modifications:\n${sourceChanges}`);
const context = BACKGROUND_CONTEXT;

function canonical(value: unknown): string {
	return `${JSON.stringify(value, null, 2)}\n`;
}


function normalize(value: unknown): unknown {
	if (Array.isArray(value)) return value.map((item) => normalize(item));
	if (typeof value === "string" && /^faux:\d+:[a-z0-9]+$/i.test(value)) return "faux:<generated>";
	if (value === null || typeof value !== "object") return value;
	const record = value as Record<string, unknown>;
	return Object.fromEntries(
		Object.entries(record)
			.filter(([key]) => !["timestamp", "durationMs", "startedAt", "endedAt"].includes(key))
			.map(([key, item]) => [key, normalize(item)]),
	);
}

function digest(value: unknown): string {
	return createHash("sha256").update(canonical(value)).digest("hex");
}

async function plainAndDeduplicatedTurn() {
	const setup = chatSetup();
	const { faux } = setup;
	const requests: unknown[] = [];
	faux.setResponses([async (request) => {
		requests.push(request.messages);
		return fauxAssistantMessage([fauxText("oracle answer")], { usage: { input: 17, output: 3, cacheRead: 0, cacheWrite: 0, totalTokens: 20 } });
	}]);
	const { harness, root: conversation } = await openChat(new MemoryStorage(), setup);
	const first = await conversation.submit({ type: "input", content: "hello", requestId: "request-1" }, context);
	const duplicate = await conversation.submit({ type: "input", content: "hello", requestId: "request-1" }, context);
	await first.wait(context);
	assert.equal(first.id, duplicate.id, "duplicate request IDs must reuse the submission");
	assert.equal(faux.state.callCount, 1, "deduplicated input must invoke the provider once");
	const entries = await conversation.entries({}, 100, undefined, context);
	assert.match(JSON.stringify(entries.items), /oracle answer/, "the assistant response must be committed");
	const trace = {
		name: "plain-dedup-context-usage",
		requestIds: [first.id, duplicate.id],
		requestCount: faux.state.callCount,
		requests,
		entries: [...entries.items].reverse(),
		usage: await harness.usage(context),
	};
	await harness.close(context);
	return normalize(trace);
}

async function toolRound() {
	const Parameters = Type.Object({ value: Type.String() });
	const tool = defineTool({
		name: "echo",
		description: "Return the supplied value",
		parameters: Parameters,
		structuredOutputSchema: Type.Object({ value: Type.String() }),
		execute: async ({ value }) => ({ output: [{ type: "text" as const, text: `echo:${value}` }], structuredOutput: { value } }),
	});
	const faux = fauxProvider();
	const models = createModels();
	models.setProvider(faux.provider);
	const requests: unknown[] = [];
	faux.setResponses([
		async (request) => {
			requests.push({ messages: request.messages, tools: request.tools });
			return fauxAssistantMessage([fauxToolCall("echo", { value: "blue" }, { id: "call-1" })], { stopReason: "toolUse" });
		},
		async (request) => {
			requests.push({ messages: request.messages, tools: request.tools });
			return fauxAssistantMessage([fauxText("finished")]);
		},
	]);
	const extension = defineExtension({ name: "echo", tools: [tool] });
	const registry = createRegistry();
	registry.install(extension);
	const harness = await Harness.open(new MemoryStorage(), { models, registry, settings: {} }, context);
	const conversation = await harness.root(context, { agent: { model: { provider: "faux", modelId: "faux-1" }, extensions: [extension] } });
	const submission = await conversation.submit({ type: "input", content: "call echo" }, context);
	await submission.wait(context);
	assert.equal(faux.state.callCount, 2, "the tool round must make both provider requests");
	const entries = await conversation.entries({}, 100, undefined, context);
	assert.match(JSON.stringify(entries.items), /echo:blue/, "the registered tool must execute and commit its exact output");
	const trace = { name: "tool-round", entries: [...entries.items].reverse(), requests, requestCount: faux.state.callCount };
	await harness.close(context);
	return normalize(trace);
}

function sqlitePath(): string {
	const directory = mkdtempSync(join(resolve(root, "target/durable-integration.RMAHq5"), "pi-durable-oracle-"));
	return join(directory, "session.sqlite");
}

async function storageCheckpointReopen() {
	const path = sqlitePath();
	const setup = chatSetup();
	setup.faux.setResponses([fauxAssistantMessage([fauxText("committed once")])]);
	let driver = await openChat(await openNodeSqliteStorage(path), setup);
	const submission = await driver.root.submit({ type: "input", content: "persist me", requestId: "reopen-1" }, context);
	await submission.wait(context);
	const beforeClose = await allEntries(driver.root);
	await driver.harness.close(context);
	driver = await openChat(await openNodeSqliteStorage(path), setup);
	const afterOpen = await allEntries(driver.root);
	const duplicate = await driver.root.submit({ type: "input", content: "persist me", requestId: "reopen-1" }, context);
	const duplicateStatus = await duplicate.status(context);
	assert.deepEqual(afterOpen, beforeClose, "reopening SQLite must preserve committed entries");
	assert.equal(duplicateStatus.status, "done", "request deduplication must survive reopening");
	assert.equal(setup.faux.state.callCount, 1, "the duplicate after reopen must not call the provider");
	const trace = {
		name: "sqlite-checkpoint-reopen-dedup",
		beforeClose,
		afterOpen,
		duplicateStatus,
		providerCalls: setup.faux.state.callCount,
	};
	await driver.harness.close(context);
	rmSync(dirname(path), { recursive: true, force: true });
	return normalize(trace);
}

async function forkDocumentPolicies() {
	const AsOf = defineDoc<{ value: string }>({
		kind: "oracle.fork.as-of", version: 1, scope: "conversation", history: "rewindable", fork: "asOf",
		checkpointWhen: () => false, initial: () => ({ value: "initial" }),
	});
	const Current = defineDoc<{ value: string }>({
		kind: "oracle.fork.current", version: 1, scope: "conversation", history: "latest", fork: "current",
		checkpointWhen: () => false, initial: () => ({ value: "initial" }),
	});
	const Initial = defineDoc<{ value: string }>({
		kind: "oracle.fork.initial", version: 1, scope: "conversation", history: "latest", fork: "initial",
		checkpointWhen: () => false, initial: () => ({ value: "initial" }),
	});
	const harness = await Harness.open(new MemoryStorage(), { models: createModels(), registry: createRegistry(), settings: {} }, context);
	const parent = await harness.root(context);
	const point = await parent.commit(async (tx) => {
		const entry = await tx.appendEntry(parent.id, { kind: "oracle.point" });
		(await tx.doc(AsOf, parent.id)).value = "at-fork";
		(await tx.doc(Current, parent.id)).value = "at-fork";
		(await tx.doc(Initial, parent.id)).value = "parent-only";
		return entry.id;
	}, context);
	await parent.commit(async (tx) => {
		(await tx.doc(AsOf, parent.id)).value = "later";
		(await tx.doc(Current, parent.id)).value = "current-at-fork-time";
	}, context);
	const child = await parent.fork(point, { ownership: { kind: "ownerless" } }, context);
	const asOf = await harness.snapshot(AsOf, child.id, context);
	const current = await harness.snapshot(Current, child.id, context);
	const initial = await harness.snapshot(Initial, child.id, context);
	assert.deepEqual(asOf, { value: "at-fork" }, "asOf documents must rewind to the fork entry");
	assert.deepEqual(current, { value: "current-at-fork-time" }, "current documents must copy the parent's current value");
	assert.equal(initial, undefined, "initial documents must not copy parent state");
	const trace = {
		name: "fork-document-policies",
		asOf: await harness.snapshot(AsOf, child.id, context),
		current: await harness.snapshot(Current, child.id, context),
		initial: await harness.snapshot(Initial, child.id, context),
		parentAsOf: await harness.snapshot(AsOf, parent.id, context),
		childHistory: await allEntries(child),
	};
	await harness.close(context);
	return normalize(trace);
}

async function sleepAndWake() {
	let clock = 1_000;
	let afterRuns = 0;
	const Nap = defineTask<null, { phase: "start" } | { phase: "after" }, number>({
		name: "oracle.nap", version: 1, initial: () => ({ phase: "start" }),
		phases: {
			start: async (_task, runtime, ctx) => runtime.sleepUntil(clock + 60_000, { phase: "after" }, ctx),
			after: async (_task, runtime, ctx) => {
				afterRuns++;
				await runtime.commit(() => completed(runtime.now()), ctx);
			},
		},
		abort: async (_task, runtime, ctx) => runtime.commit(() => ({ status: "terminal", outcome: { status: "aborted" } }), ctx),
	});
	const registry = createRegistry();
	registry.install({ name: "oracle.nap", tasks: [Nap] });
	const harness = await Harness.open(new MemoryStorage(), { models: createModels(), registry, now: () => clock }, context);
	const root = await harness.root(context);
	const taskId = await root.commit((tx) => tx.createTask(Nap, null, { ownership: { kind: "conversation" } }), context);
	harness.resume();
	await eventually(async () => (await harness.getTask(taskId, context))?.state.status === "sleeping");
	await eventually(() => !harness.wake().active);
	const asleep = await harness.getTask(taskId, context);
	const earlyWake = harness.wake();
	clock = 61_000;
	const dueWake = harness.wake();
	harness.resume();
	const settled = await harness.waitForTask(taskId, context);
	assert.equal(asleep?.state.status, "sleeping", "the task must persist a sleeping state");
	assert.deepEqual(asleep.state.checkpoint, { phase: "after" }, "sleep must persist the resume checkpoint");
	assert.equal(earlyWake.active, false, "a wake before the due time must not resume the task");
	assert.equal(dueWake.active, true, "a due wake must resume the task");
	assert.equal(settled.state.outcome.status, "completed", "the resumed phase must complete");
	assert.equal(afterRuns, 1, "the resumed phase must run exactly once");
	const trace = { name: "durable-sleep-wake", asleep: asleep?.state, earlyWake, dueWake, settled: settled.state, afterRuns };
	await harness.close(context);
	return normalize(trace);
}
async function unsafeProcessInterruption() {
	const path = sqlitePath();
	const childDriver = resolve(here, "unsafe-interruption-child.ts");
	const child = spawn(process.execPath, ["--conditions=source", "--experimental-loader", resolve(here, "source-loader.mjs"), "--experimental-strip-types", childDriver, path], { stdio: ["ignore", "pipe", "pipe"] });
	const lines = createInterface({ input: child.stdout });
	const stderr: string[] = [];
	child.stderr.setEncoding("utf8").on("data", (chunk: string) => stderr.push(chunk));
	let submissionId: string | undefined;
	let dispatched = false;
	for await (const line of lines) {
		if (line.startsWith("SUBMISSION:")) submissionId = line.slice("SUBMISSION:".length);
		if (line === "UNSAFE_DISPATCHED") dispatched = true;
		if (submissionId !== undefined && dispatched) break;
	}
	if (submissionId === undefined || !dispatched) throw new Error(`Unsafe child did not reach dispatch; stderr=${stderr.join("")}`);
	const exited = once(child, "exit");
	child.kill("SIGKILL");
	const [exitCode, signal] = await exited as [number | null, NodeJS.Signals | null];
	const setup = chatSetup();
	setup.faux.setResponses([fauxAssistantMessage([fauxText("recovered")])]);
	const executedAfterRestart = { count: 0 };
	const tool = defineTool({
		name: "unsafe", description: "Unsafe work with no automatic replay", parameters: Type.Object({}),
		execute: async () => {
			executedAfterRestart.count++;
			return { output: [] };
		},
	});
	const extension = defineExtension({ name: "unsafe", tools: [tool] });
	setup.registry.install(extension);
	const driver = await openChat(await openNodeSqliteStorage(path), setup);
	const prior = await driver.harness.submission(Number(submissionId), context);
	if (prior === undefined) throw new Error(`Recovered submission ${submissionId} was not found`);
	const settled = await prior.wait(context);
	assert.equal(signal, "SIGKILL", "the unsafe child must be killed after dispatch");
	assert.equal(executedAfterRestart.count, 0, "an unsafe tool must not execute again after restart");
	const trace = {
		name: "unsafe-tool-sigkill-recovery",
		processExit: { code: exitCode, signal },
		submission: settled,
		entries: await allEntries(driver.root),
		reexecutedUnsafeTool: executedAfterRestart.count,
		providerCallsAfterRestart: setup.faux.state.callCount,
	};
	await driver.harness.close(context);
	rmSync(dirname(path), { recursive: true, force: true });
	return normalize(trace);
}


async function ownedTaskAbort() {
	const gate = deferred<void>();
	const ownerAbortRelease = Promise.withResolvers<void>();
	const started = new Set<string>();
	const bothStarted = Promise.withResolvers<void>();
	const Held = defineTask<{ name: string }, { phase: "wait" }, null>({
		name: "oracle.held",
		version: 1,
		initial: () => ({ phase: "wait" }),
		phases: {
			wait: async (task, runtime) => {
				started.add(task.input.name);
				if (started.size === 2) bothStarted.resolve();
				await Promise.race([gate.promise, new Promise<void>((_resolve, reject) => runtime.signal.addEventListener("abort", () => reject(runtime.signal.reason), { once: true }))]);
			},
		},
		abort: async (task, runtime, ctx) => {
			if (task.input.name === "owner") await ownerAbortRelease.promise;
			return runtime.commit(() => ({ status: "terminal", outcome: { status: "aborted" } }), ctx);
		},
	});
	const registry = createRegistry();
	registry.install({ name: "oracle.held", tasks: [Held] });
	const harness = await Harness.open(new MemoryStorage(), { models: createModels(), registry }, context);
	const root = await harness.root(context);
	const ids = await root.commit(async (tx) => {
		const owner = await tx.createTask(Held, { name: "owner" }, { ownership: { kind: "conversation" } });
		const child = await tx.createTask(Held, { name: "child" }, { ownership: { kind: "task", taskId: owner } });
		return { owner, child };
	}, context);
	harness.resume();
	await bothStarted.promise;
	const abortResult = await harness.abortTask(ids.owner, context);
	await waitFor(async () => {
		const child = await harness.getTask(ids.child, context);
		return child?.state.status === "terminal" && child.state.outcome.status === "aborted";
	});
	const owner = await harness.getTask(ids.owner, context);
	const child = await harness.getTask(ids.child, context);
	assert.equal(abortResult, "marked", "aborting an owner must persist the abort request");
	assert.equal(owner.abortRequested, true, "the owner must carry its abort marker");
	assert.equal(child.owner, owner.id, "the child must belong to the owner task");
	assert.equal(child.state.outcome.status, "aborted", "the owned child must settle as aborted");
	const trace = { name: "generic-task-ownership-abort", abortResult, owner, child, started: [...started].sort() };
	ownerAbortRelease.resolve();
	await waitFor(async () => {
		const owner = await harness.getTask(ids.owner, context);
		return owner?.state.status === "terminal" && owner.state.outcome.status === "aborted";
	});
	await harness.close(context);
	return normalize(trace);
}

async function nestedToolIdentityReplayError() {
	const setup = chatSetup();
	const echo = defineTool({
		name: "echo", description: "Echo structured data", parameters: Type.Object({ value: Type.String() }),
		structuredOutputSchema: Type.Object({ value: Type.String() }),
		execute: async ({ value }) => ({
			output: [{ type: "text" as const, text: `echo ${value}` }],
			structuredOutput: { value },
		}),
	});
	const nestedResults: unknown[] = [];
	const identities: unknown[] = [];
	const batch = defineTool({
		name: "batch", description: "Run keyed nested calls", parameters: Type.Object({}),
		execute: async (_args, api, callContext) => {
			identities.push({ taskId: api.taskId, callId: api.callId });
			nestedResults.push(await api.executeTool("echo", { value: "x" }, callContext, { key: "stable" }));
			nestedResults.push(await api.executeTool("echo", { value: "x" }, callContext, { key: "stable" }));
			nestedResults.push(await api.executeTool("missing", {}, callContext, { key: "missing" }));
			return { output: [{ type: "text" as const, text: "batch complete" }] };
		},
	});
	setup.faux.setResponses([
		fauxAssistantMessage([fauxToolCall("batch", {}, { id: "batch-1" })], { stopReason: "toolUse" }),
		fauxAssistantMessage([fauxText("done")]),
	]);
	const extension = defineExtension({ name: "nested", tools: [echo, batch] });
	setup.registry.install(extension);
	const { harness, root: conversation } = await openChat(new MemoryStorage(), setup);
	await conversation.configure({ extensions: [extension] }, context);
	const submission = await conversation.submit({ type: "input", content: "run nested batch" }, context);
	await submission.wait(context);
	assert.equal(nestedResults[0].taskId, nestedResults[1].taskId, "the same key must resolve to the same task");
	assert.deepEqual(nestedResults[0].structuredOutput, { value: "x" }, "echo must produce its structured value");
	assert.equal(nestedResults[0].isError, false, "echo must complete successfully");
	assert.equal(nestedResults[2].isError, true, "missing nested tools must return an error result");
	assert.equal(nestedResults[2].diagnostics[0].code, "tool_unavailable", "missing tool errors must be structured");
	assert.equal(identities.length, 1, "the batch tool must execute once");
	assert.deepEqual(nestedResults[0], nestedResults[1], "the same nested key must replay the same result");
	const trace = {
		name: "nested-tool-identity-replay-structured-error",
		nestedResults,
		identities,
		entries: await allEntries(conversation),
		tasks: (await harness.inspect(context)).tasks,
	};
	await harness.close(context);
	return normalize(trace);
}

function storageHead(path: string): number {
	const database = new DatabaseSync(path);
	try {
		return Number(database.prepare("SELECT next_seq FROM durable_metadata WHERE singleton = 1").get()?.next_seq);
	} finally {
		database.close();
	}
}

async function readOnlyObservation() {
	const path = sqlitePath();
	const setup = chatSetup();
	setup.faux.setResponses([fauxAssistantMessage([fauxText("visible")])]);
	const source = await openChat(await openNodeSqliteStorage(path), setup);
	await (await source.root.submit({ type: "input", content: "observe" }, context)).wait(context);
	const headBeforeObservation = storageHead(path);
	const viewer = await Harness.open(await openNodeSqliteStorage(path), {
		models: setup.models, registry: setup.registry, readOnly: true,
	}, context);
	const root = await viewer.root(context);
	const before = await allEntries(root);
	const refusal = await root.submit({ type: "input", content: "must not write" }, context).then(
		() => ({ accepted: true }),
		(error: unknown) => ({ accepted: false, name: error instanceof Error ? error.name : String(error), message: error instanceof Error ? error.message : String(error) }),
	);
	assert.equal(headBeforeObservation, storageHead(path), "read-only observation and rejected writes must not advance storage");
	assert.equal(refusal.accepted, false, "read-only sessions must reject writes");
	assert.match(JSON.stringify(before), /visible/, "the read-only session must observe committed conversation history");
	const trace = {
		name: "read-only-observation",
		headBeforeObservation,
		headAfterObservation: storageHead(path),
		entries: before,
		live: await viewer.snapshot(LiveDoc, root.id, context),
		wake: viewer.wake(),
		refusal,
	};
	await viewer.close(context);
	rmSync(dirname(path), { recursive: true, force: true });
	return normalize(trace);
}
async function deferredDocumentProgress() {
	const path = sqlitePath();
	const setup = chatSetup({ tokensPerSecond: 10, tokenSize: { min: 4, max: 4 } });
	setup.settings.progress = { persistWithinMs: 1200 };
	setup.faux.setResponses([fauxAssistantMessage([fauxText("partial-progress ".repeat(6))])]);
	const storage = await openNodeSqliteStorage(path);
	const driver = await openChat(storage, setup);
	const reader = await openNodeSqliteStorage(path);
	const submission = await driver.root.submit({ type: "input", content: "stream progress" }, context);
	await waitFor(async () => {
		const live = await driver.harness.snapshot(LiveDoc, driver.root.id, context);
		const message = live?.generation?.message;
		return message !== undefined && message.content.some((item) => item.type === "text" && item.text.length > 20);
	});
	const memoryLive = await driver.harness.snapshot(LiveDoc, driver.root.id, context);
	const address = { kind: LiveDoc.definition.kind, scope: { kind: "conversation" as const, conversationId: driver.root.id } };
	const currentRecord = await reader.findDocument(address, "current", context);
	const storedLive = currentRecord === undefined ? undefined : await reader.document(currentRecord.id, "current", context);
	await submission.wait(context);
	const afterRecord = await reader.findDocument(address, "current", context);
	const storedAfter = afterRecord === undefined ? undefined : await reader.document(afterRecord.id, "current", context);
	const liveText = memoryLive?.generation?.message?.content?.find((item) => item.type === "text")?.text;
	assert.ok(liveText && liveText.length > 20, "live progress must be visible in memory");
	assert.equal(storedLive?.value?.generation?.message, undefined, "deferred progress must remain absent from SQLite before completion");
	assert.deepEqual(storedAfter?.value, {}, "completion must clear transient live state in SQLite");
	const trace = {
		name: "deferred-document-progress",
		memoryLive,
		storedDuringProgress: storedLive?.value,
		storedAfterCompletion: storedAfter,
		entries: await allEntries(driver.root),
	};
	await driver.harness.close(context);
	await reader.close(context);
	rmSync(dirname(path), { recursive: true, force: true });
	return normalize(trace);
}

async function busyModesAndWithdrawal() {
	const setup = chatSetup();
	const firstReached = Promise.withResolvers<void>();
	const firstGate = Promise.withResolvers<void>();
	const requests: unknown[] = [];
	const nextResponse: FauxResponseStep = async (request) => {
		requests.push(request.messages);
		return fauxAssistantMessage([fauxText(`successor-${requests.length - 1}`)]);
	};
	setup.faux.setResponses([
		async (request) => {
			requests.push(request.messages);
			firstReached.resolve();
			await firstGate.promise;
			return fauxAssistantMessage([fauxText("first")]);
		},
		...Array.from({ length: 4 }, () => nextResponse),
	]);
	const { harness, root: conversation } = await openChat(new MemoryStorage(), setup);
	const first = await conversation.submit({ type: "input", content: "start" }, context);
	await firstReached.promise;
	const steer = await conversation.submit({ type: "input", content: "steer", whenBusy: "steer" }, context);
	const followUp = await conversation.submit({ type: "input", content: "follow" }, context);
	const withdrawn = await conversation.submit({ type: "input", content: "withdraw me" }, context);
	const reject = await conversation.submit({ type: "input", content: "reject", whenBusy: "reject" }, context).then(
		() => ({ accepted: true }),
		(error: unknown) => ({ accepted: false, name: error instanceof Error ? error.name : String(error), message: error instanceof Error ? error.message : String(error) }),
	);
	const queuedStatuses = await Promise.all([steer, followUp, withdrawn].map((submission) => submission.status(context)));
	const inboxWhileBusy = (await harness.snapshot(InboxDoc, conversation.id, context))?.items;
	const withdrawal = await withdrawn.abort(context);
	assert.equal(reject.accepted, false, "reject mode must refuse input while busy");
	assert.equal(queuedStatuses[0].status, "queued", "steer input must be durably queued");
	assert.equal(inboxWhileBusy[0].mode, "steer", "steer mode must be persisted in the inbox");
	assert.equal(withdrawal, "aborted", "withdrawal must abort the queued submission");
	firstGate.resolve();
	await followUp.wait(context);
	assert.equal((await followUp.status(context)).status, "done", "follow-up input must eventually complete");
	const trace = {
		name: "busy-steer-followup-reject-withdraw",
		reject,
		queuedStatuses,
		withdrawal,
		inboxWhileBusy,
		inbox: (await harness.snapshot(InboxDoc, conversation.id, context))?.items,
		entries: await allEntries(conversation),
		requests,
	};
	await harness.close(context);
	return normalize(trace);
}

async function scenarioOutputs() {
	const scenarios = [
		plainAndDeduplicatedTurn,
		toolRound,
		busyModesAndWithdrawal,
		storageCheckpointReopen,
		forkDocumentPolicies,
		sleepAndWake,
		unsafeProcessInterruption,
		ownedTaskAbort,
		nestedToolIdentityReplayError,
		readOnlyObservation,
		deferredDocumentProgress,
	] as const;
	const outputs = [];
	for (const scenario of scenarios) {
		outputs.push(await scenario());
	}
	return outputs;
}
const outputs = await scenarioOutputs();
if (process.argv.includes("--write")) {
	await mkdir(fixtureDirectory, { recursive: true });
	for (const output of outputs) {
		const name = (output as { name: string }).name;
		await writeFile(resolve(fixtureDirectory, `${name}.json`), canonical({
			source: { repository: "https://github.com/earendil-works/pi", commit: pinnedCommit, package: "packages/durable" },
			normalization: "timestamp, duration, startedAt, and endedAt fields omitted; generated faux API ids mapped to faux:<generated>; all Durable IDs and commit sequences remain exact",
			trace: output,
			sha256: digest(output),
		}));
	}
	console.log(`Generated ${outputs.length} traces from Pi ${pinnedCommit}.`);
} else {
	const names = await readdir(fixtureDirectory);
	if (names.length !== outputs.length) throw new Error(`Expected ${outputs.length} fixtures, found ${names.length}`);
	for (const output of outputs) {
		const name = (output as { name: string }).name;
		const actual = { source: { repository: "https://github.com/earendil-works/pi", commit: pinnedCommit, package: "packages/durable" }, normalization: "timestamp, duration, startedAt, and endedAt fields omitted; generated faux API ids mapped to faux:<generated>; all Durable IDs and commit sequences remain exact", trace: output, sha256: digest(output) };
		const expected = JSON.parse(await readFile(resolve(fixtureDirectory, `${name}.json`), "utf8"));
		if (canonical(expected) !== canonical(actual)) throw new Error(`Pi Durable oracle mismatch: ${name}`);
	}
	console.log(`Matched ${outputs.length} upstream traces against ${names.length} checked-in fixtures.`);
}
