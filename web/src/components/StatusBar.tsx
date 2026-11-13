// The status line, as the TUI draws it: the focused channel's bot, the
// running turn with its clock and current tool call, the queue, the last
// turn's figures, turns running elsewhere, and the connection when it is
// not open. Join requests have their own bar, with buttons.
import { useEffect, useState } from 'react'
import { botSegment, short, turnFigures } from '@/lib/format'
import { focused, type State } from '@/state/store'

/** The current time, ticking each second while `active`. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!active) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [active])
  return now
}

function Sep() {
  return (
    <span aria-hidden className="text-muted-foreground/60">
      │
    </span>
  )
}

function connection(state: State): { text: string; tone: 'warn' | 'error' } | null {
  const s = state.status
  switch (s.state) {
    case 'open':
      return null
    case 'connecting':
      return { text: 'connecting…', tone: 'warn' }
    case 'reconnecting': {
      const why = s.reason === 'lagged' ? 'fell behind' : 'connection lost'
      return { text: `${why}; reconnecting (attempt ${s.attempt}, after ${Math.round(s.retryInMs / 1000)}s)`, tone: 'warn' }
    }
    case 'closed':
      return { text: `disconnected (${s.reason})`, tone: 'error' }
  }
}

export function StatusBar({ state }: { state: State }) {
  const c = focused(state)
  const ticking = state.channels.some((x) => x.running !== null)
  const now = useNow(ticking)
  const conn = connection(state)
  const parts: React.ReactNode[] = []

  if (conn) {
    parts.push(
      <span key="conn" role="status" className={conn.tone === 'error' ? 'text-destructive' : 'text-amber-700 dark:text-amber-400'}>
        {conn.text}
      </span>,
    )
  }
  if (state.lastError) {
    parts.push(
      <span key="err" role="status" className="text-destructive">
        error: {state.lastError}
      </span>,
    )
  }
  if (c) {
    const bot = botSegment(state, c)
    parts.push(
      <span key="bot" className={bot.off ? 'text-muted-foreground' : undefined}>
        {bot.text}
      </span>,
    )
    if (c.running) {
      const secs = Math.max(0, Math.floor((now - c.running.since) / 1000))
      const tool = c.running.tool ? ` → ${c.running.tool.name} ${c.running.tool.state}` : ''
      parts.push(
        <span key="turn" className="text-amber-700 dark:text-amber-400">
          turn running {secs}s{tool}
        </span>,
      )
    } else if (c.lastTurn) {
      parts.push(
        <span key="last" className="text-muted-foreground">
          {turnFigures(c.lastTurn)}
        </span>,
      )
    }
    if (c.queued.length > 0) parts.push(<span key="queue">queue {c.queued.length}</span>)
    const busy = state.channels.filter((o) => o.name !== c.name && o.running).map((o) => `#${o.name}`)
    if (busy.length > 0) {
      parts.push(
        <span key="busy" className="text-muted-foreground">
          turn in {busy.join(' ')}
        </span>,
      )
    }
  }

  return (
    <footer
      aria-label="Status"
      className="flex min-h-7 flex-wrap items-center gap-x-2 gap-y-0.5 border-t px-3 py-1 font-mono text-xs"
    >
      {parts.map((p, i) => (
        <span key={i} className="flex items-center gap-2">
          {i > 0 && <Sep />}
          {p}
        </span>
      ))}
      {state.node && (
        <span className="ml-auto text-muted-foreground" title={state.node}>
          {state.nickname ? `${state.nickname} · ` : ''}
          {short(state.node)}
        </span>
      )}
    </footer>
  )
}
