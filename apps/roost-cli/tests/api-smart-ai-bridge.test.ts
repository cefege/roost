import { describe, expect, test } from 'bun:test'
import { dispatch, type ApiDispatchIo } from '../src/api.ts'

type Client = Parameters<typeof dispatch>[0]

function harness(overrides: Record<string, unknown> = {}) {
  const lines: string[] = []
  const calls: Uint8Array[] = []
  const client = {
    sessionsList: async () => ({
      sessions: [
        {
          id: 's1',
          workerFp: 'worker',
          kind: 'shell',
          cwd: '/work',
          spawnCwd: '/spawn',
          customTitle: undefined,
          status: 'open'
        }
      ]
    }),
    sessionsInput: async ({ data }: { data: Uint8Array }) => {
      calls.push(data)
      return { accepted: true }
    },
    ...overrides
  } as unknown as Client
  const io: ApiDispatchIo = {
    readStdin: async () => new TextEncoder().encode('alpha\nbeta'),
    stdout: (line) => lines.push(line)
  }
  return { client, io, lines, calls }
}

describe('Smart AI API bridge', () => {
  test('prints stable JSON sessions', async () => {
    const h = harness()
    expect(await dispatch(h.client, 'sessions', ['--json'], h.io)).toBe(0)
    expect(JSON.parse(h.lines[0]!)).toEqual([
      {
        id: 's1',
        workerFp: 'worker',
        cwd: '/work',
        spawnCwd: '/spawn',
        title: '',
        status: 'open'
      }
    ])
  })

  test('sends exact stdin bytes and one carriage return', async () => {
    const h = harness()
    expect(await dispatch(h.client, 'input', ['s1', '--stdin', '--enter'], h.io)).toBe(0)
    expect(h.calls).toHaveLength(1)
    expect([...h.calls[0]!]).toEqual([...new TextEncoder().encode('alpha\nbeta'), 13])
    expect(h.lines).toEqual(['{"ok":true,"accepted":true}'])
  })

  test('rejects oversized stdin before the RPC', async () => {
    const h = harness()
    h.io.readStdin = async () => new Uint8Array(65_536)
    await expect(dispatch(h.client, 'input', ['s1', '--stdin', '--enter'], h.io)).rejects.toThrow(
      'input exceeds 65535 byte limit'
    )
    expect(h.calls).toHaveLength(0)
  })

  test('returns exit 2 when terminal rejects input', async () => {
    const h = harness({ sessionsInput: async () => ({ accepted: false }) })
    expect(await dispatch(h.client, 'input', ['s1', '--stdin'], h.io)).toBe(2)
    expect(h.lines).toEqual([
      '{"ok":false,"accepted":false,"error":"terminal input was not accepted"}'
    ])
  })

  test('preserves escaped positional input', async () => {
    const h = harness()
    await dispatch(h.client, 'input', ['s1', 'alpha\\nbeta', '--enter'], h.io)
    expect(new TextDecoder().decode(h.calls[0])).toBe('alpha\nbeta\r')
  })

  test('advertises stdin capability without an RPC', async () => {
    const h = harness()
    expect(await dispatch(h.client, 'input', ['--help'], h.io)).toBe(0)
    expect(h.lines[0]).toContain('--stdin')
    expect(h.calls).toHaveLength(0)
  })
})
