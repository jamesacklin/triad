# Implementation plan: the personal bot as an assistant

Written 2026-11-12 for an agent picking this up cold. It follows
[HARNESS-PLAN.md](HARNESS-PLAN.md), whose rules still hold, and uses the same
form: decisions first, then milestones with a test that says when each is
done.

## Goal

The personal bot in `#agent` should remember things between conversations,
act on a schedule without being asked, and follow standing instructions, the
way OpenClaw does. The channel bot keeps its current behavior and gains only
what is safe to show a whole channel.

The model for the gaps is OpenClaw (checkout at `~/src/openclaw`, docs under
`docs/concepts/memory.md`, `docs/automation/cron-jobs.md`,
`docs/concepts/agent-workspace.md`, `docs/tools/skills.md`). Read those for
behavior to copy. Do not copy its size: each feature below is the smallest
version that is useful.

## Where things stand

Branch `james/rig-tools-assistant-3773ad`, worktree
`/home/james/src/triad-worktrees/bot-model-api-key-config-136d78`.

**Stage 1 is built and uncommitted.** `cargo test --workspace`, `cargo fmt
--check` and `cargo clippy --workspace --all-targets` pass.

| File | Change |
| --- | --- |
| `crates/triad-agent/src/builtin.rs` (new) | `web.search` (Brave), `web.fetch`, `time.now`; the public-address check; HTML to text |
| `crates/triad-agent/src/tools.rs` | `Both` joins two tool sources; the gate cuts results to `MAX_RESULT_BYTES` |
| `crates/triad-agent/src/http.rs` | `pinned()`: a client fixed to one checked address, no redirects |
| `crates/triad-agent/src/lib.rs` | `builtin` and `search_url` in `AgentConfig`; `save_key` accepts `brave` |
| `crates/triad/src/bots.rs` | `/bot provider-key brave <key>` |
| `crates/triad/tests/bots_tools.rs` | Two assertions now expect `time` and `web` beside the MCP servers |
| `README.md` | "Built-in tools" |

Not done for stage 1: no run against the real Brave API, and no run with a
real model choosing these tools.

### How a turn works today

- `rig-core` is only the model client. `brain.rs` is the one file that names
  it. triad owns the loop (`ModelBrain::turn`, up to 16 requests), the tool
  definitions and the permissions.
- A tool source implements `Tools` (`list`, `call(name, args) -> String`).
  Tools are named `server.tool`. Every call goes through `Gate`, which checks
  the asker's `ToolSet`, counts steps and writes the audit log.
- The bot runs only when a message creates a job. `App::bot_job` in
  `bots.rs` asks `policy` about each recorded message; a job is a row in
  `bot_jobs` keyed by the asking message's `op_hash`, which is what makes a
  turn happen once across restarts.
- `triad-agent` never touches the state database. The binary hands it
  traits to call back through (`Audit`).

## Decided

1. **Memory and schedules belong to the personal bot only.** They are
   offered in `#agent` and nowhere else. A channel turn never sees a memory,
   and nothing said in a channel is saved as one. This keeps the isolation
   rule H5 tests.
2. **Tools learn who is asking.** `Tools::call` gains a context argument:
   kind (DM or channel), channel, asker, asker's tier. Tools get a scope
   (`Any` or `DmOnly`). A `DmOnly` tool is left out of what a channel turn is
   offered, and the gate refuses it there as well.
3. **Storage goes through a trait, like `Audit`.** `triad-agent` defines
   `Store` (memory and schedule rows); `bots.rs` implements it over
   `state.sqlite`. With the `sqlcipher` feature the new tables are encrypted
   with the rest.
4. **Memory is rows of text, searched by words.** No embeddings, no second
   model call. Use SQLite FTS5 if the pinned `rusqlite` build has it in both
   the plain and `sqlcipher` builds; otherwise match words with `LIKE`.
   Check before choosing.
5. **A schedule fires by posting a message.** When a schedule is due, the
   client posts its prompt in `#agent` as a line from the user, prefixed so
   it reads as scheduled (for example `[scheduled: standup] …`). That line
   becomes a job by the normal path, so limits, the transcript, the audit
   log and once-only delivery need no second implementation. No wire format
   change.
6. **A missed schedule fires once, late, or not at all.** On start, a
   one-shot schedule that came due while the session was down fires if it is
   less than a day overdue, and is otherwise dropped with a notice. A
   recurring one fires at most once for everything it missed.
7. **Schedules are simple.** A time (`at`), or an interval with an optional
   time of day (`every 1d at 09:00`). No cron syntax. Times are UTC unless
   `agent.json` has `utc_offset_minutes`; do not add a time zone library.
8. **Nothing here writes outside the data directory or runs code.** File
   tools and code execution are a later plan (see the end).

## What the user sees

In `#agent`:

```
> remember that the plumber is Dana, 555-0100
bot: Saved.
> who is the plumber?
bot: Dana, 555-0100.
> remind me every weekday at 9 to check the build
bot: Scheduled "check the build", next at 2026-11-13 09:00.
[scheduled: check the build] Remind me to check the build.
bot: Time to check the build.
```

Commands, host only: `/memory [query]`, `/forget <id>`, `/schedules`,
`/unschedule <id>`. They read and edit the same rows the tools do, so the
user can always see and remove what the bot keeps.

## Tools to add

| Tool | Scope | Arguments | Notes |
| --- | --- | --- | --- |
| `memory.save` | DM | `text`, `pinned?` | Returns the ID. At most 2,000 bytes each |
| `memory.search` | DM | `query`, `count?` | Newest first among matches |
| `memory.forget` | DM | `id` | |
| `schedule.add` | DM | `prompt`, `at?` or `every?` + `time?`, `name?` | Refuses more than 50 active |
| `schedule.list` | DM | | |
| `schedule.remove` | DM | `id` | |
| `skills.read` | Any | `name` | Reads `skills/<name>.md` from the data directory |

Pinned memories, up to 2,000 bytes in total, are put in the DM turn's system
prompt under a heading that says they are the user's saved notes. The system
prompt also tells the model, in the DM only, to save lasting facts the user
states and to search memory before saying it does not know.

## State

```sql
CREATE TABLE IF NOT EXISTS bot_memory (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS bot_schedules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    prompt TEXT NOT NULL,
    next_at_ms INTEGER NOT NULL,
    every_ms INTEGER,           -- NULL for a one-shot
    last_fired_ms INTEGER,
    created_at_ms INTEGER NOT NULL
);
```

`next_at_ms` is advanced in the same transaction that records the fire, before
the message is posted, so a crash can lose one fire and can never repeat one.

## Milestones

Build in order. Commit as `A<n>: <summary>`. `cargo test --workspace`, `cargo
fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass
at every commit.

| # | Milestone | Build | Passes when |
| --- | --- | --- | --- |
| A0 | Land stage 1 | Review the uncommitted diff and commit it. | Tests pass. By hand, with a Brave key and a real model: one question in `#agent` that needs a search and a fetch gets an answer citing a page. Record latency and tokens in FINDINGS.md. |
| A1 | Context and scope | The context argument on `Tools::call`; `Scope` on `ToolSpec`; `offered` and `Gate` enforce it. No new tools. | Unit test: a `DmOnly` fake tool is offered in a DM turn, absent from a channel turn for the host, and refused by the gate when a channel turn calls it anyway. All existing tests pass. |
| A2 | Memory | `Store`, `bot_memory`, the three tools, pinned notes in the prompt, `/memory`, `/forget`. | Process test on the mock brain: a scripted save, then a restart, then a scripted search returns the text. A pinned note appears in the next DM turn's system prompt in the mock log. Extend `bots_isolation.rs`: a saved memory never appears in any channel turn's input, and a channel turn is offered no `memory.*` tool. |
| A3 | Schedules | `bot_schedules`, the three tools, a timer arm in `event_loop`, `/schedules`, `/unschedule`. | Process test with a schedule two seconds out: the prefixed line appears once in `#agent` and gets one reply. Killed with SIGKILL after the fire is recorded and restarted: no second fire. A one-shot made overdue by editing the row fires once on start; one more than a day overdue does not. A recurring schedule's `next_at_ms` moves forward by its interval. |
| A4 | Standing instructions and skills | `skills.read`; the system prompt lists skill names and one-line summaries from `skills/*.md`; `persona` stays as it is. | Unit test on the prompt. Process test: a scripted `skills.read` returns the file's text; a name with `/` or `..` is refused. |
| A5 | Room to answer | In the DM only: raise `max_tokens` and the step limit, and send a reply over the one-message limit as a long message (`triad-core/src/long.rs`) instead of cutting it. | Process test: a mock reply of 5,000 bytes arrives whole in `#agent`; the same reply in a channel is still cut. |
| A6 | Stretch: upkeep | A built-in nightly schedule whose prompt asks the bot to merge duplicate memories and unpin stale ones. Off unless `agent.json` has `"upkeep": true`. | By hand on a real model, with the before and after rows recorded in FINDINGS.md. |

Stop and report, without working around it, if:

- A1 cannot add the context argument without changing what a channel turn
  is given.
- A2 finds any path by which memory text reaches a channel turn.
- A3 cannot make a fire happen at most once across a crash.

## Risks

| Risk | Where | What to do |
| --- | --- | --- |
| A web page tells the model to save a false memory or add a schedule. | A2, A3 | Results are already wrapped as data. Add to the DM prompt that only the user's own message may cause a save or a schedule. Test with a fetched page in the mock transcript on a real model, as H7 did for planted instructions. |
| A schedule's prompt runs with the host's full tool set while nobody watches. | A3 | Fired turns count against the DM's existing limits. `schedule.add` refuses an interval under five minutes. |
| The session is not running, so nothing fires. | A3 | Stated in "What this does not do". The fix is the always-on bot HARNESS-PLAN.md lists as a follow-up. |
| FTS5 missing from one of the two SQLite builds. | A2 | Decision 4: fall back to `LIKE`. |
| Memory grows without bound. | A2 | Refuse saves past 2,000 rows and say so in the reply. |

## What this does not do

- **File access, shell commands or code execution.** They need confirmation
  before a write, which HARNESS-PLAN.md lists as a follow-up, and a sandbox
  (`scripts/vm`). Write that as its own plan.
- **Memory or schedules in channels.**
- **Fire while the session is down.**
- **Summaries of old conversation.** The turn still sees the last 50
  messages; memory is the only thing that outlasts them.
- **Sub-agents, a browser, more search providers, caching.**

## Rules for the implementing agent

The eight rules in HARNESS-PLAN.md apply unchanged: read the pinned
versions' docs before calling them, hold the pins, no real model or service
in automated tests, never log a key or a model input, stay in scope, three
attempts then stop, and write FINDINGS.md as you go (a section "Assistant").
In addition:

1. **`rig-core` stays in `brain.rs`, `rmcp` in `mcp.rs`, and `triad-agent`
   stays out of the state database.**
2. **A changed assertion is a finding.** If an existing test must change,
   say which and why in FINDINGS.md, as stage 1 did for `bots_tools.rs`.
3. **Send test output to a file.** A failed `bots_tools` run leaves
   `mcp_fixture` processes that hold a pipe open, so `cargo test | grep`
   hangs. Use `cargo test … > out.txt 2>&1 < /dev/null`, and stop leftover
   fixtures with `pgrep -x mcp_fixture`.
4. **The full suite takes about four minutes.** Run single test files while
   working and the whole suite before each commit.

### Questions the findings must answer

- Does a real model save and recall without being told to in each message?
- Did a fetched page or a search result ever cause a save or a schedule?
- How late does a schedule fire, and what happens across a restart?
- What do pinned memories add to each DM turn in tokens?
