// The input line. Enter sends, Shift-Enter adds a line. Text starting with
// `/` is a command (`command`, with the focused channel); anything else is a
// message (`send`). Typing `/` lists the matching commands above the box;
// Up and Down pick one, Tab puts it in the line, as in the TUI.
import { forwardRef, useId, useImperativeHandle, useRef, useState, type KeyboardEvent } from 'react'
import { SendIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Command, CommandItem, CommandList } from '@/components/ui/command'
import { Textarea } from '@/components/ui/textarea'
import { suggest, type Suggestion } from '@/lib/commands'
import type { Channel } from '@/state/store'

export interface ComposerHandle {
  focus: () => void
}

interface Props {
  channel: Channel | null
  channels: string[]
  connected: boolean
  /** Returns whether the line was taken; on false the text is put back. */
  onSubmit: (line: string) => Promise<boolean>
  onPage: (pages: number) => void
  onStep: (step: number) => void
}

function placeholder(c: Channel | null, connected: boolean): string {
  if (!connected) return 'not connected'
  if (!c) return 'type /command'
  if (c.info.status === 'pending') return `waiting to be let into #${c.name}; /command`
  return c.dm ? 'message your bot or /command' : `message #${c.name} or /command`
}

export const Composer = forwardRef<ComposerHandle, Props>(function Composer(
  { channel, channels, connected, onSubmit, onPage, onStep },
  handle,
) {
  const [text, setText] = useState('')
  const [pick, setPick] = useState(0)
  const [dismissed, setDismissed] = useState<string | null>(null)
  const [atEnd, setAtEnd] = useState(true)
  const area = useRef<HTMLTextAreaElement>(null)
  const listId = useId()

  useImperativeHandle(handle, () => ({ focus: () => area.current?.focus() }))

  const suggestions: Suggestion[] = atEnd && dismissed !== text ? suggest(text, channels) : []
  const selected = suggestions.length > 0 ? Math.min(pick, suggestions.length - 1) : 0

  const set = (t: string) => {
    setText(t)
    setPick(0)
  }

  const complete = (s: Suggestion) => {
    set(s.complete)
    setAtEnd(true)
    requestAnimationFrame(() => {
      const a = area.current
      if (!a) return
      a.focus()
      a.setSelectionRange(a.value.length, a.value.length)
    })
  }

  const submit = async () => {
    const line = text
    if (line.trim() === '' || !connected) return
    set('')
    const ok = await onSubmit(line)
    if (!ok) setText((now) => (now === '' ? line : now))
  }

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return
    if (e.altKey && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
      e.preventDefault()
      onStep(e.key === 'ArrowUp' ? -1 : 1)
      return
    }
    if (e.key === 'PageUp' || e.key === 'PageDown') {
      e.preventDefault()
      onPage(e.key === 'PageUp' ? -1 : 1)
      return
    }
    if (suggestions.length > 0) {
      if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
        e.preventDefault()
        const n = suggestions.length
        setPick((selected + (e.key === 'ArrowUp' ? -1 : 1) + n) % n)
        return
      }
      if (e.key === 'Tab' && !e.shiftKey) {
        e.preventDefault()
        complete(suggestions[selected])
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        setDismissed(text)
        return
      }
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void submit()
    }
  }

  const trackCaret = () => {
    const a = area.current
    if (a) setAtEnd(a.selectionStart === a.value.length && a.selectionEnd === a.value.length)
  }

  const open = suggestions.length > 0

  return (
    <div className="relative border-t p-2">
      {open && (
        <Command
          shouldFilter={false}
          value={suggestions[selected]?.label}
          className="absolute right-2 bottom-full left-2 mb-1 h-auto w-auto max-h-64 rounded-lg border shadow-md"
        >
          <CommandList id={listId} aria-label="Commands">
            {suggestions.map((s) => (
              <CommandItem
                key={s.label}
                value={s.label}
                onSelect={() => complete(s)}
                onMouseDown={(e) => e.preventDefault()}
                className="flex items-baseline gap-3"
              >
                <span className="font-mono text-xs">{s.label}</span>
                {s.help && <span className="truncate text-xs text-muted-foreground">{s.help}</span>}
              </CommandItem>
            ))}
          </CommandList>
        </Command>
      )}
      <div className="flex items-end gap-2">
        <Textarea
          id="composer"
          ref={area}
          value={text}
          onChange={(e) => {
            set(e.target.value)
            setAtEnd(e.target.selectionStart === e.target.value.length)
          }}
          onKeyDown={onKeyDown}
          onKeyUp={trackCaret}
          onClick={trackCaret}
          rows={1}
          placeholder={placeholder(channel, connected)}
          aria-label={channel ? `Message #${channel.name}, or a /command` : 'Command'}
          aria-autocomplete="list"
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          spellCheck
          className="max-h-40 min-h-9 resize-none"
        />
        <Button
          size="icon"
          onClick={() => void submit()}
          disabled={!connected || text.trim() === ''}
          aria-label="Send"
        >
          <SendIcon aria-hidden />
        </Button>
      </div>
    </div>
  )
})
