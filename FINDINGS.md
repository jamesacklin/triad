# triad findings

Findings from building `triad`, a two-member command-line chat on p2panda 0.8,
iroh 1.4 and OpenMLS 0.9. Written as the work happened, on 2026-11-11.

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
The existing deviation below describes that choice. The spaces recommendation
remains provisional until cross-network and offline-membership tests pass.

## Milestone 5: two machines on different networks (2026-11-11)

**Result: pass.** Acceptance steps 1 to 9 pass on both machines, with the
machines on different networks and Tailscale off on the Mac. The Linux binary
was installed from `0013f14`, whose code matches the review's `d2b362d`. The
Mac's revision was not recorded. Steps 10 (roles swapped) and
the full same-LAN chat run are still to do. Milestones 6 and 7 have not been
run on two machines.

| | Mac | Linux |
| --- | --- | --- |
| Machine | MacBook Air, Apple silicon (macOS version not recorded) | Ubuntu 26.04.1, Linux 7.0.0-43, x86_64 |
| Network | Phone hotspot, Tailscale off | Home LAN behind NAT (`192.168.1.216`). Tailscale was running, but the Mac had no tailnet route. |
| Role | Creator, node `16197d43` | Joiner, node `77f7cdc2` |

The run was by hand, with `--data-dir ~/triad-m5` on both machines, group
topic `5033c0d9`.

| Step | Check | Result |
| --- | --- | --- |
| 1–3 | Create, join, 2 members at epoch 1 | Pass. Linux printed `you joined (epoch 1, 2 members)` after two sync sessions with the Mac. |
| 4–5 | 10 messages each way, in order | Pass. Both histories list the same 20 messages in send order, with identical timestamps. |
| 6–7 | Linux offline, Mac sends 5, Linux catches up | Pass. Linux processed exactly 5 new operations (`ok` went from 22 to 27), `mac-11` to `mac-15`. Its history has 25 messages and no duplicates. |
| 8 | No plaintext in `p2panda.sqlite` | Pass. `mac-01` and `linux-01` are absent on both machines, and `mac-15` on Linux. The scanned files were 1.86 MB on the Mac and 1.94 MB on Linux, including the WAL. |
| 9 | Same epoch and members, nothing failed | Pass. Both show epoch 1, the same two members and `ok=27 skipped=0 failed=0`. |

Measurements, from the Linux `triad.log`:

| Measurement | Value |
| --- | --- |
| Join, from KeyPackage publish to `joined` | 1.37 s (1.67 s from process start) |
| Catch-up of 5, from `triad chat` start to the 5 operations received | 1.0 s |
| Sync sessions per connection | 2 for the join, 2 for the catch-up. Both catch-up sessions reported `received_ops=5`, but each operation was processed once. |

The spec's step 7 expects one sync. There were two concurrent sessions, each
carrying all five operations. The likely cause is that each side opened one.
p2panda and `processed_ops` deduplicated them, so the user saw exactly five
messages. The second session wastes bandwidth but does no harm at this scale.

**Path.** `triad` does not log the path a connection uses, so the chat run's
path is not recorded. The probe between the same two machines, minutes before,
gives the best evidence:

| Probe | Path | Connect | Last echo | QUIC path RTT |
| --- | --- | --- | --- | --- |
| Same LAN | direct (`Ip(192.168.1.216:47935)`) | 27 ms | 6.5 ms | 5.7 ms |
| Mac on hotspot, Linux on home LAN | relayed (`use1-1.relay.n0.iroh.link`) | 171 ms | 88 ms | 153 ms |

On the hotspot, iroh did not find a direct path within the probe's 2 seconds
of echoes. Carrier NAT on phone hotspots commonly blocks hole punching. One
relayed probe is a single sample, not a rate.

One observation from the log: the joiner's iroh endpoint picked its home relay
after its sync sessions had finished. That was 1.6 s after `joined` in the join
run, and 2.0 s after the last sync session ended in the catch-up run. So
connecting did not wait for this side's home relay.

## Original implementation results

**Reported status:** Milestones 0 to 4, 6 and 7 pass on one Linux machine.
Milestone 5 needed the second machine (macOS) and was handed over. It has since
passed (see
[Milestone 5](#milestone-5-two-machines-on-different-networks-2026-11-11)).
The acceptance run is in
[README.md](README.md#acceptance-run-two-machines) and automated by
`scripts/cross-test.sh`. Milestones 6 and 7 were built before 5 because they
need no second machine. Their two-machine runs have not been done yet.
The measurements below identify process-based and in-process tests on one host.

## Environment

| | Machine A (this one) | Machine B |
| --- | --- | --- |
| OS | Ubuntu 26.04.1 LTS, Linux 7.0.0-43-generic | macOS, Apple silicon (version not recorded) |
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
| 1 | iroh probe | pass | `m1` | One machine: direct path, 23 ms connect, 1.3 ms echo RTT. Mac to Linux: direct on the same LAN (6.5 ms echo), relayed from the Mac's hotspot (88 ms echo). |
| 2 | p2panda stream, no encryption | pass | `m2` | 10 each way in order; after kill -9 and 5 missed messages the restart receives exactly 5; no duplicates. All payload sizes up to 64 KB arrive. |
| 3 | MLS in memory | pass | `m3` | 7 unit tests in `mls.rs`: create, KeyPackage, add, Welcome, join, 10 messages each way, reopen `mls.sqlite` mid-conversation, KeyPackage kept across restart, 3 members with a commit and a late epoch-1 message. |
| 4 | Integration | pass | `m4` | `two_node.rs` passes with encryption on; `triad inspect --grep` exits 0 on both data directories for every probed message; 23 unit tests in `group.rs` cover every acceptance-rule row. |
| 5 | Two machines | pass | `m5` | Mac on a phone hotspot creates, Linux on home LAN joins. Acceptance steps 1 to 9 pass on both: 20 messages in order, catch-up of exactly 5, no plaintext, `failed=0`. Step 10 (roles swapped) and the same-LAN chat run are not done. |
| 6 | Stretch: p2panda spaces | pass on one machine | `m6` | `triad-spaces` binary, no OpenMLS. The same two-node test passes (`crates/triad-spaces/tests/spaces_two_node.rs`). Two-machine run not done. |
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
| Three members join and exchange (M7) | 3.9–4.4 s for the whole test | `three_members_with_commit`, 4 runs |
| Probe connect time, two machines | 27 ms same LAN; 171 ms from hotspot | `triad probe listen` / `connect` |
| Path type and RTT, two machines | Same LAN: direct, echo 6.5 ms, QUIC 5.7 ms. Hotspot: relayed, echo 88 ms, QUIC 153 ms | `triad probe` |
| Join latency, two networks, MLS (M5) | 1.37 s from KeyPackage publish to `joined` | Linux `triad.log`, one run |
| Catch-up of 5, two networks (M5) | 1.0 s from `triad chat` start | Linux `triad.log`, one run |

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
   echo). With the Mac on a phone hotspot and Linux on the home LAN, relayed
   through `use1-1.relay.n0.iroh.link` (88 ms echo, 153 ms QUIC estimate): no
   direct path within 2 seconds. That is one sample per network pair, not a
   rate. The chat run's own path is not logged by `triad`. The chat worked
   either way, with join in 1.4 s and catch-up in 1.0 s.

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
| Membership changes while members are offline | Needs a single committer (the creator) online to admit anyone, because MLS cannot merge concurrent commits and p2panda gives no cross-author order (Answer 4). A member offline for more than `max_past_epochs` (2) commits loses late messages from older epochs. | Designed for it: managers add and remove members without a central committer, and changes are causally ordered in the space's own operations. Only adds with both nodes online were tested here. The joiner still needs the adder to see its key bundle once. |
| App code for encryption and membership | `mls.rs` 414 lines + `group.rs` 341 lines (non-test), plus the KeyPackage/Welcome/Commit envelopes, `processed_ops` idempotency against single-use keys, and a custom OpenMLS provider and codec. `chat.rs` is 552 lines. | None beyond the join prompt: about 60 lines handle `Member` and `Space` events and call `Space::add`. The whole binary, CLI included, is 674 lines. |
| Join latency, same machine | 2.4 s | 2.4 s |
| Catch-up of 5, same machine | 1.25–1.34 s | 0.40–0.51 s |
| What storage reveals | Envelope type names (`KeyPackage`, `Welcome`, `App`), field names, joiner identity in the KeyPackage, `to` of each Welcome, MLS group ID and epoch (Answer 7). | No application field names: message payloads are encrypted as a whole (`sent_at_ms`, `ChatBody` and message text all absent). The spaces control messages still show CBOR field names such as `ciphertext`, `PreKey` and `key_used`. Authors, timestamps and sizes are visible in both. |
| Failure cases left open | The crash gap (a key consumed before the state write loses that message). The creator is a single point of failure for membership. Multi-committer groups are impossible without a sequencer. Metadata leaks above. | Publishing does not return the operation hash, so the app stores its own message only when it comes back through the stream. No epoch or key state is visible to the app, so "why can't I read this" is hard to diagnose. Anyone holding the space ID can announce a key bundle (the app's prompt is the only filter). On restart the app re-announces the member list. API stability before 1.0. |

**Recommendation:** for this two-person, local-first use case, p2panda
spaces. They remove the most code and the hardest problems found here (single
committer, cross-author ordering, single-use keys against at-least-once
delivery), catch up faster in this test, and leak less envelope metadata.
Choose OpenMLS only if interoperability with other MLS clients or a
standardized, audited protocol is a requirement. Even then, the
single-committer limit should be treated as a design constraint, not a
temporary shortcut. The two-machine run (Milestone 5) should confirm spaces
behave the same across networks before deciding.

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

- **Finish the acceptance run:** step 10 (Linux creates, the Mac joins) and
  steps 1 to 5 on the same LAN. Log the selected path in `triad -v`, so the
  chat run's path is recorded rather than inferred from the probe.
- **One sync session per peer:** the Milestone 5 log shows two concurrent
  sessions per connection, each carrying the same operations.
- **Two-machine runs of Milestones 6 and 7:** `triad-spaces` with the
  README's acceptance steps, and a third member on either machine.
- **Spaces under offline membership changes:** add a member while another is
  offline, and remove one, to test the claim in the comparison table.
- **Hide metadata:** encode `Envelope` as an integer-tagged CBOR array (or
  wrap all of it in MLS) so type names stop leaking; add MLS padding.
- **Merge identities:** use the p2panda signing key as the MLS signature key
  and drop `triad_signer`.
- **Upstream:** propose `Node::endpoint()` or `NodeBuilder::protocol()` and
  `Node::shutdown()` to p2panda.
- **Close the crash gap:** an OpenMLS `StorageProvider` on p2panda's sqlx
  pool, so the MLS state change, `processed_ops` and the ack commit together.
