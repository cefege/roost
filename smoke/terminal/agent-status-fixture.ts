// Real-process fixtures shared by agent-status and guarded-prompt smoke.
// The integration OMP crosses the worker's local endpoint, kernel PID
// attestation, and fresh ancestry proof; screen-only OMP uses terminal detection.

import { expect, type Page } from "@playwright/test";
import type { AgentStatusView } from "@roost/shared/proto/coordinator_pb";

export interface AgentStatusSmoke {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  input(sessionId: string, text: string): Promise<void>;
  forceVisible(on: boolean): void;
  navigate(href: string): void;
}

export interface AgentStatusClient {
  agentStatusGet(
    request: { sessionId: string },
  ): Promise<{ status?: AgentStatusView }>;
}

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
const TEST_AGENT_PROGRAM_BASE64 = Buffer.from(
  TEST_AGENT_PROGRAM,
  "utf8",
).toString("base64");

const SCREEN_AGENT_PROGRAM = [
  "import os,termios,tty",
  "fd=0",
  "old=termios.tcgetattr(fd)",
  "try:",
  "    tty.setraw(fd)",
  "    os.write(1,'\\x1b]0;π > screen-only smoke\\x07\\r\\nSCREEN_AGENT_READY\\r\\n'.encode())",
  "    while True:",
  "        data=os.read(fd,4096)",
  "        if not data: break",
  "        for value in data: os.write(1, ('\\r\\nSCREEN_RAW:%02x\\r\\n'%value).encode())",
  "finally:",
  "    termios.tcsetattr(fd,termios.TCSADRAIN,old)",
].join("\n");
const SCREEN_AGENT_PROGRAM_BASE64 = Buffer.from(
  SCREEN_AGENT_PROGRAM,
  "utf8",
).toString("base64");

function integratedAgentLaunchCommand(): string {
  return [
    "if [ -n \"${ROOST_TEST_AGENT_PID:-}\" ]; then kill \"$ROOST_TEST_AGENT_PID\" 2>/dev/null || true; wait \"$ROOST_TEST_AGENT_PID\" 2>/dev/null || true; fi;",
    "ROOST_TEST_AGENT_FIFO=\"/tmp/roost-agent-$ROOST_SESSION_ID-$RANDOM\"; export ROOST_TEST_AGENT_FIFO;",
    "rm -f \"$ROOST_TEST_AGENT_FIFO\";",
    `bash -c ${JSON.stringify(`exec -a omp python3 -u -c "import base64;exec(base64.b64decode('${TEST_AGENT_PROGRAM_BASE64}'))"`)} &`,
    "ROOST_TEST_AGENT_PID=$!;",
    "while [ ! -p \"$ROOST_TEST_AGENT_FIFO\" ]; do sleep 0.05; done;",
  ].join(" ");
}

function reportCommand(
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
): string {
  const command = JSON.stringify({ state, seq, active, message });
  return `printf '%s\\n' ${JSON.stringify(command)} > "$ROOST_TEST_AGENT_FIFO"\r`;
}

export async function launchIntegratedAgent(
  page: Page,
  sessionId: string,
): Promise<void> {
  await page.evaluate(async ({ id, command }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, command);
  }, {
    id: sessionId,
    command: `${integratedAgentLaunchCommand()}\r`,
  });
}

export async function reportAgentStatus(
  page: Page,
  sessionId: string,
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
): Promise<void> {
  await page.evaluate(async ({ id, command }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, command);
  }, {
    id: sessionId,
    command: reportCommand(state, seq, active, message),
  });
}
export async function reportReplacementAgentStatus(
  page: Page,
  client: AgentStatusClient,
  sessionId: string,
  previousOccupantId: string,
): Promise<AgentStatusView> {
  let attempt = 0;
  let matchedStatus: AgentStatusView | undefined;
  await expect.poll(async () => {
    attempt++;
    await reportAgentStatus(page, sessionId, "working", attempt);
    const status = (await client.agentStatusGet({ sessionId })).status;
    if (
      status?.source !== "integration"
      || status.state !== "working"
      || status.occupantId === previousOccupantId
    ) {
      return false;
    }
    matchedStatus = status;
    return true;
  }, { timeout: 30_000 }).toBe(true);
  if (!matchedStatus) {
    throw new Error(`replacement agent status was not observed for ${sessionId}`);
  }
  return matchedStatus;
}


export async function pollAgentStatus(
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
  if (!matchedStatus) {
    throw new Error(`agent status ${state} was not observed for ${sessionId}`);
  }
  return matchedStatus;
}

export async function waitForAgentStatusRemoval(
  client: AgentStatusClient,
  sessionId: string,
): Promise<void> {
  await expect.poll(async () => {
    try {
      return !(await client.agentStatusGet({ sessionId })).status;
    } catch {
      return true;
    }
  }, { timeout: 30_000 }).toBe(true);
}

export async function launchScreenOnlyAgent(
  page: Page,
  sessionId: string,
): Promise<void> {
  const command =
    `exec -a omp python3 -u -c "import base64;exec(base64.b64decode('${SCREEN_AGENT_PROGRAM_BASE64}'))"`;
  await page.evaluate(async ({ id, input }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, `${input}\r`);
  }, { id: sessionId, input: command });
}
