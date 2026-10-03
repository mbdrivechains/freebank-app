# The FreeBank app's Linux build, pinned so anyone can rebuild a release and get the same bytes (rebuild.sh).
# Everything is fixed: the base image by digest, Ubuntu's packages from a dated snapshot, and exact Rust and Node.
FROM ubuntu:22.04@sha256:b8b6ee6aa931ecd9d0d952abc34dc0e5f7c6a30c6bb71b079fe399fde0329c02

# Ubuntu's archive as it was on this date. apt checks the snapshot's signed Release files against the keyring in the
# image, so the packages' integrity doesn't rest on TLS (the image has no CA certificates yet, hence Verify-Peer).
ARG SNAPSHOT=20261001T000000Z
RUN printf 'Acquire::https::Verify-Peer "false";\nAcquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99snapshot \
 && for s in jammy jammy-updates jammy-security; do \
      echo "deb https://snapshot.ubuntu.com/ubuntu/$SNAPSHOT $s main universe"; done > /etc/apt/sources.list \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libxdo-dev libssl-dev build-essential file \
      ca-certificates curl xz-utils pkg-config xdg-utils \
 && rm -rf /var/lib/apt/lists/*

# Node 20.20.2 and Rust 1.98.1, what GitHub's runner had for v0.2.2, checked against their published sha256.
RUN curl -fsSLo /tmp/node.tar.xz https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.xz \
 && echo "df770b2a6f130ed8627c9782c988fda9669fa23898329a61a871e32f965e007d  /tmp/node.tar.xz" | sha256sum -c - \
 && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 && rm /tmp/node.tar.xz
ENV RUSTUP_HOME=/rustup CARGO_HOME=/cargo PATH=/cargo/bin:$PATH
RUN curl -fsSLo /tmp/rustup-init https://static.rust-lang.org/rustup/archive/1.28.2/x86_64-unknown-linux-gnu/rustup-init \
 && echo "20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c  /tmp/rustup-init" | sha256sum -c - \
 && chmod +x /tmp/rustup-init && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.98.1 \
 && rm /tmp/rustup-init

COPY build.sh /usr/local/bin/freebank-build
ENTRYPOINT ["/usr/local/bin/freebank-build"]
