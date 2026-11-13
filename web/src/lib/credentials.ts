// Where the address and token are kept: sessionStorage for this tab (a
// reload reconnects, closing the tab forgets), or localStorage when the
// user ticks "remember on this device". Never a URL, never a log line.

export interface Credentials {
  url: string
  token: string
}

/**
 * Whether this page was built to be served by `triad serve` itself (a
 * production build) rather than by the Vite dev server. Served by triad,
 * the socket is on the page's own host, and that origin needs no
 * `--allow-origin`.
 */
export const SERVED_BY_TRIAD = !import.meta.env.DEV

export const DEFAULT_URL = SERVED_BY_TRIAD
  ? `${globalThis.location?.protocol === 'https:' ? 'wss' : 'ws'}://${globalThis.location?.host ?? '127.0.0.1:7878'}/v1`
  : 'ws://127.0.0.1:7878/v1'
const KEY = 'triad.connection'

function read(store: Storage | undefined): Credentials | null {
  try {
    const raw = store?.getItem(KEY)
    if (!raw) return null
    const v = JSON.parse(raw) as Partial<Credentials>
    return typeof v.url === 'string' && typeof v.token === 'string' ? { url: v.url, token: v.token } : null
  } catch {
    return null
  }
}

function storage(kind: 'session' | 'local'): Storage | undefined {
  try {
    return kind === 'session' ? globalThis.sessionStorage : globalThis.localStorage
  } catch {
    return undefined
  }
}

/** The saved connection, this tab's first. */
export function loadCredentials(): (Credentials & { remembered: boolean }) | null {
  const s = read(storage('session'))
  if (s) return { ...s, remembered: read(storage('local')) !== null }
  const l = read(storage('local'))
  return l ? { ...l, remembered: true } : null
}

export function saveCredentials(c: Credentials, remember: boolean): void {
  const value = JSON.stringify({ url: c.url, token: c.token })
  try {
    storage('session')?.setItem(KEY, value)
    if (remember) storage('local')?.setItem(KEY, value)
    else storage('local')?.removeItem(KEY)
  } catch {
    // Storage refused (private mode, quota): the tab still works until reload.
  }
}

/** Forgets the token everywhere, keeping the address for the connect screen. */
export function forgetToken(): void {
  for (const kind of ['session', 'local'] as const) {
    const c = read(storage(kind))
    try {
      if (c) storage(kind)?.setItem(KEY, JSON.stringify({ url: c.url, token: '' }))
    } catch {
      // As above.
    }
  }
}

export function forgetAll(): void {
  try {
    storage('session')?.removeItem(KEY)
    storage('local')?.removeItem(KEY)
  } catch {
    // As above.
  }
}

/** The token as `serve` writes it: 64 hex characters. */
export function looksLikeToken(t: string): boolean {
  return /^[0-9a-f]{64}$/i.test(t.trim())
}
