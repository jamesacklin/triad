import { HashIcon, LogInIcon, LogOutIcon, MessageSquareIcon, PlusIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from '@/components/ui/sidebar'
import { useTriad } from '@/state/useTriad'

interface Props {
  onNew: () => void
  onJoin: () => void
  onPick: (channel: string) => void
}

export function ChannelSidebar({ onNew, onJoin, onPick }: Props) {
  const { state, disconnect } = useTriad()
  const { isMobile, setOpenMobile } = useSidebar()
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
                {state.channels.map((c) => {
                  const focused = c.name === state.focus
                  const pending = c.info.status === 'pending'
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
                        {c.unread > 0 && <span className="sr-only">, {c.unread} unread</span>}
                      </SidebarMenuButton>
                      {c.unread > 0 && (
                        <SidebarMenuBadge aria-hidden className="text-amber-600 dark:text-amber-400">
                          {c.unread}
                        </SidebarMenuBadge>
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
