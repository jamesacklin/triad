// TriadClient owns the WebSocket to `triad serve`. It authenticates with the
// first frame, numbers requests, resolves one promise per request when its
// `done` arrives (with the reply events that carried the same `re`), and
// reconnects with backoff. Components never touch a WebSocket.
//
// The token is kept in a private field and is never logged or put in a URL.
import { CLOSE, type EventOf, type IncomingEvent, type RequestBody, type RequestId } from './protocol'

/** Why the client stopped, or why it is retrying. */
export type CloseReason =
  /** 4001: the token was wrong. */
  | 'unauthorized'
  /** 4002: no token within 5 seconds. */
  | 'no-token'
  /** The first connection never opened: serve is not running, or it refused the page's origin (403). */
  | 'unreachable'
  /** 4008: the client fell over 1024 frames behind. */
  | 'lagged'
  /** The socket dropped, or the session ended (1001). */
  | 'dropped'
  /** close() was called. */
  | 'closed'

export type ConnectionStatus =
  | { state: 'connecting' }
  | { state: 'open'; client: number; node: string; version: string }
  | { state: 'reconnecting'; reason: CloseReason; attempt: number; retryInMs: number }
  | { state: 'closed'; reason: CloseReason }

export interface Reply {
  /** Every event sent under the request (its `re`), in order, before `done`. */
  events: IncomingEvent[]
  done: EventOf<'done'>
}

/** A request that ended with `ok: false`, or was cut off by a dropped socket. */
export class RequestError extends Error {
  readonly events: IncomingEvent[]
  constructor(message: string, events: IncomingEvent[] = []) {
    super(message)
    this.name = 'RequestError'
    this.events = events
  }
}

/** The parts of the browser's WebSocket the client uses; tests pass a fake. */
export interface SocketLike {
  readonly readyState: number
  onopen: ((ev: unknown) => void) | null
  onmessage: ((ev: { data: unknown }) => void) | null
  onclose: ((ev: { code: number; reason: string }) => void) | null
  onerror: ((ev: unknown) => void) | null
  send(data: string): void
  close(code?: number, reason?: string): void
}
export type SocketFactory = (url: string) => SocketLike

export interface ClientOptions {
  url: string
  token: string
  onEvent: (ev: IncomingEvent) => void
  onStatus: (status: ConnectionStatus) => void
  socket?: SocketFactory
  /** First retry delay; doubles to `maxBackoffMs`. */
  backoffMs?: number
  maxBackoffMs?: number
}

const OPEN = 1

interface Pending {
  events: IncomingEvent[]
  resolve: (r: Reply) => void
  reject: (e: RequestError) => void
}

export class TriadClient {
  readonly url: string
  #token: string
  #opts: Required<Pick<ClientOptions, 'backoffMs' | 'maxBackoffMs'>> & ClientOptions
  #socket: SocketLike | null = null
  #nextId = 1
  #pending = new Map<RequestId, Pending>()
  #authenticated = false
  /** Whether any connection has authenticated; until then a failure is final. */
  #everOpen = false
  #attempt = 0
  #retryTimer: ReturnType<typeof setTimeout> | null = null
  #stopped = false

  constructor(opts: ClientOptions) {
    this.url = opts.url
    this.#token = opts.token
    this.#opts = { backoffMs: 1000, maxBackoffMs: 30_000, ...opts }
  }

  /** The id the next request will carry. */
  get nextId(): number {
    return this.#nextId
  }

  /** Whether requests can be sent now. */
  get ready(): boolean {
    return this.#authenticated && this.#socket?.readyState === OPEN
  }

  connect(): void {
    this.#stopped = false
    this.#open()
  }

  /** Closes for good: no reconnect. Pending requests are rejected. */
  close(): void {
    this.#stopped = true
    if (this.#retryTimer) clearTimeout(this.#retryTimer)
    this.#retryTimer = null
    const s = this.#socket
    this.#socket = null
    this.#authenticated = false
    this.#failPending('closed')
    if (s) {
      s.onclose = null
      s.close(1000)
    }
    this.#opts.onStatus({ state: 'closed', reason: 'closed' })
  }

  /**
   * Sends a request. Resolves with its reply events when `done` arrives with
   * `ok: true`; rejects with the error text when `ok` is false or the socket
   * drops first.
   */
  request(body: RequestBody): Promise<Reply> {
    if (!this.ready || !this.#socket) {
      return Promise.reject(new RequestError('not connected'))
    }
    const id = this.#nextId++
    const p = new Promise<Reply>((resolve, reject) => {
      this.#pending.set(id, { events: [], resolve, reject })
    })
    this.#socket.send(JSON.stringify({ id, ...body }))
    return p
  }

  #open(): void {
    const factory: SocketFactory = this.#opts.socket ?? ((url) => new WebSocket(url) as unknown as SocketLike)
    this.#authenticated = false
    if (this.#attempt === 0) this.#opts.onStatus({ state: 'connecting' })
    let s: SocketLike
    try {
      s = factory(this.url)
    } catch {
      // A malformed address: the constructor throws.
      this.#socket = null
      this.#opts.onStatus({ state: 'closed', reason: 'unreachable' })
      return
    }
    this.#socket = s
    s.onopen = () => {
      s.send(JSON.stringify({ op: 'auth', token: this.#token }))
    }
    s.onmessage = (e) => {
      if (typeof e.data !== 'string') return
      let ev: IncomingEvent
      try {
        ev = JSON.parse(e.data) as IncomingEvent
      } catch {
        return
      }
      if (!ev || typeof ev !== 'object' || typeof ev.event !== 'string') return
      this.#receive(ev)
    }
    s.onerror = () => {}
    s.onclose = (e) => {
      if (this.#socket !== s) return
      this.#socket = null
      this.#closed(e.code)
    }
  }

  #receive(ev: IncomingEvent): void {
    if (ev.event === 'hello') {
      const h = ev as EventOf<'hello'>
      this.#authenticated = true
      this.#everOpen = true
      this.#attempt = 0
      this.#opts.onStatus({ state: 'open', client: h.client, node: h.node, version: h.version })
    }
    if (ev.re !== undefined) {
      const p = this.#pending.get(ev.re)
      if (p) {
        if (ev.event === 'done') {
          this.#pending.delete(ev.re)
          const done = ev as EventOf<'done'>
          if (done.ok) p.resolve({ events: p.events, done })
          else p.reject(new RequestError(done.error ?? 'request failed', p.events))
        } else {
          p.events.push(ev)
        }
      }
    }
    this.#opts.onEvent(ev)
  }

  #failPending(why: string): void {
    for (const p of this.#pending.values()) p.reject(new RequestError(why, p.events))
    this.#pending.clear()
  }

  #closed(code: number): void {
    this.#authenticated = false
    this.#failPending('connection lost')
    if (this.#stopped) return
    let reason: CloseReason
    if (code === CLOSE.unauthorized) reason = 'unauthorized'
    else if (code === CLOSE.noToken) reason = 'no-token'
    else if (code === CLOSE.lagged) reason = 'lagged'
    else if (!this.#everOpen) reason = 'unreachable'
    else reason = 'dropped'
    // A wrong or missing token will not get better by retrying, and a first
    // connection that never opened needs the user to fix something.
    if (reason === 'unauthorized' || reason === 'no-token' || reason === 'unreachable') {
      this.#stopped = true
      this.#opts.onStatus({ state: 'closed', reason })
      return
    }
    const retryInMs = Math.min(this.#opts.backoffMs * 2 ** this.#attempt, this.#opts.maxBackoffMs)
    this.#attempt += 1
    this.#opts.onStatus({ state: 'reconnecting', reason, attempt: this.#attempt, retryInMs })
    this.#retryTimer = setTimeout(() => {
      this.#retryTimer = null
      if (!this.#stopped) this.#open()
    }, retryInMs)
  }
}
