# syntax=docker/dockerfile:1
# Production image for the visor daemon. linux/amd64 only — the guest kernel
# and visor-init are x86_64 artifacts. See docs/deploy/kubernetes.md.

FROM rust:1-bookworm AS builder

# clang mirrors the Makefile release recipe (aws-lc-sys also needs cmake);
# musl-tools cross-compiles visor-init for the static musl guest target. The
# rest are the guest kernel's build dependencies, because visor-kernel
# compiles it from public sources when no prebuilt binary is supplied.
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake musl-tools \
    gcc make bc flex bison libelf-dev libssl-dev git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

# After the source, deliberately: rust-toolchain.toml pins the channel, and
# rustup resolves it only from inside the tree. Adding the target earlier
# attaches it to the base image's toolchain, and the guest init build then
# fails on a missing core for musl.
RUN rustup target add x86_64-unknown-linux-musl

# VISOR_KERNEL_URL points at a published vmlinux-x86_64 to skip the ~15 minute
# compile; unset, the build script fetches public kernel sources and builds.
ARG VISOR_KERNEL_URL=""
RUN CC=clang VISOR_KERNEL_URL="${VISOR_KERNEL_URL}" \
      cargo build -p visor-runtime --release \
    && cargo build -p visor-init --release --target x86_64-unknown-linux-musl

# The kernel path is baked into the binary at compile time as the builder's
# OUT_DIR, so the runtime image must carry vmlinux-x86_64 at that exact
# absolute path. cp --parents recreates the tree under /artifacts.
RUN set -eu; \
    mkdir -p /artifacts/kernel-tree; \
    cp target/release/visor /artifacts/visor; \
    cp target/x86_64-unknown-linux-musl/release/visor-init /artifacts/visor-init; \
    kernel="$(find /build/target/release/build -type f -path '*/out/vmlinux-x86_64')"; \
    test "$(printf '%s\n' "$kernel" | wc -l)" -eq 1; \
    cp --parents "$kernel" /artifacts/kernel-tree/

FROM debian:bookworm-slim

# Runtime shell-outs: ip (TAP + cleanup), iptables (NAT/port-forward),
# mke2fs/resize2fs (rootfs + volumes). ca-certificates for registry pulls.
RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 iptables e2fsprogs ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /artifacts/visor /usr/local/bin/visor
COPY --from=builder /artifacts/visor-init /usr/libexec/visor/visor-init
COPY --from=builder /artifacts/kernel-tree/ /

ENV VISOR_HOME=/var/lib/visor
RUN mkdir -p /var/lib/visor

EXPOSE 7800

# The daemon shuts down cleanly on SIGINT (tokio ctrl_c) but installs no
# SIGTERM handler, which PID 1 would ignore.
STOPSIGNAL SIGINT

# Background `visor start` forks and the parent exits, which would kill the
# container; --foreground keeps the daemon as PID 1.
ENTRYPOINT ["/usr/local/bin/visor", "start", "--foreground"]
