// A channel's pane: stored and live messages, and that channel's notices.
// It sticks to the bottom while new lines arrive, stays put when scrolled
// up (as the TUI does until PgDn), and asks for the page before the oldest
// message when scrolled to the top. Mounted once per channel (keyed by
// name), so each channel starts at its newest line.
import { ArrowDownIcon } from 'lucide-react'
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState } from 'react'
import { Button } from '@/components/ui/button'
import { authorLabel, clock, dateTime } from '@/lib/format'
import type { Channel, Msg } from '@/state/store'
import { ReplyBlock } from './ReplyBlock'

export interface MessageListHandle {
  /** Scrolls by `pages` screens (negative is up). */
  page: (pages: number) => void
}

interface Props {
  channel: Channel
  me: string | null
  onOlder: () => void
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
      <p className="min-w-0 flex-1 whitespace-pre-wrap break-words">
        {m.action ? (
          <>
            <span className={`font-semibold ${colour}`}>* {who}</span> {m.text}
          </>
        ) : (
          <>
            <span className={`font-semibold ${colour}`}>{who}:</span> {m.text}
          </>
        )}
      </p>
    </div>
  )
}

export const MessageList = forwardRef<MessageListHandle, Props>(function MessageList(
  { channel, me, onOlder },
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
  }, [channel.items, first])

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
      </div>
      {!atBottom && (
        <Button size="sm" variant="secondary" onClick={toBottom} className="absolute right-4 bottom-3 shadow">
          <ArrowDownIcon aria-hidden /> Newest
        </Button>
      )}
    </div>
  )
})
