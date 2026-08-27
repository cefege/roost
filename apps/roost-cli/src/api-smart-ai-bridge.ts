// Stable machine-readable session discovery and terminal input for external clients.
// Kept separate so api.ts remains below its ratcheted size baseline and callers can
// test exact byte semantics without constructing an authenticated coordinator.
import type { CoordClient } from '../../worker/src/coord-client.ts'

export interface ApiDispatchIo {
  readStdin(): Promise<Uint8Array>
  stdout(line: string): void
}

export const defaultDispatchIo: ApiDispatchIo = {
  async readStdin() {
    return new Uint8Array(await Bun.stdin.arrayBuffer())
  },
  stdout(line) {
    console.log(line)
  }
}

export async function printSessions(
  client: CoordClient,
  json: boolean,
  io: ApiDispatchIo
): Promise<void> {
  const { sessions } = await client.sessionsList({ status: 'all' })
  if (json) {
    io.stdout(
      JSON.stringify(
        sessions.map((session) => ({
          id: session.id,
          workerFp: session.workerFp,
          cwd: session.cwd,
          spawnCwd: session.spawnCwd,
          title: session.customTitle ?? '',
          status: session.status
        }))
      )
    )
    return
  }
  for (const session of sessions) {
    io.stdout(
      [session.id, session.workerFp, session.kind, session.cwd, session.customTitle || ''].join('\t')
    )
  }
}

export async function sendTerminalInput(
  client: CoordClient,
  rest: string[],
  io: ApiDispatchIo
): Promise<number> {
  const sessionId = rest[0]
  if (!sessionId) throw new Error('missing sessionId')
  const stdin = rest.includes('--stdin')
  const positional = rest[1] && !rest[1].startsWith('--') ? rest[1] : undefined
  if (stdin && positional) throw new Error('input accepts either positional text or --stdin, not both')
  if (!stdin && !positional) throw new Error('missing text')

  let data = stdin
    ? await io.readStdin()
    : new TextEncoder().encode(
        positional!.replace(/\\n/g, '\n').replace(/\\t/g, '\t').replace(/\\r/g, '\r')
      )
  const enter = rest.includes('--enter')
  const maxBytes = enter ? 65_535 : 65_536
  if (data.byteLength > maxBytes) throw new Error(`input exceeds ${maxBytes} byte limit`)
  if (enter) {
    const withEnter = new Uint8Array(data.byteLength + 1)
    withEnter.set(data)
    withEnter[data.byteLength] = 0x0d
    data = withEnter
  }

  const response = await client.sessionsInput({ sessionId, data })
  if (response.accepted) {
    io.stdout('{"ok":true,"accepted":true}')
    return 0
  }
  io.stdout('{"ok":false,"accepted":false,"error":"terminal input was not accepted"}')
  return 2
}
