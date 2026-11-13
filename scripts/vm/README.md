# triad in a minimal VM

Scripts that build a small Linux guest containing only `triad`, boot it under
QEMU, and check that it can chat with a `triad` on the host. The goal is to
measure how little memory a single-tenant VM running triad needs, on both
x86_64 and arm64.

The guest is a static `triad` binary, a kernel built from `tinyconfig` plus
virtio drivers, and a 70-line init ([init.c](init.c)). There is no initramfs,
shell or libc in the image. The root filesystem is a 64 MB ext4 image rebuilt
for every run.

## Files

| File | Purpose |
| --- | --- |
| [build.sh](build.sh) | Builds static `triad`, the kernel and the init for one architecture into `target/vm/<arch>/`. |
| [run-vm.sh](run-vm.sh) | Boots the guest once with a given memory size and `triad` arguments. |
| [chat-test.sh](chat-test.sh) | Host creates a group, the guest joins, each sends one message. Exits 0 on success. |
| [bot-test.sh](bot-test.sh) | The guest creates a channel (with its bot, the default; mock brain) and hosts it; the host joins and asks it. Exits 0 on the bot's reply. |
| [serve-test.sh](serve-test.sh) | The guest runs `triad serve` (mock brain); a socket client on this machine ([ws-chat.py](ws-chat.py)), through a QEMU port forward, has it join a channel the host created and chat. Exits 0 when both sides saw both messages. |
| [update-test.sh](update-test.sh) | The guest updates itself from a release served by the host, then rolls back a bad release. Exits 0 on success. |
| [init.c](init.c) | Guest PID 1: mounts, DNS, sets `/etc/triad.env`. Then either the test run (`triad --json`, one line, memory figures, reboot) or, with `TRIAD_SUPERVISE=1`, the supervisor that keeps `triad` running and updated. |
| `kernel-*.config` | Kernel options added on top of `tinyconfig`, shared and per architecture. |

## Prerequisites

On Debian or Ubuntu, x86_64 host with KVM:

```sh
sudo apt install build-essential flex bison bc libelf-dev e2fsprogs curl \
  qemu-system-x86 qemu-system-arm gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
```

The last two packages and the rustup target are only needed for arm64.
`build.sh` downloads the Linux 6.18.63 source from kernel.org (about 150 MB)
and checks its SHA-256 before use.

## Use

```sh
scripts/vm/build.sh x86_64
scripts/vm/chat-test.sh x86_64 64      # 64 MB guest

scripts/vm/build.sh arm64
scripts/vm/chat-test.sh arm64 64
```

`chat-test.sh` needs a host `triad` on `PATH`, or `TRIAD=/path/to/triad`.
Logs for each run are kept in `target/vm/<arch>/runs/<mem>/`.

The x86_64 guest uses QEMU's `microvm` machine with KVM. The arm64 guest uses
the `virt` machine with software emulation by default, which is several times
slower; on an arm64 host set `VM_ACCEL=kvm VM_CPU=host` (Linux) or
`VM_ACCEL=hvf VM_CPU=host` (macOS). Each script lists its other environment
overrides at the top.

## Measured results (2026-11-11)

Host: x86_64 Linux, QEMU 10.2.1. One vCPU, two group members, one message each
way, fresh data directory.

| Guest RAM | x86_64 (KVM) | arm64 (emulated) |
| --- | --- | --- |
| 128 MB | pass | pass |
| 64 MB | pass, 12 MB free | pass, 16 MB free |
| 48 MB | pass, 1 MB free | pass, 4 MB free |
| 40 MB | join did not complete in time | not tested |
| 32 MB | `triad` killed by the OOM killer | not tested |

`triad` peaks at 26 to 28 MB resident, of which about 10 MB is heap and the
rest is binary pages read from disk. The kernel takes about 9 MB.

| Artifact | x86_64 | arm64 |
| --- | --- | --- |
| `triad`, static and stripped | 20.7 MB | 17.8 MB |
| Kernel image | 2.3 MB (compressed) | 5.4 MB (uncompressed) |

### The bot in the guest (2026-11-11)

`scripts/vm/bot-test.sh x86_64 <MB>`: the guest runs `create --name garden
--auto-accept` (then `--agent`, now the default) with `TRIAD_AGENT_BRAIN=mock` (set through `VM_ENV`)
and hosts the channel's bot; a `triad` on the host joins and sends `@bot
hello from the host`. Same kernel, `triad` 23.0 MB static and stripped.

| Guest RAM | Result | Join | Ask to reply | Peak RSS (VmHWM) | RssAnon | Free |
| --- | --- | --- | --- | --- | --- | --- |
| 64 MB | pass | 3.0 s | 0.27 s | 31.6 MB | 14.7 MB | 4.9 MB |
| 48 MB | pass, slow | 11.2 s | 6.7 s | 22.6 MB | 13.9 MB | 2.6 MB |
| 40 MB | join did not complete in time | | | | | |

For comparison, the same binary without bots (`chat-test.sh`, guest
joining) at 64 MB: peak 27.7 MB, RssAnon 10.9 MB, 9.9 MB free.

## Updates in the guest

With `TRIAD_SUPERVISE=1` in `VM_ENV`, init keeps `triad` running instead of
doing one test run, and updates it (the release format and the commands are
in the top-level [README](../../README.md#updates)):

- `VM_DATA_DISK=<image>` attaches an ext4 image as a second disk, mounted on
  `/data`. Channels, history and installed releases live there and outlive
  the root filesystem, which is still rebuilt for every run.
- Init starts the binary in the slot named by `/data/bin/current`, or
  `/bin/triad` from the image when there is none.
- 30 seconds after boot and then every 6 hours it runs that same binary as
  `triad update --slots /data/bin`: a separate process that checks the
  source, and exits. Between checks it costs nothing.
- When the updater has installed a release (exit status 10), init sends
  `/quit` to `triad` and starts the new slot with the arguments in
  `VM_RESUME` (default `chat`).
- A new binary is on trial for 30 seconds. If `triad` exits in that time,
  init switches back to the slot before and the updater does not install
  that release again.

The kernel and init are not updated this way; they come from the host. Init
does not check the slots' contents at boot, so the data disk is trusted as
much as the root image is.

```sh
scripts/vm/build.sh x86_64
TRIAD_VERSION=0.1.1-next VM_TRIAD_NAME=triad-next VM_KERNEL=skip scripts/vm/build.sh x86_64
scripts/vm/update-test.sh x86_64 64
```

`update-test.sh` signs two releases with a throwaway key, serves them with
`python3 -m http.server`, and boots the guest twice on one data disk.

### Measured (2026-11-12)

x86_64 with KVM, one vCPU, `triad` 24.9 MB static and stripped, one channel,
no bot and no peer. Both boots passed at both sizes. The figures are from
the end of boot 1, after the update, with the new version running.

| Guest RAM | Result | Peak RSS (VmHWM) | RssAnon | Free | Available |
| --- | --- | --- | --- | --- | --- |
| 64 MB | pass | 27.3 MB | 9.6 MB | 2.4 MB | 33.6 MB |
| 48 MB | pass | 21.2 MB | 11.0 MB | 5.0 MB | 12.5 MB |

Memory was not sampled during the download itself, the arm64 guest was not
run, and neither was a guest with a bot or a connected peer.

## Limits

- Not run on a Mac. The arm64 guest has only been booted under emulation on
  an x86_64 host, and the `hvf` setting above is untested.
- The arm64 build was tested with an unpacked copy of Ubuntu's cross
  toolchain passed through `VM_CC`, not with a system-wide install.
- `mkfs.ext4 -d` and `timeout` are GNU/Linux tools, so `run-vm.sh` needs
  changes before it runs on macOS.
- QEMU's own host-side memory was not measured, and Firecracker was not tried.
- The binary is linked statically against glibc, not musl.
- The 40 MB and 32 MB results are from the first set of runs, which used a
  busybox shell init instead of `init.c`.
