# syntax=docker/dockerfile:1.7
#
# Tabularis Web as a container image:
#   docker build -f docker/Dockerfile -t tabularis .
#
# The binary still links WebKitGTK (Tauri is not optional), so the runtime
# carries those shared libraries even though `tabularis web` never opens a window.

ARG NODE_VERSION=24.18.0
ARG DEBIAN_VERSION=trixie

# ── Frontend: platform independent, built once on the build host ───────────
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_VERSION}-slim AS web-ui
WORKDIR /src
RUN corepack enable
COPY pnpm-lock.yaml pnpm-workspace.yaml package.json ./
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm fetch --frozen-lockfile
COPY roadmap.json ./
COPY packages ./packages
COPY plugins/manifest.schema.json ./plugins/manifest.schema.json
RUN --mount=type=cache,id=pnpm-store,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile --offline \
    && pnpm build

# ── Server ───────────────────────────────────────────────────────────────────
FROM rust:1-${DEBIAN_VERSION} AS server
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libdbus-1-dev \
        pkg-config perl make \
    && rm -rf /var/lib/apt/lists/*

# Reported by the server contract (`option_env!`); empty for local builds.
ARG TABULARIS_BUILD_COMMIT=
WORKDIR /src
# The crate embeds the built frontend (`tauri::generate_context!`) and a few
# JSON schemas from outside src-tauri.
COPY plugins/manifest.schema.json ./plugins/manifest.schema.json
COPY packages/web-ui/src/schemas ./packages/web-ui/src/schemas
COPY packages/web-ui/src/themes ./packages/web-ui/src/themes
COPY --from=web-ui /src/packages/web-ui/dist ./packages/web-ui/dist
COPY src-tauri ./src-tauri
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target,target=/src/src-tauri/target \
    cd src-tauri \
    && cargo build --release --locked --bin tabularis \
    && install -Dm755 target/release/tabularis /out/tabularis

# ── Shared libraries ─────────────────────────────────────────────────────────
# Installing WebKitGTK drags in Mesa/LLVM, GStreamer, speech synthesis and more
# (~700 MB) that `tabularis web` never loads: GTK is never initialised. Only
# the libraries the binary links (its `ldd` closure) go into the runtime,
# under their canonical directory so merged-/usr symlinks such as /lib stay.
FROM debian:${DEBIAN_VERSION}-slim AS libs
RUN apt-get update \
    && apt-get install -y --no-install-recommends libwebkit2gtk-4.1-0 libdbus-1-3
COPY --from=server /out/tabularis /usr/bin/tabularis
RUN mkdir /libs \
    && ldd /usr/bin/tabularis | awk '/=> \// { print $3 }' | while read -r lib; do \
        dir="$(readlink -f "$(dirname "$lib")")"; \
        mkdir -p "/libs$dir" && cp -L "$lib" "/libs$dir/"; \
    done

# ── Runtime ──────────────────────────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION}-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /data --no-create-home tabularis \
    && install -d -o 10001 -g 10001 -m 0750 /data
COPY --from=libs /libs/ /
COPY --from=server /out/tabularis /usr/bin/tabularis
# Fails the build if a linked library is missing.
RUN ldconfig && tabularis --help >/dev/null
COPY --from=web-ui /src/packages/web-ui/dist /usr/lib/tabularis/web-ui
COPY docker/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
ENV HOME=/data \
    TABULARIS_DATA_DIR=/data \
    TABULARIS_SECRET_STORE=file \
    TABULARIS_PORT=8080
USER 10001:10001
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD ["docker-entrypoint.sh", "healthcheck"]
ENTRYPOINT ["tini", "--", "docker-entrypoint.sh"]
CMD ["web"]
