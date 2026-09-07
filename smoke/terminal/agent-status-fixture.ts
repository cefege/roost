// Real-process fixtures shared by agent-status and guarded-prompt smoke.
// The integration OMP owns the pane tty foreground like a real coding agent and
// is therefore driven out of band through a FIFO the test writes directly; its
// reports cross the worker's local endpoint, kernel PID attestation, and fresh
// ancestry proof. The screen-only OMP replaces the shell and is detected from
// terminal output alone.

import { constants, existsSync } from "node:fs";
import { open, type FileHandle } from "node:fs/promises";
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

type AgentCommand =
  | { kind: "exit" }
  | { kind: "capture"; nonce: string; bytes: number }
  | {
    kind: "report";
    state: "working" | "blocked" | "idle";
    seq: number;
    active: boolean;
    message?: string;
  };

const AGENT_FIFO_TIMEOUT_MS = 30_000;

const TEST_AGENT_PROGRAM = [
  "import json,os,socket,termios,tty",
  "fifo=os.environ['ROOST_TEST_AGENT_FIFO']",
  "os.mkfifo(fifo)",
  "def send_report(command):",
  "    params={'session_id':os.environ['ROOST_SESSION_ID'],'state':command['state'],'active':command['active']}",
  "    if command.get('message') is not None: params['message']=command['message']",
  "    request={'version':1,'method':'agent.report','capability':os.environ['ROOST_AGENT_CAPABILITY'],'params':params}",
  "    connection=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)",
  "    connection.connect(os.environ['ROOST_AGENT_SOCKET_PATH'])",
  "    connection.sendall((json.dumps(request)+'\\n').encode())",
  "    response=connection.recv(4096).decode().strip()",
  "    connection.close()",
  "    print('STATUS_ACK_%s %s' % (command['seq'],response),flush=True)",
  "def capture_input(command):",
  "    data=b''",
  "    target=command['bytes']",
  "    saved=termios.tcgetattr(0)",
  "    try:",
  "        tty.setraw(0)",
  "        os.write(1,('\\x1b[?2004h\\r\\nCAPTURE_READY:%s\\r\\n'%command['nonce']).encode())",
  "        while len(data)<target:",
  "            part=os.read(0,target-len(data))",
  "            if not part: break",
  "            data+=part",
  "    finally:",
  "        termios.tcsetattr(0,termios.TCSADRAIN,saved)",
  "    os.write(1,('\\r\\nCAPTURE:%s:%s\\r\\n'%(command['nonce'],data.hex())).encode())",
  "active=True",
  "while active:",
  "    reader=open(fifo)",
  "    line=reader.readline()",
  "    reader.close()",
  "    if not line: continue",
  "    command=json.loads(line)",
  "    if command['kind']=='exit': break",
  "    if command['kind']=='capture':",
  "        capture_input(command)",
  "        continue",
  "    send_report(command)",
  "    active=command['active']",
  "os.unlink(fifo)",
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

function agentFifoPath(sessionId: string): string {
  return `/tmp/roost-agent-${sessionId}.fifo`;
}

function integratedAgentLaunchCommand(sessionId: string): string {
  const fifo = agentFifoPath(sessionId);
  const agent =
    `exec -a omp python3 -u -c "import base64;exec(base64.b64decode('${TEST_AGENT_PROGRAM_BASE64}'))"`;
  // The agent is a foreground job, never `&`: prompt admission rejects a write
  // whose target agent does not own the pane tty's foreground process group.
  return `rm -f "${fifo}"; ROOST_TEST_AGENT_FIFO="${fifo}" bash -c ${JSON.stringify(agent)}`;
}

async function writeAgentCommand(sessionId: string, command: AgentCommand): Promise<void> {
  const fifo = agentFifoPath(sessionId);
  const line = `${JSON.stringify(command)}\n`;
  // O_NONBLOCK makes a FIFO with no reader fail with ENXIO instead of blocking,
  // so "the agent has not reopened its FIFO yet" is a retry, never a hang.
  await expect.poll(async () => {
    let handle: FileHandle | undefined;
    try {
      handle = await open(fifo, constants.O_WRONLY | constants.O_NONBLOCK);
      await handle.write(line);
      return true;
    } catch {
      return false;
    } finally {
      await handle?.close();
    }
  }, {
    timeout: AGENT_FIFO_TIMEOUT_MS,
    message: `agent fixture did not accept a ${command.kind} command on ${fifo}`,
  }).toBe(true);
}

// The agent creates its FIFO before its first read and unlinks it on the way
// out, so the FIFO is the fixture's liveness signal for the agent process.
async function waitForRunningAgent(sessionId: string): Promise<void> {
  const fifo = agentFifoPath(sessionId);
  await expect.poll(() => existsSync(fifo), {
    timeout: AGENT_FIFO_TIMEOUT_MS,
    message: `agent fixture never opened its control FIFO ${fifo}`,
  }).toBe(true);
}

async function waitForExitedAgent(sessionId: string): Promise<void> {
  const fifo = agentFifoPath(sessionId);
  await expect.poll(() => existsSync(fifo), {
    timeout: AGENT_FIFO_TIMEOUT_MS,
    message: `agent fixture process never exited; ${fifo} is still present`,
  }).toBe(false);
}

/** A live agent owns the pane tty, so nothing typed at the PTY reaches the
 *  shell until it is gone. Stopping it out of band is what lets a replacement
 *  launch, or a screen-only takeover, be typed at all. */
async function stopIntegratedAgent(sessionId: string): Promise<void> {
  if (!existsSync(agentFifoPath(sessionId))) return;
  await writeAgentCommand(sessionId, { kind: "exit" });
  await waitForExitedAgent(sessionId);
}

export async function launchIntegratedAgent(
  page: Page,
  sessionId: string,
): Promise<void> {
  await stopIntegratedAgent(sessionId);
  await page.evaluate(async ({ id, command }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, command);
  }, {
    id: sessionId,
    command: `${integratedAgentLaunchCommand(sessionId)}\r`,
  });
  await waitForRunningAgent(sessionId);
}

export async function reportAgentStatus(
  sessionId: string,
  state: "working" | "blocked" | "idle",
  seq: number,
  active = true,
  message?: string,
): Promise<void> {
  await writeAgentCommand(sessionId, { kind: "report", state, seq, active, message });
  // A deactivating report is the agent's last one: it exits straight after the
  // socket ACK, and whatever the caller does next must not race that exit.
  if (!active) await waitForExitedAgent(sessionId);
}

/** Arm the agent to consume the next `byteLength` bytes of PTY input and echo
 *  them back as `CAPTURE:<nonce>:<hex>`. Returns once the agent holds the tty
 *  in raw mode, so the captured bytes are the writer's bytes exactly. */
export async function armAgentByteCapture(
  page: Page,
  sessionId: string,
  byteLength: number,
  nonce: string,
): Promise<void> {
  await writeAgentCommand(sessionId, { kind: "capture", nonce, bytes: byteLength });
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toContainText(
    `CAPTURE_READY:${nonce}`,
    { timeout: AGENT_FIFO_TIMEOUT_MS },
  );
}

export async function reportReplacementAgentStatus(
  client: AgentStatusClient,
  sessionId: string,
  previousOccupantId: string,
): Promise<AgentStatusView> {
  let attempt = 0;
  let matchedStatus: AgentStatusView | undefined;
  await expect.poll(async () => {
    attempt++;
    await reportAgentStatus(sessionId, "working", attempt);
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

export async function launchScreenOnlyAgent(
  page: Page,
  sessionId: string,
): Promise<void> {
  await stopIntegratedAgent(sessionId);
  const command =
    `exec -a omp python3 -u -c "import base64;exec(base64.b64decode('${SCREEN_AGENT_PROGRAM_BASE64}'))"`;
  await page.evaluate(async ({ id, input }) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    await smoke.input(id, `${input}\r`);
  }, { id: sessionId, input: command });
}
