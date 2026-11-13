import { describe, expect, it } from 'vitest'
import type { EventOf, IncomingEvent } from '@/api/protocol'
import { channelInfo, hello, ME, message, msg, OP, ready, SAM } from '@/test/fake-socket'
import raw from '@/test/fixtures/session.jsonl?raw'
import { focused, initialState, reducer, type Action, type Msg, type State } from './store'

const frames = raw
  .trim()
  .split('\n')
  .map((l) => JSON.parse(l) as IncomingEvent)

/** Plays frames as the session does: `history` replies through their own action, every reply to a typed request shown. */
function play(state: State, evs: IncomingEvent[], show = true): State {
  return evs.reduce((s, ev) => {
    const a: Action =
      ev.event === 'history'
        ? { type: 'history', ev: ev as EventOf<'history'> }
        : { type: 'event', ev, show, now: 0 }
    return reducer(s, a)
  }, state)
}

const ev = (e: object) => e as IncomingEvent
const chan = (s: State, name: string) => s.channels.find((c) => c.name === name)!
const msgs = (s: State, name: string) => chan(s, name).items.filter((i): i is Msg => i.kind === 'msg')
const notices = (s: State, name: string) =>
  chan(s, name).items.filter((i) => i.kind === 'notice').map((i) => (i.kind === 'notice' ? i.ev.event : ''))

describe('the store, fed a recorded serve session', () => {
  const s = play(initialState, frames)
  const alice = (frames.find((f) => f.event === 'hello') as EventOf<'hello'>).node

  it('knows the node, the channels and which one is the DM', () => {
    expect(s.node).toBe(alice)
    expect(s.channels.map((c) => [c.name, c.dm])).toEqual([
      ['agent', true],
      ['garden', false],
    ])
    expect(s.defaultChannel).toBe('agent')
    expect(s.focus).toBe('agent')
    expect(s.bots).toBe(true)
    expect(s.botProvider).toBe('mock')
  })

  it("holds garden's messages once each, in order, merged with a later history page", () => {
    const texts = msgs(s, 'garden').map((m) => m.text)
    expect(texts[0]).toMatch(/^This channel has a bot/)
    expect(texts).toContain('hello from bob')
    expect(texts).toContain('hi bob\nsecond line')
    expect(texts.at(-1)).toBe('echo: @bot what time is it?')
    const ops = msgs(s, 'garden').map((m) => m.op)
    expect(new Set(ops).size).toBe(ops.length)
    const action = msgs(s, 'garden').find((m) => m.text === 'waves')!
    expect(action.action).toBe(true)
    const botReply = msgs(s, 'garden').at(-1)!
    expect(botReply).toMatchObject({ bot: true, mine: true, botClaim: true })
  })

  it('counts unread messages from others and from my bot, not my own', () => {
    // garden was never focused: bob's message and the bot's reply.
    expect(chan(s, 'garden').unread).toBe(2)
    expect(chan(s, 'agent').unread).toBe(0)
  })

  it('tracks members, the bot, the finished turn and the join request', () => {
    const g = chan(s, 'garden')
    expect(g.members).toBe(2)
    expect(g.bot).toEqual({ host: alice, provider: 'mock', servers: ['time', 'web'], enabled: true })
    expect(g.running).toBeNull()
    expect(g.queued).toEqual([])
    expect(g.lastTurn).toMatchObject({ state: 'done', turn_ms: 34 })
    expect(s.asks).toEqual([])
    expect(s.nickname).toBe('alice2')
    expect(s.lastError).toBe('no channel named #nowhere; see /channels')
  })

  it('puts change events and shown replies in the pane, never sync or turn chatter', () => {
    const g = notices(s, 'garden')
    for (const e of ['channel', 'invite', 'bot_on', 'join_request', 'joined', 'members', 'bot', 'grants', 'audit', 'prompt']) {
      expect(g).toContain(e)
    }
    const all = [...g, ...notices(s, 'agent')]
    for (const e of ['sync_started', 'sync_ended', 'job', 'tool_call', 'done', 'hello', 'ready']) {
      expect(all).not.toContain(e)
    }
    // Replies with no channel go to the channel the request was about, or the focus.
    expect(notices(s, 'agent')).toEqual(expect.arrayContaining(['channels', 'info', 'bot_model', 'memory', 'schedules', 'error', 'nick']))
  })

  it('does not show replies to requests the user did not make', () => {
    const quiet = play(initialState, frames, false)
    const g = notices(quiet, 'garden')
    expect(g).not.toContain('members')
    expect(g).not.toContain('bot')
    expect(g).toContain('joined') // a change: shown to everyone
    expect(chan(quiet, 'garden').members).toBe(2) // still applied
    expect(quiet.lastError).toBeNull()
  })
})

describe('the store', () => {
  const base = play(initialState, [ev(hello()), ev(ready())])

  it('focus clears unread; the API has no focus, so focused fields are ignored', () => {
    let s = play(base, [ev(message('garden', 1, 3)), ev(message('garden', 2, 4))])
    expect(chan(s, 'garden').unread).toBe(2)
    s = reducer(s, { type: 'focus', channel: 'garden' })
    expect(s.focus).toBe('garden')
    expect(chan(s, 'garden').unread).toBe(0)
    s = play(s, [ev({ event: 'channel', seq: 9, ...channelInfo('ops', { focused: true }) })])
    expect(s.focus).toBe('garden')
    expect(reducer(s, { type: 'focus', channel: 'nowhere' })).toBe(s)
  })

  it('a later ready keeps the tab focus, replaces the channel list and keeps messages', () => {
    let s = reducer(base, { type: 'focus', channel: 'garden' })
    s = play(s, [ev(message('garden', 1, 3))])
    s = play(s, [ev(ready(['agent', 'garden', 'ops'], 10))])
    expect(s.focus).toBe('garden')
    expect(s.channels.map((c) => c.name)).toEqual(['agent', 'garden', 'ops'])
    expect(msgs(s, 'garden')).toHaveLength(1)
  })

  it('pages history: older pages go first, live messages merge by op, atStart from the page that reaches back', () => {
    let s = reducer(base, {
      type: 'history',
      ev: { event: 'history', seq: 3, channel: 'garden', messages: [msg('garden', 10), msg('garden', 11)], more: true },
    })
    expect(chan(s, 'garden')).toMatchObject({ loaded: true, atStart: false })
    s = play(s, [ev(message('garden', 11, 4)), ev(message('garden', 12, 5))])
    expect(msgs(s, 'garden').map((m) => m.text)).toEqual(['m10', 'm11', 'm12'])
    s = reducer(s, {
      type: 'history',
      before: OP(10),
      ev: { event: 'history', seq: 6, channel: 'garden', messages: [msg('garden', 8), msg('garden', 9)], more: false },
    })
    expect(msgs(s, 'garden').map((m) => m.text)).toEqual(['m8', 'm9', 'm10', 'm11', 'm12'])
    expect(chan(s, 'garden').atStart).toBe(true)
  })

  it('a resync page fills a gap in time order and says nothing about the start', () => {
    let s = reducer(base, {
      type: 'history',
      ev: { event: 'history', seq: 3, channel: 'garden', messages: [msg('garden', 10), msg('garden', 14)], more: true },
    })
    s = reducer(s, {
      type: 'history',
      resync: true,
      ev: { event: 'history', seq: 4, channel: 'garden', messages: [msg('garden', 12), msg('garden', 14), msg('garden', 15)], more: false },
    })
    expect(msgs(s, 'garden').map((m) => m.text)).toEqual(['m10', 'm12', 'm14', 'm15'])
    expect(chan(s, 'garden').atStart).toBe(false)
  })

  it('runs a turn: queued, started, a tool call, done', () => {
    const job = (state: string, extra = {}) =>
      ev({ event: 'job', seq: 1, channel: 'garden', op: OP(5), asker: ME, state, reason: null, ...extra })
    let s = play(base, [job('queued')])
    expect(chan(s, 'garden').queued).toEqual([OP(5)])
    s = reducer(s, { type: 'event', ev: job('started'), show: false, now: 5000 })
    expect(chan(s, 'garden')).toMatchObject({ queued: [], running: { op: OP(5), since: 5000, tool: null } })
    s = play(s, [ev({ event: 'tool_call', seq: 2, channel: 'garden', asker: ME, tool: 'time.now', state: 'sent', op: OP(5) })])
    expect(chan(s, 'garden').running?.tool).toEqual({ name: 'time.now', state: 'sent' })
    s = play(s, [job('done', { turn_ms: 1200 })])
    expect(chan(s, 'garden').running).toBeNull()
    expect(chan(s, 'garden').lastTurn).toMatchObject({ turn_ms: 1200 })
    expect(notices(s, 'garden')).toEqual([])
    s = play(s, [job('failed', { reason: 'model error' })])
    expect(notices(s, 'garden')).toEqual(['job'])
  })

  it('forgets running and queued turns after a reconnect, which says nothing about them', () => {
    let s = play(base, [ev({ event: 'job', seq: 1, channel: 'garden', op: OP(5), asker: ME, state: 'started', reason: null })])
    s = reducer(s, { type: 'status', status: { state: 'reconnecting', reason: 'dropped', attempt: 1, retryInMs: 1000 } })
    expect(chan(s, 'garden').running).not.toBeNull()
    s = reducer(s, { type: 'status', status: { state: 'open', client: 2, node: ME, version: '0.1.0' } })
    expect(chan(s, 'garden').running).toBeNull()
  })

  it('keeps join requests until joined, declined or answered; auto ones are not asked', () => {
    const req = (member: string, auto = false) => ev({ event: 'join_request', seq: 1, channel: 'garden', member, auto })
    let s = play(base, [req(SAM), req(SAM), req(OP(1), true), req(OP(2)), req(OP(3))])
    expect(s.asks.map((a) => a.member)).toEqual([SAM, OP(2), OP(3)])
    s = play(s, [ev({ event: 'joined', seq: 2, channel: 'garden', member: SAM, members: 2, access: 'write' })])
    s = play(s, [ev({ event: 'join_declined', seq: 3, channel: 'garden', member: OP(2), reason: 'declined' })])
    expect(s.asks.map((a) => a.member)).toEqual([OP(3)])
    s = reducer(s, { type: 'answered', channel: 'garden', member: OP(3) })
    expect(s.asks).toEqual([])
  })

  it('a pending channel becomes joined when I am let in', () => {
    let s = play(base, [ev({ event: 'channel', seq: 3, ...channelInfo('ops', { status: 'pending', invite: null, role: 'member' }) })])
    expect(chan(s, 'ops')).toMatchObject({ dm: false, info: { status: 'pending' } })
    s = play(s, [ev({ event: 'joined', seq: 4, channel: 'ops', member: ME, members: 2, access: 'write' })])
    expect(chan(s, 'ops').info.status).toBe('joined')
  })

  it('bot state: the DM keeps none of its own; bots_started and bot_unavailable move the session state', () => {
    let s = play(base, [
      ev({ event: 'bot_on', seq: 3, channel: 'agent', host: ME, provider: 'mock', servers: [] }),
      ev({ event: 'bot_off', seq: 4, channel: 'garden', host: SAM, provider: 'x', servers: ['fs'] }),
    ])
    expect(chan(s, 'agent').bot).toBeNull()
    expect(chan(s, 'garden').bot).toEqual({ host: SAM, provider: 'x', servers: ['fs'], enabled: false })
    s = play(s, [ev({ event: 'bot_unavailable', seq: 5, reason: 'no key; set one with /bot provider-key' })])
    expect(s).toMatchObject({ bots: false, botsOff: 'no key; set one with /bot provider-key' })
    s = play(s, [ev({ event: 'bots_started', seq: 6, provider: 'anthropic', servers: ['time'] })])
    expect(s).toMatchObject({ bots: true, botsOff: null, botProvider: 'anthropic' })
  })

  it('unknown events are kept as notices, so a new command shows before the app knows it', () => {
    const s = play(base, [ev({ event: 'brand_new', seq: 3, re: 4, x: 1 })])
    expect(notices(s, focused(s)!.name)).toEqual(['brand_new'])
  })

  it('local notes and errors go to the named channel or the focus', () => {
    let s = reducer(base, { type: 'local', text: 'hi', now: 1 })
    s = reducer(s, { type: 'local', text: 'bad', error: true, channel: 'garden', now: 2 })
    expect(notices(s, 'agent')).toEqual(['local'])
    expect(notices(s, 'garden')).toEqual(['local'])
    expect(s.lastError).toBe('bad')
    expect(reducer(s, { type: 'clearError' }).lastError).toBeNull()
  })
})
