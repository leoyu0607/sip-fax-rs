# Test environment for SpanDSP-based fax simulation (tools/ workspace).
# Rocky Linux 9 matches the deployment target (RHEL 9). SpanDSP is built from
# a pinned upstream tag; it is only ever used by test tooling, never shipped.
FROM rockylinux/rockylinux:9

ARG SPANDSP_REF=v3.1.1

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

ENV PKG_CONFIG_PATH=/usr/local/lib64/pkgconfig:/usr/local/lib/pkgconfig \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable -c rustfmt,clippy \
 && chmod -R a+w $RUSTUP_HOME $CARGO_HOME

WORKDIR /work
