# triad findings

Findings from building `triad`, a two-member command-line chat on p2panda 0.8,
iroh 1.4 and OpenMLS 0.9. Written as the work happened, on 2026-11-11.

**Status:** Milestones 0 to 4 pass on one Linux machine. Milestone 5 needs the
second machine (macOS) and is handed over: the acceptance run is in
[README.md](README.md#acceptance-run-two-machines) and automated by
`scripts/cross-test.sh`. Milestones 6 and 7 come after 5 and have not started.
Every "same machine" number below is from two processes on one host.

## Environment

| | Machine A (this one) | Machine B |
| --- | --- | --- |
| OS | Ubuntu 26.04.1 LTS, Linux 7.0.0-43-generic | _pending (macOS, Apple silicon)_ |
| Hardware | AMD Ryzen 7 3700X, 16 threads, 121 GB RAM | _pending_ |
| Rust | 1.98.1 stable (pinned in `rust-toolchain.toml`) | same |
| Network | Home LAN behind NAT | _pending_ |

Resolved versions (from `Cargo.lock`): p2panda 0.8.0 (and p2panda-core,
-net, -sync, -store, -stream 0.8.0), iroh 1.4.1, iroh-gossip 0.102.0,
openmls 0.9.2, openmls_rust_crypto 0.6.0, openmls_basic_credential 0.6.0,
openmls_sqlite_storage 0.3.0, rusqlite 0.37.0, libsqlite3-sys 0.35.0
(bundled), sqlx 0.9.0.

## Milestone results

| # | Milestone | Result | Commit | Notes |
| --- | --- | --- | --- | --- |
| 0 | Compatibility spike | pass | `m0` | One iroh (1.4.1) and one libsqlite3-sys (0.35.0) in `Cargo.lock`. `cargo tree -d` lists libsqlite3-sys twice, both 0.35.0 (two feature sets of one version). |
| 1 | iroh probe | pass on one machine | `m1` | Direct path, 23 ms connect, 1.3 ms echo RTT. Mac-to-Linux run is part of the handover. |
| 2 | p2panda stream, no encryption | pass | `m2` | 10 each way in order; after kill -9 and 5 missed messages the restart receives exactly 5; no duplicates. All payload sizes up to 64 KB arrive. |
| 3 | MLS in memory | pass | `m3` | 7 unit tests in `mls.rs`: create, KeyPackage, add, Welcome, join, 10 messages each way, reopen `mls.sqlite` mid-conversation, KeyPackage kept across restart, 3 members with a commit and a late epoch-1 message. |
| 4 | Integration | pass | `m4` | `two_node.rs` passes with encryption on; `triad inspect --grep` exits 0 on both data directories for every probed message; 23 unit tests in `group.rs` cover every acceptance-rule row. |
| 5 | Two machines | handed over | — | No SSH access to a Mac from this session. README install steps verified from a fresh clone on Linux. |
| 6 | Stretch: p2panda spaces | not started | — | Comes after 5. |
| 7 | Stretch: three members | not started as a milestone | — | `mls.rs` already tests 3 members, a Commit and `max_past_epochs` without the network. |

## Measurements

| Measurement | Value | How |
| --- | --- | --- |
| Clean release build, M0 (empty main) | 1 min 30 s wall, 1.7 GB peak RSS | `/usr/bin/time -v cargo build --release`, 16 threads |
| Clean install from a fresh clone, M4 | see "Fresh-clone install" below | `cargo install --path crates/triad-cli --locked` into an empty target dir |
| Probe, same machine | direct path, connect 23 ms, echo RTT 1.3 ms, QUIC path RTT 2.1 ms | `triad probe listen` / `connect` |
| Join latency, same machine, no MLS (M2) | 0.13 s | `two_node.rs`, 3 runs: 0.125–0.128 s; "joined" was immediate in M2 |
| Join latency, same machine, MLS (M4) | 2.4 s from `join` start to `joined` | `two_node.rs` and `cross-test.sh local`; KeyPackage and Welcome each travel by sync |
| 20 messages exchanged, same machine | 2.6–2.7 s | `two_node.rs`, includes process start |
| Catch-up of 5 after kill -9, same machine | 1.25–1.34 s from restart to the fifth message | `two_node.rs`, 3 runs plaintext, 1 run encrypted |
| Time to first connection, two machines | _pending M5_ | |
| Path type and RTT, two machines | _pending M5_ | `triad probe` |

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
   relayed?** _Pending Milestone 5._ On one machine the probe's selected path
   was direct (`Ip(192.168.1.216:…)`).

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

_Not started: Milestone 6 follows Milestone 5._ p2panda's own `group_chat.rs`
example (spaces API, with no OpenMLS) is about 280 lines including its CLI,
which suggests the spaces version of triad would be much smaller than the
OpenMLS integration (`mls.rs` plus `group.rs`: 1,439 lines, of which roughly half are tests).

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

- **Run Milestone 5** on the Mac and Linux machine, fill in the pending rows,
  and Answer 2.
- **Milestone 6:** the same chat on p2panda spaces, then the comparison
  table.
- **Milestone 7:** three data directories through the network, exercising
  `Envelope::Commit` and `max_past_epochs` end to end (already covered by an
  MLS unit test).
- **Hide metadata:** encode `Envelope` as an integer-tagged CBOR array (or
  wrap all of it in MLS) so type names stop leaking; add MLS padding.
- **Merge identities:** use the p2panda signing key as the MLS signature key
  and drop `triad_signer`.
- **Upstream:** propose `Node::endpoint()` or `NodeBuilder::protocol()` and
  `Node::shutdown()` to p2panda.
- **Close the crash gap:** an OpenMLS `StorageProvider` on p2panda's sqlx
  pool, so the MLS state change, `processed_ops` and the ack commit together.
