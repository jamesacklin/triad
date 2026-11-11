# triad

A small command-line chat for two people, built to check whether three
libraries fit together into one stack. iroh dials peers by public key,
p2panda keeps a signed append-only log per author and syncs it, and MLS
(OpenMLS) encrypts every message before p2panda sees it. This is an
exploration: the code is disposable, and [FINDINGS.md](FINDINGS.md) is the
deliverable. [SPEC.md](SPEC.md) is the implementation spec it follows.

The [spec review](FINDINGS.md#review-against-specmd-2026-11-11) records the
correctness fixes, test results and remaining crash windows. The peer tests
pass between separate processes on one networked Linux host, but cross-network
acceptance (Milestone 5) is still pending. Don't treat this prototype as a
reliable chat system until that run passes.

```
App (triad) ── plaintext ──▶ OpenMLS ── ciphertext ──▶ p2panda ──▶ iroh ──▶ relay / peer
```

## Prerequisites

| Machine | Needs |
| --- | --- |
| macOS, Apple silicon | Xcode command line tools (`xcode-select --install`), `rustup` with stable Rust 1.96 or newer |
| Linux, x86_64 | A C toolchain (`build-essential` on Debian or Ubuntu), `rustup` with stable Rust 1.96 or newer |

The C compiler is needed because SQLite is compiled from bundled source and
the TLS backend includes C and assembly. `rust-toolchain.toml` pins the exact
toolchain; rustup installs it on first build.

No inbound ports are needed. Each node makes outbound UDP connections for
direct paths and outbound HTTPS to the relay.

## Install

The same command on both machines:

```sh
git clone https://github.com/jamesacklin/triad.git && cd triad
cargo install --path crates/triad-cli --locked
triad whoami
```

`triad whoami` creates this install's identity and prints its node ID. A
clean build takes a few minutes.

## Use

On machine A:

```sh
triad create
```

It prints an invite (`triad1-…`) and enters chat mode. On machine B:

```sh
triad join triad1-…
```

A is asked `join request from 7c1d09ee. Accept? [y/N]`. After `y`, both sides
print `joined (epoch 1, 2 members)`. Then type a line and press Enter to send
it. Lines starting with `/` are commands: `/quit`, `/status`, `/members`.

Later, `triad chat` resumes the group. A must be online for B to catch up,
because there is no always-on third node.

| Command | Behavior |
| --- | --- |
| `triad whoami` | Print the node ID. Create credentials if the data directory is new. |
| `triad create` | Create the group, print the invite, enter chat mode. |
| `triad invite` | Print the invite again. Creator only. |
| `triad join <invite>` | Save the invite, publish a KeyPackage, enter chat mode. |
| `triad chat` | Resume the existing group in chat mode. |
| `triad history [--limit N]` | Print stored decrypted messages, oldest first. |
| `triad status` | Print role, join status, MLS epoch, member IDs and operation counts by outcome. |
| `triad inspect --grep <text>` | Scan the raw bytes of `p2panda.sqlite` for the text. Exit 0 if absent, 1 if found. |
| `triad probe listen` | Start a raw iroh echo endpoint and print its address. |
| `triad probe connect <addr>` | Connect, echo one line, report whether the path was direct or relayed. |

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--data-dir <path>` | All | Use this data directory (default `~/Library/Application Support/triad` on macOS, `~/.local/share/triad` on Linux). |
| `--relay <url>` | `create`, `probe` | Override the default relay (`https://use1-1.relay.n0.iroh.link./`). Joiners take the relay from the invite. |
| `--auto-accept` | `create`, `chat` | Accept every valid join request without asking. |
| `--json` | chat mode | Print one JSON object per event (`ready`, `join_request`, `joined`, `message`, `sync_started`, `sync_ended`, `error`). |
| `-v` | All | Also print sync and connection events. |

Exit codes: 0 success, 1 runtime error, 2 usage error.

One data directory holds one group. To run two instances on one machine, give
each its own `--data-dir`.

## Acceptance run (two machines)

Put the machines on different networks, for example the Mac on home Wi-Fi and
the Linux machine on a cloud VM or a phone hotspot. Both machines need
`triad` installed as above.

1. Mac: `triad create --auto-accept -v`. Copy the invite.
2. Linux: `triad join <invite> -v`.
3. Both: confirm the output shows 2 members at epoch 1.
4. Mac: send `mac-01` through `mac-10`. Linux: send `linux-01` through `linux-10`.
5. Both: `triad history` lists 20 messages, with each sender's messages in order.
6. Linux: `/quit`. Mac: send `mac-11` through `mac-15`.
7. Linux: `triad chat -v`. Expect one sync and exactly five new messages.
8. Both: `/quit`, then `triad inspect --grep mac-01` and `triad inspect --grep linux-01`. Both must exit 0.
9. Both: `triad status` shows the same epoch, the same two members and `failed=0`.
10. Repeat steps 1 to 5 with fresh data directories (`--data-dir`) and the roles swapped, so Linux creates and the Mac joins.
11. Run steps 1 to 5 once more with both machines on the same LAN, and note whether the connection path changes.

Before step 1, a raw connectivity check is useful: `triad probe listen` on
one machine, then `triad probe connect <addr>` on the other. It reports the
path (direct or relayed) and the round-trip time.

`scripts/cross-test.sh user@other-host` automates steps 1 to 9 over SSH using
`--json` output, with fresh data directories on both sides. Set
`LOCAL_TAG`/`REMOTE_TAG` to name the messages; `scripts/cross-test.sh local`
runs both sides on one machine as a smoke test.

Record the results in FINDINGS.md: path type, round-trip time, join latency
and catch-up time.

## If something fails

| Symptom | First check |
| --- | --- |
| The two nodes never see each other | Run `triad probe` between the machines. If the probe fails, the problem is the network or relay, not p2panda. |
| The joiner stays `pending` | Confirm the creator is online and printed a join request. Run both sides with `-v`. After 120 s the joiner prints a hint and keeps waiting. |
| `triad inspect` finds plaintext | Check that both data directories are fresh (Milestone 2 stored plaintext). |
| The Linux build fails in a `-sys` crate | The C toolchain is missing. |
| macOS asks about incoming connections or local network access | Allow it; p2panda's mDNS discovery is on by default. |

Logs go to `triad.log` in the data directory (`RUST_LOG=debug` for more).

## p2panda spaces variant (Milestone 6)

`triad-spaces` is the same chat with p2panda's built-in encrypted spaces
instead of OpenMLS. Install it the same way and run the same acceptance steps
with `triad-spaces` in place of `triad` (its default data directory is
`…/triad-spaces`). It has `whoami`, `create`, `join`, `chat`, `history`,
`status` and `inspect`; `status` shows no epoch, since spaces expose none.

```sh
cargo install --path crates/triad-spaces --locked
```

## Development

```sh
cargo test                       # unit tests and the two-process integration test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -p triad-core --test payload_sizes -- --ignored --nocapture   # M2 payload measurement
cargo test -p triad-core --test shared_sqlite -- --ignored --nocapture   # one-file SQLite experiment
```

Layout: `crates/triad-spaces` (the Milestone 6 comparison binary),
`crates/triad-core` (envelopes, identity, MLS, p2panda, state, the
acceptance rules in `group.rs`), `crates/triad-probe` (raw iroh echo, the only
crate that names iroh), `crates/triad-cli` (the `triad` binary and
`tests/two_node.rs`).
