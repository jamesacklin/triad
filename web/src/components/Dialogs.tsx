// Invite, new channel and join. Each runs the same `/command` a user could
// type, through `command`; the session decides everything, and the dialog
// shows its answer or its error.
import { useEffect, useId, useState, type FormEvent, type ReactNode } from 'react'
import type { EventOf } from '@/api/protocol'
import { RequestError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import type { Session } from '@/state/session'
import { CopyButton } from './ReplyBlock'

function errorText(e: unknown): string {
  return e instanceof RequestError || e instanceof Error ? e.message : String(e)
}

function Problem({ text }: { text: string | null }) {
  if (!text) return null
  return (
    <p role="alert" className="text-sm text-destructive">
      {text}
    </p>
  )
}

interface Base {
  open: boolean
  onOpenChange: (open: boolean) => void
  session: Session | null
}

export function InviteDialog({ open, onOpenChange, session, channel }: Base & { channel: string | null }) {
  const [invite, setInvite] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!open || !session || !channel) return
    let live = true
    session
      .query({ op: 'command', channel, line: `/invite ${channel}` })
      .then((r) => {
        const ev = r.events.find((e): e is EventOf<'invite'> => e.event === 'invite')
        if (live) setInvite(ev ? ev.invite : null)
      })
      .catch((e: unknown) => {
        if (live) setError(errorText(e))
      })
    return () => {
      live = false
      setInvite(null)
      setError(null)
    }
  }, [open, session, channel])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Invite to #{channel}</DialogTitle>
          <DialogDescription>
            Send this to the person you want to add. They run <code>triad join &lt;invite&gt;</code>, and you accept
            their join request here.
          </DialogDescription>
        </DialogHeader>
        {invite ? (
          <div className="flex items-start gap-2">
            <code className="min-w-0 flex-1 break-all rounded-md bg-muted p-2 font-mono text-xs">{invite}</code>
            <CopyButton text={invite} label={`Copy the invite to #${channel}`} />
          </div>
        ) : error ? (
          <Problem text={error} />
        ) : (
          <p className="text-sm text-muted-foreground">Asking for the invite…</p>
        )}
      </DialogContent>
    </Dialog>
  )
}

function FormDialog({
  open,
  onOpenChange,
  title,
  description,
  submit,
  busy,
  error,
  onSubmit,
  children,
}: {
  open: boolean
  onOpenChange: (o: boolean) => void
  title: string
  description: ReactNode
  submit: string
  busy: boolean
  error: string | null
  onSubmit: () => void
  children: ReactNode
}) {
  const go = (e: FormEvent) => {
    e.preventDefault()
    onSubmit()
  }
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={go} className="space-y-4">
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          {children}
          <Problem text={error} />
          <DialogFooter>
            <Button type="submit" disabled={busy}>
              {submit}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

/** Runs `line`; on success closes and reports the channel the session added, if any. */
function useRun(session: Session | null, onDone: (channel: string | null) => void) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const run = async (line: string) => {
    if (!session) return
    setBusy(true)
    setError(null)
    try {
      const r = await session.query({ op: 'command', line })
      const added = r.events.find((e): e is EventOf<'channel'> => e.event === 'channel')
      onDone(added ? added.channel : null)
    } catch (e) {
      setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  return { busy, error, run, reset: () => setError(null) }
}

/** A command argument is one word; the session checks the rest. */
const NAME = /^\S+$/

export function NewChannelDialog({ open, onOpenChange, session, onCreated }: Base & { onCreated: (c: string | null) => void }) {
  const [name, setName] = useState('')
  const [noBot, setNoBot] = useState(false)
  const ids = { name: useId(), bot: useId() }
  const { busy, error, run, reset } = useRun(session, (c) => {
    setName('')
    onOpenChange(false)
    onCreated(c)
  })
  const ok = NAME.test(name.trim())
  return (
    <FormDialog
      open={open}
      onOpenChange={(o) => {
        reset()
        onOpenChange(o)
      }}
      title="New channel"
      description={<>A new space with you as its creator. Runs <code>/create</code>.</>}
      submit="Create"
      busy={busy || !ok}
      error={error}
      onSubmit={() => ok && void run(`/create ${name.trim()}${noBot ? ' --no-bot' : ''}`)}
    >
      <div className="space-y-2">
        <Label htmlFor={ids.name}>Name</Label>
        <Input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} placeholder="garden" autoComplete="off" autoFocus />
      </div>
      <div className="flex items-center gap-2">
        <Checkbox id={ids.bot} checked={noBot} onCheckedChange={(v) => setNoBot(v === true)} />
        <Label htmlFor={ids.bot}>Without my bot</Label>
      </div>
    </FormDialog>
  )
}

export function JoinDialog({ open, onOpenChange, session, onJoined }: Base & { onJoined: (c: string | null) => void }) {
  const [invite, setInvite] = useState('')
  const [name, setName] = useState('')
  const ids = { invite: useId(), name: useId() }
  const { busy, error, run, reset } = useRun(session, (c) => {
    setInvite('')
    setName('')
    onOpenChange(false)
    onJoined(c)
  })
  const ok = NAME.test(invite.trim()) && (name.trim() === '' || NAME.test(name.trim()))
  return (
    <FormDialog
      open={open}
      onOpenChange={(o) => {
        reset()
        onOpenChange(o)
      }}
      title="Join a channel"
      description={<>Paste an invite. The channel's manager is asked to let you in. Runs <code>/join</code>.</>}
      submit="Join"
      busy={busy || !ok}
      error={error}
      onSubmit={() => ok && void run(`/join ${invite.trim()}${name.trim() ? ` ${name.trim()}` : ''}`)}
    >
      <div className="space-y-2">
        <Label htmlFor={ids.invite}>Invite</Label>
        <Input id={ids.invite} value={invite} onChange={(e) => setInvite(e.target.value)} placeholder="triad1-…" autoComplete="off" spellCheck={false} autoFocus className="font-mono" />
      </div>
      <div className="space-y-2">
        <Label htmlFor={ids.name}>Local name (optional)</Label>
        <Input id={ids.name} value={name} onChange={(e) => setName(e.target.value)} placeholder="the name in the invite" autoComplete="off" />
      </div>
    </FormDialog>
  )
}
