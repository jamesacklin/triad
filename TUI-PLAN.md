# Implementation plan: a ratatui interface in place of the web demo

2026-11-12 · ready to implement · implementer: Claude Opus 5.5

## Goal

`triad chat`, `triad create` and `triad join` open a full-screen terminal
interface built with [ratatui](https://ratatui.rs/):

```
┌ channels ───┐┌ #garden · 3 members · creator ──────────────┐
│ ● bot (DM)  ││ sam: hello                                   │
│ * garden  2 ││ me: @bot what's the weather                  │
│   general   ││ bot: …                                       │
│   ops (…)   ││                                              │
└─────────────┘└──────────────────────────────────────────────┘
 bot on · anthropic · tools fs,web │ turn running 4s → fs.read │ queue 1
┌──────────────────────────────────────────────────────────────┐
│ > message or /command                                        │
└──────────────────────────────────────────────────────────────┘
```

- **Sidebar:** every channel, the bot DM first, the focused one marked,
  channels still waiting to be let in marked, unread counts.
- **Chat pane:** the focused channel's stored history followed by live
  messages and that channel's notices.
- **Status line:** the focused channel's bot (on or off, provider, servers),
  the running turn and its current tool call, queue depth, the last
  finished turn's figures, a pending join request, the last error.
- **Input:** one line. Plain text is sent to the focused channel; a line
  starting with `/` is a command, exactly as in the plain prompt.

`crates/triad-web` is then deleted.

## Read this first

The interface is built on the CLI's `Session`
(`crates/triad/src/main.rs`, `crates/triad/src/bots.rs`), **not** ported
from `triad-web`. The web demo has one room per identity and its own
copy of the bot loop (`crates/triad-web/src/rooms.rs`), with no channel
list, job events, audit log, bot DM or long messages. Nothing in it is worth
carrying over. Do not read it for design guidance.

The session already has two narrow seams, and the whole design rests on
them:

- **Output.** All ~50 output sites call `Printer::emit(json_value,
  text_closure)` (`main.rs:481`), or `info`/`error` which wrap it. Bot worker
  tasks send the same `(Value, String)` pair through `bot_events`.
- **Input.** `spawn_stdin()` (`main.rs:725`) feeds lines into an
  `mpsc::Receiver<String>`, and `event_loop` passes each to `on_line`
  (`main.rs:1040`).

The interface is a view that consumes the emitted events and submits lines.
`Session` logic does not change.

## Constraints

1. **The `--json` line protocol is a tested contract.** Every integration
   test in `crates/triad/tests/` and every script in `scripts/` drives the
   binary through it over pipes. Do not rename or remove an event or a
   field. Adding a field is allowed.
2. **Plain mode stays.** With `--plain`, or when stdin or stdout is not a
   terminal, behaviour is byte-for-byte what it is today.
3. **Dependencies.** Add `ratatui` only, pinned exactly in
   `[workspace.dependencies]` like the others:
   `ratatui = { version = "=0.30.2", default-features = false, features = ["crossterm"] }`.
   Use crossterm through the `ratatui::crossterm` re-export; do not name
   `crossterm` in a manifest. No input-widget crate, no time or timezone
   crate, no async event-stream feature. If the minimal feature set does not
   compile or lacks something needed, add the smallest feature that fixes it
   (`layout-cache` is acceptable) and say so in FINDINGS.md.
4. **Cargo feature `tui`** on `crates/triad`, in its default features, with
   `ratatui` optional behind it. `cargo build -p triad --no-default-features`
   must build and behave as plain mode. The VM image
   (`scripts/vm/build.sh`) may want it off.
5. **No `Session` refactor.** Do not convert the emit sites to a typed
   event enum. Decode `serde_json::Value` at the UI boundary.
6. **The terminal is always restored**: normal quit, an error returned from
   `chat()`, and a panic. An error message must print after restoration, on
   the normal screen.
7. Toolchain is pinned in `rust-toolchain.toml` (1.98.1, edition 2024).
   `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D
   warnings` stay clean.

## Design

### Mode selection

Add `--plain` beside `--json` on `Create`, `Join` and `Chat`. The mode is:

| Condition | Mode |
| --- | --- |
| `--json` | JSON lines (unchanged) |
| `--plain`, or stdin or stdout is not a terminal, or built without `tui` | plain text (unchanged) |
| otherwise | TUI |

Use `std::io::IsTerminal`. The VM guest and all scripts pass `--json`, so
they are unaffected.

### Threads and channels

```
session task (tokio, unchanged logic)        UI thread (std::thread)
  Printer::Tui(tx) ── (Value, String) ──────▶ rx.try_recv → App::apply
  lines.recv() ◀───── String ──────────────── blocking_send on Enter
```

- `Printer` gains a third sink. Suggested shape: replace the `json: bool`
  field with `enum Sink { Text, Json, Tui(mpsc::UnboundedSender<(Value,
  String)>) }` and keep `verbose`. Two sites read `self.printer.json`
  directly (sync events, `main.rs` near 1595 and 1612); in TUI mode treat
  them as plain mode does (only with `--verbose`).
- `event_loop` takes the `mpsc::Receiver<String>` as a parameter. Plain and
  JSON modes pass `spawn_stdin()`; TUI mode passes the receiver whose sender
  the UI thread holds.
- The UI runs on one dedicated OS thread, like `spawn_stdin` does today. Its
  loop: `event::poll(50 ms)`; handle a key if there is one; drain the event
  receiver with `try_recv`; redraw if anything changed or a turn timer is
  showing. No tokio, no `EventStream`.
- **Shutdown.** Raw mode turns Ctrl-C into a key event, so the UI thread
  sends `/quit` on Ctrl-C (and on Ctrl-D with an empty input). When the
  session ends for any reason the `Printer` sender is dropped; the UI thread
  sees the disconnect, restores the terminal and returns. `chat()` joins the
  UI thread before returning its result. `ratatui::init()` installs the
  panic hook; call `ratatui::restore()` on the way out.
- Mouse capture stays off so terminal text selection works for copying
  invites.

Logging is already safe: `tracing` writes to `<data-dir>/triad.log`
(`main.rs:240`), not stderr. Check that nothing else on the chat path
prints to stdout or stderr while the alternate screen is up.

### Module layout

`crates/triad/src/tui/`, all behind `#[cfg(feature = "tui")]`:

| File | Contents |
| --- | --- |
| `mod.rs` | `spawn(...)` returning the line receiver and a join handle; terminal init and restore; the poll loop; key handling |
| `app.rs` | `App`, the model, and `App::apply(&Value, &str)`. Pure: no terminal, no I/O apart from the backfill reads passed in |
| `view.rs` | `fn draw(frame, &App)`: the four regions |
| `input.rs` | Single-line editor: `String` plus cursor, insert, backspace, delete, left/right, home/end, Ctrl-U, Ctrl-W, horizontal scroll when wider than the box. Cursor movement by `char`, display width by `unicode-width` (already in the tree through ratatui) |

`main.rs` is already 1,647 lines. Keep additions there to the seam.

### What the model is built from

**At startup**, before the first live event, from a second read-only
`State::open(data_dir)` connection (the bots already hold a second
connection for the audit log, so this is an established pattern):

- `groups()` for the channel list, `dm_topic()` for the DM.
- `history(topic, Some(500))` and `profiles(topic)` per channel for the
  backlog. Each `StoredMessage` has `op_hash`; live `message` events carry
  the same hash as `"op"`. Deduplicate on it, because a message can arrive
  live between the backfill read and the first event.
- `bot(topic)` for each channel's accepted bot (`BotRow`: host, provider,
  servers, enabled), and `queued_jobs()` for queue depth.

Backlog author labels must match the live ones. Live labelling is
`message_sender` (`main.rs:203`) plus `Session::label` (`main.rs:1325`) plus
`bot_accepted` (`bots.rs:236`). Extract a small shared function rather than
writing a third variant; `Cmd::History` (`main.rs:334`) has a second,
simpler variant that may stay as it is.

**Live**, from emitted events. The `"event"` values that exist today:

| Event | Effect in the UI |
| --- | --- |
| `ready` | Channel list (`channels[]` with `channel`, `role`, `status`, `focused`), own node, focus |
| `channel` | A channel was added and focused; backfill it |
| `switched` | Focus changed |
| `message` | Append to `channel`'s pane (`author`, `nickname`, `text`, `mine`, `bot`, `bot_claim`, `op`); bump unread if not focused |
| `joined`, `removed`, `promoted` | Dim notice in that channel; member count in the pane title |
| `join_request`, `join_declined` | Status-line prompt, see below |
| `job` | Status line: `state` is `queued`, `started`, `done`, `refused`, `failed` or `abandoned`, with `reason`. `done` also carries `turn_ms`, `model_ms`, `tool_ms`, `requests`, `input_tokens`, `output_tokens`. `refused` and `failed` also leave a dim notice in the channel |
| `tool_call` | Status line: `tool` and `state` (`sent`, `ok`, `error`, `denied`) for the running turn |
| `bot`, `bots_started`, `bot_unavailable`, `bot_rejected` | Bot segment of the status line; read `bots.rs` for when each fires |
| `info`, `error`, and anything unrecognised | A notice line using the event's text form. If it has a `channel` field it goes to that channel, otherwise to the focused one. Never drop an event silently |

The UI needs to know at startup whether this session's bots are running.
If no existing event says so reliably, add a boolean `bots` field to
`ready` (an additive change, allowed by constraint 1).

Do not show `tool_call` or routine `job` transitions in the chat pane; they
belong to the status line.

### Behaviour

- **Sending.** Enter submits the input line through the line channel and
  clears the box. The UI does no validation of its own; errors come back as
  `error` events.
- **Switching channels.** Ctrl-N and Ctrl-P (also Alt-Down and Alt-Up) move
  the sidebar selection and submit `/switch <name>`. Focus in the model
  changes only when `switched` arrives, so the UI and session cannot
  disagree. Switching clears that channel's unread count.
- **Scrolling.** PgUp and PgDn scroll the pane. The pane sticks to the
  bottom unless scrolled up; a new message while scrolled up does not move
  the view. Wrap messages to the pane width and recompute on resize.
- **Join requests.** The session's existing mechanism stays: with a request
  pending, the next non-command line is taken as its answer (`on_line`,
  `main.rs:1042`). The status line shows `join request for #x from <id> —
  y/N` while one is pending, and the input placeholder changes to match.
  Track pending requests from `join_request`, and clear on the next
  submitted non-command line or a `joined` for that member.
- **Timestamps.** Omit them. There is no time crate in the tree and one is
  not to be added.
- **Small terminals.** Below about 60 columns hide the sidebar; never panic
  on a tiny size.

## Steps

One commit per step on the current branch. Run the full check (below) at
the end of each. Do not push or open a pull request.

**T0 — Seam.** `Printer` sink enum; `event_loop` takes the line receiver;
`--plain` flag and mode selection (TUI mode may temporarily fall back to
plain). No behaviour change. All existing tests pass untouched.

**T1 — Skeleton.** Add the dependency and feature. `tui/` with terminal
init and restore, the poll loop, the four regions, the input editor, Enter
to send, live `message` events in the pane, quit paths. Unit tests for
`input.rs`.

**T2 — Channels and history.** Sidebar from `ready`/`channel`/`switched`,
backfill and dedup, shared author labelling, unread counts, pending
markers, focus keys, scrolling, wrapping, resize.

**T3 — Status line.** Bot segment, turn lifecycle with elapsed time, tool
calls, queue depth, last turn's figures, join-request prompt, last error.

**T4 — Remove the web demo.** Delete `crates/triad-web` and its workspace
member entry; refresh `Cargo.lock`. Add a `/nick <name>` command (the web
demo could change a nickname at runtime; the CLI only has `--nickname` at
start — reuse `publish_nickname`, `main.rs:1239`). Documentation:

- README.md: replace the "Local web demo" section (line 231 on) with one
  describing the interface, its keys, `--plain`, and trying two identities
  with two terminals and two `--data-dir`s.
- SPEC.md line 38 lists "TUI widgets" as out of scope; amend that row.
- FINDINGS.md: a new section, in the style of the existing ones. Include
  the release binary size before and after with the `tui` feature on and
  off, the ratatui features finally used, and what the web demo did that
  is now gone (several identities in one process, clickable invite links,
  the message chime). Leave the older sections that mention `triad-web`
  as written; they are a record.

## Testing

- **`App::apply`** unit tests: feed sequences of real event values
  (copy shapes from the emit sites) and assert on the model — focus,
  unread, dedup against backfill, job and tool-call state, join prompt,
  unknown events kept as notices.
- **Rendering** with `ratatui::backend::TestBackend`: assert on buffer
  contents for a populated model at 100×30, at 50×12 (sidebar hidden), and
  for a long message that wraps.
- **Existing integration tests** must pass with no edits. They are the
  proof that the seam changed nothing.
- **Smoke test by hand**, since tests never open a real terminal. Run in
  tmux and read the screen with `tmux capture-pane -p`:
  `TRIAD_AGENT_BRAIN=mock cargo run -p triad -- --data-dir /tmp/t-a create
  --name garden`, send a line, ask `@bot hello`, watch the status line,
  `/create second`, switch back and forth, `/quit`. Then confirm the shell
  is usable afterwards (cooked mode, cursor visible). Repeat the exit check
  after forcing an error.

Full check, each step:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p triad --no-default-features
```

## Done when

- The three chat commands open the interface on a terminal, and `--plain`,
  `--json` and piped use are unchanged.
- The four regions behave as described above, on a session with bots and
  on one with `--no-bot`.
- The terminal is restored on quit, on error and on panic.
- `crates/triad-web` is gone and the workspace builds and tests clean.
- README.md, SPEC.md and FINDINGS.md are updated as listed.

## Stop and ask

- If a needed fact cannot be got from the emitted events or a second
  `State` connection without changing `Session` logic.
- If keeping an existing test green would require editing the test.
- If ratatui 0.30.2 cannot be used with the pinned toolchain or would pull
  in a second copy of a crate the workspace pins.
