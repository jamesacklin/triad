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
| [init.c](init.c) | Guest PID 1: mounts, DNS, runs `triad --json`, prints memory figures, reboots. |
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
