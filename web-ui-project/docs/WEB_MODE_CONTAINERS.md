# Running Tabularis Web in containers

Official images are published to the GitHub Container Registry for `linux/amd64` and `linux/arm64`:

| Tag | Base | Notes |
| --- | --- | --- |
| `ghcr.io/tabularisdb/tabularis:latest`, `:0.30`, `:0.30.0`, `:debian`, `:0.30.0-debian` | Debian slim | Default. glibc, so registry driver plugins run unchanged. |
| `ghcr.io/tabularisdb/tabularis:alpine`, `:0.30-alpine`, `:0.30.0-alpine` | Alpine | musl. Driver plugins built for glibc run through `gcompat`; prefer Debian if a plugin misbehaves. |

Prereleases get only their exact version tag (`:0.30.0-beta.1`, `:0.30.0-beta.1-alpine`). Both images run as UID `10001`, keep all state in `/data`, and include `ssh` for SSH tunnels. `kubectl` and the AWS CLI are not included; extend the image if you use Kubernetes or SSM tunnels.

The server binary still links WebKitGTK because the desktop and Web modes ship as one executable, so the images are larger than a typical web service. `tabularis web` never opens a window and needs no display.

## Quick start on your machine

```bash
docker run -d --name tabularis \
  -p 127.0.0.1:8080:8080 \
  -v tabularis-data:/data \
  ghcr.io/tabularisdb/tabularis
docker logs tabularis   # prints the generated login password
```

Open <http://localhost:8080>. Publishing on `127.0.0.1` matters: plain HTTP is accepted only for a `localhost`, `127.0.0.0/8`, or `[::1]` public URL, and the server rejects any other `Host` header. To use another host port, publish it and tell the server the URL the browser sees:

```bash
docker run -d -p 127.0.0.1:9000:8080 -e TABULARIS_PUBLIC_URL=http://localhost:9000 ...
```

## Docker Compose

[`docker/compose.yaml`](../../docker/compose.yaml) runs the same local setup. [`docker/compose.tls.yaml`](../../docker/compose.tls.yaml) adds Caddy for a public HTTPS deployment with automatic certificates:

```bash
cd docker
cp .env.example .env          # set TABULARIS_DOMAIN, a password and a secret key
docker compose -f compose.yaml -f compose.tls.yaml up -d
```

Caddy keeps the original `Host` header and upgrades WebSockets, which Tabularis requires. Any other reverse proxy must do the same; see [Remote Web UI security](./WEB_REMOTE_SECURITY.md).

## Kubernetes with Helm

```bash
helm install tabularis oci://ghcr.io/tabularisdb/charts/tabularis \
  --set publicUrl=https://tabularis.example.com \
  --set ingress.enabled=true --set ingress.className=nginx
```

The chart runs one replica with a `Recreate` strategy, because sessions and the data volume belong to one process. It creates a Secret with the login password, the proxy secret and the secret-store key (generated when not set and kept across upgrades), mounts it as files, runs read-only as non-root, and sends the public `Host` header in its probes. Use `auth.existingSecret` to bring your own Secret with the keys `web-password`, `proxy-secret` and `secret-key`, and `image.flavor=alpine` for the Alpine image. See `charts/tabularis/values.yaml` for every option.

## Environment variables

The entrypoint translates these into `tabularis web` flags. Arguments after the image name are appended, so `docker run ghcr.io/tabularisdb/tabularis --debug` works; any other command (for example `sh`) runs instead of the server.

| Variable | Default | Meaning |
| --- | --- | --- |
| `TABULARIS_PUBLIC_URL` | `http://localhost:8080` | URL the browser uses. HTTPS, or HTTP on a loopback host. Becomes `--public-url` and the first `--allowed-origin`. |
| `TABULARIS_ALLOWED_ORIGINS` | | Further origins, comma separated. |
| `TABULARIS_AUTH` | `password` | `password` or `proxy` (an authenticating reverse proxy). |
| `TABULARIS_WEB_PASSWORD` | generated | Login password, at least 12 characters. When unset, a password is generated on first start, stored in `/data/web-password`, and printed once. |
| `TABULARIS_WEB_PROXY_SECRET` | | Required for `proxy` mode, at least 32 characters. |
| `TABULARIS_SECRET_KEY` | generated | Encrypts stored credentials, at least 16 characters. See below. |
| `TABULARIS_ALLOW_HIGH_RISK` | `false` | Grants every session host-level operations. Only for a single trusted user. |
| `TABULARIS_SERVER_FILE_ROOTS` | | Server directories the browser may pick files from, comma separated. |
| `TABULARIS_PORT` | `8080` | Port inside the container. |
| `TABULARIS_HOST` | `0.0.0.0` | Bind address inside the container. |
| `TABULARIS_DEBUG` | `false` | Debug logging. |

`TABULARIS_WEB_PASSWORD`, `TABULARIS_WEB_PROXY_SECRET` and `TABULARIS_SECRET_KEY` also accept a `_FILE` variant pointing at a Docker or Kubernetes secret file.

## Stored credentials

Containers have no OS keychain. The images set `TABULARIS_SECRET_STORE=file`, which keeps every secret the desktop app would put in the keychain (database and SSH passwords, connection URIs, plugin secrets, AI keys) in `/data/secrets.enc.json`, encrypted with AES-256-GCM under a key derived from `TABULARIS_SECRET_KEY` with Argon2id.

Without `TABULARIS_SECRET_KEY`, a random key is generated in `/data/secrets.key`. Secrets then survive restarts and upgrades, but anyone who can read the volume can read them. Set your own key in production and back it up separately from the volume: a lost key makes the stored credentials unrecoverable, and a wrong key stops the container at startup instead of silently starting empty.

`TABULARIS_SECRET_STORE=file` also works outside containers, for example on a headless server without a Secret Service. `TABULARIS_SECRET_STORE_PATH` moves the store file.

## Health, logs and shutdown

`GET /healthz` is a liveness check. It still enforces the `Host` allowlist, so the image's `HEALTHCHECK` sends the public host. Logs go to stderr (`docker logs`), and audit events use the `tabularis::web_audit` target. `docker stop` sends `SIGTERM`; the server finishes requests, closes pools and tunnels, and stops driver plugins before exiting.

## Building the images yourself

```bash
docker build -f docker/Dockerfile --target debian -t tabularis:debian .
docker build -f docker/Dockerfile --target alpine -t tabularis:alpine .
docker/smoke-test.sh tabularis:debian
```

The build compiles the frontend once on the build platform and the Rust server for the target platform. Release images are built on native `amd64` and `arm64` runners by `.github/workflows/docker.yml`, which also publishes the Helm chart.
