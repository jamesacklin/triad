// A channel's pane: stored and live messages, and that channel's notices.
// It sticks to the bottom while new lines arrive, stays put when scrolled
// up (as the TUI does until PgDn), and asks for the page before the oldest
// message when scrolled to the top. Mounted once per channel (keyed by
// name), so each channel starts at its newest line.
import { ArrowDownIcon } from 'lucide-react'
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { authorLabel, clock, dateTime } from '@/lib/format'
import type { Channel, Msg, Pending } from '@/state/store'
import { MessageText } from './MessageText'
import { ReplyBlock } from './ReplyBlock'

export interface MessageListHandle {
  /** Scrolls by `pages` screens (negative is up). */
  page: (pages: number) => void
}

interface Props {
  channel: Channel
  me: string | null
  onOlder: () => void
  onRetry: (id: number) => void
  onDiscard: (id: number) => void
}

/** How near the bottom counts as "at the bottom". */
const STICK_PX = 48
/** How near the top starts loading the older page. */
const OLDER_PX = 120

function MessageLine({ m, dm }: { m: Msg; dm: boolean }) {
  const who = authorLabel(m, dm)
  const colour = m.bot
    ? 'text-fuchsia-700 dark:text-fuchsia-400'
    : m.mine
      ? 'text-cyan-700 dark:text-cyan-400'
      : 'text-green-700 dark:text-green-400'
  return (
    <div className="group flex gap-2 py-0.5 text-sm leading-relaxed">
      <time
        dateTime={new Date(m.ts).toISOString()}
        title={dateTime(m.ts)}
        className="hidden w-16 shrink-0 whitespace-nowrap sm:block pt-px text-right font-mono text-xs leading-relaxed text-muted-foreground/70"
      >
        {clock(m.ts)}
      </time>
      {m.action ? (
        // `/me` actions stay plain text.
        <p className="min-w-0 flex-1 whitespace-pre-wrap break-words">
          <span className={`font-semibold ${colour}`}>* {who}</span> {m.text}
        </p>
      ) : (
        <div className="min-w-0 flex-1 break-words">
          <span className={`font-semibold ${colour}`}>{who}:</span> <MessageText text={m.text} />
        </div>
      )}
    </div>
  )
}

const PENDING_NOTE: Record<Pending['state'], string> = {
  sending: 'sending…',
  sent: 'sent; waiting for it to come back',
  unconfirmed: 'not confirmed',
  failed: 'not sent',
}

/** A message sent from this tab, greyed out until the session echoes it. */
function PendingLine({ p, onRetry, onDiscard }: { p: Pending; onRetry: () => void; onDiscard: () => void }) {
  const bad = p.state === 'failed' || p.state === 'unconfirmed'
  return (
    <div className="flex gap-2 py-0.5 text-sm leading-relaxed" aria-label={`${PENDING_NOTE[p.state]}: ${p.text}`}>
      <span className="hidden w-16 shrink-0 whitespace-nowrap pt-px text-right font-mono text-xs leading-relaxed text-muted-foreground/70 sm:block">
        {clock(p.at)}
      </span>
      <div className="min-w-0 flex-1">
        {p.action ? (
          <p className="whitespace-pre-wrap break-words opacity-50">
            <span className="font-semibold text-cyan-700 dark:text-cyan-400">* me</span> {p.text}
          </p>
        ) : (
          <div className="break-words opacity-50">
            <span className="font-semibold text-cyan-700 dark:text-cyan-400">me:</span> <MessageText text={p.text} />
          </div>
        )}
        {bad ? (
          <p role="alert" className={`flex flex-wrap items-center gap-2 text-xs ${p.state === 'failed' ? 'text-destructive' : 'text-amber-700 dark:text-amber-400'}`}>
            <span>
              {PENDING_NOTE[p.state]}
              {p.error ? `: ${p.error}` : ''}
            </span>
            <Button size="xs" variant="outline" onClick={onRetry}>
              Retry
            </Button>
            <Button size="xs" variant="ghost" onClick={onDiscard}>
              Discard
            </Button>
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">{PENDING_NOTE[p.state]}</p>
        )}
      </div>
    </div>
  )
}

export const MessageList = forwardRef<MessageListHandle, Props>(function MessageList(
  { channel, me, onOlder, onRetry, onDiscard },
  handle,
) {
  const el = useRef<HTMLDivElement>(null)
  const stuck = useRef(true)
  const [atBottom, setAtBottom] = useState(true)
  // What the list looked like before this render, to hold the reading
  // position when an older page lands above it.
  const before = useRef<{ first: string | undefined; height: number }>({ first: undefined, height: 0 })

  const keyOf = (i: Channel['items'][number]) => (i.kind === 'msg' ? i.op : i.key)
  const first = channel.items[0] ? keyOf(channel.items[0]) : undefined

  useLayoutEffect(() => {
    const d = el.current
    if (!d) return
    const prev = before.current
    if (stuck.current) {
      d.scrollTop = d.scrollHeight
    } else if (prev.first !== undefined && prev.first !== first) {
      // Older messages were added above: keep the same lines in view.
      d.scrollTop += d.scrollHeight - prev.height
    }
    before.current = { first, height: d.scrollHeight }
  }, [channel.items, channel.pending, first])

  const onScroll = useCallback(() => {
    const d = el.current
    if (!d) return
    const bottom = d.scrollHeight - d.scrollTop - d.clientHeight < STICK_PX
    stuck.current = bottom
    setAtBottom(bottom)
    before.current = { first, height: d.scrollHeight }
    if (d.scrollTop < OLDER_PX && !channel.atStart && channel.loaded) onOlder()
  }, [channel.atStart, channel.loaded, first, onOlder])

  // A short page does not scroll, so no scroll event would ask for more.
  useEffect(() => {
    const d = el.current
    if (d && channel.loaded && !channel.atStart && !channel.paging && d.scrollHeight <= d.clientHeight) onOlder()
  }, [channel.loaded, channel.atStart, channel.items.length, channel.paging, onOlder])

  const toBottom = () => {
    const d = el.current
    if (!d) return
    stuck.current = true
    setAtBottom(true)
    d.scrollTop = d.scrollHeight
  }

  useImperativeHandle(handle, () => ({
    page: (pages: number) => {
      const d = el.current
      if (!d) return
      d.scrollBy({ top: pages * (d.clientHeight - 40) })
    },
  }))

  return (
    <div className="relative min-h-0 flex-1">
      <div
        ref={el}
        onScroll={onScroll}
        role="log"
        aria-label={`Messages in #${channel.name}`}
        tabIndex={0}
        className="h-full overflow-y-auto px-3 py-2 focus-visible:outline-2 focus-visible:-outline-offset-2"
      >
        {channel.loaded && channel.atStart && (
          <p className="py-2 text-center text-xs text-muted-foreground">
            {channel.items.some((i) => i.kind === 'msg') ? `The start of #${channel.name}.` : `No messages in #${channel.name} yet.`}
          </p>
        )}
        {(channel.paging || !channel.loaded) && (
          <p className="py-2 text-center text-xs text-muted-foreground" aria-live="off">
            Loading…
          </p>
        )}
        {channel.items.map((i) =>
          i.kind === 'msg' ? (
            <MessageLine key={i.op} m={i} dm={channel.dm} />
          ) : (
            <div key={i.key} className="py-0.5 sm:pl-18">
              <ReplyBlock notice={i} me={me} />
            </div>
          ),
        )}
        {channel.pending.map((p) => (
          <PendingLine key={`p${p.id}`} p={p} onRetry={() => onRetry(p.id)} onDiscard={() => onDiscard(p.id)} />
        ))}
      </div>
      {!atBottom && (
        <Button size="sm" variant="secondary" onClick={toBottom} className="absolute right-4 bottom-3 shadow">
          <ArrowDownIcon aria-hidden /> Newest
        </Button>
      )}
    </div>
  )
})
