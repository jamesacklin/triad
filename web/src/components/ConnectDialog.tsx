import { useId, useState, type FormEvent } from 'react'
import type { CloseReason } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { DEFAULT_URL, SERVED_BY_TRIAD, looksLikeToken, type Credentials } from '@/lib/credentials'

interface Props {
  initial: (Credentials & { remembered: boolean }) | null
  /** Why the last connection ended, if it did. */
  reason: CloseReason | null
  url: string | null
  onConnect: (c: Credentials, remember: boolean) => void
}

function Problem({ reason, url }: { reason: CloseReason; url: string }) {
  const origin = window.location.origin
  switch (reason) {
    case 'unauthorized':
      return (
        <p>
          <code>triad serve</code> refused the token (close code 4001). Copy it again from{' '}
          <code>&lt;data-dir&gt;/api-token</code>; if the file was deleted, the restarted <code>serve</code> made a new
          one.
        </p>
      )
    case 'no-token':
      return <p>The connection opened but the token did not arrive within 5 seconds (close code 4002). Try again.</p>
    case 'unreachable':
      return (
        <>
          <p>
            Could not connect to <code>{url}</code>. Either nothing is listening there, or <code>triad serve</code>{' '}
            refused this page&apos;s origin (a browser cannot tell the two apart).
            {SERVED_BY_TRIAD
              ? ' A page served by triad is allowed on a loopback address; reached any other way, start serve with:'
              : ' Start it with:'}
          </p>
          <pre className="mt-2 overflow-x-auto rounded-md bg-muted px-3 py-2 text-xs">
            triad serve --allow-origin {origin}
          </pre>
          <p className="mt-2 text-xs">
            <code>localhost</code> and <code>127.0.0.1</code> are different origins: pass exactly <code>{origin}</code>.
          </p>
        </>
      )
    default:
      return null
  }
}

export function ConnectDialog({ initial, reason, url: lastUrl, onConnect }: Props) {
  const [url, setUrl] = useState(initial?.url || DEFAULT_URL)
  const [token, setToken] = useState(reason === 'unauthorized' ? '' : (initial?.token ?? ''))
  const [remember, setRemember] = useState(initial?.remembered ?? false)
  const ids = { url: useId(), token: useId(), remember: useId(), problem: useId() }
  const tokenOk = looksLikeToken(token)
  const urlOk = /^wss?:\/\/.+/.test(url.trim())

  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (!tokenOk || !urlOk) return
    onConnect({ url: url.trim(), token: token.trim() }, remember)
  }

  return (
    <main className="flex min-h-svh items-center justify-center bg-background p-4">
      <form
        onSubmit={submit}
        aria-describedby={reason ? ids.problem : undefined}
        className="w-full max-w-md space-y-5 rounded-xl border bg-card p-6 text-card-foreground shadow-sm"
      >
        <div className="space-y-1">
          <h1 className="text-lg font-semibold">Connect to triad</h1>
          <p className="text-sm text-muted-foreground">
            This page talks to a <code>triad serve</code> session over its WebSocket.
          </p>
        </div>

        {reason && reason !== 'closed' && (
          <div
            id={ids.problem}
            role="alert"
            className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-3 text-sm"
          >
            <Problem reason={reason} url={lastUrl ?? url} />
          </div>
        )}

        <div className="space-y-2">
          <Label htmlFor={ids.url}>Address</Label>
          <Input
            id={ids.url}
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder={DEFAULT_URL}
            autoComplete="off"
            spellCheck={false}
            aria-invalid={!urlOk}
          />
          <p className="text-xs text-muted-foreground">
            {SERVED_BY_TRIAD ? (
              <>
                The <code>listening</code> line <code>serve</code> printed; by default the server this page came from.
              </>
            ) : (
              <>
                The <code>listening</code> line <code>serve</code> printed. Start it with{' '}
                <code>--allow-origin {window.location.origin}</code>.
              </>
            )}
          </p>
        </div>

        <div className="space-y-2">
          <Label htmlFor={ids.token}>Token</Label>
          <Input
            id={ids.token}
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            autoFocus
            aria-invalid={token !== '' && !tokenOk}
            className="font-mono"
          />
          <p className="text-xs text-muted-foreground">
            The 64 hex characters in <code>&lt;data-dir&gt;/api-token</code>. Whoever holds it can act as you.
          </p>
        </div>

        <div className="flex items-start gap-2">
          <Checkbox id={ids.remember} checked={remember} onCheckedChange={(v) => setRemember(v === true)} />
          <div className="grid gap-1">
            <Label htmlFor={ids.remember}>Remember on this device</Label>
            <p className="text-xs text-muted-foreground">
              Otherwise the token is kept only until this tab closes.
            </p>
          </div>
        </div>

        <Button type="submit" className="w-full" disabled={!tokenOk || !urlOk}>
          Connect
        </Button>
      </form>
    </main>
  )
}
