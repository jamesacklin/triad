# triad findings

Findings from building `triad`, a two-member command-line chat on p2panda 0.8,
iroh 1.4 and OpenMLS 0.9. Written as the work happened, on 2026-11-11.

**The OpenMLS code is no longer in the repository.** Following the
[recommendation below](#openmls-and-p2panda-spaces), the `triad` binary
(`crates/triad-cli`) and the OpenMLS and acceptance-rule modules of
`triad-core` (`mls.rs`, `group.rs`, the p2panda wrapper in `net.rs`, the
envelope types) were removed
on 2026-11-11. The spaces binary, called `triad-spaces` throughout this
document, is the only one left. It was then renamed `triad` (`crates/triad`,
default data directory `…/triad`) and took over `triad probe`. So below,
`triad` means the removed OpenMLS binary and `triad-spaces` means today's
`triad`. Everything about OpenMLS, epochs, KeyPackages and Welcomes records
work at or before commit `de3f814`, where that code can still be read and
run. Today's `triad` refuses a data directory that holds an `mls.sqlite`.

## Review against SPEC.md (2026-11-11)

Reviewed repository revision `28827445334509a6ed93978269acf20d054f7968`.
The milestone results and measurements below describe the original author's
runs, not fresh measurements from this review. The implementation follows the
intended layering and keeps dependency pins, ciphertext transport, explicit
acks and persisted plaintext history. It is still an exploration. The peer
tests the review could not run were later rerun on a network-capable host and
pass (see
[Rerun on a network-capable host](#rerun-on-a-network-capable-host-2026-11-11)).
The cross-network acceptance run later passed too (see
[Milestone 5](#milestone-5-two-machines-on-different-networks-2026-11-11)).

### Revisions

| Priority | Finding | Revision and regression coverage |
| --- | --- | --- |
| P1 | `process_commit` merged a commit before the caller checked the MLS sender against the operation author. A rejected envelope could already have changed membership. Welcome validation likewise installed the group before checking its creator. | Validate the commit sender before merging; validate the Welcome's group, authenticated sender and member identities before installing its group. Semantic identity rejections are skipped and acknowledged. Tests check that rejected input leaves both the live and persisted epoch unchanged. |
| P1 | In the three-member extension, another author's new-epoch App can arrive before the creator's Commit. The old code recorded a permanent failure and acknowledged a message that would decrypt after the commit. | Keep future-epoch Apps unacknowledged, deduplicate them in a bounded queue, and retry after epoch changes. At 256 waiting operations the session exits with an error, leaving them for replay. A deterministic rule test delivers App before Commit and then successfully processes it. This does not introduce a total order across authors. |
| P1 | An App containing a Commit, or the reverse, reached `process_message` before its content type was checked and could consume the legitimate message's single-use key. | Check the protocol content type first, and require a PrivateMessage for App. Real OpenMLS tests route each message through the wrong handler, then verify the correct handler can still consume it. |
| P1 | Chat logged storage, publication and acknowledgement errors and kept processing. The topic stream could also close and still return exit code 0. | Stop the session on these errors, return code 1 and bound runtime shutdown on error paths. A SQLite trigger injects a history-write failure: the process exits, records no processed operation and emits no successful message event. Input validation errors remain recoverable. |
| P1 | `inspect` ignored read errors. The spaces variant even reported success for a missing database. | Require the database to be readable and ignore only missing optional SQLite sidecars. Tests cover a missing database, plaintext present only in WAL, and an unreadable sidecar represented by a directory. |
| P2 | The 2,000-byte text cap and envelope cap were enforced only on sending; CBOR decoding accepted trailing data. | Enforce the text cap on decoding, the envelope cap before receive-side MLS calls, and exact CBOR consumption. Tests include multibyte UTF-8 boundaries. |
| P2 | Chat trimmed leading and trailing whitespace before validating and sending. | Preserve text as entered. A process test verifies exact whitespace and recovery after oversized UTF-8 input. |
| P2 | The replay test printed the second restart's events without asserting that replay was empty. The cross-machine script stopped reading after five catch-up messages, then counted only the output it had already read. | Assert the five-operation first replay and empty second replay; drain stdout through shutdown before counting duplicates and reject failed process exits. A shell regression with a delayed sixth message confirms the drain counts it. The two-node test now checks both authors' history order and scans every sent message for plaintext. Test subprocesses are reaped on failure. |

### Validation in this review

Review host: Linux x86_64, kernel 6.18.57, Rust 1.98.1. No dependency
versions changed. The CLI tests add a dev-dependency on the already pinned
`rusqlite` to inject a real persistence failure.

- `cargo test --workspace --lib --locked`: 44 core tests pass, including real
  MLS create/join, bidirectional encryption, persistence, three-member epochs,
  identity rejection and message-type routing.
- `cargo test --workspace --locked -- --skip two_node_chat_and_catch_up
  --skip unacked_operations_replay_after_kill --skip three_members_with_commit`:
  passes. This runs the inspection test and both single-process CLI regressions.
  The filters also omit the core three-member test, which is covered by the
  separate library run. The existing two relay measurement tests stay ignored.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked
  -- -D warnings`, `git diff --check` and `bash -n scripts/cross-test.sh`: pass.
- Full `cargo test --workspace --locked` was attempted. All three peer-based
  CLI tests timed out before joining. Node logs show `Network is unreachable`
  for mDNS and connection/discovery timeouts in this restricted environment.
  `cross-test.sh local` likewise timed out before the join. No data-loss result
  was observed because the peers never established a chat session. Neither run
  is a pass; the end-to-end revisions need a rerun on a network-capable host.
- The cross-test output-drain regression uses synthetic JSON through a FIFO.
  It verifies the script's late-duplicate check, not network delivery.

No milestone tag was added or moved. In particular, these checks do not satisfy
Milestone 5 or establish cross-network behavior for spaces.

### Rerun on a network-capable host (2026-11-11)

Revision `d2b362df4af8af1c4ed81ba1ac7b5e1e0b826735` (the review's revisions),
run on the original author's host: Linux x86_64, kernel 7.0.0-43, Rust 1.98.1,
with the public relay reachable. The review could not run the peer tests in its
sandbox; this run covers them.

- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets
  --locked -- -D warnings`: pass.
- Full `cargo test --workspace --locked`: pass, with no tests skipped beyond the
  two relay measurements, which stay ignored. That is 44 `triad-core` unit tests,
  5 `two_node.rs` process tests and 1 spaces process test. The 5 process tests
  include the three peer tests that timed out in the review:
  `two_node_chat_and_catch_up`, `unacked_operations_replay_after_kill` and
  `three_members_with_commit`. The same 5 tests also pass with
  `--test-threads=1`.
- `scripts/cross-test.sh local`, with `target/debug` on `PATH`: PASS in 9 s.
  Both members reached the same epoch with the same two members. Both histories
  had 20 messages. The joiner caught up on exactly 5 messages in 1 s with a sync
  observed. Neither `p2panda.sqlite` contained plaintext, and the joiner received
  no duplicates (10 + 5 messages from the other member).

So the review's end-to-end revisions hold between separate processes on one
host. This is still not Milestone 5: both processes share one network, and the
path type, join latency and catch-up time across networks remain unmeasured.

### Remaining limitations

The spec explicitly leaves the MLS/state.sqlite crash gap open. It also affects
handshake progress: a crash after the creator merges an add but before publishing
the Welcome can leave the joiner pending; a crash after the joiner installs its
MLS group but before updating app metadata can leave the stores inconsistent.
The KeyPackage publish and its metadata flag are separate writes too, so the
"exactly one KeyPackage across any crash" claim is stronger than the code.
These windows need a durable handshake/outbox design in a subsequent iteration;
this review does not silently claim to make them atomic.

When the session stops on a persistence error, shutdown is not clean. In about
half the runs of `history_write_failure_ends_session_without_acknowledging`
(3 of 6 consecutive runs), the triad process prints a panic from p2panda's store
during shutdown. The panic is at `p2panda-store-0.8.0/src/sqlite.rs:356`, the
assertion "can't have an already existing transaction after an just-acquired
permit". The test still passes because the exit code is 1 and nothing was
acknowledged. The likely cause is that the bounded runtime shutdown drops a task
while it still has a p2panda store transaction open. Ending the p2panda node
before tearing down the runtime should remove the panic. This rerun did not
change it.

`group.rs` still invokes mutable MLS through `GroupCrypto`, so it is testable
without a network or clock but is not the spec's strictly pure state machine.
The existing deviation below describes that choice. Spaces have since passed
the cross-network run (Milestone 6) and the offline-membership tests (see
[Spaces under offline membership changes](#spaces-under-offline-membership-changes-2026-11-11)),
which also found behaviours an app must design around.

## Milestone 5: two machines on different networks (2026-11-11)

**Result: pass, in both directions.** Acceptance steps 1 to 9 pass with the Mac
creating and Linux joining (run 1, by hand), and again with the roles swapped
(run 2, `scripts/cross-test.sh` over SSH). The swapped run covers step 10.
Both machines ran code at `0013f14`, which matches the review's `d2b362d`. Only
the full same-LAN chat run is still to do. Milestone 6 (spaces) has since
passed on the same two machines (see
[Milestone 6](#milestone-6-p2panda-spaces-on-two-machines-2026-11-11)).
Milestone 7 needs only one machine, by the spec.

| | Mac | Linux |
| --- | --- | --- |
| Machine | MacBook Air, Apple silicon, macOS 26.6.2 (25G83) | Ubuntu 26.04.1, Linux 7.0.0-43, x86_64 |
| Network | 5G phone hotspot (`172.20.10.3`, carrier IPv4 `174.209.184.31`, plus global IPv6). Tailscale stopped. | Wired home LAN behind NAT (`192.168.1.216`, public `108.32.35.114`), no global IPv6. Tailscale running, but the Mac had no tailnet route. |
| Run 1 role | Creator, node `16197d43` | Joiner, node `77f7cdc2` |
| Run 2 role | Joiner, node `03bb1ca0` | Creator, node `eb231add` |

### Run 1: Mac creates, Linux joins (by hand)

Run with `--data-dir ~/triad-m5` on both machines, group topic `5033c0d9`.

| Step | Check | Result |
| --- | --- | --- |
| 1–3 | Create, join, 2 members at epoch 1 | Pass. Linux printed `you joined (epoch 1, 2 members)` after two sync sessions with the Mac. |
| 4–5 | 10 messages each way, in order | Pass. Both histories list the same 20 messages in send order, with identical timestamps. |
| 6–7 | Linux offline, Mac sends 5, Linux catches up | Pass. Linux processed exactly 5 new operations (`ok` went from 22 to 27), `mac-11` to `mac-15`. Its history has 25 messages and no duplicates. |
| 8 | No plaintext in `p2panda.sqlite` | Pass. `mac-01` and `linux-01` are absent on both machines, and `mac-15` on Linux. The scanned files were 1.86 MB on the Mac and 1.94 MB on Linux, including the WAL. |
| 9 | Same epoch and members, nothing failed | Pass. Both show epoch 1, the same two members and `ok=27 skipped=0 failed=0`. |

### Run 2: Linux creates, the Mac joins (scripted)

`scripts/cross-test.sh` ran on Linux with the Mac as the SSH remote, reached
through a Cloudflare quick tunnel from the hotspot (see the README). The tunnel
carries only the SSH session that drives the Mac's `triad`, and iroh does not
see it. Fresh data directories were used on both sides. The script reported
PASS in 21 s:

- The Mac joined 9 s after the creator started, with 2 members at epoch 1 on both
  sides.
- Each side received the other's 10 messages in order, and both histories had 20.
- The Mac went offline, Linux sent 5, and the Mac caught up on exactly 5 in 2 s
  of script time, with a sync observed.
- Neither `p2panda.sqlite` contained plaintext.
- Both showed the same epoch and members, and the Mac received no duplicates
  (10 + 5 messages from Linux).

### Measurements

| Measurement | Run 1 (Linux joins from the LAN) | Run 2 (the Mac joins from the hotspot) |
| --- | --- | --- |
| KeyPackage publish to first sync with the creator | 1.11 s | 7.64 s |
| KeyPackage publish to `joined` | 1.37 s | 7.77 s |
| Catch-up of 5, from restart to all 5 received | 1.0 s | 0.45 s from the restarted node choosing its relay |
| Sync sessions per connection | 2 for the join, 2 for the catch-up | Same |

The joiner's own logs give these figures. The difference in join time is
discovery: once the two nodes had a connection, the join took 0.13 s in run 2.
Run 2 measured 7.6 s before the hotspot joiner and the LAN creator found each
other. One run in each direction can't separate the joiner's network from noise
in discovery.

The spec's step 7 expects one sync. Both runs show two concurrent sessions
per connection, each carrying all five operations. The likely cause is that
each side opened one. p2panda and `processed_ops` deduplicated them, so exactly
five messages appeared. The second session wastes bandwidth but does no harm at
this scale.

### Path

`triad` does not log the path a connection uses, so the chat runs' paths are
not recorded directly. Probes between the same two machines give the evidence:

| Probe | Path | Connect | Last echo | QUIC path RTT |
| --- | --- | --- | --- | --- |
| Same LAN | direct (`Ip(192.168.1.216:47935)`) | 27 ms | 6.5 ms | 5.7 ms |
| Hotspot, Mac connects (before run 1) | relayed | 171 ms | 88 ms | 153 ms |
| Hotspot, Mac connects to Linux (after run 2, 2 runs) | relayed, both | 117–120 ms | 99–106 ms | 106–123 ms |
| Hotspot, Linux connects to the Mac (after run 2, 2 runs) | relayed, both | 98–105 ms | 79–120 ms | 109–119 ms |

All five probes between the hotspot and the LAN were relayed through
`use1-1.relay.n0.iroh.link`, in both directions. The Mac advertised its
carrier IPv4 address and two global IPv6 addresses. Linux has no global IPv6,
so an IPv6 path was not available. The carrier's IPv4 NAT did not allow hole
punching within the probe's 2 seconds of echoes. The chat worked well over the
relay: messages arrived within the 2-second gaps between sends, and catch-up
took about a second.

The joiner in run 1 picked its home relay after its sync sessions had
finished. That was 1.6 s after `joined` in the join, and 2.0 s after the last
sync session ended in the catch-up. So connecting did not wait for that side's
home relay.

## Milestone 6: p2panda spaces on two machines (2026-11-11)

**Result: pass.** `triad-spaces` passes acceptance steps 1 to 9 between the
same two machines as Milestone 5. The Mac was on the 5G hotspot with Tailscale
stopped, and Linux was on the wired LAN. `scripts/cross-test.sh` ran on Linux
with `TRIAD=triad-spaces EXPECT_EPOCH=null`, and drove the Mac over the same
Cloudflare SSH tunnel. Linux created the space and the Mac joined. Both binaries
were built from `0013f14`. The script change only adapts its checks, as follows:

- Spaces report `"epoch": null`, so step 3 checks the member count and that the
  epoch is null.
- `status` lists neither an epoch nor members, so step 9 checks `failed=0` and
  says that there is no member list to compare. The member count was already
  checked at step 3. Without this change, step 9 would have compared two empty
  values and passed without checking anything.

Before the real run, a one-machine smoke test (`cross-test.sh local`) passed in
8 s.

To separate the two variants from noise, the OpenMLS and spaces runs alternated
over the same tunnel, all with Linux creating and the Mac joining from the
hotspot. All six passed:

| Variant | Join (script time, from creator start) | Catch-up of 5 | Total |
| --- | --- | --- | --- |
| OpenMLS (`triad`) | 9 s, 7 s, 8 s | 2 s, 2 s, 2 s | 21 s, 19 s, 20 s |
| p2panda spaces (`triad-spaces`) | 4 s, 6 s, 7 s | 3 s, 2 s, 2 s | 16 s, 19 s, 20 s |

The script measures in whole seconds, and its join time includes starting the
joiner over SSH, about 1 s through the tunnel. Across networks the two variants
are close. Spaces joined a little sooner in each pair, but the ranges overlap
at 7 s, and both catch up in about 2 s. The one-machine gap (spaces caught up in
0.4–0.5 s against 1.25–1.34 s for OpenMLS) does not show up at this
resolution. Discovery and the relay dominate both.

Every spaces run had exactly 15 messages from the other member at the joiner,
with no duplicates. Neither database contained `linux-01` or `mac-01` as
plaintext.

What this run does not test: membership changes while a member is offline,
which is the main remaining claim in the comparison table, and spaces with the
Mac as the creator.

## Spaces under offline membership changes (2026-11-11)

The comparison below credits spaces with handling membership changes while
members are offline, with no single committer. These tests check that claim.
They live in `crates/triad-spaces/tests/spaces_membership.rs` and are
`#[ignore]`d, at about a minute each:

```sh
cargo test -p triad-spaces --test spaces_membership -- --ignored
```

They run separate processes on one machine, each with its own data directory.
"Offline" means the process is stopped. Each node either starts from the
space's invite or, for a member added by a second manager, from an invite that
bootstraps through that manager.

To drive these cases, `triad-spaces` gained chat commands for p2panda calls it
already had access to: `/add`, `/remove` and `/promote` (to `Manage`). It also
gained `removed` and `promoted` events, and access levels in `/members`. Any
member with `Manage` access now handles join requests, not only the creator.
Removing members is outside the spec's scope for the app itself. Here it serves
only to test the library.

All four tests pass, in two consecutive runs of the whole file (74 s each):

| Test | Scenario | Result |
| --- | --- | --- |
| `add_while_member_offline` | B goes offline. A adds C, and A and C exchange messages. B returns. | Pass. B lists C as a member 1.0 s after restarting, has read both missed messages (A's and C's) by 1.1 s, and A and C read B's next message. |
| `remove_while_members_offline` | C goes offline. A removes C while B watches, and writes. A and B go offline. C returns alone, unaware, and writes. Everyone reconnects. | Pass on the safety checks: A and B never show the messages C wrote after its removal, and C never reads anything A wrote after it. A converges at once. |
| `concurrent_adds_by_two_managers` | A promotes B to `Manage`. From then on A and B are never online together. A adds D; B adds E through an invite that bootstraps through B. Everyone reconnects. | Pass. All four nodes list the same four members 1.5–4.3 s after reconnecting, and each reads the other three's new messages. |
| `removing_a_manager_invalidates_its_concurrent_add` | A promotes B to `Manage` and adds C. B goes offline, and A removes B. B returns without A, unaware, and adds E. Everyone reconnects. | Pass. As p2panda-auth's "strong removal" rule says, B's addition of E is invalidated. A and C converge on {A, C} in 0.2–0.3 s, and B and E reach the same list within 2 s. A and C show neither B's nor E's messages from that period, and B and E cannot read A's next message. |

So the claim holds for the scenarios that matter most here: no single
committer is needed, and concurrent changes converge. The first test is what an
OpenMLS group with a single committer cannot do: there, adding C needs the
creator online and then a Welcome, and B must process the Commit before it can
read C. Here it simply works. The tests also found four behaviours an app has to
design around:

- **A removed member is not told.** In `remove_while_members_offline`, C
  reconnected for about 45 s and never received its own removal. It still
  listed itself and both others as members, and its messages appeared to send
  but reached no one. The likely reason is that A and B stop syncing with a
  removed node. In the strong-removal test, by contrast, the removed B (and E)
  did learn the outcome within 2 s. One run each cannot explain the difference.
  An app that needs a removed member to know must tell it some other way, and
  must not take "my message published" as "my message was delivered".
- **A removed member who reconnects looks like a new join request.** Its key
  bundle is announced again, and the manager's stream reports `Member` exactly
  as for a newcomer. `triad-spaces` with `--auto-accept` would add C straight
  back. These tests run the manager without auto-accept and decline the request.
  An app must remember removals itself, or check the space's history, before
  treating a key bundle as a request.
- **Stale join requests.** A key bundle can arrive just before the membership
  change that adds its owner, for example when another manager added them. The
  first version of the prompt then took the next chat line as the answer. The
  prompt now drops requests from nodes that are already members. This was an
  app bug, not a library one, but the race comes from the library's event
  order.
- **New members can read history from before they joined.** C was added after
  A sent `a-1`, but C shows `a-1`. In the concurrent-adds test, E shows `a-1`,
  sent by A before E's addition, and A and D show `b-1`. MLS does not allow
  this: a joiner reads only from its join epoch onward. Whether this is wanted
  depends on the product. It means adding someone exposes the space's past
  messages to them.

## Bots (2026-11-11)

Personal bots and channel-hosted bots, built to
[HARNESS-PLAN.md](HARNESS-PLAN.md) on top of `628e58b`. The code is in the new
`crates/triad-agent` (policy, context, brain, tools, MCP) and in
`crates/triad/src/main.rs` (wiring). Every automated test uses the mock brain
and fake or fixture tools; none calls a model or a third-party service.

### Milestones

| # | Milestone | Result | Tag | Notes |
| --- | --- | --- | --- | --- |
| H0 | Dependency spike | pass | `h0` | rig-core 0.46.0, rmcp 3.9.0; pins held; no aws-lc |
| H1 | Wire format and display | pass | `h1` | plain bodies keep today's bytes |
| H2 | The personal bot | pass | `h2` | one-member space works; strangers declined |
| H3 | The channel bot, talking only | pass | `h3` | including a join prompt held open across a reply |
| H4 | Tools and permissions | pass | `h4` | gate applied twice; no bypass found |
| H5 | Isolation | pass | `h5` | nothing crossed between DM and channels |
| H6 | Real MCP | pass | `h6` | rmcp client over stdio and HTTP against a fixture server |
| H7 | A real model | built; live run handed over | `h7` | rig wiring checked offline; no API key on this host |
| H8 | The bot in the VM | pass (mock); real-model run handed over | `h8` | 48 MB smallest passing guest; +4 MB peak RSS |

### H0: dependencies

- **Versions.** `rig-core` 0.46.0 (the newest on crates.io; the earlier desk
  research resolved 0.44.0) and `rig-reqwest` 0.46.0, `rmcp` 3.9.0, all
  pinned exactly. `cargo tree` still shows p2panda 0.8.0, iroh 1.4.1,
  rusqlite 0.37.0 and one libsqlite3-sys (0.35.0, shared by rusqlite and
  sqlx-sqlite). No git dependencies.
- **The plan was wrong about TLS being free.** With their default TLS
  features, both `rig-core` (`rustls`) and `rmcp` (`reqwest`) turn on
  reqwest 0.13's `rustls` feature, which selects rustls' **aws-lc-rs**
  provider and adds `aws-lc-sys`, a C library built with cmake. That is a
  second crypto provider next to the ring that iroh uses, and a C toolchain
  dependency for the static VM build. Instead: `rig-core` with no features,
  `rig-reqwest` with no features, `rmcp` with `reqwest-tls-no-provider`, and
  reqwest with `rustls-no-provider`. `triad-agent` installs ring as the
  process's rustls provider before any bot HTTP (reqwest panics on the first
  client build otherwise, and rmcp builds clients of its own), and hands one
  `reqwest::Client` to both libraries (`AnthropicConfig::connect`,
  `StreamableHttpClientTransport::with_client`). `aws-lc-rs` is absent from
  the tree; `openssl-probe` (pure Rust, already present) is the only
  OpenSSL-named crate.
- **Root certificates with no system store.** reqwest's rustls path uses
  `rustls-platform-verifier`, which on Linux loads the system roots eagerly
  and fails the client build when there are none. `triad-agent`'s client
  falls back to the Mozilla roots bundled by `webpki-root-certs`.
  `tests/no_ca_store.rs` points `SSL_CERT_FILE` and `SSL_CERT_DIR` at an
  empty directory and checks that the fallback is taken. (rig-reqwest has
  the same fallback, but only under its `rustls` feature, which brings
  aws-lc back.) Whether a real HTTPS request succeeds from the static guest
  is checked in H8.
- **Binary size.** Adding the crates changes nothing until the binary calls
  them; the figure is measured once `triad` links the bots (H2 and H7). The
  static, stripped, size-optimized x86_64 `triad` at `75ba1fc` (as
  `scripts/vm/build.sh` builds it) is 18,447,256 bytes.
- `triad-agent` alone pulls 196 crates; `triad` at H0 has 530 (both counted
  as unique `cargo tree -e normal` lines).

### H1: wire format

`ChatBody` keeps `v: 1` and gains `bot`, `ask` and `ctl`, each skipped when
empty. `plain_body_keeps_todays_bytes` encodes a plain body next to a copy of
the old three-field struct and compares bytes;
`old_build_decodes_a_body_with_bot_fields` decodes a body with every new
field set into the old struct (serde ignores unknown fields; the old struct
never used `deny_unknown_fields`). `messages` gains `bot`. The `message`
event gains `bot` (true only when shown as the channel's bot) and `ask`, and
`bot_claim: "not accepted"` when a bot flag is not accepted.

### H2: the personal bot

`tests/bots_dm.rs`. `chat --agent` on a fresh data directory creates a space
named `agent` with the node as its only member, stores its ID in `settings`,
and focuses it. Three lines got three replies, in order, once each. With the
mock brain held for 8 s (`TRIAD_AGENT_MOCK_DELAY_MS`, a knob the plan did
not list), the process was killed with SIGKILL after `job started`; on
restart the job was reported `abandoned`, never started again, and its reply
never appeared, while a new line was answered. A second node given an invite
to the DM's space ID ran `join --auto-accept`: the owner's node saw its key
bundle (a `Member` event) and declined it without asking (`join_declined`,
a new event), and the joiner never saw itself added. `/invite` and the CLI
`invite` refuse the DM; `/add` and `/promote` too. `inspect` finds neither
the lines nor the replies in `p2panda.sqlite`.

**A one-member space works.** Nothing odd: create, publish and the stream
behave as in any space. For a DM holding two lines and two replies,
p2panda stored one space, one topic with four logs (seven operations, one of
429 bytes, likely the key bundle), one key secret and no peers for the topic
(`topics2node_infos_v1` empty). The topic is joinable by anyone who learns
its 32-byte ID: the stranger's key bundle reached the owner. The ID only
leaves the owner's machine if the owner shares it, and the owner's client is
what keeps the stranger out, not p2panda.

Places the plan did not say or was wrong:

- **`chat --agent` with no channels** used to be an error ("run `create` or
  `join`"); with `--agent` it now creates the DM and starts.
- **Restarts.** After a restart a channel's space opens shortly after
  `ready`, and a line typed before that is refused (as for every channel,
  before bots). The test waits for the node's own `joined` event.
- **Count limits do not apply to the DM.** Only the age and once rules
  do: the per-asker limit of 10 an hour would cap the owner's own assistant.
- **Spaces publish returns no operation hash** (the risk the plan listed),
  so a job is marked `done` right after its reply is published. A crash
  between the two leaves it `started`, so it is abandoned on restart: a
  reply can be lost, never sent twice.
- Further knobs beyond the plan's list: `TRIAD_AGENT_BURST_PER_MINUTE` (6)
  and `TRIAD_AGENT_MAX_STEPS` (8).

### H3: the channel bot, talking only

`tests/bots_channel.rs`, host A and members B and C, all passing:

- B and C see `bot_on` from A on joining (the `BotOn` operation syncs with
  the history). B's `@bot` line gets one reply, shown as the bot's (`bot:
  true`, author A); a line without `@bot` makes no job at A.
- **The held-open join prompt.** A ran without `--auto-accept`. While C's
  join request waited at A's prompt, B asked and A's bot replied; A's next
  typed `y` still answered C's request. Replies never pass through stdin.
- After `/block B`, B's ask is a `refused` job (`blocked`), C's is
  answered. With `TRIAD_AGENT_ASKER_PER_HOUR=2`, C's third ask is refused
  (`asker limit`). Refused asks make no model call and no reply.
- `/bot on` typed by B fails locally ("only the channel's creator").
- C (no `Manage`) published a forged `BotOn` through a test hook
  (`/debug-bot-on`, only with `TRIAD_AGENT_TEST_HOOKS` set). A and B emit
  `bot_rejected`, and `/bot` on B still names A as the host. A message with
  the bot flag from C shows as an ordinary message with
  `bot_claim: "not accepted"`.
- After `/bot off`, every client refuses `@bot` locally ("#garden has no
  bot"), the host's included, so nobody is answered.
- **Host offline.** A quit; B asked. B's line shows on B's screen as usual
  and nothing tells B that the bot's host is away. When A came back inside
  the age limit, it answered once. In a second run A came back after the
  age limit (`TRIAD_AGENT_MAX_AGE_SECS=5`, 7 s later): A recorded the job as
  `refused` (`too old`), and B never saw anything.

### H4: tools and permissions

Unit tests in `triad-agent/src/policy.rs` cover every row of "What an asker
may do" (host, admin, member with grants, member, blocked, not a member),
plus a granted member who is then promoted or removed, limits, age, once and
the DM. `tools.rs` tests the gate on its own: a tool outside the asker's set
that is called anyway is refused before it is sent, and the step limit.

`tests/bots_tools.rs` (fake tools): host A, admin B (promoted), member C,
each asking for `calendar.create_event` through the mock script. A's and
B's calls reached the fake tool's log; C's did not, the mock log shows C's
turn was offered `[]`, and C was told "I can't use calendar for you. Ask
<A> for access." After `/grant C calendar.create_event`, C's turn was
offered exactly that tool and its call went through; `linear.create_issue`
was still refused. After `/revoke C`, refused again. `/audit` lists six
rows with asker, tool and state (`ok`, `ok`, `denied`, `ok`, `denied`,
`denied`). Members get "only the host" for `/audit` and `/grant`.

**No path from a turn to a tool skips the gate.** The brain is handed a
`&dyn ToolCaller`, which is the turn's `Gate`; it never sees the `Tools`
object. The gate checks the asker's tool set and the step limit on every
call, writes `bot_tool_calls` (`sent`) before the call goes out and the
outcome after, and records refusals as `denied`. What the model is offered
is filtered by the same tool set before the turn starts.

Two things the plan did not say:

- **The "ask the host" reply is written by code, not the model.** When the
  gate refuses a tool the asker was not offered, `run_turn` replaces the
  reply with "I can't use <servers> for you. Ask <host> for access." This
  names server names, never other members. A real model given no tools
  will usually not attempt a call at all; for that case the system prompt
  lists the servers the asker cannot use and tells the model to say so and
  name the host.
- **Tool names on the wire.** Anthropic allows only `[a-zA-Z0-9_-]` in a
  tool name, so `server.tool` is sent as `server__tool` and mapped back.

**Latency on one machine (mock brain, fake tools).** From B's `@bot` line
to the reply on C's screen: 101 ms. About 27 ms is B to A (sync and
processing until the job is queued), 44 ms is A's turn (job bookkeeping and
the mock; model and tool time are both 0 ms here), and about 30 ms is A to C.
The figures come from the `job` event's `since_ask_ms`, `turn_ms`,
`model_ms` and `tool_ms`, printed by the test as `LATENCY`. A real model's
share is in H7.

### H5: isolation

`tests/bots_isolation.rs`: one host with its DM, `#garden` and `#other`, the
mock brain logging every turn's full input. A string said only in the DM
never appears in the `#garden` or `#other` turn; a string said only in
`#garden` never appears in the `#other` turn or in a later DM turn. As
controls, the `#garden` turn does contain the `#garden` string and the
second DM turn contains the first DM line. The DM turn's tool list is
exactly the host's two fake tools: triad offers no tool that reads or posts
to a channel, so there is nothing to leave out.

Isolation holds by construction: the worker reads history with
`State::history(job.topic, ..)`, the job's own channel only, and
`context::build` only trims what it is handed. **Nothing from outside a
channel appeared in a channel turn's input in any test.** The one channel
input the model does get beyond history is the member list with tiers.

### H6: real MCP

`triad-agent/src/mcp.rs` reads `mcp.json` from the data directory and
connects each server through `rmcp` 3.9: a child process over stdio
(`TokioChildProcess`, stderr discarded) or streamable HTTP
(`StreamableHttpClientTransport::with_client`, the shared ring-backed
client, with `Authorization: Bearer <token>` from the variable `token_env`
names). Each server gets 15 s to connect and list its tools; each call gets
60 s. Tools are named `<server>.<tool>`; tool names outside
`[A-Za-z0-9_-]` are skipped and reported.

The fixture is `crates/triad/examples/mcp_fixture.rs`: one tool, a
hand-written `ServerHandler` (the tool name is a flag, so the `#[tool]`
macros do not fit), served over stdio or over HTTP with hyper, answering 401
without the expected bearer token. Every call it receives is logged as the
fake tools log. It is an example, not a binary, so its server-side `rmcp`
features and hyper stay dev-dependencies and out of the `triad` build; the
test builds it with `cargo build --example mcp_fixture` on first use.

`tests/bots_tools.rs` runs the whole H4 test three times: fake tools, two
fixture servers over stdio, and two over HTTP. All pass. Each `mcp.json`
also names a server that is down at start (a missing command; a URL on a
closed port): it is reported as an `info` line before `ready`, it is left
out of `BotOn`'s server list, and the bot answers as usual.

The interrupted session here left truncated build artifacts in `target/`
(zero-length `.rmeta` files, a malformed `.rlib`); `cargo clean --profile
dev` fixed it. Nothing in the code was involved.

### H7: a real model

`AnthropicBrain` (`triad-agent/src/brain.rs`, the only file naming `rig`)
runs a manual tool loop on `rig-core` 0.46: one `CompletionRequest` per
round with the system prompt as preamble, the transcript and the ask as one
user message (the transcript fenced and labeled as context), and the
offered tools as `ToolDefinition`s. Each tool call goes through the gate;
results go back as tool results (errors with `is_error`). A round with no
tool call ends the turn. Token counts and model time are summed across
rounds and reported in the `job` event. `BotOn` announces the provider as
`anthropic (claude-opus-5-5)`. Starting with `--agent`, the `anthropic`
brain and no key exits with code 2 (`tests/bots_dm.rs`).

**Not run against the real model.** This host has no `ANTHROPIC_API_KEY`,
so the live tests (`tests/bots_live.rs`, `#[ignore]`d) were compiled but not
run. They cover the H2 exchange, one tool call against the fixture server
over stdio, and the planted-instruction experiment, and print `TURN` lines
with model time, tool time and token counts:

```sh
ANTHROPIC_API_KEY=... cargo test -p triad --test bots_live -- --ignored --nocapture
```

What was checked instead: `triad-agent/tests/anthropic_wire.rs` points the
brain at a local stand-in for `POST /v1/messages` (no key, no network) that
answers a `tool_use` and then text. The turn sends the system prompt and the
tool as `calendar__create_event`, the call reaches the fake tool through the
gate, the second request carries the `tool_result`, the reply is the final
text, and tokens sum to 250 in and 30 out over two requests. For an asker
with no tools, no tools are sent. The mock tests all pass with no key set.

So the latency split's model share, the token counts per turn and the
planted-instruction result are open; they need the run above.

A test fix on the way: `bots_tools.rs` matched "any bot reply" after an ask,
so under load another member's late reply could be taken for the asker's
(it failed once in a full parallel run). Mock replies can now include
`{ask}`, and the test matches on it.

### H8: the bot in the VM

`scripts/vm/bot-test.sh`: the guest creates `#garden` with `--agent` on the
mock brain and hosts its bot; `triad` on the host machine joins and asks.
The guest init now reads `KEY=VALUE` lines from `/etc/triad.env`
(`VM_ENV` in `run-vm.sh`), and `build.sh` builds `-p triad` (it still named
the removed `triad-cli` package). x86_64, KVM, one vCPU:

| Guest RAM | Result | Join | Ask to reply | Peak RSS | RssAnon | Free |
| --- | --- | --- | --- | --- | --- | --- |
| 64 MB | pass | 3.0 s | 0.27 s | 31.6 MB | 14.7 MB | 4.9 MB |
| 48 MB | pass, slow | 11.2 s | 6.7 s | 22.6 MB | 13.9 MB | 2.6 MB |
| 40 MB | join did not complete in time | | | | | |

**Smallest guest that passes: 48 MB**, the same as plain `triad`, but at
48 MB it is slow: binary pages are evicted and read back (peak RSS drops to
22.6 MB while anonymous memory stays at 13.9 MB). 64 MB is comfortable. The
same binary without `--agent`, as the joining guest of `chat-test.sh` at
64 MB, peaks at 27.7 MB with 10.9 MB anonymous (README's earlier 26 to 28
MB). **The bots cost about 4 MB of peak RSS** (3 to 4 MB of it anonymous:
brain, tools, a second SQLite connection, the job machinery) **and 4.6 MB of
binary**: the static, stripped x86_64 `triad` is 23.0 MB against 18.4 MB at
`75ba1fc`. (The two runs differ in role, creator against joiner, which
also moves the figure a little.)

Not done here: the manual run on the real model with an HTTP MCP server
(no API key on this host), so an HTTPS request from the guest, which has no
certificate store, has not been made; the bundled-roots fallback that would
serve it is tested only on the host (H0). The guest's kernel was not
rebuilt: `build.sh` now stops at the kernel step on this host because
objtool needs libelf headers (`gelf.h`), which are not installed; the
kernel built earlier the same day, from the same configuration, was used.

## Agent harness options (2026-11-11)

Desk research on giving an AI agent access to a triad group. Nothing here was
built. The only test run was dependency resolution, described below.

### What is encrypted at rest today

Only p2panda's store. The spec leaves the rest open ("No key protection").

| File | Contents | Encrypted at rest |
| --- | --- | --- |
| `p2panda.sqlite` | Operations as synced | Yes, ciphertext only |
| `state.sqlite` | Decrypted message history | No |
| `mls.sqlite` | Group secrets and the MLS signature key | No |
| `credentials.cbor` | Node identity key | No, file mode 0600 only |

So an agent design has to add encryption at rest, not retain it. The likely
shape is SQLCipher for `state.sqlite` and `mls.sqlite`, the same key wrapping
`credentials.cbor`, and the key held in the OS keychain or passed by the
harness over a file descriptor. Whether rusqlite's SQLCipher build feature
works alongside p2panda's sqlx, which shares the same libsqlite3-sys, is
untested.

### Shape of the integration

The agent should be its own group member, with its own data directory and node
ID, and reach triad through an interface, not the files. `triad chat --json`
already serves: one JSON event per line out, lines in on stdin. A removable
agent needs the spaces variant, since the OpenMLS variant has no removal.

Two limits hold for every option below:

- **The model provider sees plaintext.** Whatever the agent reads goes to its
  configured provider, which ends end-to-end confidentiality for the group
  unless the model runs locally.
- **The harness is a second plaintext store.** Its transcript, memory and logs
  hold what the agent read, outside anything triad encrypts.

### ZeroClaw as a bundled sidecar

ZeroClaw 0.9.0 fits as a second binary with triad as one of its channels (a
channel implements roughly `name`, `listen` and `send`). It does not fit linked
into the triad binary:

- Its manifest depends on `rusqlite` 0.40. triad pins `rusqlite` 0.37 with
  libsqlite3-sys 0.35, and p2panda's sqlx 0.9 accepts libsqlite3-sys only below
  0.38. Cargo links one native SQLite per build. Which libsqlite3-sys
  `rusqlite` 0.40 needs was not confirmed.
- It presents itself as a single binary. Nothing read says its internal crates
  are a supported library API.

Its toolchain (Rust 1.96), tokio and rustls with ring match triad's. As a
channel it would need to deduplicate on the `op` hash (delivery is at least
once), leave join requests to a human member (no `--auto-accept`), and run one
triad process per group. Its memory is SQLite with embeddings; whether that is
encrypted at rest was not checked. Only its manifest and README were read.

### Crates that link into the same binary

Each crate was added to `triad-cli` in a scratch copy of the repository at
`de3f814`, and the resolved tree inspected with `cargo tree`. Nothing was
compiled or run, so this shows the dependencies fit, not that the binary works.

| Crate | Version resolved | What it is | Crates added to triad-cli's 454 |
| --- | --- | --- | --- |
| `rig-core` | 0.44.0 (0.46.0 is the latest; not investigated) | Agent library: providers, typed tools, agent loop | 17 |
| `swiftide-agents` | 0.34.0 | Agent loop with tools and hooks, from a retrieval pipeline framework | 55 |
| `rmcp` (features `client`, `transport-child-process`) | 3.9.0 | The official MCP SDK; not an agent | 13 |

In all three the pins held: iroh 1.4.1, p2panda 0.8.0, openmls 0.9.2, rusqlite
0.37.0, sqlx 0.9.0 and one libsqlite3-sys 0.35.0. TLS stayed on rustls 0.23
with ring; no OpenSSL or aws-lc appeared. Adding `rig-core` rewrote nine
existing lock entries, which were not examined beyond the pinned crates.

`rig-core` is the preferred candidate. It adds the least and has no storage of
its own, so it creates no second database. `rmcp` is the alternative shape:
triad serves MCP tools in-process and an external harness connects.

Two that do not fit: Block's goose (its core crate uses sqlx 0.9.0 with SQLite
and would probably unify, but the `goose` name on crates.io is an unrelated
load-testing tool, so it would be a git dependency, which the spec forbids),
and ZeroClaw, as above.

Sources: [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) and its
[manifest](https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/Cargo.toml),
[rig](https://github.com/0xPlaygrounds/rig),
[swiftide's manifest](https://raw.githubusercontent.com/bosun-ai/swiftide/master/Cargo.toml),
[goose's core manifest](https://raw.githubusercontent.com/block/goose/main/crates/goose/Cargo.toml).

## Original implementation results

**Reported status:** Milestones 0 to 4, 6 and 7 pass on one Linux machine.
Milestone 5 needed the second machine (macOS) and was handed over. It has since
passed (see
[Milestone 5](#milestone-5-two-machines-on-different-networks-2026-11-11)).
The acceptance run is in
[README.md](README.md#acceptance-run-two-machines) and automated by
`scripts/cross-test.sh`. Milestones 6 and 7 were built before 5 because they
could be developed on one machine. Milestone 6's two-machine run has since
passed. Milestone 7's test uses three data directories on one machine, as the
spec says.
The measurements below identify process-based and in-process tests on one host.

## Environment

| | Machine A (this one) | Machine B |
| --- | --- | --- |
| OS | Ubuntu 26.04.1 LTS, Linux 7.0.0-43-generic | macOS 26.6.2 (25G83), arm64 |
| Hardware | AMD Ryzen 7 3700X, 16 threads, 121 GB RAM | MacBook Air |
| Rust | 1.98.1 stable (pinned in `rust-toolchain.toml`) | same |
| Network | Home LAN behind NAT | Phone hotspot for Milestone 5, Tailscale off |

Resolved versions (from `Cargo.lock`): p2panda 0.8.0 (and p2panda-core,
-net, -sync, -store, -stream 0.8.0), iroh 1.4.1, iroh-gossip 0.102.0,
openmls 0.9.2, openmls_rust_crypto 0.6.0, openmls_basic_credential 0.6.0,
openmls_sqlite_storage 0.3.0, rusqlite 0.37.0, libsqlite3-sys 0.35.0
(bundled), sqlx 0.9.0.

## Milestone results

| # | Milestone | Result | Commit | Notes |
| --- | --- | --- | --- | --- |
| 0 | Compatibility spike | pass | `m0` | One iroh (1.4.1) and one libsqlite3-sys (0.35.0) in `Cargo.lock`. `cargo tree -d` lists libsqlite3-sys twice, both 0.35.0 (two feature sets of one version). |
| 1 | iroh probe | pass | `m1` | One machine: direct path, 23 ms connect, 1.3 ms echo RTT. Mac and Linux: direct on the same LAN (6.5 ms echo), relayed between the Mac's hotspot and the LAN in 5 of 5 probes (80–120 ms echo). |
| 2 | p2panda stream, no encryption | pass | `m2` | 10 each way in order; after kill -9 and 5 missed messages the restart receives exactly 5; no duplicates. All payload sizes up to 64 KB arrive. |
| 3 | MLS in memory | pass | `m3` | 7 unit tests in `mls.rs`: create, KeyPackage, add, Welcome, join, 10 messages each way, reopen `mls.sqlite` mid-conversation, KeyPackage kept across restart, 3 members with a commit and a late epoch-1 message. |
| 4 | Integration | pass | `m4` | `two_node.rs` passes with encryption on; `triad inspect --grep` exits 0 on both data directories for every probed message; 23 unit tests in `group.rs` cover every acceptance-rule row. |
| 5 | Two machines | pass | `m5` | Mac on a 5G hotspot, Linux on a wired home LAN. Acceptance steps 1 to 9 pass in both directions: by hand with the Mac creating, then `cross-test.sh` over SSH with Linux creating (step 10). 20 messages in order, catch-up of exactly 5, no plaintext, `failed=0`. The same-LAN chat run is not done. |
| 6 | Stretch: p2panda spaces | pass | `m6` | `triad-spaces` binary, no OpenMLS. `spaces_two_node.rs` passes on one machine. `cross-test.sh` passes 3 of 3 runs with the Mac joining from the 5G hotspot and Linux creating on the LAN. |
| 7 | Stretch: three members | pass | `m7` | `two_node.rs::three_members_with_commit`: Commit before Welcome, B processes it, a message B sent at epoch 1 during A's commit decrypts at A; all three at epoch 2 with 3 members, zero failed. |

## Measurements

| Measurement | Value | How |
| --- | --- | --- |
| Clean release build, M0 (empty main) | 1 min 30 s wall, 1.7 GB peak RSS | `/usr/bin/time -v cargo build --release`, 16 threads |
| Clean install from a fresh clone, M4 | 1 min 44 s wall, 1.8 GB peak RSS | `cargo install --path crates/triad-cli --locked`, empty target dir, 16 threads |
| Release binary size | 52 MB (52,277,192 bytes), unstripped, Linux x86_64 | `ls -l` of the installed `triad` |
| Probe, same machine | direct path, connect 23 ms, echo RTT 1.3 ms, QUIC path RTT 2.1 ms | `triad probe listen` / `connect` |
| Join latency, same machine, no MLS (M2) | 0.13 s | `two_node.rs`, 3 runs: 0.125–0.128 s; "joined" was immediate in M2 |
| Join latency, same machine, MLS (M4) | 2.4 s from `join` start to `joined` | `two_node.rs` and `cross-test.sh local`; KeyPackage and Welcome each travel by sync |
| 20 messages exchanged, same machine | 2.6–2.7 s | `two_node.rs`, includes process start |
| Catch-up of 5 after kill -9, same machine | 1.25–1.34 s from restart to the fifth message | `two_node.rs`, 3 runs plaintext, 1 run encrypted |
| Same, p2panda spaces (M6) | join 2.42–2.45 s; catch-up of 5 in 0.40–0.51 s | `spaces_two_node.rs`, 3 runs |
| Two networks, the Mac joins from the hotspot | OpenMLS: join 7–9 s, catch-up 2 s. Spaces: join 4–7 s, catch-up 2–3 s | `cross-test.sh` script time, 3 runs each, alternating |
| Three members join and exchange (M7) | 3.9–4.4 s for the whole test | `three_members_with_commit`, 4 runs |
| Probe connect time, two machines | 27 ms same LAN; 98–171 ms between the hotspot and the LAN | `triad probe listen` / `connect` |
| Path type and RTT, two machines | Same LAN: direct, echo 6.5 ms, QUIC 5.7 ms. Hotspot to LAN: relayed in 5 of 5, echo 79–120 ms, QUIC 106–153 ms | `triad probe`, both directions |
| Join latency, two networks, MLS (M5) | 1.37 s (Linux joins from the LAN), 7.77 s (the Mac joins from the hotspot), from KeyPackage publish to `joined` | Joiner's `triad.log`, one run each |
| Catch-up of 5, two networks (M5) | 1.0 s (Linux), 0.45 s (the Mac, measured from choosing its relay) | Joiner's `triad.log`, one run each |

### Envelope sizes

CBOR-encoded `Envelope` sizes, from `mls::tests::envelope_sizes_stay_under_limit`.
All stay under the 3,500-byte cap.

| Envelope | Bytes |
| --- | --- |
| KeyPackage | 339 |
| Welcome, 2 members | 920 |
| Welcome, 3 members | 1,191 |
| Commit adding a 3rd member | 895 |
| App, 5-byte text | 210 |
| App, 2,000-byte text (the cap) | 2,217 |

### Payload sizes (Milestone 2)

Measured with `crates/triad-core/tests/payload_sizes.rs` (two in-process
nodes on one machine, public relay configured). Every size arrived both ways.

| Payload | Catch-up sync | Live (both online) |
| --- | --- | --- |
| 1 KB | arrived (`source=sync`) | arrived (`source=sync`) |
| 4 KB | arrived | arrived |
| 16 KB | arrived | arrived |
| 64 KB | arrived | arrived |

Live delivery also reports `Source::SyncSession`: `Node::stream` runs
p2panda's log sync in live mode, and gossip is not on this path.

## Spec deviations

- **Supporting crates beyond the listed set.** Added `p2panda-core =0.8.0`
  (already in the tree; needed for the `Digest` trait to read a hash off a
  failed event), `rusqlite =0.37.0` with `bundled` (already in the tree via
  `openmls_sqlite_storage`; used for `state.sqlite`), `serde_json` (codec for
  the OpenMLS SQLite store, as Risks suggests; JSON output; probe address
  encoding), `serde_bytes`, `hex`, `tokio-stream` (`StreamExt` for the
  subscription), `tempfile` (tests only). `ciborium` is the one CBOR crate.
- **Toolchain pinned to 1.98.1**, the stable release installed here; the spec
  asks for 1.96 or newer.
- **`tests/two_node.rs` lives at `crates/triad-cli/tests/two_node.rs`.** The
  workspace root is a virtual manifest, and Cargo integration tests must
  belong to a package. Placing it in `triad-cli` also gives it
  `CARGO_BIN_EXE_triad`.
- **Two extra `meta` columns.** `key_package_published` makes a restart while
  pending skip a second KeyPackage, as the spec asks. `joined_epoch` lets the
  App rule tell "history from before I joined" (skipped) from "key already
  consumed" (failed).
- **`mls.sqlite` holds one extra table, `triad_signer`,** with the public half
  of this install's MLS signature key, so the key pair can be found again in
  the OpenMLS store on restart.
- **`group.rs` reaches MLS through a trait (`GroupCrypto`)** rather than taking
  pure data: "the KeyPackage validates" and "the MLS sender equals the author"
  need MLS calls. Unit tests use a fake implementation, so they still need no
  network and no clock. The "store a message" effect is folded into `Record`
  so the message and `processed_ops` row land in one transaction.
- **Unanswered join requests stay unacknowledged** until the user answers, so
  quitting at the prompt re-asks on the next start.
- **Extra JSON events** beyond the required seven: `info`, `status` and
  `members` (answers to `/status`, `/members`, and verbose notices). Message
  events carry `mine`, `op` and `ts`.
- **`TRIAD_TEST_SKIP_ACK=1`** withholds every ack. It exists only for the
  replay test behind Answer 3.
- **Probe address format.** `triad probe listen` prints `probe1-` plus hex of
  the JSON-serialized `EndpointAddr`, so `probe connect` gets the relay URL and
  any direct addresses in one token.
- **Clock in chat output is UTC**, to avoid a time-zone dependency.
- **`triad history` sorts by operation timestamp, falling back to the
  sender's clock** for an own message whose operation has not come back
  through the stream yet.

## Answers

1. **Does the gossip size limit apply to stored and synced operations, or
   only to live broadcast?** Only to gossip broadcast, which topic streams do
   not use. Operations of 1, 4, 16 and 64 KB all arrived through catch-up
   sync and while both nodes were online, and every live delivery reported
   `Source::SyncSession`. In p2panda 0.8, `Node::stream` calls
   `network.log_sync.stream(topic, live_mode = true)`; gossip backs
   `ephemeral_stream` and discovery instead. The spec's 3,500-byte envelope
   cap is kept anyway, and the largest real envelope is 2,217 bytes.

2. **On these two networks, how often is the path direct and how often
   relayed?** On the same home LAN, direct (`Ip(192.168.1.216:…)`, 6.5 ms
   echo). With the Mac on a 5G phone hotspot and Linux on the wired home LAN,
   relayed every time: 5 of 5 probes, in both directions, went through
   `use1-1.relay.n0.iroh.link` with 80–120 ms echoes. Hole punching through
   the carrier's IPv4 NAT never succeeded within the probe's 2 seconds. IPv6
   was not an option, because only the Mac had a global IPv6 address. The chat
   runs' own paths are not logged by `triad`, but both passed over this pair of
   networks. Joins took 1.4 s and 7.8 s, and catch-ups about 1 s.

3. **After kill -9, does `StreamFrom::Frontier` with explicit acknowledgement
   replay exactly the unacknowledged operations?** Yes, on this evidence.
   `two_node.rs::unacked_operations_replay_after_kill` runs the joiner with
   every ack withheld, receives 3 messages, and kills it with SIGKILL. On
   restart p2panda reported `ReplayStarted { total_operations: 5 }`: the
   joiner's own KeyPackage, the Welcome and the 3 App operations, which is
   every operation it had seen. `processed_ops` turned all 5 into acks with
   no second MLS call, so no message was shown twice. A second restart
   replayed nothing. In the normal kill -9 test, where acks happen, the
   restarted joiner received exactly the 5 missed messages and nothing
   replayed.

4. **How does p2panda order operations from different authors, and would
   that order be enough for MLS commits from more than one committer?**
   Within one author's log, operations arrive in sequence order (backlinks
   are checked at ingest). Across authors there is no order: topic-stream
   operations use `BasicExtensions` (log ID, timestamp, prune flag), which
   carry no cross-author dependencies. The pipeline's `Orderer` does
   dependency-based causal ordering, but only from dependencies that the
   header declares, and plain topic operations declare none. So two members
   on different machines can see two concurrent commits in different orders.
   MLS needs every member to apply exactly one commit per epoch, and in the
   same order. That is not enough: multi-committer MLS would need a
   sequencer, a deterministic tie-break that every member applies with
   rollback (for example, the lowest operation hash wins per epoch, losers
   re-propose), or a causal-dependency extension and a fork rule. The
   single-committer rule in the spec avoids the problem.

5. **Can p2panda, OpenMLS and the app share one SQLite file, so one
   transaction covers a whole operation?** One file, yes. One transaction,
   no. `crates/triad-core/tests/shared_sqlite.rs` points all three at one
   file: p2panda's sqlx migrations, OpenMLS's refinery migrations and the
   app's tables coexist (32 tables, no name clashes), and 20 rounds of encrypt,
   publish and record succeed with the three writing concurrently. But each
   owner holds its own connection: p2panda an async sqlx pool, OpenMLS and the
   app their own rusqlite connections. A SQLite transaction belongs to one
   connection. Covering MLS state, `processed_ops` and the p2panda ack in one
   transaction would need either an OpenMLS `StorageProvider` written against
   p2panda's sqlx pool (`NodeBuilder::database_pool` accepts one) or p2panda
   exposing a transaction hook for app writes. Neither exists in these
   versions, so the crash gap in Storage rules stays open.

6. **What would it take to register a custom iroh protocol on the endpoint
   p2panda owns?** A change to p2panda. `p2panda_net::iroh_endpoint::Endpoint`
   already has `accept(protocol_id, handler)`, which registers a
   `ProtocolHandler` for an ALPN on the running endpoint, and p2panda-net uses
   it for discovery, sync and gossip. But the high-level `p2panda::Node` keeps
   its `Network` (and the endpoint inside it) `pub(crate)`, so an app on the
   `Node` API cannot reach it. The smallest upstream change is a
   `Node::endpoint()` getter or a `NodeBuilder::protocol(alpn, handler)`
   option. Until then an app needs a second iroh endpoint (and a second
   identity on the network, as the probe has) or must build on `p2panda-net`
   directly.

7. **What metadata can a holder of the topic see without group keys?**
   More than the spec expected. `triad inspect --grep` on a creator's
   `p2panda.sqlite` finds no message text, but finds the strings `KeyPackage`,
   `Welcome`, `App`, `key_package` and `ciphertext`: p2panda's CBOR encodes
   the enum variant and field names as text. So without keys a topic holder
   sees: each operation's author (Ed25519 key, which is also the MLS
   identity), timestamp (microseconds), log sequence and size; the envelope
   type; who joined (a KeyPackage's credential holds the joiner's node ID in
   clear); whom each Welcome is for (`to` is in clear); and, inside every MLS
   PrivateMessage, the group ID, epoch and content type. The MLS sender leaf
   index is encrypted, but the p2panda author reveals it anyway. Short-message
   lengths are visible, since the app sets no MLS padding.

## Layer ownership

Recommendation, from building this stack:

| Concern | Should own it | Why |
| --- | --- | --- |
| Identity | p2panda's Ed25519 key, bound into MLS by credential | It already authenticates connections and signs every operation. The MLS credential carries it, and the app checks author equals MLS sender on every message. A separate MLS signature key adds nothing in this design but a second key to store. |
| Ordering | p2panda per author; the app (or a single committer) across authors | p2panda gives reliable per-log order and catch-up, but no cross-author order (Answer 4). Group-state changes need one authority or a deterministic merge rule. |
| Group membership | MLS | It defines who can read. p2panda's topic is an open rendezvous, and anyone with the topic ID can publish into it; the app filters by MLS membership and creator authority. |
| Message history | The app | MLS deletes keys after use, so the p2panda log cannot be decrypted twice. Plaintext must be stored at decrypt time. |

What the three libraries duplicate:

- **Two signing identities.** p2panda signs operations with the node key; MLS
  signs its messages with its own signature key. Both are Ed25519. The spec
  leaves merging them open.
- **Two encryption layers on the wire.** iroh's QUIC/TLS already encrypts
  every hop. MLS adds end-to-end encryption that survives storage and
  intermediate nodes. Both are needed for the spec's threat model, but on a
  direct two-node path the TLS layer is redundant.
- **Two group-encryption systems.** p2panda 0.8 ships encrypted spaces with
  their own group key agreement, member list and key rotation. Using OpenMLS
  on top of a plain topic duplicates what spaces would do (Milestone 6
  compares them).
- **Three SQLite stores with three migration systems** (sqlx, refinery and the
  app's own schema), and two SQLite access libraries (sqlx and rusqlite).
  They share one libsqlite3-sys, which kept the build simple.
- **Two at-least-once mechanisms.** p2panda's ack cursor and the app's
  `processed_ops` both track what was handled. The app's table is needed
  because MLS cannot decrypt the same ciphertext twice; p2panda's is needed to
  avoid replaying everything.

## Several spaces per node (2026-11-11)

`triad-spaces` now holds any number of spaces in one data directory, as named
channels. One p2panda node subscribes to all of them. This needed no library
change: `Node::create_space`, `Node::space` and `Node::insert_bootstrap` all
work on a running node, so channels can be created and joined from inside
chat mode. The app change was in two places. `state.sqlite` files every
message and operation under its space ID, and the session keeps one event
stream and one member list per space.

`crates/triad/tests/spaces_channels.rs` passes on one machine in about
15 s, with three processes. A and B share three channels, two hosted by A and
one by B. C is in one of them. The test checks that:

- a channel created or joined inside chat mode works like one from the
  command line, in both directions between the same two nodes;
- messages arrive tagged with the right channel, and C is never shown
  anything from the two channels it is not in;
- B, killed and restarted, catches up on the message it missed in each of its
  three channels;
- each channel has its own history, and no plaintext is in `p2panda.sqlite`.

The existing two-node test, the four offline-membership tests and
`cross-test.sh local` still pass. Not tested: several channels across two
networks, a node whose channels use different relays, and many channels at
once (the test uses three). Not built: leaving a channel, and unread markers.

Two things an app has to know:

- **A node is reached through whoever invited it.** Each joined channel adds
  its inviter as a bootstrap node. Two members of one channel who were each
  invited by a third find each other only through that third node or mDNS.
- **The relay is per node, not per channel.** A node listens on the relay of
  its first channel and uses it for the channels it creates later.

## OpenMLS and p2panda spaces

Milestone 6 built `triad-spaces` (`crates/triad-spaces`): the same commands,
invite format, JSON events and `state.sqlite` history, with p2panda 0.8's
encrypted spaces doing key agreement and encryption. The joiner subscribes to
the space, which publishes its key bundle; the creator treats that as a join
request and calls `Space::add`. Both versions pass the same two-node test on
one machine (create, join, 10 each way in order, kill -9, exactly 5 caught up,
no plaintext in `p2panda.sqlite`).

| | OpenMLS (`triad`) | p2panda spaces (`triad-spaces`) |
| --- | --- | --- |
| Standardization | MLS is RFC 9420; OpenMLS is a mature implementation with a validation suite. Interoperable with other MLS clients in principle. | p2panda's own protocol (`p2panda-encryption`, a decentralized group key agreement with key bundles and pre-keys). Not standardized; pre-1.0 and documented as subject to change. |
| Membership changes while members are offline | Needs a single committer (the creator) online to admit anyone, because MLS cannot merge concurrent commits and p2panda gives no cross-author order (Answer 4). A member offline for more than `max_past_epochs` (2) commits loses late messages from older epochs. | Works without a committer. Tested on one machine: adds and removals while members are offline, two managers adding concurrently without ever being online together, and a manager removed while adding (strong removal invalidates the add). All converge in 0.2–4.3 s. The joiner still needs a manager to see its key bundle once. A removed member is not told, and a removed member who reconnects looks like a new join request. |
| App code for encryption and membership | `mls.rs` 414 lines + `group.rs` 341 lines (non-test), plus the KeyPackage/Welcome/Commit envelopes, `processed_ops` idempotency against single-use keys, and a custom OpenMLS provider and codec. `chat.rs` is 552 lines. | None beyond the join prompt: about 60 lines handle `Member` and `Space` events and call `Space::add`. The whole binary, CLI included, was 674 lines; the membership commands and events for the offline tests bring it to 773. |
| Join latency, same machine | 2.4 s | 2.4 s |
| Catch-up of 5, same machine | 1.25–1.34 s | 0.40–0.51 s |
| What storage reveals | Envelope type names (`KeyPackage`, `Welcome`, `App`), field names, joiner identity in the KeyPackage, `to` of each Welcome, MLS group ID and epoch (Answer 7). | No application field names: message payloads are encrypted as a whole (`sent_at_ms`, `ChatBody` and message text all absent). The spaces control messages still show CBOR field names such as `ciphertext`, `PreKey` and `key_used`. Authors, timestamps and sizes are visible in both. |
| Failure cases left open | The crash gap (a key consumed before the state write loses that message). The creator is a single point of failure for membership. Multi-committer groups are impossible without a sequencer. Metadata leaks above. | Publishing does not return the operation hash, so the app stores its own message only when it comes back through the stream. No epoch or key state is visible to the app, so "why can't I read this" is hard to diagnose. Anyone holding the space ID can announce a key bundle (the app's prompt is the only filter). On restart the app re-announces the member list. A removed member is not told and its writes vanish silently. A removed member who reconnects looks like a new join request. New members can read messages from before they joined. API stability before 1.0. |

**Recommendation:** for this two-person, local-first use case, p2panda
spaces. They remove the most code and the hardest problems found here (single
committer, cross-author ordering, single-use keys against at-least-once
delivery), catch up faster in this test, and leak less envelope metadata.
Choose OpenMLS only if interoperability with other MLS clients or a
standardized, audited protocol is a requirement. Even then, the
single-committer limit should be treated as a design constraint, not a
temporary shortcut. Across two networks (Milestone 6), spaces behaved like
the OpenMLS version: every run passed, joins were slightly faster and catch-up
took the same time. The offline-membership tests confirm the main advantage:
adds and removals converge with members offline and with two managers, and no
member has to commit for the others. They also show what the app must still
do itself. It must remember removals, tell a removed member it is out, and
decide whether new members may read old messages.

## Library issues

- **p2panda 0.8.0: no shutdown for `Node`.** There is no `Node::shutdown()`.
  The app closes the stream publisher and drops the node; the `triad` binary
  then gives the runtime 500 ms to wind down, and some background tasks may
  still be running.
- **p2panda 0.8.0: `Node::store()` is test-only** (`cfg(test | test_utils)`),
  and the iroh endpoint is crate-private (Answer 6), which limits inspection
  and extension from apps.
- **p2panda 0.8.0: envelope enum names are on the wire.** Not a bug, but CBOR
  with serde's default enum representation stores variant names and field
  names as text in every operation (Answer 7).
- **p2panda 0.8.0: `ProcessingFailed` / `DecodeFailed` events carry an
  `Event` whose hash needs the `p2panda_core::traits::Digest` trait in scope**,
  so an app needs a direct p2panda-core dependency to log it.
- **OpenMLS 0.9.2: the README examples use the in-memory provider.** Using
  `openmls_sqlite_storage` needs a custom `OpenMlsProvider` struct (about
  20 lines) and a codec, as the spec's Risks table predicted. Finding our own
  signature key after restart needs its public key, which OpenMLS does not
  store anywhere the app can list, hence `triad_signer`.
- **OpenMLS 0.9.2: no typed "already consumed" error.** A replayed
  ciphertext fails with a generic secret-tree error, so the app cannot cleanly
  distinguish "consumed key" from "too old" without comparing epochs itself.

No upstream issues were filed from this session.

## Proposed follow-ups

- **Finish the acceptance run:** steps 1 to 5 on the same LAN. Log the
  selected path in `triad -v`, so a chat run's path is recorded rather than
  inferred from the probe.
- **Slow discovery from the hotspot:** with the Mac joining from the hotspot,
  joins took 4–9 s in six runs (both variants). With Linux joining from the LAN,
  the one run took 1.4 s. Repeat the Mac-creates direction under the script to
  compare like with like. That needs SSH into Linux, for example a second
  tunnel. Then look at which discovery mechanism finds the peer.
- **One sync session per peer:** the Milestone 5 log shows two concurrent
  sessions per connection, each carrying the same operations.
- **Spaces with the Mac as creator,** and a three-member run across machines.
  Neither is required by the spec.
- **Spaces removals:** find out why a removed member never learns of its
  removal in one case but does in the other, and whether p2panda can deliver
  the removal to it. Make `triad-spaces` remember removals so `--auto-accept`
  cannot re-add a removed member.
- **Spaces history for new members:** check whether p2panda can limit a new
  member to messages after its addition, if the product needs that.
- **Hide metadata:** encode `Envelope` as an integer-tagged CBOR array (or
  wrap all of it in MLS) so type names stop leaking; add MLS padding.
- **Merge identities:** use the p2panda signing key as the MLS signature key
  and drop `triad_signer`.
- **Upstream:** propose `Node::endpoint()` or `NodeBuilder::protocol()` and
  `Node::shutdown()` to p2panda.
- **Close the crash gap:** an OpenMLS `StorageProvider` on p2panda's sqlx
  pool, so the MLS state change, `processed_ops` and the ack commit together.
