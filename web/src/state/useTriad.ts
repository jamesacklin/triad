// The React side of the store: one useReducer, and the Session that feeds
// it. The Session reads state synchronously between its own dispatches (to
// know what it holds), so dispatch also applies the same pure reducer to a
// ref; React's state follows from the same actions in the same order.
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react'
import { forgetToken, saveCredentials, type Credentials } from '@/lib/credentials'
import { Session } from './session'
import { initialState, reducer, type Action, type State } from './store'

export interface Triad {
  state: State
  /** The live session, or null before connecting. Its methods are safe to call at any time. */
  session: Session | null
  credentials: Credentials | null
  connect: (c: Credentials, remember: boolean) => void
  disconnect: () => void
}

export function useTriadSession(initial: Credentials | null): Triad {
  const [state, reactDispatch] = useReducer(reducer, initialState)
  const ref = useRef<State>(initialState)
  const [credentials, setCredentials] = useState<Credentials | null>(
    initial && initial.token ? initial : null,
  )
  const [session, setSession] = useState<Session | null>(null)

  const dispatch = useCallback((a: Action) => {
    ref.current = reducer(ref.current, a)
    reactDispatch(a)
  }, [])

  useEffect(() => {
    if (!credentials) return
    const s = new Session({
      url: credentials.url,
      token: credentials.token,
      dispatch,
      getState: () => ref.current,
    })
    // Publish the session to components from the effect that owns it.
    // oxlint-disable-next-line react/set-state-in-effect
    setSession(s)
    s.connect()
    return () => {
      s.close()
      dispatch({ type: 'reset' })
    }
  }, [credentials, dispatch])

  // A refused token is forgotten, so a reload does not try it again.
  const refused = state.status.state === 'closed' && state.status.reason === 'unauthorized'
  useEffect(() => {
    if (refused) forgetToken()
  }, [refused])

  const connect = useCallback((c: Credentials, remember: boolean) => {
    saveCredentials(c, remember)
    setCredentials({ url: c.url, token: c.token })
  }, [])

  const disconnect = useCallback(() => {
    forgetToken()
    setCredentials(null)
    setSession(null)
  }, [])

  return useMemo(
    () => ({ state, session: credentials ? session : null, credentials, connect, disconnect }),
    [state, session, credentials, connect, disconnect],
  )
}

export const TriadContext = createContext<Triad | null>(null)

export function useTriad(): Triad {
  const t = useContext(TriadContext)
  if (!t) throw new Error('useTriad outside TriadContext')
  return t
}
