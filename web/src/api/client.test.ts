import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { RequestError, TriadClient, type ConnectionStatus } from './client'
import type { IncomingEvent } from './protocol'
import { fakeSockets, hello, ready } from '@/test/fake-socket'

const TOKEN = 'f'.repeat(64)
const URL = 'ws://127.0.0.1:7878/v1'

function setup() {
  const fake = fakeSockets()
  const events: IncomingEvent[] = []
  const statuses: ConnectionStatus[] = []
  const client = new TriadClient({
    url: URL,
    token: TOKEN,
    socket: fake.factory,
    onEvent: (e) => events.push(e),
    onStatus: (s) => statuses.push(s),
  })
  const connectOpen = () => {
    client.connect()
    fake.current().open()
    fake.current().emit(hello())
    fake.current().emit(ready())
  }
  return { fake, events, statuses, client, connectOpen }
}

beforeEach(() => {
  vi.useFakeTimers()
})
afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
})

describe('TriadClient', () => {
  it('sends the token as the first frame, never in the URL, and is open on hello', () => {
    const log = vi.spyOn(console, 'log')
    const { fake, statuses, client, events } = setup()
    client.connect()
    expect(statuses).toEqual([{ state: 'connecting' }])
    const s = fake.current()
    expect(s.url).toBe(URL)
    expect(s.url).not.toContain(TOKEN)
    s.open()
    expect(s.sent).toEqual([{ op: 'auth', token: TOKEN }])
    expect(client.ready).toBe(false)
    s.emit(hello())
    s.emit(ready())
    expect(client.ready).toBe(true)
    expect(statuses.at(-1)).toEqual({ state: 'open', client: 1, node: hello().node, version: '0.1.0' })
    expect(events.map((e) => e.event)).toEqual(['hello', 'ready'])
    expect(log).not.toHaveBeenCalled()
  })

  it('refuses requests before it is authenticated', async () => {
    const { client } = setup()
    client.connect()
    await expect(client.request({ op: 'snapshot' })).rejects.toThrow('not connected')
  })

  it('resolves a request on its done, with the replies that carried its re', async () => {
    const { fake, client, connectOpen } = setup()
    connectOpen()
    const p = client.request({ op: 'command', channel: 'garden', line: '/members' })
    const req = fake.current().last()
    expect(req).toEqual({ id: 1, op: 'command', channel: 'garden', line: '/members' })
    const second = client.request({ op: 'snapshot' })
    expect(fake.current().last().id).toBe(2)
    const s = fake.current()
    s.emit({ event: 'members', channel: 'garden', members: [], access: {}, re: 1, seq: 5 })
    s.emit({ event: 'message', channel: 'garden', re: undefined, seq: 6 }) // not this request's
    s.emit({ event: 'done', ok: true, re: 1, seq: 7 })
    const r = await p
    expect(r.events.map((e) => e.event)).toEqual(['members'])
    expect(r.done.ok).toBe(true)
    s.emit({ event: 'done', ok: true, re: 2, seq: 8 })
    await expect(second).resolves.toMatchObject({ events: [] })
  })

  it('rejects a failing request with the error text and its events', async () => {
    const { fake, client, connectOpen } = setup()
    connectOpen()
    const p = client.request({ op: 'command', channel: 'garden', line: '/nosuch' })
    fake.current().emit({ event: 'error', text: 'unknown command /nosuch', re: 1, seq: 9 })
    fake.current().emit({ event: 'done', ok: false, error: 'unknown command /nosuch', re: 1, seq: 10 })
    const err = await p.catch((e: unknown) => e)
    expect(err).toBeInstanceOf(RequestError)
    expect((err as RequestError).message).toBe('unknown command /nosuch')
    expect((err as RequestError).events.map((e) => e.event)).toEqual(['error'])
  })

  it('ignores frames that are not JSON objects with an event name', () => {
    const { fake, events, connectOpen } = setup()
    connectOpen()
    fake.current().onmessage?.({ data: 'not json' })
    fake.current().onmessage?.({ data: '[1,2]' })
    fake.current().onmessage?.({ data: new ArrayBuffer(2) })
    fake.current().emit({ event: 'something_new', seq: 9, field: 1 })
    expect(events.map((e) => e.event)).toEqual(['hello', 'ready', 'something_new'])
  })

  it('4001: a wrong token is final', () => {
    const { fake, statuses, client } = setup()
    client.connect()
    fake.current().open()
    fake.current().drop(4001)
    expect(statuses.at(-1)).toEqual({ state: 'closed', reason: 'unauthorized' })
    vi.advanceTimersByTime(60_000)
    expect(fake.sockets).toHaveLength(1)
  })

  it('4002: no token in time is final', () => {
    const { fake, statuses, client } = setup()
    client.connect()
    fake.current().open()
    fake.current().drop(4002)
    expect(statuses.at(-1)).toEqual({ state: 'closed', reason: 'no-token' })
    vi.advanceTimersByTime(60_000)
    expect(fake.sockets).toHaveLength(1)
  })

  it('a first connection that never opens (403, or nothing listening) is final', () => {
    const { fake, statuses, client } = setup()
    client.connect()
    fake.current().drop(1006)
    expect(statuses.at(-1)).toEqual({ state: 'closed', reason: 'unreachable' })
    vi.advanceTimersByTime(60_000)
    expect(fake.sockets).toHaveLength(1)
  })

  it('a malformed address is unreachable', () => {
    const statuses: ConnectionStatus[] = []
    const client = new TriadClient({
      url: 'not a url',
      token: TOKEN,
      socket: () => {
        throw new SyntaxError('bad url')
      },
      onEvent: () => {},
      onStatus: (s) => statuses.push(s),
    })
    client.connect()
    expect(statuses.at(-1)).toEqual({ state: 'closed', reason: 'unreachable' })
  })

  it('4008 and drops reconnect with backoff, 1 s doubling to 30 s, and reset after hello', async () => {
    const { fake, statuses, client, connectOpen } = setup()
    connectOpen()
    const pending = client.request({ op: 'snapshot' })
    fake.current().emit({ event: 'lagged', seq: 40 })
    fake.current().drop(4008)
    await expect(pending).rejects.toThrow('connection lost')
    expect(statuses.at(-1)).toEqual({ state: 'reconnecting', reason: 'lagged', attempt: 1, retryInMs: 1000 })
    expect(client.ready).toBe(false)

    const delays: number[] = []
    for (let i = 0; i < 7; i++) {
      const st = statuses.at(-1) as Extract<ConnectionStatus, { state: 'reconnecting' }>
      delays.push(st.retryInMs)
      vi.advanceTimersByTime(st.retryInMs - 1)
      const before = fake.sockets.length
      vi.advanceTimersByTime(1)
      expect(fake.sockets.length).toBe(before + 1)
      fake.current().drop(1006) // serve still down
    }
    expect(delays).toEqual([1000, 2000, 4000, 8000, 16000, 30000, 30000])

    vi.advanceTimersByTime(30_000)
    const s = fake.current()
    s.open()
    expect(s.sent).toEqual([{ op: 'auth', token: TOKEN }])
    s.emit(hello())
    expect(statuses.at(-1)).toMatchObject({ state: 'open' })
    s.drop(1001)
    expect(statuses.at(-1)).toEqual({ state: 'reconnecting', reason: 'dropped', attempt: 1, retryInMs: 1000 })
  })

  it('close() stops for good and rejects what is pending', async () => {
    const { fake, statuses, client, connectOpen } = setup()
    connectOpen()
    const pending = client.request({ op: 'snapshot' })
    client.close()
    await expect(pending).rejects.toThrow('closed')
    expect(fake.current().closedWith).toBe(1000)
    expect(statuses.at(-1)).toEqual({ state: 'closed', reason: 'closed' })
    vi.advanceTimersByTime(60_000)
    expect(fake.sockets).toHaveLength(1)
  })
})
