// A waiting join request, oldest first, with Accept and Decline. The TUI
// reads the next plain line as y or n; the API's `answer` request replaced
// that, so here they are buttons.
import { useState } from 'react'
import { TriangleAlertIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { short } from '@/lib/format'
import type { JoinAsk } from '@/state/store'

interface Props {
  asks: JoinAsk[]
  disabled: boolean
  onAnswer: (ask: JoinAsk, accept: boolean) => Promise<unknown>
}

export function JoinRequestBar({ asks, disabled, onAnswer }: Props) {
  const [busy, setBusy] = useState(false)
  const ask = asks[0]
  if (!ask) return null
  const more = asks.length - 1
  const answer = async (accept: boolean) => {
    setBusy(true)
    try {
      await onAnswer(ask, accept)
    } finally {
      setBusy(false)
    }
  }
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-amber-500/40 bg-amber-100/60 px-3 py-1.5 text-sm dark:bg-amber-950/40"
    >
      <TriangleAlertIcon aria-hidden className="size-4 text-amber-700 dark:text-amber-400" />
      <span className="min-w-0 flex-1">
        join request for <strong>#{ask.channel}</strong> from{' '}
        <code className="font-mono" title={ask.member}>
          {short(ask.member)}
        </code>
        {more > 0 && <span className="text-muted-foreground"> (+{more} more)</span>}
      </span>
      <div className="flex gap-2">
        <Button size="sm" onClick={() => void answer(true)} disabled={busy || disabled}>
          Accept
        </Button>
        <Button size="sm" variant="outline" onClick={() => void answer(false)} disabled={busy || disabled}>
          Decline
        </Button>
      </div>
    </div>
  )
}
