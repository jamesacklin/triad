# triad web

A single-page app that does in a browser what triad's terminal interface
does, talking only to `triad serve` over its WebSocket at `/v1`
([API.md](../API.md)). It holds no session logic: everything it shows
comes from the socket, and everything it does is a `send`, `command`,
`answer`, `history` or `snapshot` request.

React 19, Vite 8, TypeScript 7, Tailwind 4, shadcn/ui 4 (radix), Vitest,
pnpm. Every version is pinned exactly.

## Served by triad

```sh
pnpm -C web install --frozen-lockfile && pnpm -C web build
cargo build -p triad --features web
target/debug/triad serve
```

`build.rs` embeds `web/dist` in the binary (the `web` feature fails the
build, naming these commands, if `web/dist` is missing). `serve` prints an
`open http://127.0.0.1:7878/` line; the page there connects to its own host
by default and needs no `--allow-origin` on a loopback address. It is sent
with a Content-Security-Policy (scripts, styles and images from the page's
origin only, sockets to `ws:`/`wss:`, no framing); see API.md, "The web
app". Rebuild both after changing the app.

## Develop it

```sh
scripts/web-dev.sh
```

builds `triad`, starts `triad serve` with the mock bot
(`TRIAD_AGENT_BRAIN=mock`), data in `demo-data/web` and
`--allow-origin http://localhost:5173`, prints where the token is, and runs
the dev server. Open <http://localhost:5173>, paste the token from that
file, connect. `DATA_DIR=…`, `LISTEN=…`, `WEB_PORT=…` and `BRAIN=` (empty:
the configured model) change the defaults.

By hand, in two terminals:

```sh
triad serve --allow-origin http://localhost:5173
cd web && pnpm install && pnpm dev --host localhost --strictPort
```

`serve` refuses any page whose origin it was not given, and `localhost` and
`127.0.0.1` are different origins: pass the address bar's origin exactly.

To try two identities, run `serve` on one data directory and the TUI or
`triad … --json` on another (see "Terminal interface" in the top-level
README).

## Scripts

| | |
|---|---|
| `pnpm dev` | Dev server. |
| `pnpm build` | Type-check, then build into `dist/`. |
| `pnpm typecheck` | `tsc -b`. |
| `pnpm lint` | oxlint, warnings are errors. |
| `pnpm test` | Vitest. |
| `pnpm gen:types` | Regenerates `src/api/types.gen.ts` from `../openapi.json`. |

`pnpm gen:types` runs openapi-typescript through `pnpm dlx` with
TypeScript 5.9.3: the generator drives the TypeScript compiler API, which
TypeScript 7 does not have. The generated file is checked in, so the app
fails to compile when the protocol document changes shape.

`node scripts/record-fixtures.mjs ../target/debug/triad` re-records
`src/test/fixtures/session.jsonl`: two throwaway identities, one on
`serve`, the other on `join --json`, exchanging messages, a join request
and a bot turn. Invites are redacted; the token is never written.

## Layout

```text
src/
  api/         types.gen.ts (generated), protocol.ts (names for it),
               client.ts (TriadClient: auth, ids, done, reconnect)
  state/       store.ts (the reducer), session.ts (wires the client to the
               store: history, resync, requests), useTriad.ts (React)
  components/  ConnectDialog, ChannelSidebar, MessageList, ReplyBlock,
               StatusBar, JoinRequestBar, Composer, Dialogs
  components/ui/  shadcn components, as generated
  lib/         commands.ts (the / menu), format.ts (text forms of events),
               credentials.ts (where the token is kept)
```

## Behaviour

- **Connecting.** Address and token are typed into the connect screen (the
  address defaults to the page's own host when `triad` serves it, and to
  `ws://127.0.0.1:7878/v1` under the dev server) and
  kept in `sessionStorage` (a reload reconnects; closing the tab forgets).
  "Remember on this device" also keeps them in `localStorage`. The token
  goes in the first frame, never in a URL, and is never logged. A refused
  token (4001) is forgotten.
- **Reconnecting.** A dropped socket, a restart of `serve` or a lag close
  (4008) retries after 1 s, doubling to 30 s. Then `ready` replaces the
  channel list, and every channel already shown is resynced: the newest
  `history` page, then older pages until one overlaps, merged by `op`.
- **Focus is the tab's own.** The API has no focus; `focused` in channel
  objects is ignored. Alt-Up and Alt-Down move between channels; unread
  counts are cleared when a channel is shown.
- **Messages** appear when the session's `message` event arrives, not when
  sent. They are plain text with line breaks; no Markdown.
- **The composer.** Enter sends, Shift-Enter adds a line, PgUp and PgDn
  scroll the pane. A line starting with `/` is a `command` with the shown
  channel; anything else a `send`. Typing `/` lists commands (Up and Down
  pick, Tab completes, Escape hides). `/switch` and `/s` are handled in the
  tab; `/quit` is not offered.
- **Replies** to commands are drawn as notices or small tables; any event
  the app does not know is shown as formatted JSON.
- **Join requests** show in a bar with Accept and Decline (`answer`).
- **Narrow windows** hide the channel list behind a button, as the TUI hides
  it under 60 columns. The theme follows the system's light or dark setting.

## Not done

No Markdown, attachments, notifications, search or message editing. No
browser automation suite (a Playwright run against a real `serve` is the
first follow-up). See "Web app" in [FINDINGS.md](../FINDINGS.md) for what
the protocol does not give the app.
