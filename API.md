# triad WebSocket API

`triad serve` runs one chat session with no terminal and exposes it on a
WebSocket. Clients (a web or mobile front end, a script, a harness outside a
VM) receive the session's events and send it requests. Any number of
clients can be connected at once; they share the session.

This document is the reference. [openapi.json](openapi.json) describes the
same frames as an OpenAPI 3.1 document, for generating client types: every
request is a schema under `Request`, every event a schema under `Event`
(both discriminated, on `op` and `event`). A test
(`crates/triad/tests/ws_docs.rs`) fails when an event the code emits is
missing from either.

## Starting it

```text
$ triad serve
listening  ws://127.0.0.1:7878/v1
token      /home/me/.local/share/triad/api-token
```

| Option | Default | Meaning |
|---|---|---|
| `--listen <addr>` | `127.0.0.1:7878` | Where to listen. Port `0` picks a free one; the `listening` line names it. |
| `--allow-remote` | off | Required for a non-loopback `--listen`. The socket is not encrypted; a third line warns so. |
| `--allow-origin <origin>` | none | Accept browser connections from this origin (repeatable). |
| `--channel <name>` | as `chat` | The session's default channel (see `command`). |
| `--auto-accept` | off | Admit every join request without asking. |
| `--no-bot` | off | Run without this user's bots (`TRIAD_NO_BOT=1`). |
| `--nickname`, `--data-dir`, `--verbose` | | As for every command. |

`serve` needs a channel to start, like `chat`: a data directory with no
channels and no bots (`--no-bot`, or no model configured) is an error. With
bots on, an empty directory starts on the bot's DM.

Only one process should use a data directory at a time; nothing locks it.

## Connecting and authenticating

The only path is `/v1`. Every connection needs the token from
`<data-dir>/api-token`: 64 hex characters, created with mode 0600 on the
first `serve`. Whoever holds it can do anything the user can: send as them,
change membership, drive the bot's tools. Delete the file and restart
`serve` to make a new one.

Present it one of two ways:

- `Authorization: Bearer <token>` on the upgrade request. A wrong token is
  refused with `401` before the upgrade.
- As the first frame, `{"op":"auth","token":"<token>"}`. Browsers cannot set
  headers on a WebSocket, so they must do this. A wrong token, or any other
  first frame, closes with `4001`.

Either way the token must arrive within 5 seconds of connecting, or the
connection is closed (`4002`). The token is never accepted in the URL.

A request with an `Origin` header (every browser sends one) is refused with
`403` unless that origin was passed with `--allow-origin`. Without this rule
any web page the user visits could open `ws://127.0.0.1:7878`.

There is no TLS. For remote use, tunnel (`ssh -L 7878:127.0.0.1:7878 host`)
or put a TLS-terminating proxy in front.

After authenticating, the server sends `hello` and then `ready`:

```text
→ {"op":"auth","token":"…"}
← {"event":"hello","protocol":1,"version":"0.1.0","node":"7c1d…","client":3,"seq":41}
← {"event":"ready","channel":"garden","node":"7c1d…","channels":[…],"bots":false,…,"seq":42}
```

## Frames

Every frame is a text frame holding one JSON object. Binary frames close
the connection (`1003`). A client frame may be at most 1 MiB (`1009`).

Every event the server sends carries:

- `event`: its name.
- `seq`: a counter for the whole process, rising by one per event. All
  clients see the same `seq` for the same broadcast event. A client also
  sees gaps where other clients were sent their own replies, so a gap is
  not by itself a loss; use the reconnect recipe below to resync.
- `re`: on events emitted while handling one of this client's requests, that
  request's `id`.

Clients must ignore events and fields they do not know. Events and fields
are only ever added; `hello.protocol` changes only for a change a client
could not ignore.

The server pings every 30 seconds; a client that sends nothing (not even a
pong, which every WebSocket library sends on its own) for 90 seconds is
closed.

## Requests

Every request except `auth` has an `id`, a number or a string the client
chooses, and an `op`. Requests from all clients are handled one at a time,
in arrival order, by the session's event loop.

| `op` | Fields | Does |
|---|---|---|
| `auth` | `token` | First frame, if the token was not in a header. No `id`, no `done`. |
| `send` | `channel`, `text`, `action?` | Publishes a message, or an action (`/me`) when `action` is true. Never read as an answer to a join request, and never as a command, even if it starts with `/`. |
| `answer` | `channel`, `member`, `accept` | Answers a waiting `join_request`: `accept: true` adds the member (their `joined` follows); `false` declines (`join_declined` with reason `declined`). |
| `history` | `channel`, `limit?`, `before?` | Stored messages, oldest first. `limit` defaults to 50, at most 500. `before` is a message's `op`: the page holds the messages before it. Without `before`, the newest page. |
| `snapshot` | | Sends `ready` to this client again. |
| `command` | `channel?`, `line` | Runs a `/command` as if typed with `channel` focused. |

`command` runs any of the chat commands (`/members`, `/invite`, `/create`,
`/join`, `/nick`, `/add`, `/remove`, `/promote`, `/status`, `/channels`,
`/msg`, `/me`, and the bot commands `/bot`, `/grant`, `/revoke`, `/grants`,
`/block`, `/unblock`, `/audit`, `/memory`, `/prompt`, `/schedules`,
`/unschedule`, `/forget`). Without `channel` it acts on the session's
default channel, the one `ready` names at its top level, which no client can
change. It refuses `/quit`, `/switch`, `/s` and the `/debug-*` test hooks,
and a line that does not start with `/` (use `send`).

### `done`

Every request (other than `auth`) ends with exactly one `done`:

```json
{"event":"done","re":7,"ok":true,"seq":120}
{"event":"done","re":8,"ok":false,"error":"no channel named #nowhere; see /channels","seq":123}
```

`ok` is false when the request failed or anything emitted under it was an
`error`; the `error` event (with the same `re`) comes first, and `done`
repeats its text. A frame that is not a valid request also gets a `done`
with `ok: false`, carrying `re` if the frame had a usable `id`.

`done` means the session has finished handling the request, not that the
network has: after a `send`, the `message` event for it arrives (to every
client) once the operation comes back through p2panda's stream, usually a
few milliseconds after `done`.

### Who receives what

Events emitted while a request is handled are routed by name:

- **Replies** go only to the client that asked, with `re`: `hello`,
  `ready`, `info`, `error`, `channels`, `members`, `invite`, `grants`,
  `audit`, `memory`, `prompt`, `schedules`, `history`, `bot`, `bot_model`,
  `done`. Any event name not listed under broadcast below is treated as a
  reply, so a new query command never leaks to other clients.
- **Changes** go to every client: `message`, `joined`, `promoted`,
  `removed`, `channel`, `nick`, `granted`, `revoked`, `blocked`,
  `unblocked`, `bot_on`, `bot_off`, `bots_started`, `bot_unavailable`,
  `bot_rejected`, `job`, `tool_call`, `schedule_fired`, `schedule_missed`,
  `unscheduled`, `forgot`, `join_request`, `join_declined`, `sync_started`,
  `sync_ended`. The asker's copy carries `re`; nobody else's does.

Events not caused by a request (a peer's message, a sync, a bot turn
finishing) go to every client, without `re`.

## Events

These are the `--json` events, unchanged, plus `hello`, `done`, `history`
and `lagged`. Field types are in [openapi.json](openapi.json); `node`,
`member`, `author`, `asker`, `host` and `peer` are 64-hex-character node IDs,
`op` a 64-hex-character operation hash, and `channel` a channel's local
name without `#`. A channel object (in `ready.channels`, `channels` and the
`channel` event) has `channel`, `group` (the space ID), `role`
(`creator`/`member`), `status` (`none`/`pending`/`joined`), `invite` (null
until joined, and always for the bot DM) and `focused` (the session's
default channel; not a client's focus).

### Socket

| Event | Fields | When |
|---|---|---|
| `hello` | `protocol`, `version`, `node`, `client` | First frame after authenticating. `client` numbers this connection. |
| `ready` | the default channel's fields, `node`, `channels`, `bots`, `bot_provider?`, `bot_servers?` | After `hello`, and in answer to `snapshot`. |
| `done` | `ok`, `error?` | Ends every request. |
| `history` | `channel`, `messages`, `more` | Answer to `history`. Each message has `author`, `text`, `nickname`, `op`, `mine`, `ts`, `bot`, `action` (and `bot_claim` when a bot claim was not accepted), as in `message`. `more` says older messages remain. |
| `lagged` | `seq` | Sent before closing with `4008` when this client is over 1024 frames behind. `seq` is the first frame it did not get. |

### Messages and channels

| Event | Fields | When |
|---|---|---|
| `message` | `channel`, `author`, `text`, `nickname`, `op`, `mine`, `ts`, `bot`, `ask`, `long`, `action`, `bot_claim?` | A message, including this node's own once published. `ts` is milliseconds since 1970. |
| `channel` | channel object | A channel was added (`/create`, `/join`, the bot DM). |
| `channels` | `channels` | Answer to `/channels`. |
| `switched` | `channel` | The session's focus moved. Never sent over the socket, which refuses `/switch`. |
| `invite` | `channel`, `invite`, `copied` | Answer to `/invite`. `copied` is always null over the socket. |
| `nick` | `nickname` | This node's nickname changed (`/nick`). |
| `info` | `text` | A note. |
| `error` | `text` | Something failed. |

### Membership

| Event | Fields | When |
|---|---|---|
| `members` | `channel`, `members`, `access` | Answer to `/members`. `access` maps node ID to `pull`/`read`/`write`/`manage`. |
| `joined` | `channel`, `member`, `members`, `access` | A member is in, including this node when a channel's space opens. |
| `promoted` | `channel`, `member`, `access` | A member's access changed. |
| `removed` | `channel`, `member`, `members` | A member left or was removed. |
| `join_request` | `channel`, `member`, `auto` | Someone asks to join a channel this node manages. Answer with `answer`, unless `auto`. |
| `join_declined` | `channel`, `member`, `reason` | A request was declined: `bot DM` or `declined`. |

### Sync

| Event | Fields | When |
|---|---|---|
| `sync_started` | `channel`, `peer`, `incoming_ops`, `incoming_bytes` | Log sync with a peer began. |
| `sync_ended` | `channel`, `peer`, `received_ops`, `sent_ops`, `error` | It ended. |

### Bots

| Event | Fields | When |
|---|---|---|
| `bots_started` | `provider`, `servers` | The bots started or changed provider. |
| `bot_unavailable` | `reason` | The bots could not start. |
| `bot_on`, `bot_off` | `channel`, `host`, `provider`, `servers` | A channel's bot was turned on or off. |
| `bot_rejected` | `channel`, `author`, `host` | A bot claim from someone other than the host. |
| `bot` | `channel`, `host`, `provider`, `servers`, `enabled`, `my_tools` | Answer to `/bot`. |
| `bot_model` | `model`, `running` | Answer to `/bot model`. |
| `job` | `channel`, `op`, `asker`, `state`, `reason`, and on `done`: `turn_ms`, `model_ms`, `tool_ms`, `requests`, `input_tokens`, `output_tokens`, `since_ask_ms`, `denied` | A bot turn's state: `queued`, `started`, `done`, `refused`, `abandoned`, `failed`. |
| `tool_call` | `channel`, `asker`, `tool`, `state`, `op` | A turn called a tool: `sent`, then `ok` or `error`; or `denied`. |
| `prompt` | `channel`, `system`, `tools`, `transcript_lines` | Answer to `/prompt`. |
| `granted` | `channel`, `member`, `pattern` | `/grant`. |
| `revoked` | `channel`, `member`, `pattern`, `removed` | `/revoke`. |
| `grants` | `channel`, `grants` | Answer to `/grants`. |
| `blocked`, `unblocked` | `member` | `/block`, `/unblock`. |
| `audit` | `channel`, `calls` | Answer to `/audit`. |
| `memory` | `query`, `memories` | Answer to `/memory`. |
| `forgot` | `id`, `removed` | `/forget`. |
| `schedules` | `schedules` | Answer to `/schedules`. |
| `unscheduled` | `id`, `removed` | `/unschedule`. |
| `schedule_fired` | `id`, `name`, `late_ms`, `next_at_ms` | A schedule fired. |
| `schedule_missed` | `id`, `name`, `late_ms` | A schedule was too late to run. |

## Close codes

| Code | Meaning |
|---|---|
| `1001` | The session ended, or the client did not answer pings for 90 seconds. |
| `1003` | The client sent a binary frame. |
| `1009` | The client sent a frame over 1 MiB. |
| `4001` | Unauthorized: the first frame was not `auth` with the right token. |
| `4002` | No token within 5 seconds. |
| `4008` | Lagged: more than 1024 frames behind. Preceded by `lagged`. |

Refusals before the upgrade are HTTP responses: `401` (wrong bearer token),
`403` (origin not allowed), `404` (not `/v1`), `405` (not GET), `400` (not a
WebSocket upgrade), `408` (the request head did not arrive within 5
seconds), `431` (a request head over 8 KiB).

## Reconnecting

There is no replay buffer. A client that reconnects, or was closed for
lagging:

1. Connects and authenticates; `ready` arrives on its own (or send
   `snapshot`). Replace your channel list with `ready.channels`.
2. For each channel you show, send `history` without `before`, and with
   `before` set to the oldest `op` of each page until you reach messages you
   already have.
3. Merge by `op`: drop any message whose `op` you already hold. Live
   `message` events that arrive while you page are merged the same way.

Join requests that arrived while you were away are not repeated; a manager
client should keep one connection open.

## Examples

A script with [websocat](https://github.com/vi/websocat):

```sh
TOKEN=$(cat ~/.local/share/triad/api-token)
websocat -H "Authorization: Bearer $TOKEN" ws://127.0.0.1:7878/v1
{"id":1,"op":"send","channel":"garden","text":"hello"}
{"id":2,"op":"command","channel":"garden","line":"/members"}
```

A browser (the page's origin passed with `--allow-origin`):

```js
const ws = new WebSocket("ws://127.0.0.1:7878/v1");
ws.onopen = () => ws.send(JSON.stringify({ op: "auth", token }));
ws.onmessage = (e) => {
  const ev = JSON.parse(e.data);
  if (ev.event === "ready") ws.send(JSON.stringify({ id: 1, op: "history", channel: ev.channel }));
};
```
