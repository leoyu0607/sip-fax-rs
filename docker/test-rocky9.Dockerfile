# Test environment for SpanDSP-based fax simulation (tools/ workspace).
# Rocky Linux 9 matches the deployment target (RHEL 9). SpanDSP is built from
# a pinned upstream tag; it is only ever used by test tooling, never shipped.
FROM rockylinux/rockylinux:9

ARG SPANDSP_REF=v3.1.1
ARG RUST_VERSION=1.99.1
ARG SIPP_VERSION=3.8.0
ARG SIPP_SHA256=e4093db1f461b36404e0355a2d72618fd7423419d1fe7e8d5299f0a728629a1d

RUN dnf -y install epel-release dnf-plugins-core \
 && dnf config-manager --set-enabled crb \
 && dnf -y install gcc gcc-c++ make autoconf automake libtool pkgconf-pkg-config git which diffutils \
        libtiff-devel libtiff-tools libjpeg-turbo-devel file \
 && dnf clean all

RUN git clone --depth 1 --branch ${SPANDSP_REF} https://github.com/freeswitch/spandsp.git /src/spandsp \
 && cd /src/spandsp \
 && ./bootstrap.sh \
 && ./configure --prefix=/usr/local --disable-static \
 && make -j"$(nproc)" \
 && make install \
 && printf "/usr/local/lib\n/usr/local/lib64\n" > /etc/ld.so.conf.d/usr-local.conf && ldconfig \
 && rm -rf /src/spandsp

# SIPp: independent third-party SIP implementation used as the interop peer (M1+).
RUN curl -sSLo /usr/local/bin/sipp https://github.com/SIPp/sipp/releases/download/v${SIPP_VERSION}/sipp \
 && echo "${SIPP_SHA256}  /usr/local/bin/sipp" | sha256sum -c - \
 && chmod +x /usr/local/bin/sipp

ENV PKG_CONFIG_PATH=/usr/local/lib64/pkgconfig:/usr/local/lib/pkgconfig \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION} -c rustfmt,clippy,llvm-tools-preview -t x86_64-unknown-linux-musl,aarch64-unknown-linux-musl \
 && chmod -R a+w $RUSTUP_HOME $CARGO_HOME

WORKDIR /work
