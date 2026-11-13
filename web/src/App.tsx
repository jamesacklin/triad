import { useCallback, useRef, useState } from 'react'
import { UserPlusIcon } from 'lucide-react'
import { ChannelSidebar } from '@/components/ChannelSidebar'
import { Composer, type ComposerHandle } from '@/components/Composer'
import { ConnectDialog } from '@/components/ConnectDialog'
import { InviteDialog, JoinDialog, NewChannelDialog } from '@/components/Dialogs'
import { JoinRequestBar } from '@/components/JoinRequestBar'
import { MessageList, type MessageListHandle } from '@/components/MessageList'
import { StatusBar } from '@/components/StatusBar'
import { Button } from '@/components/ui/button'
import { SidebarInset, SidebarProvider, SidebarTrigger } from '@/components/ui/sidebar'
import { Toaster } from '@/components/ui/sonner'
import { TooltipProvider } from '@/components/ui/tooltip'
import { switchTarget } from '@/lib/commands'
import { loadCredentials } from '@/lib/credentials'
import { paneTitle } from '@/lib/format'
import { focused } from '@/state/store'
import { TriadContext, useTriadSession, type Triad } from '@/state/useTriad'

const saved = loadCredentials()

export default function App() {
  const triad = useTriadSession(saved)
  const { state, credentials } = triad
  const closed = state.status.state === 'closed' ? state.status.reason : null

  if (!credentials || (closed && closed !== 'closed')) {
    return (
      <ConnectDialog
        // A new key per failure resets the form with what is now saved.
        key={`${closed ?? 'none'}`}
        initial={loadCredentials() ?? saved}
        reason={closed}
        url={credentials?.url ?? null}
        onConnect={triad.connect}
      />
    )
  }
  return (
    <TriadContext.Provider value={triad}>
      <TooltipProvider>
        <Chat triad={triad} />
        <Toaster />
      </TooltipProvider>
    </TriadContext.Provider>
  )
}

function Chat({ triad }: { triad: Triad }) {
  const { state, session } = triad
  const channel = focused(state)
  const connected = state.status.state === 'open'
  const list = useRef<MessageListHandle>(null)
  const composer = useRef<ComposerHandle>(null)
  const [dialog, setDialog] = useState<'invite' | 'new' | 'join' | null>(null)

  const show = useCallback(
    (name: string) => {
      void session?.show(name)
      requestAnimationFrame(() => composer.current?.focus())
    },
    [session],
  )

  const step = (n: number) => {
    const cs = state.channels
    if (cs.length === 0) return
    const at = Math.max(0, cs.findIndex((c) => c.name === state.focus))
    show(cs[(at + n + cs.length) % cs.length].name)
  }

  const onOlder = useCallback(() => {
    if (state.focus) void session?.older(state.focus)
  }, [session, state.focus])

  const submit = async (line: string): Promise<boolean> => {
    if (!session) return false
    session.clearError()
    const trimmed = line.trim()
    if (trimmed.startsWith('/')) {
      const target = switchTarget(trimmed)
      if (target !== null) {
        if (!state.channels.some((c) => c.name === target)) {
          localError(`no channel named #${target}; see /channels`)
          return false
        }
        show(target)
        return true
      }
      const r = await session.command(state.focus ?? undefined, trimmed)
      // A channel this request added (its `channel` event carries our `re`): show it.
      const added = r?.events.find((e) => e.event === 'channel') as { channel?: string } | undefined
      if (added?.channel) show(added.channel)
      return r !== null
    }
    if (!state.focus) {
      localError('no channel to send to')
      return false
    }
    return (await session.send(state.focus, line)) !== null
  }

  function localError(text: string) {
    session?.note(text, true)
  }

  return (
    <SidebarProvider className="h-svh">
      <ChannelSidebar onNew={() => setDialog('new')} onJoin={() => setDialog('join')} onPick={show} />
      <SidebarInset className="flex h-svh min-w-0 flex-col">
        <header className="flex h-11 shrink-0 items-center gap-2 border-b px-2">
          <SidebarTrigger aria-label="Show or hide the channel list" />
          {channel ? (
            <h1 className="min-w-0 flex-1 truncate text-sm">
              <span className="font-semibold">{paneTitle(channel)[0]}</span>
              <span className="text-muted-foreground"> · {paneTitle(channel).slice(1).join(' · ')}</span>
            </h1>
          ) : (
            <h1 className="flex-1 text-sm text-muted-foreground">{connected ? 'No channel' : 'Connecting…'}</h1>
          )}
          {channel && !channel.dm && channel.info.status === 'joined' && (
            <Button variant="outline" size="sm" onClick={() => setDialog('invite')} disabled={!connected}>
              <UserPlusIcon aria-hidden /> Invite
            </Button>
          )}
        </header>
        {channel ? (
          <MessageList key={channel.name} ref={list} channel={channel} me={state.node} onOlder={onOlder} />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-center text-sm text-muted-foreground">
            {connected ? 'Create or join a channel to start.' : 'Waiting for triad serve…'}
          </div>
        )}
        <StatusBar state={state} />
        <JoinRequestBar
          asks={state.asks}
          disabled={!connected}
          onAnswer={(a, accept) => session?.answer(a.channel, a.member, accept) ?? Promise.resolve(false)}
        />
        <Composer
          ref={composer}
          channel={channel}
          channels={state.channels.map((c) => c.name)}
          connected={connected}
          onSubmit={submit}
          onPage={(p) => list.current?.page(p)}
          onStep={step}
        />
      </SidebarInset>
      <InviteDialog open={dialog === 'invite'} onOpenChange={(o) => setDialog(o ? 'invite' : null)} session={session} channel={state.focus} />
      <NewChannelDialog open={dialog === 'new'} onOpenChange={(o) => setDialog(o ? 'new' : null)} session={session} onCreated={(c) => c && show(c)} />
      <JoinDialog open={dialog === 'join'} onOpenChange={(o) => setDialog(o ? 'join' : null)} session={session} onJoined={(c) => c && show(c)} />
    </SidebarProvider>
  )
}
