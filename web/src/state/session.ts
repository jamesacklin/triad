// Wires a TriadClient to the store: turns socket events into actions, asks
// for history when a channel is shown, resyncs after a reconnect, and sends
// what the user types. It follows API.md's recipes and holds no session
// logic: anything the session decides comes back as an event.
import {
  RequestError,
  TriadClient,
  type ClientOptions,
  type ConnectionStatus,
  type Reply,
} from '@/api/client'
import type { EventOf, IncomingEvent, RequestBody } from '@/api/protocol'
import { oldestOp, type Action, type State } from './store'

export const PAGE = 50

export interface SessionOptions {
  url: string
  token: string
  dispatch: (a: Action) => void
  getState: () => State
  socket?: ClientOptions['socket']
  now?: () => number
  backoffMs?: number
}

export class Session {
  readonly client: TriadClient
  #dispatch: (a: Action) => void
  #state: () => State
  #now: () => number
  /** Requests whose replies belong in the pane (the user typed them), with the channel they were about. */
  #shown = new Map<number | string, string | undefined>()
  /** Channels with a history request in flight, so scrolling does not ask twice. */
  #paging = new Set<string>()
  /** Set by a reconnect, consumed by the next `ready`. (`serve` sends `ready` twice on connect, so counting them would not do.) */
  #resyncDue = false
  #syncing = false

  constructor(opts: SessionOptions) {
    this.#dispatch = opts.dispatch
    this.#state = opts.getState
    this.#now = opts.now ?? Date.now
    this.client = new TriadClient({
      url: opts.url,
      token: opts.token,
      socket: opts.socket,
      backoffMs: opts.backoffMs,
      onEvent: (ev) => this.#onEvent(ev),
      onStatus: (s) => this.#onStatus(s),
    })
  }

  connect(): void {
    this.client.connect()
  }

  close(): void {
    this.client.close()
  }

  #onStatus(status: ConnectionStatus): void {
    if (status.state !== 'open') this.#paging.clear()
    if (status.state === 'reconnecting') this.#resyncDue = true
    this.#dispatch({ type: 'status', status })
  }

  #onEvent(ev: IncomingEvent): void {
    const re = ev.re
    const shown = re !== undefined && this.#shown.has(re)
    const target = re !== undefined ? this.#shown.get(re) : undefined
    if (ev.event === 'done' && re !== undefined) this.#shown.delete(re)
    if (ev.event === 'history') return // applied by the request that asked for it
    this.#dispatch({ type: 'event', ev, show: shown, target, now: this.#now() })
    if (ev.event === 'ready') void this.#afterReady()
  }

  /** On every `ready`: load the focused channel; after a reconnect, resync every channel already shown. */
  async #afterReady(): Promise<void> {
    const reconnect = this.#resyncDue
    this.#resyncDue = false
    if (reconnect && !this.#syncing) {
      this.#syncing = true
      try {
        for (const c of this.#state().channels) {
          if (c.loaded) await this.resync(c.name)
        }
      } finally {
        this.#syncing = false
      }
    }
    const f = this.#state().focus
    if (f) await this.show(f)
  }

  /** Focus a channel in this tab, loading its newest page if it has none. */
  async show(channel: string): Promise<void> {
    this.#dispatch({ type: 'focus', channel })
    const c = this.#state().channels.find((x) => x.name === channel)
    if (!c || c.loaded || this.#paging.has(channel)) return
    await this.#page(channel)
    // The member count and the channel's bot are not in the channel object;
    // ask once, quietly (the replies update state and are not shown).
    const after = this.#state().channels.find((x) => x.name === channel)
    if (after && after.info.status === 'joined' && !after.dm) {
      await this.#quiet({ op: 'command', channel, line: '/members' })
      await this.#quiet({ op: 'command', channel, line: '/bot' })
    }
  }

  /** Scrolled to the top: fetch the page before the oldest message held. */
  async older(channel: string): Promise<void> {
    const c = this.#state().channels.find((x) => x.name === channel)
    if (!c || c.atStart || !c.loaded || this.#paging.has(channel)) return
    const before = oldestOp(c)
    if (!before) return
    await this.#page(channel, before)
  }

  /**
   * API.md's reconnect recipe for one channel: the newest page, then older
   * pages until one overlaps what is already held. Merged by `op`.
   */
  async resync(channel: string): Promise<void> {
    const held = this.#state().channels.find((x) => x.name === channel)?.ops ?? new Set<string>()
    let before: string | undefined
    for (;;) {
      const page = await this.#page(channel, before, false)
      if (!page || !page.more || page.messages.length === 0) return
      if (page.messages.some((m) => held.has(m.op))) return
      before = page.messages[0].op
    }
  }

  async #page(channel: string, before?: string, mark = true): Promise<EventOf<'history'> | null> {
    if (mark) this.#paging.add(channel)
    try {
      const body: RequestBody = before
        ? { op: 'history', channel, limit: PAGE, before }
        : { op: 'history', channel, limit: PAGE }
      const reply = await this.client.request(body)
      const ev = reply.events.find((e): e is EventOf<'history'> => e.event === 'history')
      if (!ev) return null
      this.#dispatch({ type: 'history', ev, before, resync: !mark })
      return ev
    } catch (e) {
      if (e instanceof RequestError && e.message !== 'connection lost' && e.message !== 'closed') {
        this.#dispatch({ type: 'local', channel, text: `history: ${e.message}`, error: true, now: this.#now() })
      }
      return null
    } finally {
      if (mark) this.#paging.delete(channel)
    }
  }

  async #quiet(body: RequestBody): Promise<Reply | null> {
    try {
      return await this.client.request(body)
    } catch {
      return null
    }
  }

  /** Sends a request whose replies are shown in the pane. Resolves with the reply, or null if it failed (the error is shown). */
  async ask(body: RequestBody, channel?: string): Promise<Reply | null> {
    // request() sends synchronously, so nothing for this id can arrive
    // before the map has it.
    const id = this.client.nextId
    this.#shown.set(id, channel)
    try {
      return await this.client.request(body)
    } catch (e) {
      this.#shown.delete(id)
      const text = e instanceof Error ? e.message : String(e)
      // A failure the session reported already arrived as an `error` event.
      const reported = e instanceof RequestError && e.events.some((x) => x.event === 'error')
      if (!reported) this.#dispatch({ type: 'local', channel, text, error: true, now: this.#now() })
      return null
    }
  }

  /** Plain text to a channel. It appears when the session's `message` event arrives. */
  send(channel: string, text: string, action = false): Promise<Reply | null> {
    return this.ask(action ? { op: 'send', channel, text, action } : { op: 'send', channel, text }, channel)
  }

  /** A `/command` line, run as if typed with `channel` focused. */
  command(channel: string | undefined, line: string): Promise<Reply | null> {
    return this.ask(channel ? { op: 'command', channel, line } : { op: 'command', line }, channel)
  }

  async answer(channel: string, member: string, accept: boolean): Promise<boolean> {
    const r = await this.ask({ op: 'answer', channel, member, accept }, channel)
    if (r) this.#dispatch({ type: 'answered', channel, member })
    return r !== null
  }
}
