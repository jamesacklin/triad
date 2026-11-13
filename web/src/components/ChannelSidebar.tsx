import { useState } from 'react'
import { ChevronDownIcon, HashIcon, LogInIcon, LogOutIcon, MessageSquareIcon, PlusIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  useSidebar,
} from '@/components/ui/sidebar'
import { channelTree, topicLabel } from '@/state/store'
import { useTriad } from '@/state/useTriad'

interface Props {
  onNew: () => void
  onJoin: () => void
  onPick: (channel: string) => void
}

export function ChannelSidebar({ onNew, onJoin, onPick }: Props) {
  const { state, disconnect } = useTriad()
  const { isMobile, setOpenMobile } = useSidebar()
  // Channels whose topics are hidden; open by default.
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set())
  const toggle = (name: string) =>
    setClosed((s) => {
      const next = new Set(s)
      if (!next.delete(name)) next.add(name)
      return next
    })
  const pick = (name: string) => {
    onPick(name)
    if (isMobile) setOpenMobile(false)
  }

  return (
    <Sidebar aria-label="Channels">
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel>Channels</SidebarGroupLabel>
          <SidebarGroupContent>
            {state.channels.length === 0 ? (
              <p className="px-2 py-1 text-sm text-muted-foreground">
                {state.status.state === 'open' ? 'No channels yet.' : 'Connecting…'}
              </p>
            ) : (
              <SidebarMenu>
                {channelTree(state.channels).map(({ channel: c, topics }) => {
                  const focused = c.name === state.focus
                  const pending = c.info.status === 'pending'
                  const open = topics.length > 0 && !closed.has(c.name)
                  // A closed channel shows its topics' unread count with its own.
                  const unread = c.unread + (open ? 0 : topics.reduce((n, t) => n + t.unread, 0))
                  return (
                    <SidebarMenuItem key={c.name}>
                      <SidebarMenuButton
                        isActive={focused}
                        aria-current={focused ? 'page' : undefined}
                        onClick={() => pick(c.name)}
                        className={pending ? 'text-muted-foreground' : undefined}
                      >
                        {c.dm ? <MessageSquareIcon aria-hidden /> : <HashIcon aria-hidden />}
                        <span className="truncate">
                          {c.name}
                          {c.dm && <span className="text-muted-foreground"> (DM)</span>}
                          {pending && <span title="waiting to be let in"> (…)</span>}
                        </span>
                        {unread > 0 && <span className="sr-only">, {unread} unread</span>}
                      </SidebarMenuButton>
                      {topics.length > 0 && (
                        <SidebarMenuAction
                          aria-expanded={open}
                          aria-label={open ? `Hide topics of #${c.name}` : `Show topics of #${c.name}`}
                          onClick={() => toggle(c.name)}
                        >
                          <ChevronDownIcon aria-hidden className={open ? undefined : '-rotate-90'} />
                        </SidebarMenuAction>
                      )}
                      {unread > 0 && (
                        <SidebarMenuBadge
                          aria-hidden
                          className={`text-amber-600 dark:text-amber-400${topics.length > 0 ? ' right-7' : ''}`}
                        >
                          {unread}
                        </SidebarMenuBadge>
                      )}
                      {open && (
                        <SidebarMenuSub aria-label={`Topics of #${c.name}`}>
                          {topics.map((t) => {
                            const here = t.name === state.focus
                            const waiting = t.info.status === 'pending'
                            return (
                              <SidebarMenuSubItem key={t.name}>
                                <SidebarMenuSubButton asChild isActive={here}>
                                  <button
                                    type="button"
                                    aria-current={here ? 'page' : undefined}
                                    onClick={() => pick(t.name)}
                                    className={`w-full${waiting ? ' text-muted-foreground' : ''}`}
                                  >
                                    <span className="truncate">
                                      {topicLabel(t)}
                                      {waiting && <span title="waiting to be let in"> (…)</span>}
                                      {t.unread > 0 && <span className="sr-only">, {t.unread} unread</span>}
                                    </span>
                                    {t.unread > 0 && (
                                      <span aria-hidden className="ml-auto text-xs tabular-nums text-amber-600 dark:text-amber-400">
                                        {t.unread}
                                      </span>
                                    )}
                                  </button>
                                </SidebarMenuSubButton>
                              </SidebarMenuSubItem>
                            )
                          })}
                        </SidebarMenuSub>
                      )}
                    </SidebarMenuItem>
                  )
                })}
              </SidebarMenu>
            )}
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarFooter>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" className="flex-1" onClick={onNew} disabled={state.status.state !== 'open'}>
            <PlusIcon aria-hidden /> New
          </Button>
          <Button variant="outline" size="sm" className="flex-1" onClick={onJoin} disabled={state.status.state !== 'open'}>
            <LogInIcon aria-hidden /> Join
          </Button>
        </div>
        <Button variant="ghost" size="sm" onClick={disconnect} className="justify-start text-muted-foreground">
          <LogOutIcon aria-hidden /> Disconnect
        </Button>
      </SidebarFooter>
    </Sidebar>
  )
}
