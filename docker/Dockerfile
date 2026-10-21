# syntax=docker/dockerfile:1.7
#
# Tabularis Web as a container image, in two flavours:
#   docker build -f docker/Dockerfile --target debian -t tabularis:debian .
#   docker build -f docker/Dockerfile --target alpine -t tabularis:alpine .
#
# The binary still links WebKitGTK (Tauri is not optional), so both runtimes
# carry those shared libraries even though `tabularis web` never opens a window.

ARG NODE_VERSION=24.18.0
ARG DEBIAN_VERSION=trixie
ARG ALPINE_VERSION=3.22

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

# ── Rust builders: one per libc ──────────────────────────────────────────────
FROM rust:1-${DEBIAN_VERSION} AS build-debian
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libdbus-1-dev \
        pkg-config perl make \
    && rm -rf /var/lib/apt/lists/*

FROM rust:1-alpine${ALPINE_VERSION} AS build-alpine
RUN apk add --no-cache \
        build-base pkgconf perl make linux-headers \
        webkit2gtk-4.1-dev gtk+3.0-dev libsoup3-dev dbus-dev
# GTK and WebKit are shared libraries: link against them dynamically.
ENV RUSTFLAGS="-C target-feature=-crt-static"

# The same build on both libcs. The crate embeds the built frontend
# (`tauri::generate_context!`) and a few JSON schemas from outside src-tauri.
FROM build-debian AS server-debian
# Reported by the server contract (`option_env!`); empty for local builds.
ARG TABULARIS_BUILD_COMMIT=
WORKDIR /src
COPY plugins/manifest.schema.json ./plugins/manifest.schema.json
COPY packages/web-ui/src/schemas ./packages/web-ui/src/schemas
COPY packages/web-ui/src/themes ./packages/web-ui/src/themes
COPY --from=web-ui /src/packages/web-ui/dist ./packages/web-ui/dist
COPY src-tauri ./src-tauri
RUN --mount=type=cache,id=cargo-registry-debian,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target-debian,target=/src/src-tauri/target \
    cd src-tauri \
    && cargo build --release --locked --bin tabularis \
    && install -Dm755 target/release/tabularis /out/tabularis

FROM build-alpine AS server-alpine
# Reported by the server contract (`option_env!`); empty for local builds.
ARG TABULARIS_BUILD_COMMIT=
WORKDIR /src
COPY plugins/manifest.schema.json ./plugins/manifest.schema.json
COPY packages/web-ui/src/schemas ./packages/web-ui/src/schemas
COPY packages/web-ui/src/themes ./packages/web-ui/src/themes
COPY --from=web-ui /src/packages/web-ui/dist ./packages/web-ui/dist
COPY src-tauri ./src-tauri
RUN --mount=type=cache,id=cargo-registry-alpine,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-target-alpine,target=/src/src-tauri/target \
    cd src-tauri \
    && cargo build --release --locked --bin tabularis \
    && install -Dm755 target/release/tabularis /out/tabularis

# ── Shared libraries ─────────────────────────────────────────────────────────
# Installing WebKitGTK drags in Mesa/LLVM, GStreamer, speech synthesis and more
# (~700 MB) that `tabularis web` never loads: GTK is never initialised. Only
# the libraries the binary links (its `ldd` closure) go into the runtime,
# under their canonical directory so merged-/usr symlinks such as /lib stay.
FROM debian:${DEBIAN_VERSION}-slim AS libs-debian
RUN apt-get update \
    && apt-get install -y --no-install-recommends libwebkit2gtk-4.1-0 libdbus-1-3
COPY --from=server-debian /out/tabularis /usr/bin/tabularis
RUN mkdir /libs \
    && ldd /usr/bin/tabularis | awk '/=> \// { print $3 }' | while read -r lib; do \
        dir="$(readlink -f "$(dirname "$lib")")"; \
        mkdir -p "/libs$dir" && cp -L "$lib" "/libs$dir/"; \
    done

FROM alpine:${ALPINE_VERSION} AS libs-alpine
RUN apk add --no-cache webkit2gtk-4.1 dbus-libs
COPY --from=server-alpine /out/tabularis /usr/bin/tabularis
RUN mkdir /libs \
    && ldd /usr/bin/tabularis | awk '/=> \// { print $3 }' | while read -r lib; do \
        dir="$(readlink -f "$(dirname "$lib")")"; \
        mkdir -p "/libs$dir" && cp -L "$lib" "/libs$dir/"; \
    done

# ── Runtimes ─────────────────────────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION}-slim AS debian
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /data --no-create-home tabularis \
    && install -d -o 10001 -g 10001 -m 0750 /data
COPY --from=libs-debian /libs/ /
COPY --from=server-debian /out/tabularis /usr/bin/tabularis
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

FROM alpine:${ALPINE_VERSION} AS alpine
# gcompat lets the glibc binaries of registry driver plugins start on musl.
RUN apk add --no-cache ca-certificates curl tini openssh-client gcompat \
    && addgroup -S -g 10001 tabularis \
    && adduser -S -D -H -u 10001 -G tabularis -h /data tabularis \
    && install -d -o 10001 -g 10001 -m 0750 /data
COPY --from=libs-alpine /libs/ /
COPY --from=server-alpine /out/tabularis /usr/bin/tabularis
# Fails the build if a linked library is missing.
RUN tabularis --help >/dev/null
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
ENTRYPOINT ["/sbin/tini", "--", "docker-entrypoint.sh"]
CMD ["web"]
