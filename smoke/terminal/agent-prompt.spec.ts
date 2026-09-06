// Status-fenced prompt delivery through the real coord, worker, keeper, and PTY.
// An attested OMP fixture supplies the fence while foreground PTY readers prove
// accepted bytes, definite zero-write rejection, and independent raw input.

import {
  AgentPromptInputOutcome,
  AgentPromptWaitOutcome,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import type { Page } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import {
  launchIntegratedAgent,
  launchScreenOnlyAgent,
  pollAgentStatus,
  reportAgentStatus,
  reportReplacementAgentStatus,
  waitForAgentStatusRemoval,
} from "./agent-status-fixture.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnSmokeShell,
} from "./terminal-helpers.ts";
import type { TerminalTestStack } from "./stack.ts";

function promptRequest(
  status: AgentStatusView,
  text: string,
  wait?: { states: string[]; timeoutMs: number },
) {
  if (!status.statusEpoch || !status.occupantId) {
    throw new Error("prompt smoke requires a complete observed-status fence");
  }
  return {
    sessionId: status.sessionId,
    expectedStatusEpoch: status.statusEpoch,
    expectedOccupantId: status.occupantId,
    expectedRevision: status.revision,
    text,
    waitStates: wait?.states ?? [],
    ...(wait ? { waitTimeoutMs: wait.timeoutMs } : {}),
  };
}

async function spawnIntegratedAgent(
  page: Page,
  stack: TerminalTestStack,
): Promise<{ sessionId: string; status: AgentStatusView }> {
  const sessionId = (await spawnSmokeShell(page, stack.workerFp)).session_id;
  await navigateToSmokeSession(page, sessionId);
  await launchIntegratedAgent(page, sessionId);
  await pollAgentStatus(
    stack.client,
    sessionId,
    "idle",
    (candidate) => candidate.source === "screen",
  );
  await reportAgentStatus(page, sessionId, "working", 1);
  const status = await pollAgentStatus(
    stack.client,
    sessionId,
    "working",
    (candidate) => candidate.source === "integration" && candidate.promptable,
  );
  return { sessionId, status };
}

async function armTerminalByteCapture(
  page: Page,
  sessionId: string,
  byteLength: number,
  nonce: string,
): Promise<void> {
  const ready = `CAPTURE_READY:${nonce}`;
  const program = [
    "import os,termios,tty",
    "fd=0",
    `target=${byteLength}`,
    "data=b''",
    "old=termios.tcgetattr(fd)",
    "try:",
    "    tty.setraw(fd)",
    `    os.write(1,${JSON.stringify(`\x1b[?2004h\r\n${ready}\r\n`)}.encode())`,
    "    while len(data)<target:",
    "        part=os.read(fd,target-len(data))",
    "        if not part: break",
    "        data+=part",
    "finally:",
    "    termios.tcsetattr(fd,termios.TCSADRAIN,old)",
    `os.write(1,('\\r\\nCAPTURE:${nonce}:'+data.hex()+'\\r\\n').encode())`,
  ].join("\n");
  const encoded = Buffer.from(program, "utf8").toString("base64");
  await inputSmokeTerminal(
    page,
    sessionId,
    `python3 -u -c "import base64;exec(base64.b64decode('${encoded}'))"\r`,
  );
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toContainText(ready, {
    timeout: 30_000,
  });
}

async function expectCapturedBytes(
  page: Page,
  sessionId: string,
  nonce: string,
  bytes: Uint8Array,
): Promise<void> {
  const expected = `CAPTURE:${nonce}:${Buffer.from(bytes).toString("hex")}`;
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toContainText(expected, {
    timeout: 30_000,
  });
}

async function expectRejectedWithoutWrite(
  page: Page,
  client: TerminalTestStack["client"],
  status: AgentStatusView,
  label: string,
  rawCanary: number,
): Promise<void> {
  const nonce = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  await armTerminalByteCapture(page, status.sessionId, 1, nonce);
  const text = `${label}-${nonce}`;
  const response = await client.sessionsPrompt(promptRequest(status, text));
  expect(response.inputOutcome).toBe(AgentPromptInputOutcome.REJECTED);
  expect(response.writtenBytes).toBe(0);
  expect(response.waitOutcome).toBeUndefined();
  expect(response.reason.length).toBeGreaterThan(0);
  expect(response.reason.length).toBeLessThanOrEqual(200);
  expect(response.reason).not.toContain(text);

  const canary = Uint8Array.of(rawCanary);
  const rawResponse = await client.sessionsInput({
    sessionId: status.sessionId,
    data: canary,
  });
  expect(rawResponse.accepted).toBe(true);
  await expectCapturedBytes(page, status.sessionId, nonce, canary);
}

test("accepted agent prompt uses composer encoding while SessionsInput stays raw", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "POSIX agent integration contract");
  test.setTimeout(180_000);

  const { sessionId, status } = await spawnIntegratedAgent(smokePage, stack);
  const promptText = "one\r\ntwo\x1bZ";
  const expectedPromptBytes = new TextEncoder().encode(
    "\x1b[200~one\rtwoZ\x1b[201~\r",
  );
  const promptNonce = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  await armTerminalByteCapture(
    smokePage,
    sessionId,
    expectedPromptBytes.byteLength,
    promptNonce,
  );

  const responsePromise = stack.client.sessionsPrompt(promptRequest(status, promptText, {
    states: ["idle"],
    timeoutMs: 30_000,
  }));
  await expectCapturedBytes(smokePage, sessionId, promptNonce, expectedPromptBytes);
  await reportAgentStatus(smokePage, sessionId, "idle", 2);
  const response = await responsePromise;
  expect(response.inputOutcome).toBe(AgentPromptInputOutcome.ACCEPTED);
  expect(response.writtenBytes).toBe(expectedPromptBytes.byteLength);
  expect(response.waitOutcome).toBe(AgentPromptWaitOutcome.MATCHED);
  expect(response.reason).not.toContain(promptText);

  const rawBytes = new TextEncoder().encode(promptText);
  expect(Buffer.from(rawBytes).equals(Buffer.from(expectedPromptBytes))).toBe(false);
  const rawNonce = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  await armTerminalByteCapture(smokePage, sessionId, rawBytes.byteLength, rawNonce);
  const rawResponse = await stack.client.sessionsInput({ sessionId, data: rawBytes });
  expect(rawResponse.accepted).toBe(true);
  await expectCapturedBytes(smokePage, sessionId, rawNonce, rawBytes);
});

test("blocked, replaced, and screen-only occupants reject before a keeper write", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "POSIX agent integration contract");
  test.setTimeout(180_000);

  const { sessionId } = await spawnIntegratedAgent(smokePage, stack);
  await reportAgentStatus(smokePage, sessionId, "blocked", 2);
  const blocked = await pollAgentStatus(stack.client, sessionId, "blocked");
  await expectRejectedWithoutWrite(smokePage, stack.client, blocked, "blocked", 0x42);

  await reportAgentStatus(smokePage, sessionId, "working", 3);
  const replacedFence = await pollAgentStatus(stack.client, sessionId, "working");
  await launchIntegratedAgent(smokePage, sessionId);
  const replacement = await reportReplacementAgentStatus(
    smokePage,
    stack.client,
    sessionId,
    replacedFence.occupantId!,
  );
  expect(replacement.source).toBe("integration");
  await expectRejectedWithoutWrite(smokePage, stack.client, replacedFence, "replaced", 0x52);

  await reportAgentStatus(smokePage, sessionId, "idle", 2, false);
  await waitForAgentStatusRemoval(stack.client, sessionId);
  await launchScreenOnlyAgent(smokePage, sessionId);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toContainText("SCREEN_AGENT_READY", { timeout: 30_000 });
  const screenOnly = await pollAgentStatus(
    stack.client,
    sessionId,
    "idle",
    (status) => status.source === "screen",
  );
  expect(screenOnly.promptable).toBe(false);

  const screenPromptText = `screen-${crypto.randomUUID()}`;
  const rejected = await stack.client.sessionsPrompt(promptRequest(screenOnly, screenPromptText));
  expect(rejected.inputOutcome).toBe(AgentPromptInputOutcome.REJECTED);
  expect(rejected.writtenBytes).toBe(0);
  expect(rejected.waitOutcome).toBeUndefined();
  expect(rejected.reason).not.toContain(screenPromptText);
  expect(rejected.reason.length).toBeGreaterThan(0);
  expect(rejected.reason.length).toBeLessThanOrEqual(200);

  const rawCanary = Uint8Array.of(0x02);
  const rawResponse = await stack.client.sessionsInput({ sessionId, data: rawCanary });
  expect(rawResponse.accepted).toBe(true);
  await expect(slot).toContainText("SCREEN_RAW:02", { timeout: 30_000 });
  const observedRawBytes = (await slot.textContent())?.match(/SCREEN_RAW:[0-9a-f]{2}/g) ?? [];
  expect(observedRawBytes).toEqual(["SCREEN_RAW:02"]);
});
