# Implementation plan: personal bots and channel-hosted bots

2026-11-11 · draft for review · implementer: Claude Opus 5.5

## Goal

Two kinds of bot, both running inside a user's own `triad` process:

- **A personal bot** for every user, reached in a DM that only they are in.
  It is siloed: it sees no channel and no channel sees it.
- **A channel bot** for every channel, run by the channel's host. Members
  address it in the channel (`@bot add this to Linear`). It uses the host's
  model and the host's MCP connections, and the host decides what members may
  do with it.

This plan builds that on `triad` as it stands at `628e58b`: one data
directory is one user, a channel is a p2panda encrypted space, and members
have `Manage` or `Write` access. It needs no new p2panda features.

Like the rest of the repository, this is an exploration. The code is
disposable and a new FINDINGS.md section is the deliverable.

## Decided

**Bots run in the user's own process.** They are linked into the binary and
publish through the user's node and key. A bot message is the user's message
with a flag saying the bot wrote it. Bots are online only while their user's
`triad` is running.

**Personal bots stay out of channels.** This replaces the earlier draft, in
which each user brought their own harness into a group and harnesses talked
to each other. There is now one bot per channel, so `/bring`, addressing
someone else's bot, and the hop limit are gone.

**Creating a channel means offering your bot to it.** The channel's creator
is its host. The bot uses the creator's connections and the creator's
inference, and every member can talk to it. A channel created by a user who
runs with `--agent` gets its bot on by default, announced to members. No
other member can run the channel's bot.

**Admins can do everything, other members cannot.** A member with `Manage`
access may use every tool the host's bot has. Any other member can talk to
the bot and gets no tools, unless the host has granted that member specific
ones. The rule for hosts is the same as for groups: do not make someone an
admin unless you would trust them with everything.

**Trust is configured by the host, never inferred.** Grants are explicit, per
member, per channel.

**The model is Anthropic `claude-opus-5-5` through `rig-core`,** with the key
in `ANTHROPIC_API_KEY`. MCP connections go through `rmcp`.

## What the user sees

```text
$ triad chat --agent
node    7c1d09ee
* #agent (creator, joined)                    # my personal bot, only me
  #garden (creator, joined)                   # I host this one
what's on my calendar tomorrow?
[#agent] my bot: Two things…

/switch garden
[#garden] me: This channel has a bot, run by me. It sends the channel's
          messages to Anthropic and can use: calendar, linear.
[#garden] 3f9a12c4: @bot add the hedge trimming to the calendar for Friday 9am
[#garden] bot: I can't use the calendar for you. Ask 7c1d09ee for access.
/grant 3f9a calendar.create_event
[#garden] 3f9a12c4: @bot add the hedge trimming to the calendar for Friday 9am
[#garden] bot: Added "Hedge trimming", Friday 09:00.
/audit
3f9a12c4  calendar.create_event  ok  2 min ago
```

## Architecture

```mermaid
flowchart LR
  subgraph proc["Host's triad process"]
    s["Session<br/>event loop"] --> pol["Policy<br/>answer? which tools?"]
    pol --> ctx["Context<br/>one channel only"]
    ctx --> b["Brain<br/>rig-core or mock"]
    b <--> t["Tools<br/>gate, then MCP or fake"]
    b --> s
  end
  s <-- "#agent: host alone" --> st[("state.sqlite")]
  s <-- "#garden: host and members" --> o["Members"]
  b -. "plaintext context" .-> p["model provider"]
  t -. "tool calls" .-> m["Host's MCP servers"]
```

Each message the session records is shown to the policy. The policy returns
whether to answer and the exact set of tools this asker may use. A job is
queued, the brain runs off the event loop with only those tools, and the
reply is published through the same `Space::publish` a typed line uses.

| Piece | Where | Owns |
| --- | --- | --- |
| Wire additions | `crates/triad-core/src/envelope.rs` | Optional `ChatBody` fields: bot flag, ask flag, control |
| Bot tables | `crates/triad-core/src/state.rs` | `settings`, `bots`, `bot_grants`, `bot_blocks`, `bot_jobs`, `bot_tool_calls` |
| Policy | `crates/triad-agent/src/policy.rs` (new crate) | Whether to answer and which tools. Pure: no I/O, no clock. |
| Context | `crates/triad-agent/src/context.rs` | Builds the model input for one turn from one channel |
| Brain | `crates/triad-agent/src/brain.rs` | `trait Brain`, the mock, and the only file that names `rig` |
| Tools | `crates/triad-agent/src/tools.rs`, `mcp.rs` | `trait Tools`, the permission gate, fake tools, and the only file that names `rmcp` |
| Wiring | `crates/triad/src/main.rs` | `--agent`, the DM, bot commands, `@bot` parsing, display |

`rig-core` and `rmcp` types must not appear outside `brain.rs` and `mcp.rs`.

## Wire format

`ChatBody` keeps `v: 1`. The new fields are optional and skipped when empty,
as `Invite.name` already is, so a body with none of them encodes to the same
bytes as today and an older build ignores the fields it does not know.

```rust
pub struct ChatBody {
    pub v: u8,
    pub sent_at_ms: u64,
    pub text: String,
    /// True when the author's bot wrote this, not the author.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bot: bool,
    /// True when this message addresses the channel's bot.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ask: bool,
    /// An announcement. `text` holds a readable fallback for old builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctl: Option<Control>,
}

pub enum Control {
    /// I run this channel's bot. Shown to members: where their messages go,
    /// and the names of the MCP servers the bot can reach.
    BotOn { provider: String, servers: Vec<String> },
    /// This channel's bot is off.
    BotOff,
}
```

The author of a bot message is the host, and p2panda has already verified the
author's signature, so no other member can forge a bot message.

`@bot` anywhere in a typed line sets `ask`. The sender's client refuses it
with an input error when the channel has no bot. A bot never reads `text` to
decide whether it was addressed; it reads `ask`.

## Rules

These are the acceptance rules. Each needs a unit test in `policy.rs` or a
process test.

**The personal bot**

- Its DM is a space named `agent` with one member. It is created the first
  time chat mode starts with `--agent`, and its ID is stored in `settings`.
- The DM never gains a member. `/invite` refuses it, and a join request for
  it is declined without asking, even under `--auto-accept`.
- It answers every line its owner types there. It may use every tool, because
  only its owner can ask.
- It has no access to channels: no channel history in its context and no tool
  that reads or posts to one.

**The channel bot**

- A channel has at most one bot, run by its creator: the host. Creating a
  channel while running with `--agent` turns the bot on and publishes
  `BotOn`. `/bot off` and `/bot on` change it, and work only on the node
  whose role in the channel is `creator`.
- Other members cannot verify who created a space. `Meta.creator` on a
  joiner is whoever handed out the invite. So a client accepts the first
  `BotOn` it sees in a channel from a member with `Manage` access, and from
  then on accepts `BotOn` and `BotOff` only from that same author.
- A message with `bot` true is shown as the channel's bot only when its
  author is that accepted host. Otherwise it is shown as an ordinary message
  with a note that the claim was not accepted.
- It answers a message when all of these hold: `ask` is true, `bot` is false,
  `ctl` is absent, the author is a current member, and no limit applies.

**What an asker may do**

Decided by the host's own view of the space's member list at the moment the
message is processed.

| Asker | Gets an answer | Tools offered |
| --- | --- | --- |
| The host | Yes | All |
| Member with `Manage` access | Yes | All |
| Member with grants | Yes | Only the granted tools |
| Member | Yes | None |
| Blocked node | No | None |
| Not a member | No | None |

- A grant names a member and a tool as `server.tool`, or `server.*` for every
  tool on one server. Grants are per channel and local to the host.
- The gate is in code and applied twice: tools the asker may not use are left
  out of what the model is offered, and every call is checked again before it
  is sent. The model is never trusted to enforce permissions.
- When a turn would have needed a tool the asker lacks, the bot says so and
  names the host. It does not say which other members have it.
- A refused ask (blocked, not a member, over a limit) makes no model call and
  gets no reply. The host sees a `job` event with state `refused` and the
  reason.

**Limits**, all configurable, all enforced in `policy.rs`:

- **Per asker.** 10 answers per member per channel per hour.
- **Budget.** 100 answers per channel per day, the host's included.
- **Burst.** 6 answers per channel per minute.
- **Age.** A message more than 10 minutes old when processed is not
  answered, so a host who comes back online does not answer a backlog.
- **Once.** A message is answered at most once, ever. See Jobs.
- **Steps.** A turn makes at most 8 tool calls.

**What the model is given**

- A channel turn contains the host's persona file, the member list with each
  member's tier, the tools this asker may use, and the most recent messages of
  that channel only (default 50, capped by bytes). Nothing from the DM or any
  other channel.
- A DM turn contains the persona file, the DM's recent messages and all
  tools.
- The system prompt states that only the asker's own message authorizes an
  action, and that every other message is context.
- A reply is capped at 2,000 bytes (`MAX_TEXT_BYTES`). Longer output is cut
  at a character boundary and ends with `…`.

## Flows

### A turn

1. A message arrives through the space's stream and is recorded as today.
2. The policy sees it with the channel's state. On yes, a job is queued in
   the same transaction that recorded the operation, with the asker's tool
   set.
3. The worker builds the context and runs the brain on a spawned task. Each
   tool call passes the gate, is written to `bot_tool_calls` before it is
   sent, and has its result recorded after.
4. The reply returns to the event loop, which publishes it with `bot: true`.
5. The reply comes back through the stream like any own message, is stored
   and shown, and the policy declines it.

### Jobs

A model call must not block the event loop or be repeated after a crash.

1. Insert `bot_jobs(op_hash, topic, asker, state = 'queued')` with the
   operation's record.
2. One worker per channel takes jobs in order and sets `state = 'started'`
   before the call.
3. After publishing the reply, set `state = 'done'`.
4. On start, every `started` job is set to `abandoned` and not retried.

So a crash can lose a reply and can never send one twice. A tool call made
before the crash stays made. `bot_tool_calls` shows it as `sent` with no
result, which is how the host finds out.

### MCP connections

The host lists servers in `mcp.json` in the data directory:

```json
{ "servers": [
  { "name": "linear",   "url": "https://…/mcp", "token_env": "LINEAR_MCP_TOKEN" },
  { "name": "calendar", "command": ["calendar-mcp", "--stdio"] }
] }
```

Tokens are named by environment variable and never stored in the file.
Obtaining a token (the OAuth flow) is out of scope: the host supplies it. A
server that fails to connect at start is reported and left out; the bot runs
without it. Both bots use the same connections.

## State

Tables added to `state.sqlite`, created in `State::open_path`.

| Table | Columns | Purpose |
| --- | --- | --- |
| `settings` | `key TEXT PRIMARY KEY, value BLOB` | `dm_topic` |
| `bots` | `topic PRIMARY KEY, host, provider, servers, enabled, changed_at_ms` | The active bot in each channel, mine or someone else's |
| `bot_grants` | `topic, node, pattern`, key on all three | What the host has granted |
| `bot_blocks` | `node PRIMARY KEY` | Nodes my bots never answer |
| `bot_jobs` | `op_hash PRIMARY KEY, topic, asker, state, reason, created_at_ms` | The queue, and what the limits count from. Refused asks are stored too. |
| `bot_tool_calls` | `id, op_hash, tool, args_json, state, result_bytes, at_ms` | The audit log |

`messages` gains `bot INTEGER NOT NULL DEFAULT 0`.

## CLI and events

| Command | Behavior |
| --- | --- |
| `create`, `join`, `chat` with `--agent` | Run my bots in this session. Creates the DM on first use. |
| `/bot on`, `/bot off` | Turn the focused channel's bot on or off and announce it. Creator only. |
| `/bot` | Show the focused channel's bot: host, provider, servers, and my own tools there. |
| `/grant <node> <server.tool \| server.*>`, `/revoke <node> [pattern]` | Edit grants in the focused channel. Host only. |
| `/grants` | List grants in the focused channel. Host only. |
| `/block <node>`, `/unblock <node>` | Edit my block list, which covers every channel I host. |
| `/audit [N]` | Show the last N tool calls in the focused channel: who asked, which tool, outcome. Host only. |

Without `--agent` nothing calls a model or a tool. Bot messages from other
hosts still display.

Environment: `TRIAD_AGENT_BRAIN` (`anthropic` or `mock`, default
`anthropic`), `TRIAD_AGENT_MODEL` (default `claude-opus-5-5`),
`ANTHROPIC_API_KEY`, `TRIAD_AGENT_PERSONA` (path, optional),
`TRIAD_AGENT_TOOLS` (`mcp` or `fake`, default `mcp`),
`TRIAD_AGENT_MAX_AGE_SECS` (600), `TRIAD_AGENT_ASKER_PER_HOUR` (10),
`TRIAD_AGENT_CHANNEL_PER_DAY` (100), and for tests `TRIAD_AGENT_MOCK_SCRIPT`,
`TRIAD_AGENT_MOCK_LOG` and `TRIAD_AGENT_FAKE_TOOLS`. Keys and tokens are read
from the environment only and never written to disk or logs. Starting with
`--agent`, the `anthropic` brain and no key is a usage error, exit code 2.

JSON changes: `message` gains `bot` and `ask`. New events: `bot_on` and
`bot_off` (from anyone, with `channel`, `host`, `provider`, `servers`), `job`
(host only, with `state` and `reason`), and `tool_call` (host only, with
`asker`, `tool`, `state`). Existing event names and fields do not change; the
current tests must pass unmodified.

Text display: `my bot:` in the DM, `bot:` in a channel.

### Test doubles

Every automated test uses these. No automated test may call a real model or a
real third-party service.

- **Mock brain.** With no script it replies `echo: <asker's text>`.
  `TRIAD_AGENT_MOCK_SCRIPT` is a file of lines `<substring> => <reply>` or
  `<substring> => call <server.tool> <json> => <reply>`; the first match
  wins. A scripted call goes through the real gate, so a script can attempt a
  call the asker is not allowed. `TRIAD_AGENT_MOCK_LOG` receives the full
  input of every turn, including the offered tool list, as one JSON line.
- **Fake tools.** `TRIAD_AGENT_FAKE_TOOLS` is a JSON file naming servers and
  tools. Each call appends a line to a log file and returns a fixed result.

## Milestones

Build in order. Commit as `H<n>: <summary>` and tag `h<n>` when the milestone
passes. `cargo test`, `cargo fmt --check` and `cargo clippy --all-targets --
-D warnings` pass at every tag.

| # | Milestone | Build | Passes when |
| --- | --- | --- | --- |
| H0 | Dependency spike | Add `rig-core` and `rmcp`, pinned exactly, to a new `triad-agent` crate. No behavior. | Release build succeeds. `cargo tree` still shows p2panda 0.8.0, iroh 1.4.1, rusqlite 0.37.0 and one libsqlite3-sys. Recorded: both versions, the binary size change, and whether TLS finds root certificates in a static build with no system certificate store. |
| H1 | Wire format and display | The `ChatBody` fields, `@bot` parsing, the `messages` column, JSON fields, labels. No bot. | A test proves a body with no new fields encodes to today's bytes, and that a copy of today's struct decodes a body that has them. All existing tests pass unmodified. |
| H2 | The personal bot | `--agent`, the one-member DM space, jobs, the mock brain. | Process test: the user sends three lines in `#agent` and gets three replies, in order, once each. The process is killed with SIGKILL after a job starts, then restarted: no reply is sent twice and the abandoned job is not retried. Another node holding an invite to the DM's space ID is never admitted. `inspect` finds no DM text in `p2panda.sqlite`. |
| H3 | The channel bot, talking only | `Control`, bot on at create, `/bot`, the answer rules, blocks and limits. No tools. | Process test with host A and members B and C: B and C see `bot_on` from A on joining. B's `@bot` line gets one reply shown as the bot's. Lines without `@bot` get none. After `/block B`, B gets none and C still does. With the per-asker limit set to 2, C's third ask is refused. After `/bot off`, nobody is answered. `/bot on` typed by B, who did not create the channel, fails locally. A `BotOn` injected by C, who lacks `Manage`, is not accepted by A or B. B asks while A is offline: A returns inside the age limit and answers once; in a second run A returns after it and does not answer. |
| H4 | Tools and permissions | `trait Tools`, the gate, fake tools, grants, the audit log. | Unit tests cover every row of "What an asker may do". Process test with host A, admin B (promoted) and member C, scripted so each ask attempts `calendar.create_event`: A's and B's calls reach the fake tool's log; C's does not, the mock log shows C's turn was offered no tools, and C is told to ask A. After `/grant C calendar.create_event`, C's call succeeds and `linear.create_issue` still does not. After `/revoke`, it fails again. `/audit` lists each call with its asker. |
| H5 | Isolation | The context builder's rules, tested. | Process test: a string said only in A's DM never appears in the mock log for a channel turn; a string said only in `#garden` never appears in a DM turn or in a `#other` turn; the DM turn's tool list contains nothing that reads or posts to a channel. |
| H6 | Real MCP | `mcp.rs` on `rmcp`: `mcp.json`, stdio and HTTP transports. | A small MCP server built as a test fixture exposes one tool. The H4 test passes against it over stdio and over HTTP, still on the mock brain. A server that is down at start is reported and the bot still answers. |
| H7 | A real model | The `rig-core` brain, the system prompt, the provider label. | An `#[ignore]`d test, run by hand with a key, completes the H2 exchange and one tool call against the fixture server. FINDINGS.md records latency and token counts. The mock tests still pass with no key set. |
| H8 | The bot in the VM | `scripts/vm` runs the guest with `--agent`. | `chat-test.sh` variant: the guest hosts a channel on the mock brain, the host machine joins and gets a reply from its bot. Then one manual run on the real model with an HTTP MCP server. Recorded: smallest guest memory that passes, and peak resident memory against the 26 to 28 MB of plain `triad`. |
| H9 | Stretch: encryption at rest | SQLCipher for `state.sqlite`, key from the environment at start. | The file contains no message text when scanned as `inspect` scans `p2panda.sqlite`. If the SQLCipher feature cannot coexist with p2panda's sqlx on one libsqlite3-sys, stop and record why. |

Stop and report, without working around it, if:

- H0 cannot add `rig-core` or `rmcp` without moving a pinned version.
- H1 cannot keep today's bytes for a plain message.
- H2 cannot create a space with one member, or cannot keep others out of it.
- H4 finds any path by which a tool call reaches a tool without passing the
  gate.

## Risks

| Risk | Exposed in | Fallback |
| --- | --- | --- |
| **A member without tools steers an admin's turn.** The channel history in an admin's turn includes messages from members who may not use tools. One of them can write text meant to make the model act when an admin next asks. The gate cannot see this: the asker really is an admin. | H4, H7 | The system prompt rule, and each message labeled with its author's tier. This is not enforcement. Record what the real model does with a planted instruction, and see the first follow-up below. |
| `rig-core` and `rmcp` were only resolved against the old tree, before OpenMLS was removed, and separately. | H0 | Take the newest versions that leave the pins alone. If `rig-core` cannot be added, write a minimal Anthropic Messages client on `reqwest` behind `Brain` and record why. |
| A one-member space may behave oddly: nothing to sync with, and its key bundle is announced on a topic. | H2 | Keep the DM out of p2panda: a local-only channel stored in `state.sqlite`. Record what the space did. |
| An admin running modified code publishes `BotOn` before the creator's reaches a member. That member then accepts the wrong host. | H3 | Not solved: members cannot verify the creator. An admin is already trusted with everything. Record what members see, and whether p2panda exposes a space's creator. |
| Admin status changes while an ask is queued, or the host's view of the member list lags. | H4 | The check uses the host's view when the message is processed. Record the window. |
| The guest has no shell or libc, so stdio MCP servers cannot run there. | H8 | HTTP servers only in the VM. |
| The guest has no certificate store, and has 12 MB free at 64 MB. | H0, H8 | Bundle web roots or ship a CA file. Measure and raise guest memory; the figure is the finding. |
| Spaces publish returns no operation hash. | H2 | Mark the job `done` at publish. The abandon rule covers a crash between the two. |
| The join-request prompt takes the next typed line as its answer. A bot reply arriving then must not be read as that answer. | H2 | Replies never pass through the stdin path. Add a test that holds a join request open while a reply arrives. |

## What this does not do

- **Confidentiality from the provider.** Every message in a bot turn goes to
  the model provider in plaintext, and tool arguments go to the MCP servers.
  `BotOn` says so in the channel. A channel that must stay end to end private
  cannot have a bot.
- **Privacy of the host's data from admins.** A tool result becomes part of
  the bot's reply, which the whole channel reads. An admin who asks what is on
  the host's calendar shows it to everyone.
- **A bot when the host is offline.** The channel bot runs in the host's
  process.
- **Proof that a message came from a model.** `bot` is set by the host's
  client, and a host running modified code can set it on anything they send.
- **Confirmation before a write.** An allowed call runs at once.
- **Obtaining MCP tokens, encryption at rest (unless H9), more than one bot
  per channel, streaming, attachments, replies longer than one message.**

## Rules for the implementing agent

1. **Read before calling.** `rig-core`, `rmcp` and p2panda 0.8 are newer
   than most training data. Read the docs and `examples/` of the exact pinned
   version before writing a call. Do not write calls from memory.
2. **Hold the pins.** No version changes to existing dependencies, no git
   dependencies, no forks.
3. **Existing tests are a contract.** `spaces_two_node.rs`,
   `spaces_channels.rs`, `spaces_membership.rs` and `cross-test.sh local` pass
   unmodified at every tag.
4. **No real model or third-party service in automated tests.**
5. **Never log a key, a token or a model input** above trace level.
6. **Stay in scope.** Anything outside this plan goes under Proposed
   follow-ups in FINDINGS.md.
7. **Three attempts, then stop** and report what was tried.
8. **Write FINDINGS.md as you go.** Add a section "Bots" with: a milestone
   table, the measurements each milestone names, every place this plan was
   wrong, and answers to the questions below.

### Questions the findings must answer

- Given a planted instruction from a member without tools, did the real model
  act on it during an admin's turn?
- How long from `@bot` to a reply on another member's screen, split into
  sync time, model time and tool time?
- What does a member see when the host is offline, and when the host returns?
- Did anything from outside a channel ever appear in a channel turn's input?
- Did a one-member space work as the DM, and what does p2panda store and
  announce for it?
- What do the bots cost in guest memory and binary size?
- Is "admins everything, others nothing, plus grants" enough, or did the
  tests want something between?

### Follow-ups already expected

- Confirmation by the host, or by the asker, before a tool that writes. MCP
  tool annotations mark read-only tools, which would let reads run freely.
- A bot that stays up when the host is offline, as its own node in a VM.
- Per-channel tool sets, so a host can leave a connection out of one channel.
