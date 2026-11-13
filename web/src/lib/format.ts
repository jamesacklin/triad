// Text forms for what the socket sends as JSON. The terminal interface gets
// these lines ready-made with every event; a socket client does not, so the
// wording here follows the session's own (crates/triad/src/main.rs, bots.rs).
import type { EventName, EventOf, IncomingEvent } from '@/api/protocol'
import type { Bot, Channel, Msg, State } from '@/state/store'

/** A node ID as the terminal shows it: its first 8 hex characters. */
export function short(id: string | null | undefined): string {
  return (id ?? '').slice(0, 8)
}

/** A message author's label, as the plain prompt and the TUI give it. */
export function authorLabel(m: Msg, dm: boolean): string {
  const who = m.mine ? 'me' : m.nickname || short(m.author)
  if (m.botClaim && m.bot) return dm ? 'my bot' : 'bot'
  if (m.botClaim) return `${who} (bot claim not accepted)`
  return who
}

export function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? '' : 's'}`
}

function secs(ms: unknown): string {
  const n = typeof ms === 'number' && ms >= 0 ? ms : 0
  return `${Math.floor(n / 1000)}.${Math.floor((n % 1000) / 100)}s`
}

function count(v: unknown): number | null {
  return typeof v === 'number' ? v : null
}

/**
 * A finished turn's figures. `denied` is a list of tool names and the
 * token counts can be null (the mock brain reports none).
 */
export function turnFigures(j: EventOf<'job'>): string {
  const requests = count(j.requests) ?? 0
  let s = `last turn ${secs(j.turn_ms)} (model ${secs(j.model_ms)}, tools ${secs(j.tool_ms)}) · ${plural(requests, 'request')}`
  const i = count(j.input_tokens)
  const o = count(j.output_tokens)
  if (i !== null || o !== null) s += ` · ${i ?? 0}→${o ?? 0} tokens`
  const denied = j.denied?.length ?? 0
  if (denied > 0) s += ` · ${denied} denied`
  return s
}

function tools(servers: string[]): string {
  return servers.length === 0 ? 'no tools' : `tools ${servers.join(',')}`
}

/** The bot part of the status bar for a channel, as the TUI's bot_segment. `off` dims it. */
export function botSegment(state: State, c: Channel): { text: string; off: boolean } {
  if (c.dm) {
    if (state.bots) return { text: `bot on · ${state.botProvider ?? ''} · ${tools(state.botServers)}`, off: false }
    if (state.botsOff) return { text: `bot off: ${state.botsOff.split(';')[0]}`, off: true }
    return { text: 'bot off', off: true }
  }
  const b: Bot | null = c.bot
  if (b && b.enabled && b.host === state.node && !state.bots) {
    return { text: 'my bot, not running here (/bot on)', off: true }
  }
  if (b && b.enabled) {
    const by = b.host === state.node ? 'mine' : `run by ${short(b.host)}`
    return { text: `bot on (${by}) · ${b.provider ?? ''} · ${tools(b.servers)}`, off: false }
  }
  if (b) return { text: 'bot off', off: true }
  return { text: 'no bot', off: true }
}

/** The chat pane's title parts, as the TUI's pane_title. */
export function paneTitle(c: Channel): string[] {
  const parts = [`#${c.name}`]
  if (c.dm) parts.push('your bot')
  if (c.info.status === 'pending') parts.push('waiting to be let in')
  if (c.members !== null) parts.push(plural(c.members, 'member'))
  parts.push(c.info.role)
  return parts
}

export function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.round(s / 60)}m`
  if (s < 86400) return `${Math.round(s / 3600)}h`
  return `${Math.round(s / 86400)}d`
}

export function clock(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
}

export function dateTime(ts: number): string {
  return new Date(ts).toLocaleString()
}

type E<N extends EventName> = EventOf<N>

/**
 * The one-line text of an event, in the session's words, or null when the
 * event needs more than a line (a table) or is not known.
 */
export function noticeText(ev: IncomingEvent, me: string | null): string | null {
  const who = (m: string) => (m === me ? 'you' : short(m))
  const byMe = (m: string | null) => (m === me ? 'me' : short(m))
  const list = (xs: string[]) => (xs.length === 0 ? 'none' : xs.join(', '))
  switch (ev.event) {
    case 'info':
    case 'error':
      return (ev as E<'info'>).text
    case 'channel': {
      const c = ev as E<'channel'>
      if (c.status === 'pending') {
        return `waiting to be let into #${c.channel}; messages there are disabled until then`
      }
      return `now in #${c.channel}`
    }
    case 'nick':
      return `you are now ${(ev as E<'nick'>).nickname}`
    case 'joined': {
      const e = ev as E<'joined'>
      return `${who(e.member)} joined (${plural(e.members, 'member')})`
    }
    case 'promoted': {
      const e = ev as E<'promoted'>
      return `${who(e.member)} is now ${e.access}`
    }
    case 'removed': {
      const e = ev as E<'removed'>
      return `${who(e.member)} removed (${plural(e.members, 'member')})`
    }
    case 'join_request': {
      const e = ev as E<'join_request'>
      return `join request for #${e.channel} from ${short(e.member)}${e.auto ? ' (auto)' : ''}`
    }
    case 'join_declined': {
      const e = ev as E<'join_declined'>
      return e.reason === 'bot DM'
        ? `declined ${short(e.member)} for #${e.channel}: your bot DM never gains a member`
        : `declined ${short(e.member)} for #${e.channel}`
    }
    case 'bot_on': {
      const e = ev as E<'bot_on'>
      return `bot on, run by ${byMe(e.host)}: sends to ${e.provider}; servers: ${list(e.servers)}`
    }
    case 'bot_off':
      return `bot off (${byMe((ev as E<'bot_off'>).host)})`
    case 'bot': {
      const e = ev as E<'bot'>
      if (!e.host || !e.enabled) return `#${e.channel} has no bot`
      return `#${e.channel} bot: run by ${byMe(e.host)}, sends to ${e.provider ?? ''}, servers: ${list(e.servers)}; your tools: ${e.my_tools}`
    }
    case 'bot_model': {
      const e = ev as E<'bot_model'>
      if (!e.model) {
        const running = e.running ? ` (running: ${e.running})` : ''
        return `no model chosen for your bots${running}; type /bot model <vendor/model>, then /bot provider-key <key> if the vendor needs one`
      }
      return e.running && e.running !== e.model ? `bot model: ${e.model} (running: ${e.running})` : `bot model: ${e.model}`
    }
    case 'bots_started':
      return `bots now use ${(ev as E<'bots_started'>).provider}`
    case 'bot_unavailable':
      return `bots off: ${(ev as E<'bot_unavailable'>).reason}. /bot on retries.`
    case 'bot_rejected':
      return `bot announcement from ${short((ev as E<'bot_rejected'>).author)} not accepted`
    case 'granted': {
      const e = ev as E<'granted'>
      return `granted ${e.pattern} to ${short(e.member)} in #${e.channel}`
    }
    case 'revoked': {
      const e = ev as E<'revoked'>
      return `revoked ${e.removed} grant(s) from ${short(e.member)} in #${e.channel}`
    }
    case 'blocked':
      return `blocked ${short((ev as E<'blocked'>).member)}`
    case 'unblocked':
      return `unblocked ${short((ev as E<'unblocked'>).member)}`
    case 'forgot': {
      const e = ev as E<'forgot'>
      return e.removed ? `forgot memory #${e.id}` : `no memory #${e.id}`
    }
    case 'unscheduled': {
      const e = ev as E<'unscheduled'>
      return e.removed ? `removed schedule #${e.id}` : `no schedule #${e.id}`
    }
    case 'schedule_fired': {
      const e = ev as E<'schedule_fired'>
      return `schedule #${e.id} "${e.name}" fired`
    }
    case 'schedule_missed': {
      const e = ev as E<'schedule_missed'>
      return `schedule #${e.id} "${e.name}" was due ${ago(e.late_ms)} ago, while triad was not running; dropped`
    }
    case 'job': {
      const e = ev as E<'job'>
      return `bot job for ${short(e.asker)}: ${e.state}${e.reason ? ` (${e.reason})` : ''}`
    }
    default:
      return null
  }
}
