// The app's one store: a reducer fed by the socket's events. It keeps what
// the session told it and the tab's own view state (focus, unread counts).
// It decides nothing on the session's behalf.
import type {
  ChannelInfo,
  EventOf,
  HistoryMessage,
  IncomingEvent,
} from '@/api/protocol'
import type { ConnectionStatus } from '@/api/client'

/** A chat message, from `message` or `history`. */
export interface Msg {
  kind: 'msg'
  op: string
  ts: number
  author: string
  text: string
  nickname: string | null
  mine: boolean
  bot: boolean
  action: boolean
  botClaim: boolean
}

/** Anything else shown in a channel's pane: an event, or a note of the app's own. */
export interface Notice {
  kind: 'notice'
  key: string
  /** Receipt time, for ordering against stored messages. */
  ts: number
  /** The event to render, or a note of the app's own. */
  ev: IncomingEvent | { event: 'local'; text: string; error?: boolean }
}

export type Item = Msg | Notice

/**
 * A message this tab sent that the session has not yet echoed back as a
 * `message` event. Shown greyed out after the channel's items.
 *
 * - `sending`: the `send` request is out.
 * - `sent`: its `done` was ok; waiting for the `message` event.
 * - `unconfirmed`: no echo within 30 s, or the socket dropped before
 *   `done`. It may or may not have gone out; a later echo or history page
 *   still settles it.
 * - `failed`: `done` said `ok: false`; it was not published.
 */
export interface Pending {
  id: number
  text: string
  action: boolean
  /** When it was sent, by this tab's clock (ms since 1970). */
  at: number
  state: 'sending' | 'sent' | 'unconfirmed' | 'failed'
  error: string | null
}

/** How long a pending message waits for its echo before showing as unconfirmed. */
export const PENDING_TIMEOUT_MS = 30_000
/**
 * How much older than the send an echo may claim to be and still settle it
 * (the `ts` is the sending node's clock, which may differ from the tab's).
 */
const SETTLE_SKEW_MS = 10 * 60_000

export interface Bot {
  host: string | null
  provider: string | null
  servers: string[]
  enabled: boolean
}

export interface Turn {
  op: string
  /** When this tab saw it start (ms since 1970). */
  since: number
  tool: { name: string; state: string } | null
}

export interface Channel {
  name: string
  info: ChannelInfo
  /**
   * The personal bot's DM. Not a field of the protocol: API.md says a
   * channel's `invite` is null until joined "and always for the bot DM",
   * so a joined channel with no invite is the DM. See FINDINGS.md.
   */
  dm: boolean
  items: Item[]
  ops: ReadonlySet<string>
  /** The newest history page has been fetched. */
  loaded: boolean
  /** No older messages remain. */
  atStart: boolean
  /** A page of history is being fetched. */
  paging: boolean
  /** Messages sent from this tab, not yet echoed, oldest first. */
  pending: Pending[]
  unread: number
  members: number | null
  bot: Bot | null
  queued: string[]
  running: Turn | null
  lastTurn: EventOf<'job'> | null
}

export interface JoinAsk {
  channel: string
  member: string
}

export interface State {
  status: ConnectionStatus
  /** This node, from `ready`. */
  node: string | null
  /** The session's default channel (`ready.channel`): where a command with no channel acts. */
  defaultChannel: string | null
  /** Sidebar order: the DM first, then by name. */
  channels: Channel[]
  /** The tab's focused channel. The API has no focus; this is the tab's own. */
  focus: string | null
  bots: boolean
  botProvider: string | null
  botServers: string[]
  botsOff: string | null
  asks: JoinAsk[]
  lastError: string | null
  nickname: string | null
  noticeSeq: number
}

export const initialState: State = {
  status: { state: 'connecting' },
  node: null,
  defaultChannel: null,
  channels: [],
  focus: null,
  bots: false,
  botProvider: null,
  botServers: [],
  botsOff: null,
  asks: [],
  lastError: null,
  nickname: null,
  noticeSeq: 0,
}

export type Action =
  | { type: 'status'; status: ConnectionStatus }
  /**
   * An event from the socket. `show` says whether a reply belongs in the
   * pane (it answers something the user typed); `target` is the channel the
   * request was about, for replies that do not name one.
   */
  | { type: 'event'; ev: IncomingEvent; show: boolean; target?: string; now: number }
  /**
   * A `history` reply; `before` is the `op` the request paged from. A
   * resync page (after a reconnect) never says where the start is.
   */
  | { type: 'history'; ev: EventOf<'history'>; before?: string; resync?: boolean }
  | { type: 'focus'; channel: string }
  | { type: 'paging'; channel: string; on: boolean }
  /** A message was sent from this tab: show it at once, greyed out. */
  | { type: 'pending'; channel: string; id: number; text: string; action: boolean; now: number }
  /**
   * Its `send` request ended: ok, failed with the session's error, or cut
   * off by the socket (`lost`: it may or may not have gone out).
   */
  | { type: 'pendingDone'; channel: string; id: number; ok: boolean; error?: string; lost?: boolean }
  /** {@link PENDING_TIMEOUT_MS} passed since it was sent. */
  | { type: 'pendingStale'; channel: string; id: number }
  | { type: 'pendingRetry'; channel: string; id: number; now: number }
  | { type: 'pendingDiscard'; channel: string; id: number }
  | { type: 'local'; text: string; error?: boolean; channel?: string; now: number }
  | { type: 'answered'; channel: string; member: string }
  | { type: 'clearError' }
  | { type: 'reset' }

// Events that only move state, never a line in the pane (the TUI shows sync
// lines only with --verbose, and turns in the status line).
const SILENT = new Set(['hello', 'ready', 'done', 'history', 'lagged', 'sync_started', 'sync_ended', 'tool_call', 'snapshot', 'switched'])

// Events routed to every client (API.md, "Who receives what"). These are
// changes, shown to everyone; every other event is a reply, shown only to
// the client whose user asked.
export const BROADCAST = new Set([
  'message', 'joined', 'promoted', 'removed', 'channel', 'nick', 'granted', 'revoked',
  'blocked', 'unblocked', 'bot_on', 'bot_off', 'bots_started', 'bot_unavailable',
  'bot_rejected', 'job', 'tool_call', 'schedule_fired', 'schedule_missed', 'unscheduled',
  'forgot', 'join_request', 'join_declined', 'sync_started', 'sync_ended',
])

function isDm(info: ChannelInfo): boolean {
  return info.status === 'joined' && info.invite === null
}

function newChannel(info: ChannelInfo): Channel {
  return {
    name: info.channel,
    info,
    dm: isDm(info),
    items: [],
    ops: new Set(),
    loaded: false,
    atStart: false,
    paging: false,
    pending: [],
    unread: 0,
    members: null,
    bot: null,
    queued: [],
    running: null,
    lastTurn: null,
  }
}

function sortChannels(cs: Channel[]): Channel[] {
  return [...cs].sort((a, b) =>
    a.dm !== b.dm ? (a.dm ? -1 : 1) : a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
  )
}

/** Adds or updates channels from channel objects. `replace` drops channels not listed. */
function upsertChannels(state: State, infos: ChannelInfo[], replace: boolean): Channel[] {
  const byName = new Map(state.channels.map((c) => [c.name, c]))
  const out: Channel[] = replace ? [] : [...state.channels]
  for (const info of infos) {
    const old = byName.get(info.channel)
    const next = old ? { ...old, info, dm: isDm(info) } : newChannel(info)
    if (replace) out.push(next)
    else {
      const i = out.findIndex((c) => c.name === info.channel)
      if (i >= 0) out[i] = next
      else out.push(next)
    }
  }
  return sortChannels(out)
}

function updateChannel(state: State, name: string | undefined, f: (c: Channel) => Channel): State {
  if (!name) return state
  const i = state.channels.findIndex((c) => c.name === name)
  if (i < 0) return state
  const channels = [...state.channels]
  channels[i] = f(channels[i])
  return { ...state, channels }
}

function toMsg(m: HistoryMessage): Msg {
  return {
    kind: 'msg',
    op: m.op,
    ts: m.ts,
    author: m.author,
    text: m.text,
    nickname: m.nickname,
    mine: m.mine,
    bot: m.bot,
    action: m.action,
    botClaim: m.bot || (m.bot_claim !== undefined && m.bot_claim !== null),
  }
}

/** Inserts `item` before the first item newer than it, so a page fetched late lands in time order. */
function insertByTs(items: Item[], item: Item): Item[] {
  let i = items.length
  while (i > 0 && items[i - 1].ts > item.ts) i--
  return [...items.slice(0, i), item, ...items.slice(i)]
}

/**
 * Settles the oldest pending message that `m` is the echo of. The API does
 * not link a `send` to its `message`, so the match is on what both carry:
 * this node's own authorship (not its bot's), the text and the action flag,
 * in the same channel. Oldest first, so two identical sends settle one
 * each. A failed send was not published, so it never matches.
 */
function settle(pending: Pending[], m: HistoryMessage): Pending[] {
  if (!m.mine || m.bot) return pending
  const i = pending.findIndex(
    (p) => p.state !== 'failed' && p.text === m.text && p.action === m.action && m.ts >= p.at - SETTLE_SKEW_MS,
  )
  return i < 0 ? pending : [...pending.slice(0, i), ...pending.slice(i + 1)]
}

/** Merges messages by `op`: any already held are dropped. Each new one may settle a pending send. */
function mergeMessages(c: Channel, msgs: HistoryMessage[]): Channel {
  let items = c.items
  let pending = c.pending
  const ops = new Set(c.ops)
  for (const m of msgs) {
    if (ops.has(m.op)) continue
    ops.add(m.op)
    items = insertByTs(items, toMsg(m))
    pending = settle(pending, m)
  }
  return { ...c, items, ops, pending }
}

/** Applies `f` to one pending message (null removes it); the same state when nothing changes. */
function updatePending(state: State, channel: string, id: number, f: (p: Pending) => Pending | null): State {
  const c = state.channels.find((x) => x.name === channel)
  const i = c ? c.pending.findIndex((p) => p.id === id) : -1
  if (!c || i < 0) return state
  const next = f(c.pending[i])
  if (next === c.pending[i]) return state
  const pending = [...c.pending]
  if (next) pending[i] = next
  else pending.splice(i, 1)
  return updateChannel(state, channel, (x) => ({ ...x, pending }))
}

function addNotice(state: State, channel: string | undefined | null, ev: Notice['ev'], now: number): State {
  const name =
    (channel && state.channels.some((c) => c.name === channel) ? channel : null) ?? state.focus
  const key = `n${state.noticeSeq}`
  const next = { ...state, noticeSeq: state.noticeSeq + 1 }
  if (!name) return next
  return updateChannel(next, name, (c) => ({
    ...c,
    items: [...c.items, { kind: 'notice', key, ts: now, ev }],
  }))
}

function str(ev: IncomingEvent, k: string): string | undefined {
  const v = (ev as Record<string, unknown>)[k]
  return typeof v === 'string' ? v : undefined
}

function onEvent(state: State, ev: IncomingEvent, show: boolean, target: string | undefined, now: number): State {
  const channel = str(ev, 'channel')
  const broadcast = BROADCAST.has(ev.event)
  // A reply to someone else's request never reaches this client, and a
  // reply to the app's own (silent) request is applied but not shown.
  const visible = !SILENT.has(ev.event) && (broadcast || show || ev.re === undefined)
  const notice = (s: State) => (visible ? addNotice(s, channel ?? target, ev, now) : s)

  switch (ev.event) {
    case 'ready': {
      const r = ev as EventOf<'ready'>
      const channels = upsertChannels(state, r.channels, true)
      const focus =
        state.focus && channels.some((c) => c.name === state.focus) ? state.focus : r.channel
      return {
        ...state,
        node: r.node,
        defaultChannel: r.channel,
        channels: channels.map((c) => (c.name === focus ? { ...c, unread: 0 } : c)),
        focus,
        bots: r.bots,
        botProvider: r.bots ? (r.bot_provider ?? null) : state.botProvider,
        botServers: r.bots ? (r.bot_servers ?? []) : state.botServers,
      }
    }
    case 'channel': {
      const info = ev as EventOf<'channel'>
      const { event: _e, seq: _s, re: _r, ...rest } = info
      return notice({ ...state, channels: upsertChannels(state, [rest as ChannelInfo], false) })
    }
    case 'channels': {
      const list = (ev as EventOf<'channels'>).channels
      return notice({ ...state, channels: upsertChannels(state, list, false) })
    }
    case 'message': {
      const m = ev as EventOf<'message'>
      return updateChannel(state, m.channel, (c) => {
        if (c.ops.has(m.op)) return c
        const merged = mergeMessages(c, [m])
        // My own lines are not unread; my bot's replies are.
        const unread = state.focus !== c.name && (!m.mine || m.bot) ? c.unread + 1 : c.unread
        return { ...merged, unread }
      })
    }
    case 'joined':
    case 'removed':
    case 'promoted': {
      const e = ev as EventOf<'joined'> | EventOf<'removed'> | EventOf<'promoted'>
      let s = updateChannel(state, e.channel, (c) => {
        const members = 'members' in e && typeof e.members === 'number' ? e.members : c.members
        const joinedMe = e.event === 'joined' && e.member === state.node
        const info: ChannelInfo = joinedMe ? { ...c.info, status: 'joined' } : c.info
        return { ...c, members, info }
      })
      if (e.event === 'joined') {
        s = { ...s, asks: s.asks.filter((a) => !(a.channel === e.channel && a.member === e.member)) }
      }
      return notice(s)
    }
    case 'members': {
      const e = ev as EventOf<'members'>
      return notice(updateChannel(state, e.channel, (c) => ({ ...c, members: e.members.length })))
    }
    case 'join_request': {
      const e = ev as EventOf<'join_request'>
      const asks =
        e.auto || state.asks.some((a) => a.channel === e.channel && a.member === e.member)
          ? state.asks
          : [...state.asks, { channel: e.channel, member: e.member }]
      return notice({ ...state, asks })
    }
    case 'join_declined': {
      const e = ev as EventOf<'join_declined'>
      return notice({
        ...state,
        asks: state.asks.filter((a) => !(a.channel === e.channel && a.member === e.member)),
      })
    }
    case 'error': {
      const text = (ev as EventOf<'error'>).text
      return notice(visible ? { ...state, lastError: text } : state)
    }
    case 'job':
      return onJob(state, ev as EventOf<'job'>, now, visible)
    case 'tool_call': {
      const e = ev as EventOf<'tool_call'>
      return updateChannel(state, e.channel, (c) =>
        c.running && c.running.op === e.op
          ? { ...c, running: { ...c.running, tool: { name: e.tool, state: e.state } } }
          : c,
      )
    }
    case 'bot_on':
    case 'bot_off':
    case 'bot': {
      const e = ev as EventOf<'bot_on'> | EventOf<'bot_off'> | EventOf<'bot'>
      const enabled = e.event === 'bot' ? e.enabled : e.event === 'bot_on'
      const bot: Bot = { host: e.host, provider: e.provider, servers: e.servers, enabled }
      return notice(updateChannel(state, e.channel, (c) => (c.dm ? c : { ...c, bot })))
    }
    case 'bots_started': {
      const e = ev as EventOf<'bots_started'>
      return notice({ ...state, bots: true, botsOff: null, botProvider: e.provider, botServers: e.servers })
    }
    case 'bot_unavailable': {
      const e = ev as EventOf<'bot_unavailable'>
      return notice({ ...state, bots: false, botsOff: e.reason })
    }
    case 'nick':
      return notice({ ...state, nickname: (ev as EventOf<'nick'>).nickname })
    default:
      return notice(state)
  }
}

/** A bot job changed state. Routine transitions only move the status bar; a refusal or failure also leaves a line. */
function onJob(state: State, e: EventOf<'job'>, now: number, visible: boolean): State {
  const s = updateChannel(state, e.channel, (c) => {
    if (e.state === 'queued') {
      return c.queued.includes(e.op) ? c : { ...c, queued: [...c.queued, e.op] }
    }
    const queued = c.queued.filter((op) => op !== e.op)
    if (e.state === 'started') {
      return { ...c, queued, running: { op: e.op, since: now, tool: null } }
    }
    const running = c.running && c.running.op === e.op ? null : c.running
    return { ...c, queued, running, lastTurn: e.state === 'done' ? e : c.lastTurn }
  })
  if (visible && (e.state === 'refused' || e.state === 'failed')) {
    return addNotice(s, e.channel, e, now)
  }
  return s
}

export function reducer(state: State, action: Action): State {
  switch (action.type) {
    case 'status': {
      const s = { ...state, status: action.status }
      // After a reconnect nothing says which turns are still running or
      // queued (no replay, and `ready` does not carry them): forget them
      // rather than show a clock that never stops. See FINDINGS.md.
      if (action.status.state === 'open' && state.status.state === 'reconnecting') {
        return { ...s, channels: s.channels.map((c) => ({ ...c, running: null, queued: [] })) }
      }
      return s
    }
    case 'event':
      return onEvent(state, action.ev, action.show, action.target, action.now)
    case 'history': {
      const { ev, before, resync } = action
      return updateChannel(state, ev.channel, (c) => {
        const oldest = c.items.find((i): i is Msg => i.kind === 'msg')?.op
        const merged = mergeMessages(c, ev.messages)
        // Only a page reaching past what was held says where the start is.
        const reachesBack =
          !resync && (before === undefined ? !c.loaded || oldest === undefined : before === oldest)
        return {
          ...merged,
          loaded: true,
          atStart: reachesBack ? !ev.more : c.atStart,
        }
      })
    }
    case 'focus': {
      if (!state.channels.some((c) => c.name === action.channel)) return state
      return updateChannel({ ...state, focus: action.channel }, action.channel, (c) => ({ ...c, unread: 0 }))
    }
    case 'paging':
      return updateChannel(state, action.channel, (c) => (c.paging === action.on ? c : { ...c, paging: action.on }))
    case 'pending': {
      const p: Pending = { id: action.id, text: action.text, action: action.action, at: action.now, state: 'sending', error: null }
      return updateChannel(state, action.channel, (c) => ({ ...c, pending: [...c.pending, p] }))
    }
    case 'pendingDone':
      return updatePending(state, action.channel, action.id, (p) => {
        if (action.ok) return p.state === 'sending' ? { ...p, state: 'sent' } : p
        if (action.lost) {
          return { ...p, state: 'unconfirmed', error: 'the connection dropped before the server answered' }
        }
        return { ...p, state: 'failed', error: action.error ?? 'not sent' }
      })
    case 'pendingStale':
      return updatePending(state, action.channel, action.id, (p) =>
        p.state === 'sending' || p.state === 'sent'
          ? { ...p, state: 'unconfirmed', error: p.error ?? 'no answer within 30 seconds' }
          : p,
      )
    case 'pendingRetry':
      return updatePending(state, action.channel, action.id, (p) => ({ ...p, state: 'sending', error: null, at: action.now }))
    case 'pendingDiscard':
      return updatePending(state, action.channel, action.id, () => null)
    case 'local': {
      const s = action.error ? { ...state, lastError: action.text } : state
      return addNotice(s, action.channel, { event: 'local', text: action.text, error: action.error }, action.now)
    }
    case 'answered':
      return {
        ...state,
        asks: state.asks.filter((a) => !(a.channel === action.channel && a.member === action.member)),
      }
    case 'clearError':
      return state.lastError === null ? state : { ...state, lastError: null }
    case 'reset':
      return initialState
  }
}

export function focused(state: State): Channel | null {
  return state.channels.find((c) => c.name === state.focus) ?? null
}

/** The oldest message `op` held for a channel: where the next older page starts. */
export function oldestOp(c: Channel): string | undefined {
  return c.items.find((i): i is Msg => i.kind === 'msg')?.op
}
