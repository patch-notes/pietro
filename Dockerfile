# syntax=docker/dockerfile:1.7
#
# Pietro container — three stages:
#
#   1) frontend   : node-alpine, builds the React SPA into frontend/dist/.
#   2) backend    : rust-alpine, builds a fully-static musl binary with the
#                   SPA already in place (rust-embed picks it up at compile
#                   time, build.rs enforces that index.html exists for
#                   release builds — see pietro.md §13 / src/spa.rs).
#   3) runtime    : FROM scratch, just the static binary + a CA bundle so
#                   reqwest (rustls) can talk to the OIDC issuer over TLS.
#
# Multi-arch: every FROM image (node:20-alpine, rust:1-alpine) ships both
# linux/amd64 and linux/arm64 manifests; `cargo build --release` inside
# rust-alpine compiles natively for whichever $TARGETPLATFORM podman is
# building right now. No cross-toolchain dance needed — qemu-user handles
# foreign-arch builds when invoked via `podman build --platform=...`.
#
# Build (multi-arch, via the Makefile wrapper):
#   make docker                          # both arches → manifest list
#   make docker-build-amd64              # just amd64 (fast, native)
#
# Run:
#   podman run --rm -p 18080:18080 \
#     -v $PWD/pietro.yaml:/etc/pietro/pietro.yaml:ro \
#     -e PIETRO_COOKIE_KEY=... -e PIETRO_API_KEY_PEPPER=... \
#     pietro:latest serve --config /etc/pietro/pietro.yaml

############################
# 1) Frontend (React + Vite)
############################
FROM mirror.gcr.io/library/node:20-alpine AS frontend

WORKDIR /app/frontend

# Install deps with a clean, reproducible lockfile resolve. Copy package
# manifests first so this layer caches when only source changes.
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci

# Now the rest of the frontend tree.
COPY frontend/ ./

# Produces frontend/dist/{index.html,assets/...}
RUN npm run build


############################
# 2) Backend (Rust → musl)
############################
FROM mirror.gcr.io/library/rust:1-alpine AS backend

# musl-dev gives us the C runtime headers the linker needs for fully-static
# binaries; the rest are tiny build-time conveniences.
RUN apk add --no-cache musl-dev

WORKDIR /src

# rust:alpine targets <arch>-unknown-linux-musl natively, so the default
# `cargo build --release` already produces a static binary. We still set
# RUSTFLAGS=-Ctarget-feature=-crt-static off (the default is +crt-static on
# alpine) only if needed; we WANT crt-static, so leave it alone.
ENV CARGO_TERM_COLOR=always

# Copy the manifests first to maximise layer cache on dep changes.
COPY Cargo.toml Cargo.lock ./

# Bring in the rest of the backend sources.
COPY build.rs ./
COPY migrations/ ./migrations/
COPY src/ ./src/

# Pull in the freshly-built SPA so rust-embed can slurp it at compile time.
COPY --from=frontend /app/frontend/dist ./frontend/dist

# Static musl release build. `--locked` keeps Cargo.lock honest.
RUN cargo build --release --locked

# Sanity: confirm the binary is actually static. On Alpine, `ldd` is
# musl's, which prints `lib => /path` lines for any dynamic dependency
# and either errors out or says "Not a valid dynamic program" for fully
# static binaries. We use the absence of `=>` arrows as the static signal.
RUN set -eux; \
    if ldd target/release/pietro 2>/dev/null | grep -q '=>'; then \
        echo "ERROR: pietro has dynamic library dependencies" >&2; \
        ldd target/release/pietro >&2; \
        exit 1; \
    fi; \
    echo "ok: pietro is statically linked"


############################
# 3) Runtime (scratch)
############################
FROM scratch AS runtime

# CA bundle so the OIDC discovery + token round-trip (rustls) can verify
# upstream certificates. ~200 KB; the only thing we add to scratch.
COPY --from=backend /etc/ssl/cert.pem /etc/ssl/cert.pem

# The binary itself.
COPY --from=backend /src/target/release/pietro /pietro

# Pietro listens on whatever `listen_addr` says in pietro.yaml; 18080 is the
# example default. Documenting it here is informational only.
EXPOSE 18080

# No shell in scratch — use exec form. The operator supplies `serve --config
# ...` (or `migrate`) at `docker run` time.
ENTRYPOINT ["/pietro"]
CMD ["serve", "--config", "/etc/pietro/pietro.yaml"]
