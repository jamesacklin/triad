import { describe, expect, it } from 'vitest'
import type { EventOf } from '@/api/protocol'
import { COMMANDS, redact, suggest, switchTarget } from './commands'
import { authorLabel, noticeText, turnFigures } from './format'
import type { Msg } from '@/state/store'

const labels = (text: string) => suggest(text, ['agent', 'garden', 'general']).map((s) => s.label)

// The same cases as the TUI's commands.rs tests, less /quit.
describe('suggest', () => {
  it('prefixes list commands', () => {
    expect(labels('hello')).toEqual([])
    expect(labels('/')).toHaveLength(COMMANDS.length)
    expect(labels('/gr')).toEqual(['/grant <node> <server.tool|server.*>', '/grants'])
    expect(labels('/bot')).toEqual(['/bot on', '/bot off', '/bot model [vendor/model]', '/bot provider-key [vendor] <key>'])
    expect(labels('/bot m')).toEqual(['/bot model [vendor/model]'])
    expect(labels('/bot model ')).toEqual(['/bot model [vendor/model]'])
    expect(labels('/bot ')).toEqual(labels('/bot'))
    expect(labels('/qu')).toEqual([])
    expect(labels('/nope')).toEqual([])
  })

  it('usage while typing arguments', () => {
    expect(labels('/bot model open')).toEqual(['/bot model [vendor/model]'])
    expect(labels('/nick')).toEqual(['/nick <name>'])
    expect(labels('/nick sam')).toEqual(['/nick <name>'])
  })

  it('channel names', () => {
    expect(labels('/s g')).toEqual(['/s garden', '/s general'])
    expect(labels('/switch ')).toEqual(['/switch agent', '/switch garden', '/switch general'])
    expect(labels('/switch garden')).toEqual([])
    expect(suggest('/msg ga', ['garden'])[0].complete).toBe('/msg garden ')
    expect(suggest('/bot pro', [])[0].complete).toBe('/bot provider-key ')
  })

  it('hides keys and reads /switch', () => {
    expect(redact('/bot provider-key sk-1')).toBe('/bot provider-key ••••')
    expect(redact('/bot provider-key openai sk-1')).toBe('/bot provider-key openai ••••')
    expect(redact('/bot model x')).toBe('/bot model x')
    expect(switchTarget('/s garden')).toBe('garden')
    expect(switchTarget('/switch #garden ')).toBe('garden')
    expect(switchTarget('/switch')).toBeNull()
    expect(switchTarget('/msg garden hi')).toBeNull()
  })
})

describe('format', () => {
  const m = (over: Partial<Msg>): Msg => ({
    kind: 'msg', op: 'o', ts: 0, author: 'bb11223344', text: 't', nickname: null,
    mine: false, bot: false, action: false, botClaim: false, ...over,
  })
  it('labels authors as the plain prompt does', () => {
    expect(authorLabel(m({ mine: true }), false)).toBe('me')
    expect(authorLabel(m({ nickname: 'sam' }), false)).toBe('sam')
    expect(authorLabel(m({}), false)).toBe('bb112233')
    expect(authorLabel(m({ bot: true, botClaim: true, mine: true }), true)).toBe('my bot')
    expect(authorLabel(m({ bot: true, botClaim: true }), false)).toBe('bot')
    expect(authorLabel(m({ botClaim: true, nickname: 'eve' }), false)).toBe('eve (bot claim not accepted)')
  })

  it('reads a finished turn as it really arrives (denied a list, tokens null)', () => {
    const job = { event: 'job', state: 'done', turn_ms: 1234, model_ms: 1000, tool_ms: 0, requests: 1, input_tokens: null, output_tokens: null, denied: [] } as unknown as EventOf<'job'>
    expect(turnFigures(job)).toBe('last turn 1.2s (model 1.0s, tools 0.0s) · 1 request')
    const real = { ...job, requests: 2, input_tokens: 900, output_tokens: 40, denied: ['fs.write'] } as unknown as EventOf<'job'>
    expect(turnFigures(real)).toBe('last turn 1.2s (model 1.0s, tools 0.0s) · 2 requests · 900→40 tokens · 1 denied')
  })

  it('words events as the session does', () => {
    const me = 'aa'.repeat(32)
    expect(noticeText({ event: 'joined', seq: 1, channel: 'g', member: me, members: 2, access: 'write' }, me)).toBe('you joined (2 members)')
    expect(noticeText({ event: 'bot_off', seq: 1, channel: 'g', host: me, provider: 'mock', servers: [] }, me)).toBe('bot off (me)')
    expect(noticeText({ event: 'members', seq: 1, channel: 'g', members: [], access: {} }, me)).toBeNull()
    expect(noticeText({ event: 'brand_new', seq: 1 }, me)).toBeNull()
  })
})
