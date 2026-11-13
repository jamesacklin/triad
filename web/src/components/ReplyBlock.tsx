// A notice in a channel's pane: a change event or a reply to something the
// user typed. The socket sends JSON only, so each known shape is drawn here;
// anything else is shown as formatted JSON, so a new command works before
// the app knows it.
import { CheckIcon, CopyIcon } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import type { EventOf, IncomingEvent } from '@/api/protocol'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { ago, dateTime, noticeText, short } from '@/lib/format'
import type { Notice } from '@/state/store'

export function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      setCopied(false)
    }
  }
  return (
    <Button variant="outline" size="sm" onClick={copy} aria-label={copied ? 'Copied' : label}>
      {copied ? <CheckIcon aria-hidden /> : <CopyIcon aria-hidden />}
      {copied ? 'Copied' : 'Copy'}
    </Button>
  )
}

function Line({ children, error }: { children: ReactNode; error?: boolean }) {
  return (
    <p
      role={error ? 'alert' : undefined}
      className={`whitespace-pre-wrap break-words text-sm ${error ? 'text-destructive' : 'text-muted-foreground'}`}
    >
      {children}
    </p>
  )
}

function Block({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="my-1 max-w-full overflow-hidden rounded-md border bg-muted/30" aria-label={title}>
      <h3 className="border-b px-3 py-1.5 text-xs font-medium text-muted-foreground">{title}</h3>
      <div className="overflow-x-auto px-1 py-1 text-sm">{children}</div>
    </section>
  )
}

function Node({ id, me }: { id: string; me: string | null }) {
  return (
    <span className="font-mono text-xs" title={id}>
      {short(id)}
      {id === me && <span className="text-muted-foreground"> (me)</span>}
    </span>
  )
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="px-2 py-1 text-muted-foreground">{children}</p>
}

function Rows({ head, rows }: { head: string[]; rows: ReactNode[][] }) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          {head.map((h) => (
            <TableHead key={h} className="h-8">
              {h}
            </TableHead>
          ))}
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((r, i) => (
          <TableRow key={i}>
            {r.map((cell, j) => (
              <TableCell key={j} className="py-1 align-top">
                {cell}
              </TableCell>
            ))}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  )
}

function Json({ ev }: { ev: IncomingEvent }) {
  const { seq: _seq, re: _re, ...rest } = ev as Record<string, unknown>
  return (
    <Block title={String(ev.event)}>
      <pre className="whitespace-pre-wrap break-all px-2 py-1 font-mono text-xs">{JSON.stringify(rest, null, 2)}</pre>
    </Block>
  )
}

function Invite({ ev }: { ev: EventOf<'invite'> }) {
  return (
    <Block title={`invite to #${ev.channel}`}>
      <div className="flex items-start gap-2 px-2 py-1">
        <code className="min-w-0 flex-1 break-all font-mono text-xs">{ev.invite}</code>
        <CopyButton text={ev.invite} label={`Copy the invite to #${ev.channel}`} />
      </div>
    </Block>
  )
}

export function ReplyBlock({ notice, me }: { notice: Notice; me: string | null }) {
  const ev = notice.ev
  if (ev.event === 'local') {
    const l = ev as { text: string; error?: boolean }
    return <Line error={l.error}>{l.error ? `error: ${l.text}` : l.text}</Line>
  }
  const e = ev as IncomingEvent
  switch (e.event) {
    case 'error':
      return <Line error>error: {(e as EventOf<'error'>).text}</Line>
    case 'invite':
      return <Invite ev={e as EventOf<'invite'>} />
    case 'channel': {
      const c = e as EventOf<'channel'>
      return (
        <>
          <Line>{noticeText(e, me)}</Line>
          {c.invite && <Invite ev={{ event: 'invite', channel: c.channel, invite: c.invite, copied: null, seq: c.seq }} />}
        </>
      )
    }
    case 'members': {
      const m = e as EventOf<'members'>
      return (
        <Block title={`#${m.channel} · ${m.members.length} members`}>
          <Rows head={['member', 'access']} rows={m.members.map((id) => [<Node key="m" id={id} me={me} />, m.access[id] ?? ''])} />
        </Block>
      )
    }
    case 'channels': {
      const c = e as EventOf<'channels'>
      return (
        <Block title="channels">
          <Rows
            head={['channel', 'role', 'status', 'invite']}
            rows={c.channels.map((x) => [
              `#${x.channel}`,
              x.role,
              x.status,
              x.invite ? <CopyButton key="i" text={x.invite} label={`Copy the invite to #${x.channel}`} /> : '',
            ])}
          />
        </Block>
      )
    }
    case 'grants': {
      const g = e as EventOf<'grants'>
      return (
        <Block title={`grants in #${g.channel}`}>
          {g.grants.length === 0 ? (
            <Empty>no grants in #{g.channel}</Empty>
          ) : (
            <Rows head={['member', 'pattern']} rows={g.grants.map((x) => [<Node key="m" id={x.member} me={me} />, <code key="p">{x.pattern}</code>])} />
          )}
        </Block>
      )
    }
    case 'audit': {
      const a = e as EventOf<'audit'>
      const now = notice.ts
      return (
        <Block title={`tool calls in #${a.channel}`}>
          {a.calls.length === 0 ? (
            <Empty>no tool calls in #{a.channel}</Empty>
          ) : (
            <Rows
              head={['asker', 'tool', 'state', 'bytes', 'when']}
              rows={a.calls.map((c) => [
                <Node key="a" id={c.asker} me={me} />,
                <code key="t">{c.tool}</code>,
                c.state,
                c.result_bytes ?? '',
                <span key="w" title={dateTime(c.at_ms)}>{ago(now - c.at_ms)} ago</span>,
              ])}
            />
          )}
        </Block>
      )
    }
    case 'memory': {
      const m = e as EventOf<'memory'>
      return (
        <Block title={m.query ? `memories matching “${m.query}”` : 'memories'}>
          {m.memories.length === 0 ? (
            <Empty>no saved memories match</Empty>
          ) : (
            <Rows
              head={['id', 'memory', 'saved']}
              rows={m.memories.map((x) => [
                `#${x.id}`,
                <span key="t" className="whitespace-pre-wrap">
                  {x.pinned && <Badge variant="secondary" className="mr-1">pinned</Badge>}
                  {x.text}
                </span>,
                dateTime(x.created_at_ms),
              ])}
            />
          )}
        </Block>
      )
    }
    case 'schedules': {
      const s = e as EventOf<'schedules'>
      return (
        <Block title="schedules">
          {s.schedules.length === 0 ? (
            <Empty>no schedules</Empty>
          ) : (
            <Rows
              head={['id', 'name', 'next', 'every', 'prompt']}
              rows={s.schedules.map((x) => [
                `#${x.id}`,
                x.name,
                x.next_at_ms === null ? '—' : dateTime(x.next_at_ms),
                x.every_ms === null ? 'once' : ago(x.every_ms),
                <span key="p" className="whitespace-pre-wrap">{x.prompt}</span>,
              ])}
            />
          )}
        </Block>
      )
    }
    case 'prompt': {
      const p = e as EventOf<'prompt'>
      return (
        <Block title={`system prompt for a turn of yours in #${p.channel}`}>
          <pre className="max-h-80 overflow-auto whitespace-pre-wrap px-2 py-1 font-mono text-xs">{p.system}</pre>
          <p className="px-2 py-1 text-xs text-muted-foreground">
            tools: {p.tools.length === 0 ? 'none' : p.tools.join(', ')} · plus the last {p.transcript_lines} messages of #
            {p.channel}
          </p>
        </Block>
      )
    }
    default: {
      const text = noticeText(e, me)
      return text !== null ? <Line>{text}</Line> : <Json ev={e} />
    }
  }
}
