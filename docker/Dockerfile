# Game Cacher image: the game-cacher binary plus the web UI. Build from the repository root:
#   docker build -f docker/Dockerfile -t game-cacher:dev .
#
# Kept to syntax both BuildKit and podman/buildah accept: no heredocs and no
# COPY --exclude.

FROM redhat/ubi10 AS web-builder
ARG NVM_VERSION=v0.40.3
ENV NVM_DIR=/root/.nvm
# Node's official binaries link against libatomic, which ubi10 doesn't ship.
RUN dnf -y install libatomic && dnf clean all
RUN mkdir -p /output /build/game-cacher-ui $NVM_DIR
RUN set -ex \
    && curl -fsSL -o /tmp/nvm-install.sh https://raw.githubusercontent.com/nvm-sh/nvm/${NVM_VERSION}/install.sh \
    && bash /tmp/nvm-install.sh \
    && rm /tmp/nvm-install.sh \
    && . $NVM_DIR/nvm.sh \
    && nvm install --lts
COPY game-cacher-ui/package*.json /build/game-cacher-ui/
RUN --mount=type=cache,target=/root/.npm set -ex \
    && . $NVM_DIR/nvm.sh \
    && cd /build/game-cacher-ui \
    && npm ci
COPY game-cacher-ui /build/game-cacher-ui
RUN set -ex \
    && . $NVM_DIR/nvm.sh \
    && cd /build/game-cacher-ui \
    && npx vite build \
    && cp -r dist /output/ui

FROM redhat/ubi10 AS bin-builder
ARG RUST_VERSION=1.100.0
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN dnf -y install gcc glibc-devel && dnf clean all
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION}
# Only the workspace, so UI and docs edits don't invalidate the cargo layer.
COPY Cargo.toml Cargo.lock /build/
COPY game-cacher /build/game-cacher
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git --mount=type=cache,target=/build/target set -ex \
    && cd /build \
    && cargo build --release --locked --bin game-cacher \
    && mkdir -p /output \
    && cp target/release/game-cacher /output/

FROM redhat/ubi10
RUN dnf -y install ca-certificates && dnf clean all
COPY --from=bin-builder /output/game-cacher /opt/game-cacher/bin/game-cacher
COPY --from=web-builder /output/ui /opt/game-cacher/ui
RUN mkdir -p /data
VOLUME /data
# 53 DNS (udp+tcp), 80 cache HTTP, 443 SNI passthrough, 8080 admin UI/API/metrics.
EXPOSE 53/udp 53/tcp 80 443 8080
ENV GC_STORAGE_PATH=/data GC_UI_PATH=/opt/game-cacher/ui GC_ADMIN_LISTEN=0.0.0.0:8080
# Follows GC_ADMIN_LISTEN's port. (Ignored when podman builds OCI-format images.)
HEALTHCHECK --interval=30s --timeout=5s CMD h="${GC_ADMIN_LISTEN%:*}"; [ "$h" = "0.0.0.0" ] && h=127.0.0.1; curl -fsS "http://$h:${GC_ADMIN_LISTEN##*:}/healthz" || exit 1
ENTRYPOINT [ "/opt/game-cacher/bin/game-cacher" ]
