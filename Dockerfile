# syntax=docker/dockerfile:1.7
#
# The erpcraft server with its admin UI, for production.
#
#   docker build -t erpcraft .
#
# Three stages: the UI is built with Bun, the server in release mode with
# Rust, and both are copied into a distroless image that has no shell and
# runs as an unprivileged user. See docs/ops/self-hosting.md.

# ---------------------------------------------------------------- the UI
FROM oven/bun:1.3.9 AS ui
WORKDIR /src
# The grid package is used from its source by the UI.
COPY packages/grid ./packages/grid
COPY ui/package.json ui/bun.lock ./ui/
RUN cd ui && bun install --frozen-lockfile
COPY ui ./ui
RUN cd ui && bun run build

# ---------------------------------------------------------------- the server
FROM rust:1.95-slim-bookworm AS server
# aws-lc (the crypto under rustls) is built from C. The package lists are
# not kept: this stage is thrown away, but its layers are cached.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake perl clang \
    && apt-get clean
WORKDIR /src
# ARCHITECTURE.md is served to agents by the MCP endpoint.
COPY Cargo.toml Cargo.lock ARCHITECTURE.md ./
COPY crates ./crates
COPY modules ./modules
COPY apps ./apps
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin erpcraft \
    && cp target/release/erpcraft /usr/local/bin/erpcraft

# ---------------------------------------------------------------- runtime
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=server /usr/local/bin/erpcraft /usr/local/bin/erpcraft
COPY --from=ui /src/ui/dist /app/ui
ENV ERPCRAFT_BIND=0.0.0.0:8600 \
    ERPCRAFT_UI_DIR=/app/ui \
    RUST_LOG=info
USER nonroot:nonroot
EXPOSE 8600
# No shell or curl in the image: the binary asks its own /healthz.
HEALTHCHECK --interval=15s --timeout=5s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/erpcraft", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/erpcraft"]
