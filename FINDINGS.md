# triad findings

Findings from building `triad`, a two-member command-line chat on p2panda 0.8,
iroh 1.4 and OpenMLS 0.9. Written as the work happened. Dates are 2026-11-11
unless noted.

## Environment

| | Machine A (this one) | Machine B |
| --- | --- | --- |
| OS | Ubuntu 26.04.1 LTS, Linux 7.0.0-43-generic | _pending (macOS, Apple silicon)_ |
| Hardware | AMD Ryzen 7 3700X, 16 threads, 121 GB RAM | _pending_ |
| Rust | 1.98.1 stable (pinned in `rust-toolchain.toml`) | same |
| Network | Home LAN behind NAT | _pending_ |

Resolved versions (from `Cargo.lock`): p2panda 0.8.0 (and p2panda-net,
-sync, -store 0.8.0), iroh 1.4.1, iroh-gossip 0.102.0, openmls 0.9.2,
openmls_rust_crypto 0.6.0, openmls_basic_credential 0.6.0,
openmls_sqlite_storage 0.3.0, rusqlite 0.37.0, libsqlite3-sys 0.35.0
(bundled), sqlx 0.9.0.

## Milestone results

| # | Milestone | Result | Commit | Notes |
| --- | --- | --- | --- | --- |
| 0 | Compatibility spike | pass | `m0` | One iroh (1.4.1) and one libsqlite3-sys (0.35.0) in `cargo tree`. |
| 1 | iroh probe | pass (one machine) | `m1` | Direct path, 23 ms connect, 1.3 ms echo RTT. Cross-machine run pending (needs machine B). |
| 2 | p2panda stream, no encryption | pass | `m2` | 10 each way in order; kill -9, 5 missed, restart receives exactly 5; no duplicates. |

## Measurements

| Measurement | Value | How |
| --- | --- | --- |
| Clean release build (M0, empty main) | 1 min 30 s wall, 1.7 GB peak RSS | `/usr/bin/time -v cargo build --release`, 16 threads |
| Probe, same machine | direct path, connect 23 ms, echo RTT 1.3 ms, QUIC path RTT 2.1 ms | `triad probe listen` / `connect` |
| Join to joined, same machine (M2, no MLS) | 0.13 s | `two_node.rs`, 3 runs: 0.125–0.128 s |
| 20 messages exchanged, same machine | 2.6 s | `two_node.rs`, includes process start |
| Catch-up of 5 after kill -9, same machine | 1.25–1.31 s from restart to fifth message | `two_node.rs`, 3 runs |

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
p2panda's log sync in live mode, and gossip is not on this path. The 4,096-byte
gossip limit therefore does not constrain topic-stream operations.

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
- **Probe address format.** `triad probe listen` prints `probe1-` plus hex of
  the JSON-serialized `EndpointAddr`, so `probe connect` gets the relay URL and
  any direct addresses in one token.
- **Clock in chat output is UTC**, to avoid a time-zone dependency.

## Answers

_Filled in as evidence arrives._

1. **Does the gossip size limit apply to stored and synced operations, or only
   to live broadcast?** Only to gossip broadcast, which topic streams do not
   use. 1, 4, 16 and 64 KB operations all arrived through catch-up sync and
   while both nodes were online, and every live delivery reported
   `Source::SyncSession`. In p2panda 0.8, `Node::stream` calls
   `network.log_sync.stream(topic, live_mode = true)`; gossip backs
   `ephemeral_stream` instead.

## Layer ownership

_Pending._

## OpenMLS and p2panda spaces

_Pending (Milestone 6)._

## Library issues

_Pending._

## Proposed follow-ups

_Pending._
