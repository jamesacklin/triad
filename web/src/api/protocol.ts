// Names for the generated protocol types. Everything here is derived from
// types.gen.ts (generated from openapi.json), so a change to the protocol
// document that renames or reshapes a frame fails to compile.
import type { components } from './types.gen'

export type Schemas = components['schemas']

export type Event = Schemas['Event']
export type EventName = Event['event']
export type EventOf<N extends EventName> = Extract<Event, { event: N }>

export type ChannelInfo = Schemas['ChannelInfo']
export type HistoryMessage = Schemas['HistoryMessage']
export type RequestId = Schemas['RequestId']

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never

/** A request as the app writes it: the client adds the `id`. */
export type RequestBody = DistributiveOmit<
  Exclude<Schemas['Request'], Schemas['AuthRequest']>,
  'id'
>

/**
 * An event as it arrives. The server may send events and fields this build
 * does not know (API.md: clients must ignore them), so the name is widened.
 */
export type IncomingEvent = Event | { event: string; seq: number; re?: RequestId; [k: string]: unknown }

export function isEvent<N extends EventName>(ev: IncomingEvent, name: N): ev is EventOf<N> {
  return ev.event === name
}

/** Close codes from API.md. */
export const CLOSE = {
  sessionEnded: 1001,
  unauthorized: 4001,
  noToken: 4002,
  lagged: 4008,
} as const
