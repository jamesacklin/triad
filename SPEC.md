# Implementation Spec: Two-Node Chat on iroh, MLS, and p2panda

2026-11-11 · James

> A copy of the implementation spec this repository was built from, exported from the original doc on 2026-11-11. Where the code differs from it, FINDINGS.md lists each deviation and why.

## Purpose and success criteria

Build a small Rust command-line chat app, working name `triad`, that proves iroh, p2panda, and MLS compose into one working stack. This is an exploration. The code is disposable and the findings report is the main deliverable.

The exploration answers three questions:

1. Can p2panda on iroh carry an append-only message log between two machines on different networks, including catch-up after one has been offline?
2. Can MLS encrypt that log end to end, so that p2panda only ever stores and sends ciphertext?
3. Where do the three libraries overlap or conflict, and which layer should own identity, ordering, and group membership?

Done means every item below passes with one macOS machine and one Linux machine on different networks:

- Both machines build and install from the same repository with one documented command each.
- Machine A creates a group and prints an invite. Machine B joins with it.
- Each side sends 10 messages. The other side shows all 10, decrypted, in send order.
- B goes offline, A sends 5 messages, B returns and receives all 5 with no manual resend.
- An inspection of what p2panda stores and transmits shows no plaintext message bodies.
- `FINDINGS.md` is complete (see the last section).

## Scope and non-goals

The app is one binary with a terminal interface, one group per data directory, and two members. Everything else is out of scope unless a milestone names it.

| Area | In scope | Out of scope |
| --- | --- | --- |
| Platforms | macOS on Apple silicon, Linux on x86_64 | Windows, mobile, Redox, browsers |
| Group size | 2 members, with a stretch test at 3 | Large groups, performance tuning |
| Membership | Create, invite, join | Remove, ban, roles, multi-device |
| Messages | UTF-8 text up to 2,000 bytes | Attachments, edits, reactions, threads |
| Authority | Group creator is the only member who commits MLS changes | Leaderless membership changes |
| Identity | One Ed25519 key per install, shown as hex | Names, key rotation, Azimuth, any PKI |
| Interface | CLI commands and a line-based chat prompt | GUI, TUI widgets, notifications |
| Network | Public iroh relays and discovery defaults | Self-hosted relays, Tor, LAN-only modes |
| Security | Honest peers, confidentiality against the transport and storage layers | Audit-grade key storage, metadata privacy, malicious members |

The single-committer rule is deliberate. It mirrors the host-authoritative group model and avoids concurrent MLS commits, which MLS does not resolve on its own.

## Architecture

p2panda is the only component the app talks to for networking. iroh runs inside p2panda, and MLS wraps every payload before p2panda sees it.

_(Architecture diagram in the original doc: App → OpenMLS (plaintext) → p2panda → iroh → relay/peer, with only ciphertext crossing into p2panda.)_

Read each column top down. The app hands plaintext to OpenMLS, and only ciphertext crosses the dashed line into p2panda, iroh, and the relay.

| Layer | Crate | Owns | Does not own |
| --- | --- | --- | --- |
| App | `triad` (binary) | CLI, chat prompt, rendering, invite parsing | Network or crypto calls |
| Group core | `triad-core` (library) | Envelope codec, join state machine, acknowledgement rules | Terminal I/O |
| Encryption | `openmls` | Group keys, epochs, member list, encrypt and decrypt | Transport, delivery order |
| Log and sync | `p2panda` | Signed append-only log per author, SQLite storage, discovery, catch-up sync | Plaintext |
| Transport | `iroh`, inside `p2panda-net` | QUIC connections dialed by public key, hole punching, relay fallback | Persistence |

A message takes this path from sender to receiver:

1. The user types a line. The app builds a `ChatBody` holding the text and a client timestamp.
2. OpenMLS encrypts the body as an MLS application message in the current epoch.
3. The app wraps the ciphertext in `Envelope::App` and publishes it on the group's p2panda topic.
4. p2panda signs it as an operation, appends it to the author's log, and stores it locally.
5. p2panda delivers it over an iroh connection: live if the peer is connected, by sync when the peer next comes online.
6. The peer's p2panda validates and orders the operation, then emits `StreamEvent::Processed`.
7. The peer's app passes the ciphertext to OpenMLS, renders the plaintext, and acknowledges the operation.

Three rules hold the layers together:

- **Ack after decrypt.** The node runs with `AckPolicy::Explicit`. An operation is acknowledged only after MLS processing and the local state write succeed, so a crash replays it.
- **Identity binding.** The MLS credential identity is the member's 32-byte p2panda public key. A handshake envelope is accepted only when the operation's author equals the identity inside it.
- **One endpoint.** Chat mode does not open its own iroh endpoint. p2panda 0.8's `Node` does not expose the one it owns, so raw iroh is used only in the Milestone 1 connectivity probe.

## Dependencies and version pinning

Pin the exact versions below in `Cargo.toml` and commit `Cargo.lock`. They were the newest stable releases on crates.io on 11 November 2026.

| Crate | Version | Role | Notes |
| --- | --- | --- | --- |
| [`p2panda`](https://crates.io/crates/p2panda) | 0.8.0 | Node API: topic streams, sync, storage | Released 9 November 2026. Pre-1.0, so APIs can break. Needs Rust 1.96. |
| [`iroh`](https://crates.io/crates/iroh) | 1.4.1 | Transport | Pulled in by `p2panda-net` 0.8.0 with the `tls-ring` feature. A direct dependency only in the probe binary. |
| [`iroh-gossip`](https://crates.io/crates/iroh-gossip) | 0.102.0 | Live broadcast inside p2panda | Transitive only. Still a 0.x crate. |
| [`openmls`](https://crates.io/crates/openmls) | 0.9.2 | MLS, RFC 9420 | Needs Rust 1.91. |
| [`openmls_rust_crypto`](https://crates.io/crates/openmls_rust_crypto) | 0.6.0 | Crypto provider |  |
| [`openmls_basic_credential`](https://crates.io/crates/openmls_basic_credential) | 0.6.0 | Signature keys, basic credentials |  |
| [`openmls_sqlite_storage`](https://crates.io/crates/openmls_sqlite_storage) | 0.3.0 | Persistent MLS state | Uses `rusqlite` 0.37 with bundled SQLite. |

Supporting crates are the agent's choice within these limits: `tokio` 1.x, `serde` 1.x, one CBOR crate, `clap` 4.x, `anyhow`, `tracing`. Pin the toolchain to stable Rust 1.96 or newer in `rust-toolchain.toml`.

### Calls this spec relies on

These names were checked against the published sources of the pinned versions. If the pinned docs disagree with this spec, the docs win and the difference goes in `FINDINGS.md`.

| Library | Calls |
| --- | --- |
| p2panda 0.8.0 ([crate docs](https://docs.rs/p2panda/0.8.0/p2panda/), [`NodeBuilder`](https://docs.rs/p2panda/0.8.0/p2panda/struct.NodeBuilder.html), [`Node`](https://docs.rs/p2panda/0.8.0/p2panda/struct.Node.html)) | `p2panda::builder()` with `.credentials()`, `.database_url()`, `.ack_policy(AckPolicy::Explicit)`, `.relay_url()`, `.bootstrap(node_id, relay_url)`, `.spawn()`. `node.id()`. `node.stream::<M>(topic)` and `node.stream_from(topic, StreamFrom::Start, None)`. `publisher.publish(msg)`. `StreamEvent::Processed { operation, source }`, `SyncStarted`, `SyncEnded`, `ProcessingFailed`. `operation.author()`, `.id()`, `.timestamp()`, `.message()`, `.ack()`. `Credentials::generate()`, which is serde-serializable. `Topic::random()`. |
| iroh 1.4.1 ([docs](https://docs.rs/iroh/1.4.1/iroh/)) | Probe only. `Endpoint::bind(presets::N0)`, `endpoint.online()`, `endpoint.addr()`, `endpoint.connect(addr, ALPN)`, `Router::builder(endpoint).accept(ALPN, handler).spawn()`, `ProtocolHandler::accept`. The crate ships `examples/echo.rs`; start from it. |
| OpenMLS 0.9.2 ([docs](https://docs.rs/openmls/0.9.2/openmls/)) | `SignatureKeyPair::new()` and `.store()`. `KeyPackage::builder().build(ciphersuite, provider, &signer, credential_with_key)`. `MlsGroup::new()` with `MlsGroupCreateConfig::builder()`. `group.add_members()` then `group.merge_pending_commit()`. `StagedWelcome::new_from_welcome()` then `.into_group()`. `group.create_message()`. `group.process_message()`. `MlsGroup::load()`. |

### Fixed configuration

- **Ciphersuite:** `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`.
- **MLS group config:** `use_ratchet_tree_extension(true)` so a Welcome is self-contained, and `max_past_epochs(2)` so late messages from the previous epoch still decrypt.
- **Relay:** p2panda sets no relay by default, and without one a node is reachable only on a directly routable address. The app always passes one. The default is `https://use1-1.relay.n0.iroh.link./`, one of the public relays listed in iroh 1.4.1's `src/defaults.rs`, and `--relay` overrides it. These relays are shared development infrastructure, so do not load test against them.
- **Network ID:** leave p2panda's default. It is not a secret and gives no isolation that this test needs.

## Identity, keys, and local storage

Each install has one identity: the p2panda node ID, a 32-byte Ed25519 public key shown as hex. Everything lives in one data directory, and deleting it creates a new identity.

| Key | Type | Used for | Stored in |
| --- | --- | --- | --- |
| p2panda credentials | Ed25519 signing key plus X25519 identity secret | Node ID, connection handshake, signing every operation | `credentials.cbor` |
| MLS signature key | Ed25519 | Signing KeyPackages and MLS messages | `mls.sqlite` |
| MLS KeyPackage keys | HPKE over X25519 | Receiving the Welcome | `mls.sqlite` |
| MLS group secrets | Per epoch | Encrypting and decrypting messages | `mls.sqlite` |

The two key families are independent. The identity binding rule in Architecture is the only link between them.

### Data directory

The default is `~/Library/Application Support/triad` on macOS and `~/.local/share/triad` on Linux. `--data-dir` overrides it, which is also how two instances run on one machine in tests.

| File | Written by | Contents |
| --- | --- | --- |
| `credentials.cbor` | App | Serialized `p2panda::Credentials`, file mode 0600 |
| `p2panda.sqlite` | p2panda | Operations, sync state, stream cursors, address book |
| `mls.sqlite` | OpenMLS | Signature key, KeyPackages, group state |
| `state.sqlite` | App | Group metadata, processed operation IDs, decrypted message history |
| `triad.log` | App | `tracing` output |

`state.sqlite` has three tables:

- `meta`: topic ID, MLS group ID, creator node ID, relay URL, role (`creator` or `member`), join status (`none`, `pending`, `joined`).
- `processed_ops`: operation hash and outcome (`ok`, `skipped`, `failed`).
- `messages`: operation hash, author node ID, sender timestamp, text.

### Storage rules

- **Plaintext history is the app's job.** MLS deletes message keys after use, so replaying the p2panda log cannot decrypt old messages again. Decrypted text is saved to `messages` at the moment of decryption.
- **Processing is made idempotent by the app.** p2panda delivers at least once and MLS rejects a ciphertext it has already consumed. Before any MLS call, look the operation hash up in `processed_ops`. If present, acknowledge and stop.
- **Write order per operation:** MLS call, then one `state.sqlite` transaction for `messages` and `processed_ops`, then the p2panda acknowledgement.
- **Known gap:** a crash between the MLS call and the `state.sqlite` write loses that message, because the key is gone on replay. Record such an operation as `failed`, acknowledge it, and count occurrences in `FINDINGS.md`. Do not fix this in the toy.
- **No key protection.** Files are plaintext on disk. No passphrase, no OS keychain.

## Data model and wire formats

One p2panda topic carries one group, and every operation on it holds one `Envelope`. p2panda serializes the envelope to CBOR. MLS objects inside it are TLS-serialized bytes, as OpenMLS produces them.

```rust
#[derive(Serialize, Deserialize)]
enum Envelope {
    /// Joiner to creator. Public. An MLS KeyPackage.
    KeyPackage { v: u8, key_package: ByteBuf },
    /// Creator to one joiner. An MLS Welcome, addressed by node ID.
    Welcome { v: u8, to: [u8; 32], welcome: ByteBuf },
    /// Creator to existing members. An MLS Commit. Used from 3 members up.
    Commit { v: u8, commit: ByteBuf },
    /// Any member to the group. An MLS PrivateMessage.
    App { v: u8, ciphertext: ByteBuf },
}

/// Plaintext inside the MLS application message, CBOR encoded.
#[derive(Serialize, Deserialize)]
struct ChatBody { v: u8, sent_at_ms: u64, text: String }

/// Invite. Rendered as "triad1-" + lowercase hex of the CBOR bytes.
#[derive(Serialize, Deserialize)]
struct Invite { v: u8, topic: [u8; 32], creator: [u8; 32], relay: String }
```

`v` is 1 everywhere. A receiver that sees another value records the operation as `skipped` and acknowledges it. OpenMLS 0.9 encrypts handshake messages by default, so keep the default wire format policy.

### Acceptance rules

The receiver checks each envelope against the operation's author, which p2panda has already verified by signature. Anything that fails a check is recorded as `skipped` and acknowledged.

| Envelope | Accept only when | Then |
| --- | --- | --- |
| `KeyPackage` | I am the creator. The KeyPackage validates. Its credential identity equals the operation author. The author is not already a member. | Confirm with the user, or accept at once under `--auto-accept`. Add the member. |
| `Welcome` | The author is the creator named in my invite. `to` is my node ID. My join status is `pending`. | Join the group. Set status to `joined`. |
| `Commit` | The author is the creator. My status is `joined`. | Process it and merge the staged commit. |
| `App` | My status is `joined`. The MLS sender's credential identity equals the operation author. | Decrypt, store in `messages`, render. |

### Cases the rules must cover

- **My own operations come back.** p2panda runs locally published operations through the same subscription. MLS cannot decrypt a member's own ciphertext, so store the plaintext and the operation hash at publish time. When the operation returns, it is already in `processed_ops`.
- **History from before I joined.** A joiner syncs the creator's whole log, including `App` envelopes from earlier epochs. These cannot decrypt. Record them as `skipped`.
- **Size.** Text is capped at 2,000 bytes. Keep every envelope under 3,500 bytes. p2panda's gossip layer defaults to a 4,096-byte message limit, and whether that limit applies to stored operations is an open question for Milestone 2.
- **Display order.** Live messages print in arrival order. `triad history` sorts by p2panda operation timestamp, then by operation hash.

## Protocol flows

All four flows run over the single p2panda topic. A is the creator on one machine and B is the joiner on the other.

### Create a group

1. **A** runs `triad create`. The app generates credentials and an MLS signature key if the data directory is new.
2. **A** generates a random topic and creates an MLS group whose group ID is the topic bytes. The group is at epoch 0 with A as its only member.
3. **A** saves `meta` with role `creator` and status `joined`.
4. **A** prints the invite, which holds the topic, A's node ID, and the relay URL.

### Invite and join

1. **B** runs `triad join <invite>`. The app saves `meta` with role `member` and status `pending`.
2. **B** starts its node with `.relay_url(relay)` and `.bootstrap(creator, relay)`, then subscribes to the topic.
3. **B** generates one KeyPackage and publishes `Envelope::KeyPackage`.
4. **A** receives it, applies the acceptance rules, and asks `Accept 3f9a12c4? [y/N]`.
5. **A** calls `add_members`, merges the pending commit to reach epoch 1, and publishes `Envelope::Welcome` addressed to B. When other members already exist, A publishes `Envelope::Commit` first.
6. **B** receives the Welcome, builds the group with `StagedWelcome::new_from_welcome` and `into_group`, and checks that the creator's identity is in the member list.
7. **B** sets status to `joined`. Both sides print the member count.

B keeps its KeyPackage keys in `mls.sqlite`, so a restart while `pending` does not publish a second KeyPackage. If no Welcome arrives within 120 seconds, B prints a hint and keeps waiting.

### Send and receive

Sending follows the seven-step path in Architecture. Two additions apply:

- The prompt refuses input while status is `pending`.
- The sender writes its own plaintext to `messages` using the operation hash that `publish` returns.

### Offline catch-up

1. **B** quits with Ctrl-C. The app shuts the node down cleanly.
2. **A** sends messages. p2panda appends them to A's log and stores them.
3. **B** starts `triad chat` again. The app loads credentials, opens the MLS store, calls `MlsGroup::load`, and subscribes with the default `StreamFrom::Frontier`.
4. p2panda reconnects to A and syncs the missing operations. The app prints `SyncStarted` and `SyncEnded` counts at `-v`.
5. **B** processes the new operations in log order through the acceptance rules.

A must be online when B returns, because this test has no third always-on node. If the two are never online together, nothing is delivered. Note the observed behavior in `FINDINGS.md`.

## CLI surface

One binary, `triad`, with the subcommands below. Chat mode reads lines from stdin and prints events to stdout, so the same binary serves a person and a test script.

| Command | Behavior |
| --- | --- |
| `triad whoami` | Print the node ID. Create credentials if the data directory is new. |
| `triad create` | Create the group, print the invite, enter chat mode. |
| `triad invite` | Print the invite again. Creator only. |
| `triad join <invite>` | Save the invite, publish a KeyPackage, enter chat mode. |
| `triad chat` | Resume the existing group in chat mode. |
| `triad history [--limit N]` | Print stored decrypted messages, oldest first. |
| `triad status` | Print role, join status, MLS epoch, member IDs, and operation counts by outcome. |
| `triad inspect --grep <text>` | Scan the raw bytes of `p2panda.sqlite` for the text. Exit 0 if absent, 1 if found. |
| `triad probe listen` | Milestone 1 only. Start a raw iroh echo endpoint and print its address. |
| `triad probe connect <addr>` | Milestone 1 only. Connect, echo one line, report whether the path was direct or relayed. |

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--data-dir <path>` | All | Use this data directory. |
| `--relay <url>` | `create`, `probe` | Override the default relay. Joiners take the relay from the invite. |
| `--auto-accept` | `create`, `chat` | Accept every valid join request without asking. |
| `--json` | Chat mode | Print one JSON object per event instead of text. |
| `-v` | All | Also print sync and connection events. |

In chat mode, a line starting with `/` is a command: `/quit`, `/status`, `/members`. Any other line is sent as a message. Exit codes are 0 for success, 1 for a runtime error, and 2 for a usage error.

A creator session looks like this:

```text
$ triad create
node    3f9a12c4
group   9b2e77a1 (epoch 0, 1 member)
invite  triad1-a56f0c...
Waiting for peers. Type a message, or /quit.
join request from 7c1d09ee. Accept? [y/N] y
7c1d09ee joined (epoch 1, 2 members)
[12:04:10] 7c1d09ee: hello from linux
hello from mac
[12:04:31] me: hello from mac
```

With `--json`, the same events are lines such as `{"event":"joined","member":"7c1d09ee...","epoch":1}` and `{"event":"message","author":"7c1d09ee...","text":"hello from linux","op":"..."}`. Emit `ready`, `join_request`, `joined`, `message`, `sync_started`, `sync_ended`, and `error`. The integration tests depend on these names.

## Project layout

A Cargo workspace with one library, one binary, and one probe crate. All pinned versions sit in `[workspace.dependencies]`.

```text
triad/
  Cargo.toml             workspace and pinned versions
  rust-toolchain.toml
  README.md              install and run steps for macOS and Linux
  FINDINGS.md            the report
  crates/
    triad-core/          library, no terminal I/O
      src/envelope.rs    Envelope, ChatBody, Invite, encode and decode
      src/identity.rs    credentials file, node ID formatting
      src/mls.rs         OpenMLS wrapper: provider, create, key package, add, join, encrypt, decrypt
      src/net.rs         p2panda wrapper: node builder, topic stream, publish, acknowledge
      src/state.rs       state.sqlite: meta, processed_ops, messages
      src/group.rs       acceptance rules and the join state machine
    triad-probe/         raw iroh echo; the only crate that names iroh directly
    triad-cli/           the `triad` binary
      src/main.rs        clap commands
      src/chat.rs        stdin loop, text and JSON output
      src/inspect.rs     plaintext scan of p2panda.sqlite
  tests/
    two_node.rs          two processes, two data directories, one machine
  scripts/
    cross-test.sh        the Mac to Linux acceptance run
```

Three structural rules:

- **`group.rs`**** is a pure state machine.** Its input is an operation's author, hash, and envelope plus the current state. Its output is a list of effects: publish an envelope, store a message, ask the user, acknowledge. The acceptance rules are then unit-testable with no network and no clock.
- **`mls.rs`**** and ****`net.rs`**** hide their libraries.** No OpenMLS or p2panda type appears in another module's signatures. A breaking release then touches one file.
- **Chat mode never opens an iroh endpoint.** `triad-probe` is linked only into the `probe` subcommands, and `cargo tree` must still show a single `iroh` version.

## Milestones

Build in the order below and do not start a milestone until the previous one passes. The one exception is Milestone 3, which has no network dependency and can be built alongside 1 and 2.

| # | Milestone | Build | Passes when |
| --- | --- | --- | --- |
| 0 | Compatibility spike | The workspace with every pinned dependency and an empty `main`. | `cargo build --release` succeeds. `cargo tree` shows one `iroh` version and one `libsqlite3-sys` version. Toolchain, build time, and binary size are in `FINDINGS.md`. |
| 1 | iroh probe | `triad probe listen` and `triad probe connect`, adapted from iroh's `examples/echo.rs`. | The echo works between two processes on one machine, then between the Mac and the Linux machine. Path type and round-trip time are recorded. |
| 2 | p2panda stream, no encryption | `net.rs`, `state.rs`, chat mode, and `tests/two_node.rs`. `Envelope::App` temporarily carries plaintext. | On one machine: 10 messages each way arrive in order. B is killed, A sends 5, B restarts and receives exactly 5. `messages` holds no duplicates. |
| 3 | MLS in memory | `mls.rs` with unit tests and no network. | Tests cover create, KeyPackage, add, Welcome, join, and messages in both directions. One test closes and reopens `mls.sqlite` mid-conversation and keeps decrypting. |
| 4 | Integration | `group.rs`, the join flow, encrypted `App` envelopes, `triad inspect`. | `two_node.rs` passes with encryption on. `triad inspect --grep` exits 0 on both data directories for every sent message. Unit tests cover each row of the acceptance rules. |
| 5 | Two machines | `README.md` install steps and `scripts/cross-test.sh`. | Every item in the Purpose checklist passes with the Mac and the Linux machine on different networks. |
| 6 | Stretch: p2panda spaces | A separate example binary that runs the same chat on p2panda's built-in encrypted spaces, with no OpenMLS. | The two-machine test passes. The comparison table in `FINDINGS.md` is filled in. |
| 7 | Stretch: three members | A third data directory on one machine. Exercises `Envelope::Commit` and `max_past_epochs`. | After the third member joins, all three exchange messages. A message B sends while A is committing still decrypts. |

Milestone 2 also measures one thing the later design depends on. Publish payloads of 1 KB, 4 KB, 16 KB, and 64 KB, both live and through catch-up sync, and record which sizes arrive.

Stop and report, without working around the problem, in three cases:

- Milestone 0 cannot produce a build even with the fallbacks in Risks.
- Milestone 1 connects on one machine but never between the two machines, including over the relay.
- Milestone 2 loses or duplicates operations across a restart.

## Build, install, and cross-machine test

Both machines build from source with the same command. No inbound ports are needed: each node makes outbound UDP connections for direct paths and outbound HTTPS to the relay.

### Prerequisites

| Machine | Needs |
| --- | --- |
| macOS, Apple silicon | Xcode command line tools (`xcode-select --install`), `rustup` with stable Rust 1.96 or newer |
| Linux, x86_64 | A C toolchain (`build-essential` on Debian or Ubuntu), `rustup` with stable Rust 1.96 or newer |

The C compiler is required because SQLite is compiled from bundled source and the TLS backend includes C and assembly.

### Install

```sh
git clone <repo-url> triad && cd triad
cargo install --path crates/triad-cli --locked
triad whoami
```

### Acceptance run

Put the machines on different networks, for example the Mac on home Wi-Fi and the Linux machine on a cloud VM or a phone hotspot.

1. Mac: `triad create --auto-accept -v`. Copy the invite.
2. Linux: `triad join <invite> -v`.
3. Both: confirm the output shows 2 members at epoch 1.
4. Mac: send `mac-01` through `mac-10`. Linux: send `linux-01` through `linux-10`.
5. Both: `triad history` lists 20 messages, with each sender's messages in order.
6. Linux: `/quit`. Mac: send `mac-11` through `mac-15`.
7. Linux: `triad chat -v`. Expect one sync and exactly five new messages.
8. Both: `/quit`, then `triad inspect --grep mac-01` and `triad inspect --grep linux-01`. Both must exit 0.
9. Both: `triad status` shows the same epoch, the same two members, and zero `failed` operations.
10. Repeat steps 1 to 5 with fresh data directories and the roles swapped, so Linux creates and the Mac joins.

Run steps 1 to 5 once more with both machines on the same LAN and note whether the connection path changes. `scripts/cross-test.sh` automates steps 1 to 9 over SSH using `--json` output, and is optional.

### If something fails

| Symptom | First check |
| --- | --- |
| The two nodes never see each other | Run `triad probe` between the machines. If the probe fails, the problem is the network or relay, not p2panda. |
| The joiner stays `pending` | Confirm the creator is online and printed a join request. Run both sides with `-v`. |
| `triad inspect` finds plaintext | Check that Milestone 2's plaintext `App` path was removed and that both data directories are fresh. |
| The Linux build fails in a `-sys` crate | The C toolchain is missing. |

## Risks, open questions, and decision points

The largest risk is p2panda's age: 0.8.0 was two days old when this spec was written and the project states that APIs can break before 1.0. Each risk below names the milestone that exposes it and the fallback to take.

| Risk | Exposed in | Fallback |
| --- | --- | --- |
| p2panda 0.8.0 behaves differently from its docs, or has a blocking bug. | 0, 2 | Stay on the exact pin. If blocked, build on the lower-level crates (`p2panda-net`, `p2panda-sync`, `p2panda-store`) and record why. |
| Two SQLite users in one binary. `openmls_sqlite_storage` needs `libsqlite3-sys` ^0.35 through `rusqlite`. p2panda's `sqlx` 0.9 accepts 0.30.1 up to, not including, 0.38. They should unify on 0.35. | 0 | Use `openmls_memory_storage` with its `persistence` feature and write the store to a file after every MLS call. |
| `SqliteStorageProvider` is generic over a codec that the app must supply. | 3 | Write a small `serde_json` codec. This is expected work, not a blocker. |
| An envelope exceeds p2panda's 4,096-byte default gossip limit. A Welcome grows with group size. | 2, 7 | Raise the limit through `NodeBuilder::gossip_config`. Record the size that failed. |
| The public relay is slow, rate limited, or unreachable from one network. | 1, 5 | Run `iroh-relay` on the Linux machine or a VM and pass `--relay`. |
| First contact is slow. p2panda finds peers by a random walk that starts at the bootstrap node. | 4, 5 | None. Measure the time from `join` to the creator's join request and report it. |
| At-least-once delivery meets single-use MLS keys. | 4 | Already handled by `processed_ops`. The crash gap in Storage rules stays open by design. |
| macOS prompts for incoming connections or local network access. p2panda's mDNS discovery is active by default. | 1, 5 | Approve the prompt. If discovery misbehaves, set `mdns_mode` to disabled and rely on the invite's bootstrap entry. |

### Questions the findings must answer

- Does the gossip size limit apply to stored and synced operations, or only to live broadcast?
- On these two networks, how often is the path direct and how often relayed?
- After `kill -9`, does `StreamFrom::Frontier` with explicit acknowledgement replay exactly the unacknowledged operations?
- How does p2panda order operations from different authors, and would that order be enough for MLS commits from more than one committer?
- Can p2panda, OpenMLS, and the app share one SQLite file, so one transaction covers a whole operation?
- What would it take to register a custom iroh protocol on the endpoint p2panda owns?
- What metadata can a holder of the topic see without group keys? Expect author, timing, size, and envelope type.

### Decision points

- **After Milestone 2:** if p2panda sync is not reliable across restarts, stop. The alternative is a plain iroh connection with an app-owned log on the creator, which is a different spec.
- **After Milestone 6:** recommend OpenMLS or p2panda spaces for group encryption. Compare standardization, behavior when members change the group while offline, lines of app code, and the failure cases each one left open.
- **Not decided here:** an always-on node per user, a second device per user, and whether the MLS signature key should be the p2panda key. Note observations only.

## Working rules and the findings report

The agent works alone through Milestone 4 on one machine, then needs the second machine for Milestone 5. `FINDINGS.md` is written as the work happens, not at the end.

### Rules for the implementing agent

1. **Read before calling.** These releases are newer than most training data. Before writing against a library, read its docs for the pinned version and the `examples/` directory in the downloaded crate source. Do not write calls from memory.
2. **Hold the pins.** No version changes, git dependencies, or forks unless a fallback in Risks calls for one. Record any change with its reason.
3. **One milestone, one commit.** Commit as `M<n>: <summary>` and tag `m<n>` when the acceptance test passes.
4. **Keep the checks green.** `cargo test`, `cargo fmt --check`, and `cargo clippy -- -D warnings` pass at every tag.
5. **Stay in scope.** Anything outside the scope table goes under Proposed follow-ups in the report, not into the code.
6. **Protect secrets in logs.** Never log key material. Log plaintext only at trace level.
7. **Decide small things, ask about big ones.** If an ambiguity would change a wire format or a success criterion, ask. Otherwise take the simplest option and note it.
8. **Three attempts, then stop.** If one blocker survives three distinct approaches, stop and report what was tried.
9. **Hand over cleanly at Milestone 5.** Without SSH access to the second machine, stop after Milestone 4, confirm the README install steps work from a fresh clone, and give the person the acceptance run to perform.

### `FINDINGS.md` contents

| Section | What goes in it |
| --- | --- |
| Environment | Both machines' OS and hardware, Rust version, resolved crate versions, the networks used. |
| Milestone results | One row per milestone: pass or fail, date, commit, one line of notes. |
| Measurements | Build time, binary size, time to first connection, direct or relayed path, round-trip time, join latency, catch-up time for 5 messages, payload size results. |
| Spec deviations | Every place this spec was wrong or the pinned docs differed, with what was done instead. |
| Answers | The seven questions from the Risks section, each with evidence. |
| Layer ownership | A recommendation for which layer should own identity, ordering, and group membership, and what the three libraries duplicate. |
| OpenMLS and p2panda spaces | The Milestone 6 comparison: standardization, offline membership changes, app code size, open failure cases. |
| Library issues | Bugs and rough edges per library, with upstream issue links where they exist. |
| Proposed follow-ups | Out-of-scope ideas and the next experiment worth running. |

## Sources

Versions and dependency requirements were read from the crates.io registry on 11 November 2026. API names were checked against the published crate sources for the pinned versions.

- [p2panda 0.8.0 crate documentation](https://docs.rs/p2panda/0.8.0/p2panda/)
- [p2panda `NodeBuilder`](https://docs.rs/p2panda/0.8.0/p2panda/struct.NodeBuilder.html)
- [p2panda `Node`](https://docs.rs/p2panda/0.8.0/p2panda/struct.Node.html)
- Crate sources read for API names: `p2panda` 0.8.0, `p2panda-core` 0.8.0, `iroh` 1.4.1, `openmls` 0.9.2, `openmls_sqlite_storage` 0.3.0
