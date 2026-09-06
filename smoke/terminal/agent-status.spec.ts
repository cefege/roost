// Coding-agent status path, end to end without model credentials:
// spawned PTY report -> worker/coord/Sync -> session, folder, notification,
// and route-driven attention surfaces, including acknowledgement decay.

import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { AgentStatusView } from "@roost/shared/proto/coordinator_pb";

interface AgentStatusSmoke {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  input(sessionId: string, text: string): Promise<void>;
  forceVisible(on: boolean): void;
  navigate(href: string): void;
}


interface AgentStatusClient {
  agentStatusGet(request: { sessionId: string }): Promise<{ status?: AgentStatusView }>;
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

const TEST_AGENT_PROGRAM = [
  "import json,os,socket",
  "path=os.environ['ROOST_TEST_AGENT_FIFO']",
  "os.mkfifo(path)",
  "active=True",
  "while active:",
  "    reader=open(path)",
  "    line=reader.readline()",
  "    reader.close()",
  "    if not line: continue",
  "    command=json.loads(line)",
  "    params={'session_id':os.environ['ROOST_SESSION_ID'],'state':command['state'],'active':command['active']}",
  "    if command.get('message') is not None: params['message']=command['message']",
  "    request={'version':1,'method':'agent.report','capability':os.environ['ROOST_AGENT_CAPABILITY'],'params':params}",
  "    connection=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)",
  "    connection.connect(os.environ['ROOST_AGENT_SOCKET_PATH'])",
  "    connection.sendall((json.dumps(request)+'\\n').encode())",
  "    response=connection.recv(4096).decode().strip()",
  "    connection.close()",
  "    print('STATUS_ACK_%s %s' % (command['seq'],response),flush=True)",
  "    active=command['active']",
  "os.unlink(path)",
].join("\n");
const TEST_AGENT_PROGRAM_BASE64 = Buffer.from(TEST_AGENT_PROGRAM, "utf8").toString("base64");

function reportCommand(
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
  restartAgent = false,
): string {
  const restart = restartAgent
    ? [
        "if [ -n \"${ROOST_TEST_AGENT_PID:-}\" ]; then kill \"$ROOST_TEST_AGENT_PID\" 2>/dev/null || true; wait \"$ROOST_TEST_AGENT_PID\" 2>/dev/null || true; fi;",
        "ROOST_TEST_AGENT_FIFO=\"/tmp/roost-agent-$ROOST_SESSION_ID-$RANDOM\"; export ROOST_TEST_AGENT_FIFO;",
        "rm -f \"$ROOST_TEST_AGENT_FIFO\";",
        `bash -c ${JSON.stringify(`exec -a omp python3 -u -c "import base64;exec(base64.b64decode('${TEST_AGENT_PROGRAM_BASE64}'))"`)} &`,
        "ROOST_TEST_AGENT_PID=$!;",
        "while [ ! -p \"$ROOST_TEST_AGENT_FIFO\" ]; do sleep 0.05; done;",
      ].join(" ")
    : "";
  const command = JSON.stringify({ state, seq, active, message });
  return `${restart} printf '%s\\n' ${JSON.stringify(command)} > "$ROOST_TEST_AGENT_FIFO"\r`;
}

async function report(
  page: Page,
  sessionId: string,
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
  restartAgent = false,
): Promise<void> {
  await page.evaluate(async ({ id, command }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, command);
  }, {
    id: sessionId,
    command: reportCommand(state, seq, active, message, restartAgent),
  });
}

async function pollAgentStatus(
  client: AgentStatusClient,
  sessionId: string,
  state: "working" | "blocked" | "idle",
  accept: (status: AgentStatusView) => boolean = () => true,
): Promise<AgentStatusView> {
  let matchedStatus: AgentStatusView | undefined;
  await expect.poll(async () => {
    try {
      const status = (await client.agentStatusGet({ sessionId })).status;
      if (!status || status.state !== state || !accept(status)) return false;
      matchedStatus = status;
      return true;
    } catch {
      return false;
    }
  }, { timeout: 30_000 }).toBe(true);
  if (!matchedStatus) throw new Error(`agent status ${state} was not observed for ${sessionId}`);
  return matchedStatus;
}

test("agent status reaches every browser surface and notification ACK", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop status surfaces");
  test.setTimeout(180_000);

  const [activeId, backgroundId] = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    const first = await smoke.spawnShell(workerFp, "/tmp");
    const second = await smoke.spawnShell(workerFp, "/tmp");
    smoke.forceVisible(true);
    return [first.session_id, second.session_id];
  }, stack.workerFp);

  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${activeId}`)).toBeVisible();
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toBeVisible();

  const tabStatus = smokePage
    .getByTestId(`tab-${backgroundId}`)
    .getByTestId(`agent-status-${backgroundId}`);
  const folderStatus = smokePage.locator('[data-testid^="folder-agent-status-"]').first();

  // One report is checked for its socket ACK so a silently rejected report
  // (bad pid ownership, unwritable socket) can't masquerade as a UI bug; the
  // remaining reports are proven by the surfaces they move.
  await report(smokePage, backgroundId, "working", 1, true, undefined, true);
  await smokePage.goto(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${backgroundId}`)).toContainText(
    `STATUS_ACK_1 {"ok":true}`,
    { timeout: 30_000 },
  );
  const firstStatus = await pollAgentStatus(stack.client, backgroundId, "working");
  expect(firstStatus).toMatchObject({
    sessionId: backgroundId,
    agentId: "omp",
    state: "working",
    source: "integration",
    promptable: true,
  });
  expect(firstStatus.statusEpoch).toMatch(UUID_PATTERN);
  expect(firstStatus.occupantId).toMatch(UUID_PATTERN);
  const sameShellEpoch = firstStatus.statusEpoch!;
  const sameShellOccupant = firstStatus.occupantId!;
  const serializedPublicStatus = JSON.stringify(firstStatus, (_key, value) =>
    typeof value === "bigint" ? value.toString() : value
  );
  expect(serializedPublicStatus).not.toMatch(/"pid"\s*:/i);
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "working");
  await expect(folderStatus).toContainText("1 working");

  // Search switches the sidebar from folder rows to the full per-session row
  // without changing the active terminal. This locks the distinct session-row
  // surface as well as the folder rollup above.
  await smokePage.getByTestId("brand-row-search").click();
  await smokePage.getByTestId("sidebar-search").fill("/tmp");
  const sessionRow = smokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${backgroundId}"]`,
  );
  await expect(sessionRow).toBeVisible();
  await expect(sessionRow.getByTestId(`agent-status-${backgroundId}`)).toHaveAttribute(
    "data-level",
    "working",
  );
  await smokePage.getByTestId("sidebar-search-clear").click();
  await expect(folderStatus).toBeVisible();

  await report(smokePage, backgroundId, "blocked", 2, true, "Approval needed");
  await expect(tabStatus).toHaveAttribute("data-level", "blocked", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "blocked");
  await expect(folderStatus).toContainText("1 needs input");
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);
  const blockedToast = smokePage.getByTestId("toast").filter({ hasText: "needs your input" });
  await expect(blockedToast).toBeVisible({ timeout: 10_000 });
  await expect(blockedToast.getByTestId("toast-details")).toContainText("Approval needed");
  const blockedStatus = await pollAgentStatus(stack.client, backgroundId, "blocked");
  expect([blockedStatus.statusEpoch, blockedStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/search?scope=attention`);
  const blockedAttention = smokePage.getByTestId(`global-search-result-${backgroundId}`);
  await expect(blockedAttention).toBeVisible();
  await expect(
    blockedAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Needs input");
  await blockedAttention.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toHaveAttribute("data-active", "true");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  // Viewing acknowledges the notification revision, but current blocked state
  // remains attention until the agent itself transitions.
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(blockedAttention).toBeVisible();
  await expect(
    blockedAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Needs input");

  // Move away before completion so the transition is genuinely backgrounded.
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`tab-${activeId}`)).toHaveAttribute("data-active", "true");
  await report(smokePage, backgroundId, "working", 3);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  const resumedStatus = await pollAgentStatus(stack.client, backgroundId, "working");
  expect([resumedStatus.statusEpoch, resumedStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);
  await report(smokePage, backgroundId, "idle", 4);
  await expect(tabStatus).toHaveAttribute("data-level", "done", { timeout: 30_000 });
  const idleStatus = await pollAgentStatus(stack.client, backgroundId, "idle");
  expect([idleStatus.statusEpoch, idleStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);
  await expect(folderStatus).toHaveAttribute("data-level", "done");
  await expect(folderStatus).toContainText("1 done");
  const doneToast = smokePage.getByTestId("toast").filter({ hasText: "finished" });
  await expect(doneToast).toBeVisible({ timeout: 10_000 });
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  const doneAttention = smokePage.getByTestId(`global-search-result-${backgroundId}`);
  await expect(doneAttention).toBeVisible();
  await expect(
    doneAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Done");
  await doneAttention.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(tabStatus).toHaveAttribute("data-level", "idle");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(doneAttention).toHaveCount(0);
  await expect(smokePage.getByText("Nothing needs attention", { exact: true })).toBeVisible();

  await report(smokePage, backgroundId, "idle", 5, false);
  await expect(tabStatus).toHaveCount(0, { timeout: 30_000 });
  await expect(folderStatus).toHaveCount(0);

  await report(smokePage, activeId, "working", 1, true, undefined, true);
  const firstReplacementStatus = await pollAgentStatus(stack.client, activeId, "working");
  expect(firstReplacementStatus.statusEpoch).toBe(sameShellEpoch);
  expect(firstReplacementStatus.occupantId).toMatch(UUID_PATTERN);

  await report(smokePage, activeId, "working", 1, true, undefined, true);
  const secondReplacementStatus = await pollAgentStatus(
    stack.client,
    activeId,
    "working",
    (status) => status.occupantId !== firstReplacementStatus.occupantId,
  );
  expect(secondReplacementStatus.statusEpoch).toBe(firstReplacementStatus.statusEpoch);
  expect(secondReplacementStatus.occupantId).toMatch(UUID_PATTERN);
  expect(secondReplacementStatus.occupantId).not.toBe(firstReplacementStatus.occupantId);
});
