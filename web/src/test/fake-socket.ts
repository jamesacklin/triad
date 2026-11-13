// A fake WebSocket for the client's tests: records what is sent and lets a
// test play the server.
import type { SocketFactory, SocketLike } from '@/api/client'

export class FakeSocket implements SocketLike {
  readyState = 0
  sent: unknown[] = []
  closedWith: number | null = null
  onopen: ((ev: unknown) => void) | null = null
  onmessage: ((ev: { data: unknown }) => void) | null = null
  onclose: ((ev: { code: number; reason: string }) => void) | null = null
  onerror: ((ev: unknown) => void) | null = null
  readonly url: string

  constructor(url: string) {
    this.url = url
  }

  send(data: string): void {
    if (this.readyState !== 1) throw new Error('send on a socket that is not open')
    this.sent.push(JSON.parse(data))
  }

  close(code = 1000): void {
    this.closedWith = code
    this.readyState = 3
  }

  // The server's side.
  open(): void {
    this.readyState = 1
    this.onopen?.({})
  }

  emit(frame: object): void {
    this.onmessage?.({ data: JSON.stringify(frame) })
  }

  drop(code: number, reason = ''): void {
    this.readyState = 3
    this.onclose?.({ code, reason })
  }

  /** The requests sent, without the auth frame. */
  requests(): Array<Record<string, unknown> & { id: number; op: string }> {
    return this.sent.filter((f) => (f as { op: string }).op !== 'auth') as never
  }

  last(): Record<string, unknown> & { id: number; op: string } {
    const r = this.requests()
    return r[r.length - 1]
  }
}

export function fakeSockets(): { factory: SocketFactory; sockets: FakeSocket[]; current: () => FakeSocket } {
  const sockets: FakeSocket[] = []
  return {
    sockets,
    factory: (url) => {
      const s = new FakeSocket(url)
      sockets.push(s)
      return s
    },
    current: () => sockets[sockets.length - 1],
  }
}

export const ME = 'aa'.padEnd(64, '0')
export const SAM = 'bb'.padEnd(64, '1')
export const OP = (n: number) => n.toString(16).padStart(64, '0')

export function hello(seq = 1) {
  return { event: 'hello', protocol: 1, version: '0.1.0', node: ME, client: 1, seq }
}

export function channelInfo(channel: string, extra: Partial<Record<string, unknown>> = {}) {
  return {
    channel,
    group: 'cc'.padEnd(64, '0'),
    role: 'creator',
    status: 'joined',
    invite: channel === 'agent' ? null : `triad1-${channel}`,
    focused: false,
    ...extra,
  }
}

export function ready(channels: string[] = ['agent', 'garden'], seq = 2) {
  return {
    event: 'ready',
    seq,
    ...channelInfo(channels[0]),
    node: ME,
    channels: channels.map((c) => channelInfo(c)),
    bots: true,
    bot_provider: 'mock',
    bot_servers: ['time'],
  }
}

export function msg(channel: string, n: number, extra: Partial<Record<string, unknown>> = {}) {
  return {
    author: SAM,
    text: `m${n}`,
    nickname: 'sam',
    op: OP(n),
    mine: false,
    ts: 1000 + n,
    bot: false,
    action: false,
    ...extra,
    channel,
  }
}

export function message(channel: string, n: number, seq: number, extra: Partial<Record<string, unknown>> = {}) {
  return { event: 'message', seq, ask: false, long: false, ...msg(channel, n, extra) }
}
