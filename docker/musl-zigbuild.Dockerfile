# Static musl build environment: the Rocky 9 test image (Rust toolchain with the musl targets)
# plus zig 0.14.1 and cargo-zigbuild, the combination the CI `build-musl` job uses.
#
# Why a container: the zig of a developer machine is whatever its package manager has (0.16 on
# a current Mac cannot build the C dependencies of sqlx / aws-lc: `ar: unable to open
# libsqlite3.a`), and zig 0.14.x is what the release binaries are built with. scripts/musl-docker.sh
# builds this image and runs the build in it, so macOS and Windows machines get the same binary.
FROM sip-fax-rs-test:rocky9

ARG ZIG_VERSION=0.14.1

RUN dnf -y install xz python3-pip jq \
 && dnf clean all \
 && curl -sSfL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" | tar -xJ -C /opt \
 && ln -s "/opt/zig-x86_64-linux-${ZIG_VERSION}/zig" /usr/local/bin/zig \
 && pip3 install cargo-zigbuild \
 && zig version | grep -q "^${ZIG_VERSION}$"
