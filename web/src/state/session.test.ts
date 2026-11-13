import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fakeSockets, hello, message, msg, OP, ready } from '@/test/fake-socket'
import { Session } from './session'
import { initialState, reducer, type Action, type Msg, type State } from './store'

function setup() {
  const fake = fakeSockets()
  let state: State = initialState
  const session = new Session({
    url: 'ws://127.0.0.1:7878/v1',
    token: 't'.repeat(64),
    socket: fake.factory,
    dispatch: (a: Action) => {
      state = reducer(state, a)
    },
    getState: () => state,
    now: () => 0,
  })
  const s = () => fake.current()
  const reply = (frames: object[]) => {
    const id = s().last().id
    for (const f of frames) s().emit({ seq: 0, re: id, ...f })
    s().emit({ event: 'done', ok: true, re: id, seq: 0 })
  }
  const fail = (text: string) => {
    const id = s().last().id
    s().emit({ event: 'error', text, re: id, seq: 0 })
    s().emit({ event: 'done', ok: false, error: text, re: id, seq: 0 })
  }
  return { fake, session, s, reply, fail, state: () => state }
}

const texts = (st: State, ch: string) =>
  st.channels.find((c) => c.name === ch)!.items.filter((i): i is Msg => i.kind === 'msg').map((m) => m.text)
const history = (channel: string, ns: number[], more: boolean) => ({
  event: 'history',
  channel,
  messages: ns.map((n) => msg(channel, n)),
  more,
})

beforeEach(() => vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] }))
afterEach(() => vi.useRealTimers())

async function connected() {
  const t = setup()
  t.session.connect()
  t.s().open()
  t.s().emit(hello())
  t.s().emit(ready(['agent', 'garden']))
  await vi.advanceTimersByTimeAsync(0)
  // ready → the focused channel's newest page.
  expect(t.s().last()).toMatchObject({ op: 'history', channel: 'agent', limit: 50 })
  expect(t.s().last()).not.toHaveProperty('before')
  t.reply([history('agent', [1, 2], false)])
  await vi.advanceTimersByTimeAsync(0)
  return t
}

describe('Session', () => {
  it('loads the focused channel on ready; the DM gets no member or bot query', async () => {
    const t = await connected()
    expect(texts(t.state(), 'agent')).toEqual(['m1', 'm2'])
    expect(t.state().channels[0]).toMatchObject({ loaded: true, atStart: true })
    expect(t.s().requests()).toHaveLength(1)
  })

  it('showing a channel loads its newest page once, then asks quietly for members and bot', async () => {
    const t = await connected()
    void t.session.show('garden')
    expect(t.state().focus).toBe('garden')
    expect(t.s().last()).toMatchObject({ op: 'history', channel: 'garden' })
    void t.session.show('garden') // already in flight: no second request
    expect(t.s().requests()).toHaveLength(2)
    t.reply([history('garden', [10, 11], true)])
    await vi.advanceTimersByTimeAsync(0)
    expect(t.s().last()).toMatchObject({ op: 'command', channel: 'garden', line: '/members' })
    t.reply([{ event: 'members', channel: 'garden', members: ['a', 'b', 'c'], access: {} }])
    await vi.advanceTimersByTimeAsync(0)
    expect(t.s().last()).toMatchObject({ op: 'command', channel: 'garden', line: '/bot' })
    t.reply([{ event: 'bot', channel: 'garden', host: null, provider: null, servers: [], enabled: false, my_tools: 'none' }])
    await vi.advanceTimersByTimeAsync(0)
    const g = t.state().channels.find((c) => c.name === 'garden')!
    expect(g.members).toBe(3)
    expect(g.bot).toMatchObject({ enabled: false })
    expect(g.items.filter((i) => i.kind === 'notice')).toEqual([]) // quiet: not shown
    void t.session.show('agent')
    void t.session.show('garden')
    expect(t.s().requests()).toHaveLength(4) // loaded already
  })

  it('scrolling to the top pages back from the oldest op until the start', async () => {
    const t = await connected()
    void t.session.show('garden')
    t.reply([history('garden', [10, 11], true)])
    await vi.advanceTimersByTimeAsync(0)
    t.reply([])
    await vi.advanceTimersByTimeAsync(0)
    t.reply([])
    await vi.advanceTimersByTimeAsync(0)
    void t.session.older('garden')
    expect(t.s().last()).toMatchObject({ op: 'history', channel: 'garden', before: OP(10), limit: 50 })
    t.s().emit(message('garden', 12, 50)) // live, while paging
    t.reply([history('garden', [8, 9], false)])
    await vi.advanceTimersByTimeAsync(0)
    expect(texts(t.state(), 'garden')).toEqual(['m8', 'm9', 'm10', 'm11', 'm12'])
    const n = t.s().requests().length
    void t.session.older('garden')
    expect(t.s().requests()).toHaveLength(n) // at the start
  })

  it('after a reconnect: resyncs every loaded channel, paging back until it overlaps, merged by op', async () => {
    const t = await connected()
    t.s().drop(1001) // serve restarted
    expect(t.state().status).toMatchObject({ state: 'reconnecting' })
    await vi.advanceTimersByTimeAsync(1000)
    t.s().open()
    t.s().emit(hello())
    t.s().emit(ready(['agent', 'garden']))
    await vi.advanceTimersByTimeAsync(0)
    // agent held m1, m2; four arrived meanwhile, so the newest page does not overlap.
    expect(t.s().last()).toMatchObject({ op: 'history', channel: 'agent' })
    expect(t.s().last()).not.toHaveProperty('before')
    t.reply([history('agent', [5, 6], true)])
    await vi.advanceTimersByTimeAsync(0)
    expect(t.s().last()).toMatchObject({ op: 'history', channel: 'agent', before: OP(5) })
    t.reply([history('agent', [2, 3, 4], true)])
    await vi.advanceTimersByTimeAsync(0)
    expect(texts(t.state(), 'agent')).toEqual(['m1', 'm2', 'm3', 'm4', 'm5', 'm6'])
    // garden was never loaded: nothing to resync. The focus was loaded already.
    expect(t.s().requests().filter((r) => r.channel === 'garden')).toEqual([])
  })

  it('a second ready on one connection (serve sends two at start) is not a reconnect', async () => {
    const t = await connected()
    t.s().emit(ready(['agent', 'garden'], 3))
    await vi.advanceTimersByTimeAsync(0)
    expect(t.s().requests()).toHaveLength(1)
  })

  it('typed requests: their replies are shown in the channel they were about; failures leave the error', async () => {
    const t = await connected()
    const p = t.session.command('garden', '/members')
    expect(t.s().last()).toEqual({ id: 2, op: 'command', channel: 'garden', line: '/members' })
    t.reply([{ event: 'members', channel: 'garden', members: [], access: {} }])
    expect(await p).not.toBeNull()
    const p2 = t.session.command('garden', '/nosuch')
    t.fail('unknown command /nosuch')
    expect(await p2).toBeNull()
    const garden = t.state().channels.find((c) => c.name === 'garden')!
    expect(garden.items.map((i) => (i.kind === 'notice' ? i.ev.event : i.kind))).toEqual(['members', 'error'])
    expect(t.state().lastError).toBe('unknown command /nosuch')
    // A reply with no channel field goes to the channel the command named.
    const p3 = t.session.command('garden', '/memory')
    t.reply([{ event: 'memory', query: '', memories: [] }])
    await p3
    expect(t.state().channels.find((c) => c.name === 'garden')!.items).toHaveLength(3)
  })

  it('send and answer', async () => {
    const t = await connected()
    const p = t.session.send('garden', 'hello\nthere')
    expect(t.s().last()).toEqual({ id: 2, op: 'send', channel: 'garden', text: 'hello\nthere' })
    t.reply([])
    await p
    void t.session.send('garden', 'waves', true)
    expect(t.s().last()).toMatchObject({ op: 'send', action: true })
    t.reply([])
    t.s().emit({ event: 'join_request', channel: 'garden', member: 'b'.repeat(64), auto: false, seq: 9 })
    expect(t.state().asks).toHaveLength(1)
    const a = t.session.answer('garden', 'b'.repeat(64), false)
    expect(t.s().last()).toMatchObject({ op: 'answer', channel: 'garden', member: 'b'.repeat(64), accept: false })
    t.reply([])
    expect(await a).toBe(true)
    expect(t.state().asks).toEqual([])
  })

  it('a request when the socket is down fails with a note, not silently', async () => {
    const t = await connected()
    t.s().drop(1006)
    expect(await t.session.send('garden', 'hi')).toBeNull()
    expect(t.state().lastError).toBe('not connected')
  })
})
